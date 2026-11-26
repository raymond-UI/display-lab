//! The MCP adapter end to end.
//!
//! The fake presenter is put behind a small MCP server shaped like a real
//! app's agent interface: protobuf-style JSON, numeric output ids sent as
//! strings, output kinds as enum names, frames as PNG image blocks, a
//! session id, and one tool that answers as an SSE stream. Like such apps,
//! `list_outputs` names the screen an output was configured for even after
//! it is unplugged, and a separate `list_screens` says what is connected.
//! The shipped scenarios then run through `McpAdapter` exactly as they do
//! against the fake app directly, and they must still catch the same bugs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine;
use lab_adapter::{Adapter, AdapterError};
use lab_core::{Displays, ProfileSet, Scenario};
use lab_fake::{FakeApp, FakeBugs, FakeCapture, FakeDisplays, World};
use lab_mcp::{Config, McpAdapter};
use lab_runner::{Outcome, Report, Runner};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---- the server -----------------------------------------------------------

const SESSION: &str = "lab-test-session";
const OUTPUTS: [(&str, &str, &str); 2] = [
    ("101", "OUTPUT_KIND_AUDIENCE", "audience"),
    ("102", "OUTPUT_KIND_STAGE", "stage"),
];

/// A fake world with the app behind MCP: the handles the runner uses, and
/// the URL of the app's server.
struct Lab {
    displays: FakeDisplays,
    capture: FakeCapture,
    url: String,
}

fn lab(bugs: FakeBugs) -> Lab {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let world = Arc::new(Mutex::new(World::default()));
    serve_on(listener, world.clone(), bugs);
    Lab {
        displays: FakeDisplays::new(world.clone()),
        capture: FakeCapture::new(world),
        url,
    }
}

struct Server {
    app: Mutex<FakeApp>,
    /// The app's own view of the screens, for `list_screens`.
    screens: Mutex<FakeDisplays>,
    /// The last screen each output was on: what it stays configured for.
    configured: Mutex<HashMap<&'static str, Value>>,
}

fn serve_on(listener: TcpListener, world: Arc<Mutex<World>>, bugs: FakeBugs) {
    let server = Arc::new(Server {
        app: Mutex::new(FakeApp::new(world.clone(), bugs)),
        screens: Mutex::new(FakeDisplays::new(world)),
        configured: Mutex::new(HashMap::new()),
    });
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let server = server.clone();
            std::thread::spawn(move || {
                let _ = server.connection(stream);
            });
        }
    });
}

impl Server {
    /// One connection, any number of keep-alive requests.
    fn connection(&self, stream: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = stream;
        loop {
            let mut start = String::new();
            if reader.read_line(&mut start)? == 0 {
                return Ok(());
            }
            let mut headers = vec![];
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                }
            }
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
            };
            let len: usize = header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body)?;

            if !start.starts_with("POST /mcp ") {
                respond(&mut out, 405, "text/plain", &[], b"POST only")?;
                continue;
            }
            let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let method = message["method"].as_str().unwrap_or("").to_string();
            if method != "initialize" {
                // After the handshake the client must carry the session and
                // the agreed revision, as the spec asks.
                if header("mcp-session-id") != Some(SESSION)
                    || header("mcp-protocol-version").is_none()
                {
                    respond(&mut out, 400, "text/plain", &[], b"missing session headers")?;
                    continue;
                }
            }
            if message.get("id").is_none() {
                respond(&mut out, 202, "text/plain", &[], b"")?;
                continue;
            }
            let id = message["id"].clone();
            let reply = match self.handle(&method, &message["params"]) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, msg)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
                }
            };
            let tool = message["params"]["name"].as_str().unwrap_or("");
            if tool == "get_live_state" {
                let sse = format!(
                    "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}),
                    reply
                );
                respond(&mut out, 200, "text/event-stream", &[], sse.as_bytes())?;
            } else {
                let extra: &[(&str, &str)] = if method == "initialize" {
                    &[("Mcp-Session-Id", SESSION)]
                } else {
                    &[]
                };
                respond(
                    &mut out,
                    200,
                    "application/json",
                    extra,
                    reply.to_string().as_bytes(),
                )?;
            }
        }
    }

    fn handle(&self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-presenter-mcp", "version": "1"},
            })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let args = &params["arguments"];
                Ok(match self.tool(name, args) {
                    Ok(result) => result,
                    Err(e) => json!({
                        "content": [{"type": "text", "text": e.to_string()}],
                        "isError": true,
                    }),
                })
            }
            other => Err((-32601, format!("no method {other}"))),
        }
    }

    fn tool(&self, name: &str, args: &Value) -> Result<Value, AdapterError> {
        let mut app = self.app.lock().unwrap();
        let ok = || json!({"content": [{"type": "text", "text": "{\"status\":\"OK\"}"}]});
        match name {
            "get_version" => Ok(ok()),
            "start" => app.launch(&[]).map(|_| ok()),
            "stop" => app.stop().map(|_| ok()),
            "next" | "previous" | "blank" | "unblank" => app.input(name, &json!({})).map(|_| {
                // Like a real control tool, answer with a small preview
                // image that the adapter must ignore.
                let preview = png_b64(&lab_core::Frame::solid(4, 4, [255, 0, 255, 255]));
                json!({"content": [{"type": "text", "text": "{}"},
                                   {"type": "image", "data": preview, "mimeType": "image/png"}]})
            }),
            "go_to" => {
                let slide = args["slide_index"].as_u64().unwrap_or(0);
                app.input("go_to", &json!({ "slide": slide })).map(|_| ok())
            }
            "get_live_state" => app.state().map(|s| {
                let live = json!({"live": s, "seq": "3"});
                json!({"content": [{"type": "text", "text": live.to_string()}],
                       "structuredContent": live})
            }),
            "list_outputs" => {
                let bindings = app.bindings()?;
                let mut configured = self.configured.lock().unwrap();
                let outputs: Vec<Value> = OUTPUTS
                    .iter()
                    .filter_map(|(id, kind, lab)| {
                        let b = bindings.iter().find(|b| b.output == *lab)?;
                        let screen = match &b.screen {
                            Some(s) => {
                                let (x, y) = s.origin.unwrap_or_default();
                                let screen = json!({
                                    "edid_hash": edid_hash(s.vendor, s.product, s.serial),
                                    "name": s.name,
                                    // protobuf JSON: 64-bit as strings, 32-bit as numbers
                                    "vendor": s.vendor, "product": s.product,
                                    "serial": s.serial.map(|n| n.to_string()),
                                    "position": {"x": x, "y": y},
                                    "connected": true,
                                });
                                configured.insert(*lab, screen.clone());
                                screen
                            }
                            // Unplugged: still configured for the screen it
                            // was on, which is no longer connected.
                            None => {
                                let mut stale = configured
                                    .get(lab)
                                    .cloned()
                                    .unwrap_or_else(|| json!({"edid_hash": "never-seen"}));
                                stale["connected"] = json!(false);
                                stale
                            }
                        };
                        Some(json!({"id": id, "kind": kind, "slot": 0, "screen": screen}))
                    })
                    .collect();
                // Text only, no structuredContent: the adapter must parse it.
                let text = json!({"list_outputs": {"outputs": outputs}, "status": "OK"});
                Ok(json!({"content": [{"type": "text", "text": text.to_string()}]}))
            }
            "list_screens" => {
                let screens: Vec<Value> = self
                    .screens
                    .lock()
                    .unwrap()
                    .screens()
                    .unwrap()
                    .iter()
                    .map(|s| {
                        let id = &s.identity;
                        json!({
                            "edid_hash": edid_hash(Some(id.vendor), Some(id.product), Some(id.serial)),
                            "name": id.name,
                            "position": {"x": s.origin.0, "y": s.origin.1},
                        })
                    })
                    .collect();
                let data = json!({"list_screens": {"screens": screens}});
                Ok(
                    json!({"content": [{"type": "text", "text": data.to_string()}],
                          "structuredContent": data}),
                )
            }
            "render_live_frame" => {
                let id = args["output"].as_str().unwrap_or("");
                let lab = OUTPUTS
                    .iter()
                    .find(|(i, _, _)| *i == id)
                    .map(|(_, _, lab)| *lab)
                    .ok_or_else(|| AdapterError::Remote {
                        code: 3,
                        message: format!("no output {id:?}"),
                    })?;
                let w = args["width"].as_u64().unwrap_or(0) as u32;
                let h = args["height"].as_u64().unwrap_or(0) as u32;
                let frame = app.expected_frame(lab, w, h)?;
                Ok(json!({"content": [
                    {"type": "image", "data": png_b64(&frame), "mimeType": "image/png"},
                    {"type": "text", "text": format!("rendered {w}x{h}")},
                ]}))
            }
            other => Err(AdapterError::Remote {
                code: -32602,
                message: format!("Unknown tool: {other}"),
            }),
        }
    }
}

/// Stands in for the hash of a screen's EDID: the same screen gives the same
/// value, a different unit of the same model does not.
fn edid_hash(vendor: Option<u32>, product: Option<u32>, serial: Option<u32>) -> String {
    let n = |v: Option<u32>| v.map_or("?".to_string(), |v| v.to_string());
    format!("edid-{}-{}-{}", n(vendor), n(product), n(serial))
}

fn png_b64(frame: &lab_core::Frame) -> String {
    base64::engine::general_purpose::STANDARD.encode(lab_compare::encode_png(frame).unwrap())
}

fn respond(
    out: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        _ => "Method Not Allowed",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

// ---- the config -----------------------------------------------------------

/// How the adapter learns which screen an output is on.
#[derive(Clone, Copy, Debug)]
enum Style {
    /// The output's screen record, with a `connected` flag.
    Flag,
    /// The output names its configured screen; `list_screens` says whether
    /// it is connected.
    Join,
}

const FLAG_BINDINGS: &str = r#"
[bindings]
name = "/screen/name"
vendor = "/screen/vendor"
product = "/screen/product"
serial = "/screen/serial"
x = "/screen/position/x"
y = "/screen/position/y"
connected = "/screen/connected"
"#;

const JOIN_BINDINGS: &str = r#"
[bindings]
name = "/name"
x = "/position/x"
y = "/position/y"

[bindings.screens]
tool = "list_screens"
list = "/list_screens/screens"
key = "/edid_hash"
output_key = "/screen/edid_hash"
"#;

fn config(url: &str) -> Config {
    config_with(url, Style::Join)
}

fn config_with(url: &str, style: Style) -> Config {
    let bindings = match style {
        Style::Flag => FLAG_BINDINGS,
        Style::Join => JOIN_BINDINGS,
    };
    Config::from_toml(
        &format!(
            r#"
[app]
ready = {{ tool = "get_version" }}
start = {{ tool = "start" }}
stop = {{ tool = "stop" }}

[server]
url = "{url}"

[setup]
tool = "get_version"

[actions]
next = {{ tool = "next" }}
previous = {{ tool = "previous" }}
blank = {{ tool = "blank" }}
unblank = {{ tool = "unblank" }}
go_to = {{ tool = "go_to", args = {{ slide_index = "$slide" }} }}

[state]
tool = "get_live_state"
pointer = "/live"

[outputs]
tool = "list_outputs"
list = "/list_outputs/outputs"
key = "/kind"
names = {{ audience = "OUTPUT_KIND_AUDIENCE", stage = "OUTPUT_KIND_STAGE" }}

[expected_frame]
tool = "render_live_frame"
args = {{ output = "$output.id", width = "$width", height = "$height" }}
{bindings}"#
        ),
        "test-config",
    )
    .unwrap()
}

// ---- running scenarios ----------------------------------------------------

fn scenario(dir: &str, name: &str) -> (PathBuf, Scenario) {
    let path = root().join(dir).join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path).unwrap();
    let s = Scenario::from_toml(&text, &path.display().to_string()).unwrap();
    (path, s)
}

fn all_scenarios() -> Vec<(PathBuf, Scenario)> {
    let mut out = vec![];
    for dir in ["scenarios/catalogue", "scenarios/fake-presenter"] {
        let mut paths: Vec<_> = std::fs::read_dir(root().join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        for p in paths {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            out.push(scenario(dir, &name));
        }
    }
    out
}

/// Fake screens, the fake app behind MCP, and the MCP adapter in between.
fn run_over_mcp(path: &Path, scenario: &Scenario, bugs: FakeBugs, style: Style) -> Report {
    let profiles = ProfileSet::load_dir(&root().join("profiles")).unwrap();
    let mut lab = lab(bugs);
    let mut adapter = McpAdapter::new(config_with(&lab.url, style));
    let mut runner = Runner {
        displays: &mut lab.displays,
        capture: &mut lab.capture,
        adapter: &mut adapter,
        profiles: &profiles,
        backend: "fake".into(),
        artifacts: None,
        scenario_dir: path.parent().unwrap().to_path_buf(),
    };
    runner.run(scenario)
}

#[test]
fn every_shipped_scenario_passes_over_mcp() {
    let failures: Vec<String> = all_scenarios()
        .iter()
        .map(|(p, s)| run_over_mcp(p, s, FakeBugs::default(), Style::Join))
        .filter(|r| !r.passed)
        .map(|r| r.markdown())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The run failed on an expectation, not on an action or the transport. A
/// bug test that fails because MCP broke proves nothing.
fn caught(r: &Report) -> bool {
    let failed = r.steps.iter().find(|s| s.outcome == Outcome::Fail);
    !r.passed && failed.is_some_and(|s| s.kind.starts_with("expect_"))
}

#[test]
fn bugs_are_still_caught_over_mcp() {
    let hotplug = FakeBugs {
        ignore_hotplug: true,
        ..Default::default()
    };
    let by_name = FakeBugs {
        bind_by_name: true,
        ..Default::default()
    };
    let cases = [
        ("scenarios/catalogue", "cable-pull", hotplug),
        ("scenarios/catalogue", "stage-monitor-drops", hotplug),
        ("scenarios/fake-presenter", "projector-swapped", by_name),
    ];
    for style in [Style::Join, Style::Flag] {
        for (dir, name, bugs) in cases {
            let (p, s) = scenario(dir, name);
            let good = run_over_mcp(&p, &s, FakeBugs::default(), style);
            assert!(
                good.passed,
                "{name} ({style:?}) failed a good app:\n{}",
                good.markdown()
            );
            let bad = run_over_mcp(&p, &s, bugs, style);
            assert!(
                caught(&bad),
                "{name} ({style:?}) missed {bugs:?}:\n{}",
                bad.markdown()
            );
        }
    }
}

// ---- the adapter on its own ----------------------------------------------

fn launched() -> McpAdapter {
    let mut adapter = McpAdapter::new(config(&lab(FakeBugs::default()).url));
    adapter.launch(&[]).unwrap();
    adapter
}

#[test]
fn capabilities_follow_the_config() {
    let adapter = McpAdapter::new(config("http://127.0.0.1:9/mcp"));
    let caps = &adapter.info().capabilities;
    assert!(caps.setup && caps.state && caps.bindings && caps.expected_frame);

    let bare = Config::from_toml(
        "[server]\nurl = \"http://127.0.0.1:9/mcp\"\n[actions]\nnext = { tool = \"next\" }",
        "bare",
    )
    .unwrap();
    let caps = McpAdapter::new(bare).info().capabilities.clone();
    assert!(!(caps.setup || caps.state || caps.bindings || caps.expected_frame));
}

#[test]
fn actions_state_and_frames_round_trip() {
    let mut a = launched();
    assert_eq!(a.info().name, "fake-presenter-mcp");
    a.setup().unwrap();
    a.input("go_to", &json!({"slide": 4})).unwrap();
    // State arrives as an SSE stream, behind a progress notification.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while a.state().unwrap()["slide"] != 4 {
        assert!(
            std::time::Instant::now() < deadline,
            "state never showed slide 4"
        );
    }
    // No screens are plugged here, so the app configured no outputs. Frames
    // themselves are proven by the scenario runs (letterbox, stand-in).
    let e = a
        .expected_frame("audience", 64, 36)
        .unwrap_err()
        .to_string();
    assert!(e.contains("lists no output"), "{e}");
    a.stop().unwrap();
}

#[test]
fn app_errors_and_mistakes_are_reported_plainly() {
    let mut a = launched();
    let e = a.input("teleport", &json!({})).unwrap_err().to_string();
    assert!(e.contains("teleport") && e.contains("next"), "{e}");

    // go_to without its argument: the template names what is missing.
    let e = a.input("go_to", &json!({})).unwrap_err().to_string();
    assert!(e.contains("$slide"), "{e}");

    // The app lists no "booth" output.
    let e = a.expected_frame("booth", 8, 8).unwrap_err().to_string();
    assert!(e.contains("booth"), "{e}");
    a.stop().unwrap();

    // After stop the app itself says it is not running: an isError result.
    let mut a = launched();
    a.stop().unwrap();
    assert!(a.state().is_err(), "a stopped adapter must not answer");
}

#[test]
fn launch_waits_for_a_server_that_is_slow_to_start() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let url = format!("http://{addr}/mcp");
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        let world = Arc::new(Mutex::new(World::default()));
        serve_on(TcpListener::bind(addr).unwrap(), world, FakeBugs::default());
    });
    let mut a = McpAdapter::new(config(&url));
    a.launch(&[]).unwrap();
    a.stop().unwrap();
}

#[test]
fn launch_gives_up_on_a_server_that_never_comes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut c = config(&format!("http://{addr}/mcp"));
    c.app.ready_timeout_s = 1;
    let e = McpAdapter::new(c).launch(&[]).unwrap_err().to_string();
    assert!(e.contains("not up within 1 s"), "{e}");
}

#[test]
fn stdio_servers_work_too() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: no python3");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdio_server.py");
    let c = Config::from_toml(
        &format!(
            "[server]\ncommand = [\"python3\", {:?}]\n[actions]\nnext = {{ tool = \"next\" }}\nwarp = {{ tool = \"warp\" }}\n[state]\ntool = \"state\"",
            script.display().to_string()
        ),
        "stdio",
    )
    .unwrap();
    let mut a = McpAdapter::new(c);
    a.launch(&[]).unwrap();
    assert_eq!(a.info().name, "stdio-fixture");
    a.input("next", &json!({})).unwrap();
    a.input("next", &json!({})).unwrap();
    assert_eq!(a.state().unwrap(), json!({"slide": 3}));
    let e = a.input("warp", &json!({})).unwrap_err().to_string();
    assert!(e.contains("Unknown tool: warp"), "{e}");
    a.stop().unwrap();
}

#[test]
fn the_shipped_example_config_loads() {
    let c = Config::load(&root().join("adapters/example-mcp/app.mcp.toml")).unwrap();
    let caps = McpAdapter::new(c).info().capabilities.clone();
    assert!(caps.state && caps.bindings && caps.expected_frame && !caps.setup);
}
