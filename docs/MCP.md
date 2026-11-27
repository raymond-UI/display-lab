# The MCP adapter

If your app already has an MCP server (many do now, so agents can drive them), you do not need to write an adapter. Describe how the lab's requests map to your app's tools in a TOML file and pass it with `mcp:`:

```sh
displaylab run --adapter mcp:my-app.mcp.toml scenarios/catalogue/*.toml
```

The adapter runs inside the lab. It speaks MCP over Streamable HTTP or stdio, calls one tool per lab request, and offers exactly the capabilities your file describes. A file with only `[server]` and `[actions]` is enough for the catalogue scenarios, which only look at the screens.

[`adapters/example-mcp/app.mcp.toml`](../adapters/example-mcp/app.mcp.toml) is a commented starting point.

## Sections

| Section | Required | What it does |
| --- | --- | --- |
| `[server]` | yes | Where the MCP server is: `url` for Streamable HTTP, or `command` to spawn one on stdio. |
| `[app]` | no | How to start the app, how to tell it is up, and what to call on start and stop. |
| `[actions.<name>]` | yes | One per lab action (`next`, `go_to`, ...). Scenarios send these with `step = "input"`. |
| `[setup]` | no | A tool for one-time setup. Offers the `setup` capability. |
| `[state]` | no | A tool whose result is the app's state. Offers `state`. |
| `[outputs]` | no | How to list the app's outputs and tell which is which. Needed by `[bindings]`, and by `[expected_frame]` when its arguments use `$output`. |
| `[bindings]` | no | Where, inside one output, the screen it draws on is described. Offers `bindings`. |
| `[expected_frame]` | no | A tool that renders what an output should show, returned as a PNG image block. Offers `expected_frame`. |
| `[compare]` | no | Your app's own compare thresholds (`min_ssim`, `max_delta_e`, `max_bad_fraction`). |

### `[server]`

```toml
[server]
url = "http://127.0.0.1:8000/mcp"   # or: command = ["node", "server.js"]
protocol = "2025-06-18"             # the MCP revision to ask for (default)
timeout_s = 60                      # per call
```

Over HTTP the adapter posts one JSON-RPC message per request, accepts a JSON or SSE reply, keeps the `Mcp-Session-Id` the server hands out, and sends `MCP-Protocol-Version` after the handshake. It never uses a proxy from the environment: the server is expected to be local.

Raise `timeout_s` if your app asks a person to approve changes. The call waits for the answer.

### `[app]`

```toml
[app]
name = "My App"                          # for reports; defaults to the server's name
bundle_id = "com.example.myapp"          # macOS: capture only this app's windows
launch = ["/Applications/My App.app/Contents/MacOS/My App"]
ready = { tool = "get_version" }         # called until it succeeds
ready_timeout_s = 30
start = { tool = "open", args = { path = "fixtures/demo.doc" } }
stop = { tool = "quit" }
```

Without `launch`, the app must already be running (or be the stdio server). The lab connects, retrying while nothing is listening, then calls `ready` until it succeeds. A launched app is killed when the scenario ends. Relative paths are relative to where you run `displaylab`.

Set `bundle_id` on macOS. The lab then captures only your app's windows, so the desktop, other apps and the system's own marks never count as your app's picture. Without it, a screen showing the wallpaper passes `expect_content`. The report says which way the screens were captured.

### Calls and argument templates

Every call is a `tool` and optional `args`. Arguments are JSON with placeholders. A string that is exactly `"$name"` or `"$name.field"` is replaced by that value, keeping its type, so `"$width"` becomes the number `1920`. Anything else is kept as written, and `"$$x"` is the literal string `"$x"`.

| Variable | Where | Value |
| --- | --- | --- |
| any input argument | `[actions]` | `{ step = "input", action = "go_to", args = { slide = 3 } }` makes `$slide` = 3 |
| `$args` | `[actions]` | all input arguments, as one object |
| `$output` | `[expected_frame]` | the app's JSON for the output (from `[outputs]`), so `"$output.id"` is its id |
| `$output_name` | `[expected_frame]` | the lab's name for it, e.g. `audience` |
| `$width`, `$height` | `[expected_frame]` | the size the lab wants |

A missing variable fails the step with its name and the ones that are set.

### Results

The adapter reads a tool's `structuredContent`, or else the first text block that parses as JSON. A result with `isError: true` fails the step with the tool's text. Image blocks in an action's result (a preview, say) are ignored. `[expected_frame]` takes the first image block, which must be a PNG.

### `[state]`

```toml
[state]
tool = "get_state"
pointer = "/live"        # a JSON pointer into the result; the whole result if absent
```

### `[outputs]` and `[bindings]`

```toml
[outputs]
tool = "list_outputs"
list = "/outputs"        # pointer to the array of outputs
key = "/kind"            # pointer, inside one output, to what says which output it is
names = { audience = "AUDIENCE", stage = "STAGE" }   # lab name = the app's value at key

[bindings]               # pointers inside one output; set the ones your app has
name = "/screen/name"
vendor = "/screen/vendor"
product = "/screen/product"
serial = "/screen/serial"
x = "/screen/position/x"
y = "/screen/position/y"
connected = "/screen/connected"   # optional: false means unbound
```

An output whose screen fields are all missing is unbound. Numbers may arrive as JSON numbers or as digit strings, as protobuf's JSON mapping writes 64-bit integers.

Some apps keep naming the screen an output was configured for after it is unplugged, and list the connected screens with another tool. Join the two:

```toml
[bindings]               # now read from the matching connected screen
name = "/name"
x = "/position/x"
y = "/position/y"

[bindings.screens]
tool = "list_screens"
list = "/screens"
key = "/edid_hash"                # identifies a connected screen
output_key = "/screen/edid_hash"  # the same value, inside one output
```

An output is bound only when its screen is in that list.

### `[expected_frame]`

```toml
[expected_frame]
tool = "render_output"
args = { output = "$output.id", width = "$width", height = "$height" }
```

## How it is tested

`crates/lab-mcp/tests/fake_over_mcp.rs` puts the lab's fake presenter behind a small MCP server with the quirks real apps have: ids as strings, enum names, a session id, an SSE reply, text-only results, preview images on actions, and outputs that outlive their screens. Every shipped scenario runs through the MCP adapter and must pass, and each bug the fake app can simulate must still be caught, with both binding styles.
