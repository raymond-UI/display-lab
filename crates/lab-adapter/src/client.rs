//! The lab's side of the protocol: an [`Adapter`] backed by JSON-RPC over a
//! pair of streams, usually a child process's stdin and stdout.

use crate::protocol::{compatible, AppInfo, Binding, WireFrame, PROTOCOL_VERSION};
use crate::{Adapter, AdapterError};
use lab_core::{Frame, ScreenInfo};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How long one call may take. Rendering an expected frame at 4K in a debug
/// build is the slowest legitimate answer; nothing should come close.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct RpcAdapter {
    info: AppInfo,
    writer: Box<dyn Write + Send>,
    lines: Receiver<std::io::Result<String>>,
    child: Option<Child>,
    next_id: u64,
    timeout: Duration,
}

impl RpcAdapter {
    /// Start the adapter process `command[0]` with `command[1..]` and say
    /// hello. Its stderr is passed through, so an adapter can log there.
    pub fn spawn(command: &[String]) -> Result<RpcAdapter, AdapterError> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| AdapterError::Transport("empty adapter command".into()))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| AdapterError::Transport(format!("starting {program}: {e}")))?;
        let missing = || AdapterError::Transport("adapter process has no stdio".into());
        let stdin = child.stdin.take().ok_or_else(missing)?;
        let stdout = child.stdout.take().ok_or_else(missing)?;
        let mut adapter = RpcAdapter::connect(stdout, stdin)?;
        adapter.child = Some(child);
        Ok(adapter)
    }

    /// Speak the protocol over any pair of streams.
    pub fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Result<RpcAdapter, AdapterError> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("adapter-reader".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let end = line.is_err();
                    if tx.send(line).is_err() || end {
                        break;
                    }
                }
            })
            .map_err(|e| AdapterError::Transport(format!("reader thread: {e}")))?;

        let mut adapter = RpcAdapter {
            info: AppInfo {
                name: String::new(),
                version: String::new(),
                bundle_id: None,
                protocol: PROTOCOL_VERSION.into(),
                capabilities: Default::default(),
            },
            writer: Box::new(writer),
            lines: rx,
            child: None,
            next_id: 1,
            timeout: CALL_TIMEOUT,
        };
        let hello = adapter.call("initialize", json!({ "protocol": PROTOCOL_VERSION }))?;
        let info: AppInfo = serde_json::from_value(hello)
            .map_err(|e| AdapterError::Protocol(format!("initialize result: {e}")))?;
        if !compatible(&info.protocol) {
            return Err(AdapterError::Protocol(format!(
                "adapter speaks protocol {}, the lab speaks {PROTOCOL_VERSION}",
                info.protocol
            )));
        }
        adapter.info = info;
        Ok(adapter)
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, AdapterError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = request.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
            .map_err(|e| AdapterError::Transport(format!("writing {method}: {e}")))?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => return Err(AdapterError::Transport(format!("reading: {e}"))),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AdapterError::Timeout {
                        method: method.into(),
                        seconds: self.timeout.as_secs(),
                    })
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AdapterError::Transport(format!(
                        "adapter closed its output while the lab waited for {method}"
                    )))
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Response = serde_json::from_str(&line)
                .map_err(|e| AdapterError::Protocol(format!("{e}: {line}")))?;
            // A message without an id is a notification (an adapter's log
            // line, say). Nothing in protocol 1 needs one; skip it.
            let Some(got) = message.id else { continue };
            if got != id {
                return Err(AdapterError::Protocol(format!(
                    "answer to request {got} arrived while waiting for {id}"
                )));
            }
            return match (message.result, message.error) {
                (_, Some(e)) => Err(AdapterError::Remote {
                    code: e.code,
                    message: e.message,
                }),
                (Some(result), None) => Ok(result),
                (None, None) => Ok(Value::Null),
            };
        }
    }

    fn need(&self, capability: bool, name: &'static str) -> Result<(), AdapterError> {
        if capability {
            Ok(())
        } else {
            Err(AdapterError::Unsupported(name))
        }
    }
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl Adapter for RpcAdapter {
    fn info(&self) -> &AppInfo {
        &self.info
    }

    fn launch(&mut self, screens: &[ScreenInfo]) -> Result<(), AdapterError> {
        self.call("launch", json!({ "screens": screens })).map(drop)
    }

    fn setup(&mut self) -> Result<(), AdapterError> {
        self.need(self.info.capabilities.setup, "setup")?;
        self.call("setup", json!({})).map(drop)
    }

    fn input(&mut self, action: &str, args: &Value) -> Result<(), AdapterError> {
        self.call("input", json!({ "action": action, "args": args }))
            .map(drop)
    }

    fn state(&mut self) -> Result<Value, AdapterError> {
        self.need(self.info.capabilities.state, "state")?;
        self.call("state", json!({}))
    }

    fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
        self.need(self.info.capabilities.bindings, "bindings")?;
        #[derive(Deserialize)]
        struct Answer {
            bindings: Vec<Binding>,
        }
        let value = self.call("bindings", json!({}))?;
        let answer: Answer = serde_json::from_value(value)
            .map_err(|e| AdapterError::Protocol(format!("bindings result: {e}")))?;
        Ok(answer.bindings)
    }

    fn expected_frame(
        &mut self,
        output: &str,
        width: u32,
        height: u32,
    ) -> Result<Frame, AdapterError> {
        self.need(self.info.capabilities.expected_frame, "expected_frame")?;
        let value = self.call(
            "expected_frame",
            json!({ "output": output, "width": width, "height": height }),
        )?;
        let wire: WireFrame = serde_json::from_value(value)
            .map_err(|e| AdapterError::Protocol(format!("expected_frame result: {e}")))?;
        let frame = wire.decode().map_err(AdapterError::Protocol)?;
        if (frame.width, frame.height) != (width, height) {
            return Err(AdapterError::Protocol(format!(
                "asked for {width}x{height}, got {}x{}",
                frame.width, frame.height
            )));
        }
        Ok(frame)
    }

    fn stop(&mut self) -> Result<(), AdapterError> {
        let result = self.call("shutdown", json!({})).map(drop);
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        result
    }
}

impl Drop for RpcAdapter {
    fn drop(&mut self) {
        // A run that panicked or bailed out early must not leave the app's
        // adapter running.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
