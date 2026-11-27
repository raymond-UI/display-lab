# Scenarios

A scenario is a TOML file: the screens it uses, and a timeline of steps. Steps run in order. The first failed expectation stops the run, and the remaining steps are reported as skipped.

```toml
name = "cable-pull"
description = "The projector cable comes out mid-presentation."

[[screens]]
id = "laptop"            # the scenario's own name for it
profile = "laptop-1080p" # a file in profiles/
host = true              # the machine's own screen; never unplugged

[[screens]]
id = "projector"
profile = "hd-projector"
mode = "1920x1080@60"    # optional; the profile's first mode by default
plugged = true           # optional; false for a screen that arrives later
report_delay_ms = 3000   # optional; this screen wakes slowly this time

[[steps]]
step = "launch"

[[steps]]
step = "unplug"
screen = "projector"
```

Unknown fields are errors, so a typo never silently does nothing. `displaylab validate` checks files without running them.

## Actions

| Step | Fields | Does |
| --- | --- | --- |
| `launch` | | Starts the app through its adapter. |
| `setup` | | The adapter's one-time setup for unattended runs. |
| `input` | `action`, `args`, `mark` | Sends the app an input. `mark` names the moment for a later latency check. |
| `plug` | `screen`, `mode` | Plugs a screen in. Returns at once; the OS may report it later. |
| `unplug` | `screen` | Pulls a screen out and waits until the OS stops reporting it. |
| `wait` | `ms` | Waits. Prefer an expectation with `within_ms`. |

## Expectations

Every expectation polls until it holds or `within_ms` runs out (default 3000).

`expect_content`, `expect_blank` and `expect_changed` ignore marks smaller than one pixel in 2000 (about 1000 pixels on a 1080p screen). The OS draws small marks of its own, such as the macOS screen-recording dot, and those are not the app's picture.

| Step | Fields | Holds when | Needs |
| --- | --- | --- | --- |
| `expect_content` | `screen` | Something other than black is on the screen. | |
| `expect_blank` | `screen` | Only black is on the screen. | |
| `expect_changed` | `screen`, `since`, `max_latency_ms` | The picture differs from just before the input (`since` names a mark; default the last input). Records the latency. | |
| `expect_reference` | `screen`, `image`, `compare` | The screen matches a PNG, relative to the scenario file. | |
| `expect_matches` | `screen`, `output`, `compare` | The screen matches the frame the app says `output` should show. | `expected_frame` |
| `expect_bound` | `output`, `screen` | The app reports `output` on that screen. | `bindings` |
| `expect_unbound` | `output` | The app reports `output` on no screen. | `bindings` |
| `expect_state` | `pointer`, `equals` | The app's state has that value at that JSON pointer. | `state` |

`compare` overrides thresholds for one step: `compare = { min_ssim = 0.97, max_delta_e = 5.0, max_bad_fraction = 0.01 }`.

## Latency

`expect_changed` measures from the moment the input was sent to the presentation time of the first changed frame, both on the lab's one monotonic clock. Where the backend can (macOS), it tests every frame as it arrives, so a transition is timed from its start, not from when it settles, and the saved frame is the first one that changed. On a virtual screen that is up to the compositor, not the glass. Use it to catch regressions between builds; a true glass-to-glass number needs a capture card on a real output.
