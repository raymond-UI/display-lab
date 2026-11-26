# Display Lab display driver (Windows)

A small indirect display driver (IddCx, user mode) that gives the lab virtual screens on Windows.

It starts with no monitors. The lab plugs each screen over a named pipe with its own EDID, so Windows sees that screen's manufacturer, product, serial and name. Unplugging reports that one monitor's departure, so the other screens never blink.

## The pipe

`\\.\pipe\DisplayLabIdd` takes one ASCII line per connection and answers one line, `OK ...` or `ERR <reason>`:

| Request | Answer |
| --- | --- |
| `PING` | `OK displaylab-idd 1` |
| `PLUG <slot> <edid hex> <WxH@MILLIHZ,...>` | `OK`. The first mode is preferred; the screen offers exactly these. |
| `UNPLUG <slot>` | `OK` |
| `LIST` | `OK <used slots>` |

Slots are 0 to 15. Only SYSTEM, administrators and the interactive user can open the pipe.

`displaylab __edid <profile> [mode]` prints the `<edid hex> <mode>` arguments for a profile.

## Building and signing

Visual Studio with the Windows Driver Kit:

```powershell
msbuild driver\windows\DisplayLabIdd.vcxproj /p:Configuration=Release /p:Platform=x64
```

The build does not sign. An indirect display driver runs in user mode, so it installs with any code-signing certificate the machine trusts (in `Root` and `TrustedPublisher`); it needs no Microsoft signature and no test mode. `ci/windows-driver.ps1` shows the whole sequence: sign the DLL, `inf2cat`, sign the catalog, trust the certificate, `devcon install DisplayLabIdd.inf Root\DisplayLabIdd`.

## License

The driver is based on Microsoft's [IddSampleDriver](https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay) and is used under the Microsoft Public License ([LICENSE-MS-PL](LICENSE-MS-PL)). The files in this folder are under that license; the rest of Display Lab is MIT or Apache 2.0.
