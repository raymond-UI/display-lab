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

## Installing

From a release, in an administrator terminal:

```powershell
displaylab driver install          # the `driver` folder next to displaylab.exe
displaylab driver install --from <folder>
displaylab driver status
displaylab driver uninstall
```

`install` trusts the package's certificate (Root and TrustedPublisher), creates the `Root\DisplayLabIdd` device and installs the driver on it, the same steps as `devcon install`, then waits for the driver to answer.

## Building and signing

Visual Studio with the Windows Driver Kit:

```powershell
msbuild driver\windows\DisplayLabIdd.vcxproj /p:Configuration=Release /p:Platform=x64
```

The build does not sign. An indirect display driver runs in user mode, so it installs with any code-signing certificate the machine trusts (in `Root` and `TrustedPublisher`); it needs no Microsoft signature and no test mode.

`ci/build-windows-driver.ps1 -Out <folder>` restores the WDK from NuGet, builds, stamps the INF, signs the DLL, makes and signs the catalog, and leaves the package with its `.cer`. Without `-Pfx` it signs with a throwaway certificate, as CI does; releases sign with the project certificate (`ci/new-driver-certificate.ps1` makes one, stored as the `WINDOWS_DRIVER_PFX` secrets).

## License

The driver is based on Microsoft's [IddSampleDriver](https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay) and is used under the Microsoft Public License ([LICENSE-MS-PL](LICENSE-MS-PL)). The files in this folder are under that license; the rest of Display Lab is MIT or Apache 2.0.
