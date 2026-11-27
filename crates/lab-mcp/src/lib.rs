//! A built-in adapter for apps that already speak MCP.
//!
//! Many apps now ship an MCP server so an agent can drive them. If yours
//! does, you do not need to write an adapter: a TOML file maps each lab
//! request to one of the app's tools, and [`McpAdapter`] does the rest.
//! `docs/MCP.md` describes the file.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod client;
mod config;
pub mod template;

pub use client::{McpClient, McpError, ToolResult};
pub use config::{
    AppSection, BindingsSection, Call, Config, ConfigError, OutputsSection, ScreensJoin,
    ServerSection, StateSection, DEFAULT_PROTOCOL,
};

use base64::Engine;
use lab_adapter::{Adapter, AdapterError, AppInfo, Binding, Capabilities, ScreenRef};
use lab_core::{Frame, ScreenInfo};
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use template::{as_i64, at, render, Vars};

/// The adapter protocol's error code for an app-side error.
const APP_ERROR: i64 = -32002;
/// The lab adapter protocol this in-process adapter implements.
const ADAPTER_PROTOCOL: &str = "1.0";
/// How often to try a server that is still starting.
const READY_POLL: Duration = Duration::from_millis(200);

pub struct McpAdapter {
    config: Config,
    info: AppInfo,
    client: Option<McpClient>,
    app: Option<Child>,
}

impl McpAdapter {
    pub fn new(config: Config) -> McpAdapter {
        let info = AppInfo {
            name: config.app.name.clone().unwrap_or_else(|| "mcp app".into()),
            version: String::new(),
            bundle_id: config.app.bundle_id.clone(),
            protocol: ADAPTER_PROTOCOL.into(),
            capabilities: Capabilities {
                setup: config.setup.is_some(),
                state: config.state.is_some(),
                bindings: config.bindings.is_some(),
                expected_frame: config.expected_frame.is_some(),
                compare: config.compare,
            },
        };
        McpAdapter {
            config,
            info,
            client: None,
            app: None,
        }
    }

    pub fn load(path: &std::path::Path) -> Result<McpAdapter, ConfigError> {
        Ok(McpAdapter::new(Config::load(path)?))
    }

    fn connect(&self) -> Result<McpClient, McpError> {
        let timeout = Duration::from_secs(self.config.server.timeout_s.max(1));
        let mut client = match &self.config.server.url {
            Some(url) => McpClient::http(url, timeout),
            None => McpClient::stdio(&self.config.server.command, timeout)?,
        };
        client.initialize(&self.config.server.protocol)?;
        Ok(client)
    }

    /// Connect, retrying while the server is not listening yet, then wait for
    /// the ready tool to succeed.
    fn wait_until_up(&mut self) -> Result<(), AdapterError> {
        let limit = Duration::from_secs(self.config.app.ready_timeout_s);
        let deadline = Instant::now() + limit;
        let late = |last: &dyn std::fmt::Display| {
            AdapterError::Transport(format!(
                "the app was not up within {} s; last: {last}",
                limit.as_secs()
            ))
        };
        loop {
            self.exited_early()?;
            match self.connect() {
                Ok(client) => {
                    self.client = Some(client);
                    break;
                }
                // Only "nothing is listening" is worth retrying; an HTTP 404
                // or a bad reply will not fix itself.
                Err(McpError::Unreachable(_)) if Instant::now() < deadline => {
                    std::thread::sleep(READY_POLL);
                }
                Err(McpError::Unreachable(e)) => return Err(late(&e)),
                Err(other) => return Err(mcp_error(other)),
            }
        }
        let Some(ready) = self.config.app.ready.clone() else {
            return Ok(());
        };
        loop {
            self.exited_early()?;
            let last = match self.call(&ready, &Vars::new()) {
                Ok(_) => return Ok(()),
                Err(e) => e,
            };
            if Instant::now() >= deadline {
                return Err(late(&last));
            }
            std::thread::sleep(READY_POLL);
        }
    }

    fn exited_early(&mut self) -> Result<(), AdapterError> {
        if let Some(child) = &mut self.app {
            if let Ok(Some(status)) = child.try_wait() {
                self.app = None;
                return Err(AdapterError::Transport(format!(
                    "the app exited while starting ({status})"
                )));
            }
        }
        Ok(())
    }

    fn client(&mut self) -> Result<&mut McpClient, AdapterError> {
        self.client
            .as_mut()
            .ok_or_else(|| AdapterError::Transport("the app has not been launched".into()))
    }

    fn call_tool(
        &mut self,
        tool: &str,
        args_template: &Value,
        vars: &Vars,
    ) -> Result<ToolResult, AdapterError> {
        let args = render(args_template, vars)
            .map_err(|e| AdapterError::Protocol(format!("arguments for {tool}: {e}")))?;
        let result = self.client()?.call_tool(tool, &args).map_err(mcp_error)?;
        if result.is_error {
            let message = if result.text.is_empty() {
                format!("{tool} failed")
            } else {
                format!("{tool}: {}", result.text)
            };
            return Err(AdapterError::Remote {
                code: APP_ERROR,
                message,
            });
        }
        Ok(result)
    }

    fn call(&mut self, call: &Call, vars: &Vars) -> Result<ToolResult, AdapterError> {
        self.call_tool(&call.tool.clone(), &call.args.clone(), vars)
    }

    fn data(tool: &str, result: ToolResult) -> Result<Value, AdapterError> {
        result.data.ok_or_else(|| {
            AdapterError::Protocol(format!(
                "{tool} returned no structured content and no JSON text"
            ))
        })
    }

    /// The app's outputs, each as `(lab name, the app's JSON for it)`.
    fn outputs(&mut self) -> Result<Vec<(String, Value)>, AdapterError> {
        let Some(section) = self.config.outputs.clone() else {
            return Err(AdapterError::Unsupported("outputs"));
        };
        let result = self.call_tool(&section.tool, &section.args, &Vars::new())?;
        let data = Self::data(&section.tool, result)?;
        let list = at(&data, &section.list)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AdapterError::Protocol(format!(
                    "{}: no array at {:?} in {data}",
                    section.tool, section.list
                ))
            })?;
        Ok(list
            .iter()
            .filter_map(|o| {
                let key = at(o, &section.key)?;
                let name = section
                    .names
                    .iter()
                    .find(|(_, v)| *v == key)
                    .map(|(name, _)| name.clone())
                    .or_else(|| key.as_str().map(str::to_string))
                    .unwrap_or_else(|| key.to_string());
                Some((name, o.clone()))
            })
            .collect())
    }

    fn screen_of(b: &BindingsSection, output: &Value) -> Option<ScreenRef> {
        let read = |p: &Option<String>| p.as_ref().and_then(|p| at(output, p));
        if let Some(connected) = read(&b.connected) {
            if connected.as_bool() == Some(false) {
                return None;
            }
        }
        let number = |p: &Option<String>| read(p).and_then(as_i64);
        let unsigned = |p: &Option<String>| number(p).and_then(|n| u32::try_from(n).ok());
        let origin = match (number(&b.x), number(&b.y)) {
            (Some(x), Some(y)) => Some((x as i32, y as i32)),
            _ => None,
        };
        let screen = ScreenRef {
            name: read(&b.name)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            vendor: unsigned(&b.vendor),
            product: unsigned(&b.product),
            serial: unsigned(&b.serial),
            origin,
        };
        (!screen.is_empty()).then_some(screen)
    }

    fn stop_app(&mut self) {
        if let Some(mut child) = self.app.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn mcp_error(e: McpError) -> AdapterError {
    match e {
        McpError::Unreachable(m) => AdapterError::Transport(m),
        McpError::Timeout { method, seconds } => AdapterError::Timeout { method, seconds },
        McpError::Rpc { code, message } => AdapterError::Remote { code, message },
        McpError::Protocol(m) => AdapterError::Protocol(m),
    }
}

impl Adapter for McpAdapter {
    fn info(&self) -> &AppInfo {
        &self.info
    }

    fn launch(&mut self, _screens: &[ScreenInfo]) -> Result<(), AdapterError> {
        self.stop()?;
        if let Some((program, args)) = self.config.app.launch.split_first() {
            let child = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| AdapterError::Transport(format!("starting {program:?}: {e}")))?;
            self.app = Some(child);
        }
        if let Err(e) = self.wait_until_up() {
            self.stop_app();
            self.client = None;
            return Err(e);
        }
        if let Some(name) = self.client.as_ref().and_then(|c| c.server_name.clone()) {
            if self.config.app.name.is_none() {
                self.info.name = name;
            }
        }
        if let Some(start) = self.config.app.start.clone() {
            self.call(&start, &Vars::new())?;
        }
        Ok(())
    }

    fn setup(&mut self) -> Result<(), AdapterError> {
        let Some(setup) = self.config.setup.clone() else {
            return Err(AdapterError::Unsupported("setup"));
        };
        self.call(&setup, &Vars::new()).map(drop)
    }

    fn input(&mut self, action: &str, args: &Value) -> Result<(), AdapterError> {
        let Some(call) = self.config.actions.get(action).cloned() else {
            let known: Vec<&str> = self.config.actions.keys().map(String::as_str).collect();
            return Err(AdapterError::Remote {
                code: -32601,
                message: format!(
                    "no action {action:?} in the MCP config (it maps: {})",
                    known.join(", ")
                ),
            });
        };
        let mut vars = args.as_object().cloned().unwrap_or_default();
        vars.insert("args".into(), args.clone());
        // Control tools often answer with a preview image too; it is not
        // what the screens show, so it is ignored.
        self.call(&call, &vars).map(drop)
    }

    fn state(&mut self) -> Result<Value, AdapterError> {
        let Some(state) = self.config.state.clone() else {
            return Err(AdapterError::Unsupported("state"));
        };
        let result = self.call_tool(&state.tool, &state.args, &Vars::new())?;
        let data = Self::data(&state.tool, result)?;
        match &state.pointer {
            None => Ok(data),
            Some(p) => at(&data, p).cloned().ok_or_else(|| {
                AdapterError::Protocol(format!("{}: nothing at {p:?} in {data}", state.tool))
            }),
        }
    }

    fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
        let Some(section) = self.config.bindings.clone() else {
            return Err(AdapterError::Unsupported("bindings"));
        };
        let outputs = self.outputs()?;
        let Some(join) = &section.screens else {
            return Ok(outputs
                .into_iter()
                .map(|(output, json)| Binding {
                    screen: Self::screen_of(&section, &json),
                    output,
                })
                .collect());
        };
        let result = self.call_tool(&join.tool, &join.args, &Vars::new())?;
        let data = Self::data(&join.tool, result)?;
        let live = at(&data, &join.list)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(); // no array: nothing is connected
        Ok(outputs
            .into_iter()
            .map(|(output, json)| {
                let screen = at(&json, &join.output_key).and_then(|k| {
                    live.iter()
                        .find(|s| at(s, &join.key) == Some(k))
                        .and_then(|s| Self::screen_of(&section, s))
                });
                Binding { output, screen }
            })
            .collect())
    }

    fn expected_frame(
        &mut self,
        output: &str,
        width: u32,
        height: u32,
    ) -> Result<Frame, AdapterError> {
        let Some(call) = self.config.expected_frame.clone() else {
            return Err(AdapterError::Unsupported("expected_frame"));
        };
        let app_output = if self.config.outputs.is_some() {
            self.outputs()?
                .into_iter()
                .find(|(name, _)| name == output)
                .map(|(_, json)| json)
                .ok_or_else(|| AdapterError::Remote {
                    code: APP_ERROR,
                    message: format!("the app lists no output {output:?}"),
                })?
        } else {
            json!({ "name": output })
        };
        let mut vars = Vars::new();
        vars.insert("output".into(), app_output);
        vars.insert("output_name".into(), json!(output));
        vars.insert("width".into(), json!(width));
        vars.insert("height".into(), json!(height));
        let result = self.call(&call, &vars)?;
        let (mime, data) = result
            .images
            .first()
            .ok_or_else(|| AdapterError::Protocol(format!("{} returned no image", call.tool)))?;
        if !(mime.is_empty() || mime == "image/png") {
            return Err(AdapterError::Protocol(format!(
                "{} returned {mime}; only PNG frames are read",
                call.tool
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| {
                AdapterError::Protocol(format!("{}: image is not base64: {e}", call.tool))
            })?;
        lab_compare::decode_png(&bytes, &call.tool)
            .map_err(|e| AdapterError::Protocol(e.to_string()))
    }

    fn stop(&mut self) -> Result<(), AdapterError> {
        let mut first_error = None;
        if self.client.is_some() {
            if let Some(stop) = self.config.app.stop.clone() {
                first_error = self.call(&stop, &Vars::new()).err();
            }
        }
        if let Some(mut client) = self.client.take() {
            client.close();
        }
        self.stop_app();
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for McpAdapter {
    fn drop(&mut self) {
        self.stop_app();
    }
}
