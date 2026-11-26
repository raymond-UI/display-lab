# Display Lab

Test what an app puts on the screens.

Display Lab fakes real display setups (a 4:3 projector, a stage monitor, a projector that wakes late, a cable pulled mid-presentation), drives the app under test, watches every screen, and fails loudly when the picture is wrong, late, or on the wrong screen.

It works with any desktop app. Everything app-specific sits behind one **adapter**: a small program in any language that speaks a line-based JSON-RPC protocol over stdio, or, if your app already has an MCP server, just a TOML file that maps the lab's requests to your app's tools.

## Quick start

```sh
cargo build --workspace

# The fake backend runs anywhere, with a built-in fake presentation app.
./target/debug/displaylab run --backend fake scenarios/catalogue/*.toml

# Make the fake app misbehave and watch the catalogue catch it.
./target/debug/displaylab run --backend fake --fake-bug ignore-hotplug scenarios/catalogue/*.toml

# On a Mac, against your own app:
./target/debug/displaylab run --adapter "node my-adapter.js" scenarios/catalogue/cable-pull.toml

# Or, if your app has an MCP server, with no adapter code at all:
./target/debug/displaylab run --adapter mcp:my-app.mcp.toml scenarios/catalogue/*.toml
```

Each run writes `out/<scenario>/report.md`, `report.json`, and the captured, expected and diff frames for any step that failed. Exit code 0 means every scenario passed, 1 means one failed, and 2 means the lab itself could not run (a bad file, a missing backend, an adapter that would not start).

Other commands:

```sh
displaylab profiles                       # list display profiles
displaylab validate scenarios/**/*.toml   # check files without running anything
```

## How it fits together

| Crate | What it does |
| --- | --- |
| `lab-core` | Profiles, scenarios, frames, and the `Displays` and `Capture` traits every backend implements. One monotonic clock for the whole lab. |
| `lab-compare` | SSIM plus a CIE76 ΔE budget, with thresholds proven on a production render pipeline. Blank and change checks. PNG in and out. |
| `lab-adapter` | The adapter protocol, the `Adapter` trait, a client that spawns adapter processes, and `serve` for adapters written in Rust. |
| `lab-mcp` | The built-in MCP adapter: drives an app through its own MCP server (Streamable HTTP or stdio) from a TOML mapping. |
| `lab-runner` | Plays a scenario against a backend and an adapter. Every wait is a poll against a deadline, never a fixed sleep. Writes the report. |
| `lab-fake` | In-memory screens and a fake presenter with real binding rules and switchable bugs. How the lab tests itself. |
| `lab-mac` | `CGVirtualDisplay` screens, one host process each, and ScreenCaptureKit capture. |
| `lab-win` | Not written yet. See the crate docs for what must be checked first. |
| `displaylab` | The CLI. |

## Status

| Part | State |
| --- | --- |
| Core, compare, adapter protocol, runner, report, CLI | Built and tested on Linux (72 tests across the workspace, clippy clean) |
| Fake backend and fake presenter | Built and tested; the full catalogue passes, and fails as expected with bugs injected |
| MCP adapter | Built and tested: every shipped scenario passes with the fake presenter behind an MCP server, and every injected bug is still caught |
| macOS backend | Runs on GitHub's hosted macOS 26 runners: plugs virtual screens with the requested identity, captures them, unplugs and replugs in a new mode (`macos-virtual-screen`) |
| Windows backend | Not written. CI shows a hosted runner can install the Virtual Display Driver and gets a new monitor, but the driver takes one EDID for every screen, so per-screen identities need a fork |

## Writing scenarios and adapters

- [docs/SCENARIOS.md](docs/SCENARIOS.md): the scenario file format and every step.
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the adapter protocol.
- [docs/MCP.md](docs/MCP.md): the MCP adapter, for apps that already have an MCP server.
- [adapters/example-python](adapters/example-python/adapter.py): a dependency-free adapter template.
- [adapters/example-mcp](adapters/example-mcp/app.mcp.toml): a starting MCP config.

`scenarios/catalogue` holds scenarios that work for any app: they only look at the screens and send the standard `next` action. `scenarios/fake-presenter` shows the app-aware checks (bindings, expected frames, state) in the fake app's vocabulary. A real app keeps scenarios like those next to its own adapter.

## CI

`.github/workflows/ci.yml` runs on every push:

| Job | What it proves |
| --- | --- |
| `linux` | fmt, clippy, all tests, and the full catalogue on the fake backend: it passes against a good app and fails against a broken one |
| `macos-build` | the macOS backend compiles and the tests pass on macOS |
| `macos-virtual-screen` | A hosted macOS runner creates, captures, unplugs and replugs a real virtual screen. Grants Screen Recording by writing the TCC database (`ci/grant-screen-recording.sh`), which Apple does not support and a future image may break, so the job cannot fail the run |
| `windows` | clippy, tests and the catalogue on Windows |
| `windows-display-driver` | A hosted Windows runner installs the Virtual Display Driver (trusting its publisher first, with the bundled devcon) and gets a second monitor (`ci/probe-virtual-display-driver.ps1`) |

## macOS notes

- The lab must run inside a logged-in GUI session. A launch daemon or a headless runner cannot create displays.
- The process that runs the lab (your terminal, or the CI runner) needs the Screen Recording permission.
- Keep profiles to a small fixed set of identities. macOS keeps a colour profile for every display identity it has ever seen.
- A mode change is an unplug and a plug in the new mode. Changing a virtual display's mode in place makes it impossible to remove until its process exits.
- Virtual displays must be at least 720x720.
- `CGVirtualDisplay` is a private CoreGraphics API, the same one every virtual-display app on macOS uses. Apple can change it in any release; the screen host checks every class and selector before calling it and fails with a clear message if one is missing.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
