# Display Lab

Test what an app puts on the screens.

Display Lab fakes real display setups (a 4:3 projector, a stage monitor, a projector that wakes late, a cable pulled mid-presentation), drives the app under test, watches every screen, and fails loudly when the picture is wrong, late, or on the wrong screen.

It works with any desktop app. Everything app-specific sits behind one **adapter**: a small program in any language that speaks a line-based JSON-RPC protocol over stdio, or, if your app already has an MCP server, just a TOML file that maps the lab's requests to your app's tools.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/raymond-UI/display-lab/main/install.sh | sh
```

That puts one file, `displaylab`, in `~/.local/bin`. Run it again to update; `rm ~/.local/bin/displaylab` removes it. On Windows, download the zip from [Releases](https://github.com/raymond-UI/display-lab/releases).

## Quick start

```sh
# Fake screens and a fake app: works anywhere, touches nothing.
displaylab run --backend fake catalogue

# Make the fake app misbehave and watch the catalogue catch it.
displaylab run --backend fake --fake-bug ignore-hotplug catalogue

# On a Mac: plug a real virtual projector, capture it, remove it.
displaylab run smoke

# Against your own app, with an adapter or your app's MCP server.
displaylab run --adapter "node my-adapter.js" cable-pull
displaylab run --adapter mcp:my-app.mcp.toml catalogue
```

The first time it touches real screens on a Mac, it asks for the Screen Recording permission for your terminal. Turn it on in System Settings, reopen the terminal, and run again.

The shipped profiles and scenarios are built in: `displaylab scenarios` and `displaylab profiles` list them. Pass a file or a folder to run your own, and `--profiles <folder>` to add your own display profiles.

Each run writes `out/<scenario>/report.md`, `report.json`, and the frame each check judged, pass or fail, plus the expected and diff frames for a failed match. Frames from an earlier run are cleared first. Exit code 0 means every scenario passed, 1 means one failed, and 2 means the lab itself could not run (a bad file, a missing backend, an adapter that would not start).

```sh
displaylab validate my-scenarios/   # check files without running anything
```

### Building from source

```sh
cargo build --release -p displaylab   # target/release/displaylab
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
| `lab-win` | Screens from Display Lab's own indirect display driver (`driver/windows`), one EDID per screen, and DXGI Desktop Duplication capture. Installs the driver. |
| `displaylab` | The CLI. |

## Status

| Part | State |
| --- | --- |
| Core, compare, adapter protocol, runner, report, CLI | Built and tested on Linux (86 tests across the workspace, clippy clean) |
| Fake backend and fake presenter | Built and tested; the full catalogue passes, and fails as expected with bugs injected |
| MCP adapter | Built and tested: every shipped scenario passes with the fake presenter behind an MCP server, and every injected bug is still caught |
| macOS backend | Runs on GitHub's hosted macOS 26 runners: plugs virtual screens with the requested identity, captures them, unplugs and replugs in a new mode (`macos-virtual-screen`) |
| Windows backend | Runs on GitHub's hosted Windows runners: Display Lab's own display driver plugs screens one at a time, each with its own identity, and Desktop Duplication captures them (`windows-display-driver` runs the smoke scenario). Releases include the driver once its signing certificate is set up |

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
| `release` (on a `v*` tag) | builds the macOS universal, Linux and Windows binaries that `install.sh` downloads, and publishes them as a release |
| `macos-build` | the macOS backend compiles and the tests pass on macOS |
| `macos-virtual-screen` | A hosted macOS runner creates, captures, unplugs and replugs a real virtual screen. Grants Screen Recording by writing the TCC database (`ci/grant-screen-recording.sh`), which Apple does not support and a future image may break, so the job cannot fail the run |
| `windows` | clippy, tests and the catalogue on Windows |
| `windows-display-driver` | Builds Display Lab's display driver, signs it with a throwaway certificate, installs it, then plugs and unplugs screens and checks each keeps its own identity (`ci/windows-driver.ps1`) |

## Windows notes

- Virtual screens come from Display Lab's own display driver. Install it once, in an administrator terminal:

  ```powershell
  displaylab driver install     # from the `driver` folder in the release zip
  displaylab driver status
  displaylab driver uninstall   # removes the driver, its device and its certificate
  ```

  Installing trusts the certificate the driver is signed with, for code signing on that machine; the driver runs in user mode and needs no test mode or reboot.
- A screen's name on Windows is what its EDID holds: the first 13 characters of the profile's name. The lab still reports the full name, and finds each screen by its manufacturer, product and serial.
- The lab runs as the signed-in user, in their desktop session. Only that user and administrators can plug screens.

## macOS notes

- The lab must run inside a logged-in GUI session. A launch daemon or a headless runner cannot create displays.
- The process that runs the lab (your terminal, or the CI runner) needs the Screen Recording permission.
- Keep profiles to a small fixed set of identities. macOS keeps a colour profile for every display identity it has ever seen.
- A mode change is an unplug and a plug in the new mode. Changing a virtual display's mode in place makes it impossible to remove until its process exits.
- Virtual displays must be at least 720x720.
- Name your app's bundle id (`bundle_id` in an MCP config, or in the adapter's `initialize` answer) and the lab captures only its windows. Without it, the lab captures whole screens, and the wallpaper or another app's window counts as your app's picture.
- macOS may make a newly plugged screen the main display, which moves your windows onto it. The lab gives the main role straight back to the screen that had it. macOS undoes the change when the lab's screens are removed.
- `CGVirtualDisplay` is a private CoreGraphics API, the same one every virtual-display app on macOS uses. Apple can change it in any release; the screen host checks every class and selector before calling it and fails with a clear message if one is missing.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
