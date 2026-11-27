//! The adapter's side of the protocol, for adapters written in Rust:
//! implement [`Adapter`] and call [`serve`] with stdin and stdout.

use crate::protocol::{compatible, WireFrame, PROTOCOL_VERSION};
use crate::{Adapter, AdapterError};
use lab_core::ScreenInfo;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// The app does not offer what was asked (protocol-specific, in the
/// implementation-defined range).
pub const UNSUPPORTED: i64 = -32001;
/// The adapter tried and failed.
pub const APP_ERROR: i64 = -32002;

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Answer requests until `shutdown` or end of input. Returns once the
/// adapter has been stopped.
pub fn serve<A: Adapter>(
    mut adapter: A,
    input: impl Read,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in BufReader::new(input).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                reply(&mut output, Value::Null, Err((PARSE_ERROR, e.to_string())))?;
                continue;
            }
        };
        let Some(id) = request.id else {
            // Notifications need no answer, and protocol 1 defines none.
            continue;
        };
        let shutdown = request.method == "shutdown";
        let result = dispatch(&mut adapter, &request.method, request.params);
        reply(&mut output, id, result)?;
        if shutdown {
            return Ok(());
        }
    }
    // The lab went away without saying goodbye. Stop the app anyway.
    let _ = adapter.stop();
    Ok(())
}

fn reply(
    out: &mut impl Write,
    id: Value,
    result: Result<Value, (i64, String)>,
) -> std::io::Result<()> {
    let message = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    let mut line = message.to_string();
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}

fn dispatch<A: Adapter>(
    adapter: &mut A,
    method: &str,
    params: Value,
) -> Result<Value, (i64, String)> {
    fn args<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, (i64, String)> {
        serde_json::from_value(params).map_err(|e| (INVALID_PARAMS, e.to_string()))
    }
    let app = |e: AdapterError| match e {
        AdapterError::Unsupported(what) => (UNSUPPORTED, format!("not offered: {what}")),
        other => (APP_ERROR, other.to_string()),
    };

    match method {
        "initialize" => {
            #[derive(Deserialize)]
            struct P {
                protocol: String,
            }
            let p: P = args(params)?;
            if !compatible(&p.protocol) {
                return Err((
                    INVALID_REQUEST,
                    format!(
                        "lab speaks protocol {}, this adapter speaks {PROTOCOL_VERSION}",
                        p.protocol
                    ),
                ));
            }
            serde_json::to_value(adapter.info()).map_err(|e| (APP_ERROR, e.to_string()))
        }
        "launch" => {
            #[derive(Deserialize)]
            struct P {
                #[serde(default)]
                screens: Vec<ScreenInfo>,
            }
            let p: P = args(params)?;
            adapter.launch(&p.screens).map_err(app).map(|_| json!({}))
        }
        "setup" => adapter.setup().map_err(app).map(|_| json!({})),
        "input" => {
            #[derive(Deserialize)]
            struct P {
                action: String,
                #[serde(default)]
                args: Value,
            }
            let p: P = args(params)?;
            adapter
                .input(&p.action, &p.args)
                .map_err(app)
                .map(|_| json!({}))
        }
        "state" => adapter.state().map_err(app),
        "bindings" => adapter
            .bindings()
            .map_err(app)
            .map(|b| json!({ "bindings": b })),
        "expected_frame" => {
            #[derive(Deserialize)]
            struct P {
                output: String,
                width: u32,
                height: u32,
            }
            let p: P = args(params)?;
            adapter
                .expected_frame(&p.output, p.width, p.height)
                .map_err(app)
                .and_then(|f| {
                    serde_json::to_value(WireFrame::encode(&f))
                        .map_err(|e| (APP_ERROR, e.to_string()))
                })
        }
        "shutdown" => adapter.stop().map_err(app).map(|_| json!({})),
        other => Err((METHOD_NOT_FOUND, format!("no method {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppInfo, Binding, Capabilities, RpcAdapter, ScreenRef};
    use lab_core::Frame;
    use std::sync::{Arc, Mutex};

    /// A tiny app: a counter that `next` advances, drawn as a grey level.
    struct Counter {
        info: AppInfo,
        log: Arc<Mutex<Vec<String>>>,
        n: u8,
    }

    impl Adapter for Counter {
        fn info(&self) -> &AppInfo {
            &self.info
        }
        fn launch(&mut self, screens: &[ScreenInfo]) -> Result<(), AdapterError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("launch {}", screens.len()));
            Ok(())
        }
        fn input(&mut self, action: &str, _args: &Value) -> Result<(), AdapterError> {
            match action {
                "next" => {
                    self.n += 1;
                    Ok(())
                }
                other => Err(AdapterError::Remote {
                    code: 1,
                    message: format!("unknown action {other}"),
                }),
            }
        }
        fn state(&mut self) -> Result<Value, AdapterError> {
            Ok(json!({ "slide": self.n }))
        }
        fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
            Ok(vec![Binding {
                output: "main".into(),
                screen: Some(ScreenRef {
                    name: Some("Projector".into()),
                    ..Default::default()
                }),
            }])
        }
        fn expected_frame(
            &mut self,
            _output: &str,
            width: u32,
            height: u32,
        ) -> Result<Frame, AdapterError> {
            Ok(Frame::solid(width, height, [self.n, self.n, self.n, 255]))
        }
        fn stop(&mut self) -> Result<(), AdapterError> {
            self.log.lock().unwrap().push("stop".into());
            Ok(())
        }
    }

    fn pair(
        caps: Capabilities,
    ) -> (
        RpcAdapter,
        Arc<Mutex<Vec<String>>>,
        std::thread::JoinHandle<()>,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let app = Counter {
            info: AppInfo {
                name: "counter".into(),
                version: "0".into(),
                bundle_id: None,
                protocol: PROTOCOL_VERSION.into(),
                capabilities: caps,
            },
            log: log.clone(),
            n: 0,
        };
        let (to_app_r, to_app_w) = std::io::pipe().unwrap();
        let (from_app_r, from_app_w) = std::io::pipe().unwrap();
        let server = std::thread::spawn(move || {
            serve(app, to_app_r, from_app_w).unwrap();
        });
        let client = RpcAdapter::connect(from_app_r, to_app_w).unwrap();
        (client, log, server)
    }

    #[test]
    fn a_whole_session_over_the_wire() {
        let caps = Capabilities {
            state: true,
            bindings: true,
            expected_frame: true,
            ..Default::default()
        };
        let (mut lab, log, server) = pair(caps);
        assert_eq!(lab.info().name, "counter");

        lab.launch(&[]).unwrap();
        lab.input("next", &Value::Null).unwrap();
        lab.input("next", &Value::Null).unwrap();
        assert_eq!(lab.state().unwrap(), json!({ "slide": 2 }));
        assert_eq!(lab.bindings().unwrap()[0].output, "main");
        let frame = lab.expected_frame("main", 4, 3).unwrap();
        assert_eq!(frame.pixel(3, 2), [2, 2, 2, 255]);

        // An app-side failure comes back as a remote error, not a hang.
        assert!(matches!(
            lab.input("jump", &Value::Null),
            Err(AdapterError::Remote {
                code: APP_ERROR,
                ..
            })
        ));

        lab.stop().unwrap();
        server.join().unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec!["launch 0".to_string(), "stop".to_string()]
        );
    }

    #[test]
    fn missing_capabilities_are_refused_without_a_round_trip() {
        let (mut lab, _log, server) = pair(Capabilities::default());
        assert!(matches!(
            lab.state(),
            Err(AdapterError::Unsupported("state"))
        ));
        assert!(matches!(
            lab.bindings(),
            Err(AdapterError::Unsupported("bindings"))
        ));
        assert!(matches!(
            lab.expected_frame("main", 2, 2),
            Err(AdapterError::Unsupported("expected_frame"))
        ));
        lab.stop().unwrap();
        server.join().unwrap();
    }
}
