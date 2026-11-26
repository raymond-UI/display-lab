//! A small, blocking MCP client: enough to call tools, nothing more.
//!
//! Two transports. Streamable HTTP posts one JSON-RPC message per request
//! and accepts either a JSON reply or a short SSE stream; it keeps the
//! session id and protocol header the server hands out. Stdio spawns the
//! server and speaks one JSON message per line.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// Replies bigger than this are refused. Frames come back as PNGs, and a 4K
/// PNG in base64 is well under it.
const MAX_REPLY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Nothing answered: the server is not up (yet), or the stdio server
    /// exited.
    #[error("{0}")]
    Unreachable(String),
    #[error("no answer to {method} within {seconds} s")]
    Timeout { method: String, seconds: u64 },
    /// A JSON-RPC error from the server.
    #[error("MCP error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("{0}")]
    Protocol(String),
}

/// One tool's result, with the parts the adapter reads pulled out.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub is_error: bool,
    /// `structuredContent`, or else the first text block if it parses as
    /// JSON.
    pub data: Option<Value>,
    /// Every text block, joined, for error messages.
    pub text: String,
    /// `(mime type, base64 data)` of each image block, in order.
    pub images: Vec<(String, String)>,
}

impl ToolResult {
    fn from_value(v: &Value) -> ToolResult {
        let blocks = v.get("content").and_then(Value::as_array);
        let blocks = blocks.map(Vec::as_slice).unwrap_or_default();
        let texts: Vec<&str> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        let images = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
            .filter_map(|b| {
                let data = b.get("data").and_then(Value::as_str)?;
                let mime = b.get("mimeType").and_then(Value::as_str).unwrap_or("");
                Some((mime.to_string(), data.to_string()))
            })
            .collect();
        let data = v
            .get("structuredContent")
            .filter(|s| !s.is_null())
            .cloned()
            .or_else(|| texts.iter().find_map(|t| serde_json::from_str(t).ok()));
        ToolResult {
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
            data,
            text: texts.join("\n"),
            images,
        }
    }
}

enum Transport {
    Http(Http),
    Stdio(Stdio_),
}

pub struct McpClient {
    transport: Transport,
    next_id: u64,
    timeout: Duration,
    /// The revision the server agreed to, once initialized.
    pub protocol: Option<String>,
    pub server_name: Option<String>,
}

impl McpClient {
    pub fn http(url: &str, timeout: Duration) -> McpClient {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            // MCP servers for desktop apps are local; never route them
            // through a proxy from the environment.
            .proxy(None)
            .build()
            .into();
        McpClient::new(
            Transport::Http(Http {
                agent,
                url: url.to_string(),
                session: None,
                protocol: None,
            }),
            timeout,
        )
    }

    pub fn stdio(command: &[String], timeout: Duration) -> Result<McpClient, McpError> {
        Ok(McpClient::new(
            Transport::Stdio(Stdio_::spawn(command)?),
            timeout,
        ))
    }

    fn new(transport: Transport, timeout: Duration) -> McpClient {
        McpClient {
            transport,
            next_id: 1,
            timeout,
            protocol: None,
            server_name: None,
        }
    }

    /// The handshake: `initialize`, then `notifications/initialized`.
    pub fn initialize(&mut self, protocol: &str) -> Result<(), McpError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": protocol,
                "capabilities": {},
                "clientInfo": {"name": "display-lab", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        let agreed = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(protocol)
            .to_string();
        self.server_name = result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Transport::Http(h) = &mut self.transport {
            h.protocol = Some(agreed.clone());
        }
        self.protocol = Some(agreed);
        self.notify("notifications/initialized", json!({}))
    }

    pub fn call_tool(&mut self, name: &str, args: &Value) -> Result<ToolResult, McpError> {
        let result = self.request("tools/call", json!({"name": name, "arguments": args}))?;
        Ok(ToolResult::from_value(&result))
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = match &mut self.transport {
            Transport::Http(h) => h.send(&message, method, Some(id), self.timeout)?,
            Transport::Stdio(s) => s.send(&message, method, Some(id), self.timeout)?,
        };
        let reply = reply
            .ok_or_else(|| McpError::Protocol(format!("the server sent no reply to {method}")))?;
        if let Some(err) = reply.get("error") {
            return Err(McpError::Rpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no message")
                    .to_string(),
            });
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol(format!("reply to {method} has no result: {reply}")))
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &mut self.transport {
            Transport::Http(h) => h.send(&message, method, None, self.timeout)?,
            Transport::Stdio(s) => s.send(&message, method, None, self.timeout)?,
        };
        Ok(())
    }

    pub fn close(&mut self) {
        if let Transport::Stdio(s) = &mut self.transport {
            s.close();
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.close();
    }
}

struct Http {
    agent: ureq::Agent,
    url: String,
    session: Option<String>,
    protocol: Option<String>,
}

impl Http {
    /// Post one message. `id` is the request id to find in the reply, or
    /// `None` for a notification, which gets no reply.
    fn send(
        &mut self,
        message: &Value,
        method: &str,
        id: Option<u64>,
        timeout: Duration,
    ) -> Result<Option<Value>, McpError> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(p) = &self.protocol {
            req = req.header("MCP-Protocol-Version", p);
        }
        let mut resp = req.send(message.to_string()).map_err(|e| match e {
            ureq::Error::Timeout(_) => McpError::Timeout {
                method: method.into(),
                seconds: timeout.as_secs(),
            },
            other => McpError::Unreachable(format!("{}: {other}", self.url)),
        })?;
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(s.to_string());
        }
        let status = resp.status().as_u16();
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_REPLY_BYTES)
            .read_to_string()
            .map_err(|e| match e {
                ureq::Error::Timeout(_) => McpError::Timeout {
                    method: method.into(),
                    seconds: timeout.as_secs(),
                },
                other => McpError::Protocol(format!("reading the reply to {method}: {other}")),
            })?;
        if !(200..300).contains(&status) {
            let mut snippet: String = body.chars().take(300).collect();
            if snippet.is_empty() {
                snippet = "(empty body)".into();
            }
            return Err(McpError::Protocol(format!(
                "HTTP {status} for {method}: {snippet}"
            )));
        }
        let Some(id) = id else { return Ok(None) };
        let messages = if sse {
            sse_messages(&body)
        } else if body.trim().is_empty() {
            vec![]
        } else {
            vec![serde_json::from_str(&body)
                .map_err(|e| McpError::Protocol(format!("reply to {method} is not JSON: {e}")))?]
        };
        Ok(find_reply(messages, id))
    }
}

/// The JSON messages in an SSE body: each event's `data:` lines, joined.
fn sse_messages(body: &str) -> Vec<Value> {
    let mut out = vec![];
    for event in body.replace("\r\n", "\n").split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str(&data.join("\n")) {
            out.push(v);
        }
    }
    out
}

/// The reply with `id`, from a single message or a batch.
fn find_reply(messages: Vec<Value>, id: u64) -> Option<Value> {
    messages
        .into_iter()
        .flat_map(|m| match m {
            Value::Array(items) => items,
            one => vec![one],
        })
        .find(|m| m.get("id").and_then(Value::as_u64) == Some(id) && m.get("method").is_none())
}

struct Stdio_ {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Stdio_ {
    fn spawn(command: &[String]) -> Result<Stdio_, McpError> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| McpError::Unreachable("empty server command".into()))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| McpError::Unreachable(format!("starting {program:?}: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Unreachable("no stdout from the server".into()))?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Stdio_ {
            stdin: child.stdin.take(),
            child,
            lines,
        })
    }

    fn send(
        &mut self,
        message: &Value,
        method: &str,
        id: Option<u64>,
        timeout: Duration,
    ) -> Result<Option<Value>, McpError> {
        let gone = || McpError::Unreachable("the MCP server exited".into());
        let stdin = self.stdin.as_mut().ok_or_else(gone)?;
        writeln!(stdin, "{message}")
            .and_then(|_| stdin.flush())
            .map_err(|_| gone())?;
        let Some(id) = id else { return Ok(None) };
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(McpError::Timeout {
                        method: method.into(),
                        seconds: timeout.as_secs(),
                    })
                }
                Err(RecvTimeoutError::Disconnected) => return Err(gone()),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue; // a log line on stdout; not ours to judge
            };
            // A server request (ping, sampling) waits for an answer we do
            // not give; refusing it keeps the server from hanging.
            if let (Some(req_id), Some(_)) = (v.get("id"), v.get("method")) {
                let refuse = json!({"jsonrpc": "2.0", "id": req_id,
                    "error": {"code": -32601, "message": "display-lab handles no server requests"}});
                let _ = writeln!(stdin, "{refuse}").and_then(|_| stdin.flush());
                continue;
            }
            if let Some(reply) = find_reply(vec![v], id) {
                return Ok(Some(reply));
            }
        }
    }

    fn close(&mut self) {
        // Closing stdin is how the stdio transport asks a server to exit.
        self.stdin.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_bodies_yield_their_messages() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    id: 2\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\n\
                    data: \"result\":{}}\n\n";
        let messages = sse_messages(body);
        assert_eq!(messages.len(), 2);
        assert!(find_reply(messages, 7).is_some());
    }

    #[test]
    fn tool_results_prefer_structured_content() {
        let r = ToolResult::from_value(&json!({
            "content": [{"type": "text", "text": "{\"a\": 1}"},
                        {"type": "image", "data": "AAAA", "mimeType": "image/png"}],
            "structuredContent": {"a": 2}
        }));
        assert_eq!(r.data, Some(json!({"a": 2})));
        assert_eq!(
            r.images,
            vec![("image/png".to_string(), "AAAA".to_string())]
        );
        assert!(!r.is_error);

        let r = ToolResult::from_value(&json!({
            "content": [{"type": "text", "text": "rendered"}, {"type": "text", "text": "{\"a\": 1}"}],
            "isError": true
        }));
        assert_eq!(r.data, Some(json!({"a": 1})));
        assert!(r.is_error);
        assert_eq!(r.text, "rendered\n{\"a\": 1}");
    }
}
