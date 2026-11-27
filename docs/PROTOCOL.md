# Adapter protocol 1.0

An adapter is the one piece of Display Lab written per app. It launches the app, sends it input, and optionally reports what the app is doing.

If your app already has an MCP server, you may not need one: the built-in MCP adapter maps lab requests to your app's tools from a TOML file. See [MCP.md](MCP.md).

The lab starts the adapter as a child process and talks to it over **JSON-RPC 2.0 on stdin and stdout, one message per line**. The adapter's stderr is passed through to the lab's, so log there. Anything on stdout that is not a protocol message is an error.

Requests arrive one at a time. The lab waits up to 30 s for each answer.

## Methods

### `initialize` (required)

The first request. Params: `{"protocol": "1.0"}`.

Answer:

```json
{
  "name": "my-app",
  "version": "2.3.0",
  "bundle_id": "com.example.myapp",
  "protocol": "1.0",
  "capabilities": {
    "setup": false,
    "state": true,
    "bindings": true,
    "expected_frame": true,
    "compare": { "min_ssim": 0.98 }
  }
}
```

`bundle_id` is optional. On macOS it makes the lab capture only that app's windows, so the desktop and other apps never count as the app's picture. Backends that capture whole screens ignore it and say so in the report.

The major version must match the lab's. Every capability defaults to `false`. The lab refuses a scenario step that needs a capability the adapter lacks before it touches any screen.

`compare` sets the app's own frame-compare thresholds (`min_ssim`, `max_delta_e`, `max_bad_fraction`). Unset fields keep the lab's defaults: SSIM ≥ 0.99, and at most 0.5% of pixels beyond ΔE 3.

### `launch` (required)

Params: `{"screens": [ScreenInfo, ...]}`, the screens the OS reports at that moment. Start the app. Answer `{}` once it is running.

### `input` (required)

Params: `{"action": "next", "args": ...}`. The action names are the adapter's to define. One is shared by every app so the generic catalogue can run anywhere:

| Action | Meaning |
| --- | --- |
| `next` | Advance. Anything that visibly changes the main output. |

Answer `{}` once the app has accepted the input, not once it is drawn: the lab measures how long drawing takes.

### `shutdown` (required)

Stop the app. Answer `{}`, then exit.

### `setup` (capability `setup`)

One-time preparation for an unattended run, such as approving remote control in the app's own UI. Answer `{}`.

### `state` (capability `state`)

Answer any JSON object. Scenarios test values in it by JSON pointer (`"/slide"`).

### `bindings` (capability `bindings`)

Which output the app draws on which screen.

```json
{
  "bindings": [
    { "output": "audience", "screen": { "name": "Lab HD Projector", "serial": 2001 } },
    { "output": "stage", "screen": null }
  ]
}
```

`screen` is `null` for an output that is configured but drawn on no screen. Otherwise give whatever fields the app knows (`name`, `vendor`, `product`, `serial`, `origin` as `[x, y]`). The lab matches on every field you set and ignores the rest.

### `expected_frame` (capability `expected_frame`)

Params: `{"output": "audience", "width": 1024, "height": 768}`. Answer what that output should look like right now at that size:

```json
{ "width": 1024, "height": 768, "rgba_base64": "..." }
```

The pixels are 8-bit RGBA, row-major, with no row padding. The size must be the one asked for.

## Types

`ScreenInfo`:

```json
{
  "id": 3,
  "identity": { "vendor": 7440, "product": 1280, "serial": 2001, "name": "Lab HD Projector" },
  "mode": { "width": 1920, "height": 1080, "refresh_hz": 60.0, "hidpi": false },
  "origin": [1920, 0],
  "host": false
}
```

## Errors

Answer a JSON-RPC error object. Codes the lab understands:

| Code | Meaning |
| --- | --- |
| -32601 | No such method |
| -32602 | Bad params |
| -32001 | Not offered: a capability this adapter does not have, or cannot answer right now |
| -32002 | The app tried and failed; put the reason in `message` |

An error fails the step that caused it, with your message in the report.

## Templates

- Python, no dependencies: [adapters/example-python/adapter.py](../adapters/example-python/adapter.py)
- Rust: implement `lab_adapter::Adapter` and call `lab_adapter::serve(adapter, stdin, stdout)`.
- No code, for apps with an MCP server: [adapters/example-mcp/app.mcp.toml](../adapters/example-mcp/app.mcp.toml), described in [MCP.md](MCP.md).
