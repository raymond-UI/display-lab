# Build, sign, install and probe the Display Lab display driver on a hosted
# Windows runner.
#
# What it has to prove before the Rust backend is worth writing:
#   1. The driver builds with what the runner image has.
#   2. A driver signed with our own certificate installs once that
#      certificate is trusted: no Microsoft signing, no test mode.
#   3. Two screens plugged with different EDIDs show up as two identities.
#   4. Unplugging one leaves the other where it was.
#
# Prints everything it learns; ends with a summary and exits 1 if a check
# failed.

param(
    # displaylab, for the plug arguments (`displaylab __edid <profile>`).
    [string]$Displaylab = "target\debug\displaylab.exe"
)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $env:RUNNER_TEMP 'displaylab-idd'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = @()
function Check([bool]$ok, [string]$what) {
    if ($ok) { "PASS  $what" } else { "FAIL  $what"; $script:failures += $what }
}

"== Windows"
[System.Environment]::OSVersion.VersionString
"session id: " + (Get-Process -Id $PID).SessionId

# --- 1. Build -----------------------------------------------------------------

"== Toolchain"
# The WDK comes from NuGet (driver\windows\packages.config): the hosted image
# has Visual Studio's driver toolset but not the kit's headers.
$packages = Join-Path $root 'driver\windows\packages'
nuget restore (Join-Path $root 'driver\windows\packages.config') -PackagesDirectory $packages -NonInteractive
Get-ChildItem $packages -Directory | ForEach-Object { "package: $($_.Name)" }
$kits = "${env:ProgramFiles(x86)}\Windows Kits\10"

# A tool from the restored packages, else from an installed kit.
function Tool([string]$name, [string]$arch) {
    foreach ($dir in @($packages, "$kits\bin", "$kits\Tools")) {
        $found = Get-ChildItem $dir -Recurse -Filter $name -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -like "*\$arch\*" } | Sort-Object FullName -Descending | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}
$iddcx = Get-ChildItem $packages, "$kits\Include" -Recurse -Filter iddcx.h -ErrorAction SilentlyContinue | Select-Object -First 1
"iddcx.h: $($iddcx.FullName)"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$msbuild = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
"msbuild: $msbuild"
$toolset = Get-ChildItem -Recurse -Directory -ErrorAction SilentlyContinue `
    (Join-Path (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $msbuild))) 'Microsoft\VC') -Filter 'WindowsUserModeDriver10.0' |
    Select-Object -First 1
"driver toolset: $($toolset.FullName)"

"== Build"
$proj = Join-Path $root 'driver\windows\DisplayLabIdd.vcxproj'
$out = Join-Path $work 'build\'
& $msbuild $proj /nologo /v:minimal /p:Configuration=Release /p:Platform=x64 "/p:OutDir=$out" "/p:IntDir=$work\obj\"
Check ($LASTEXITCODE -eq 0) 'the driver builds'
Get-ChildItem -Recurse $out | ForEach-Object { $_.FullName }

$dll = Get-ChildItem -Recurse $out -Filter DisplayLabIdd.dll | Select-Object -First 1
if (-not $dll) {
    Check $false 'the build produced DisplayLabIdd.dll'
    "== Summary"; $failures | ForEach-Object { "FAIL  $_" }; exit 1
}

"== Stamp the INF"
# What stampinf would do: the version, the architecture, the UMDF version
# the project builds against (UMDF_VERSION_MAJOR/MINOR in the vcxproj).
$version = "1.0.$((Get-Date).ToString('yyMM')).$((Get-Date).ToString('ddHH'))"
$stamped = (Get-Content (Join-Path $root 'driver\windows\DisplayLabIdd.inf') -Raw).
    Replace('DriverVer=', "DriverVer=$((Get-Date).ToString('MM/dd/yyyy')),$version").
    Replace('$ARCH$', 'amd64').
    Replace('$UMDFVERSION$', '2.25.0')
$inf = Join-Path $work 'DisplayLabIdd.inf'
Set-Content -Path $inf -Value $stamped -Encoding ascii
Get-Content $inf | Select-String 'DriverVer|UmdfLibraryVersion|NTamd64'

# --- 2. Sign with our own certificate, trust it, install ----------------------

$pkg = Join-Path $work 'package'
New-Item -ItemType Directory -Force -Path $pkg | Out-Null
Copy-Item $dll.FullName, $inf $pkg

$signtool = Tool 'signtool.exe' 'x64'
$inf2cat = Tool 'inf2cat.exe' 'x86'
"signtool: $signtool"
"inf2cat: $inf2cat"

"== Certificate"
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Display Lab driver (CI)' `
    -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature -NotAfter (Get-Date).AddDays(2)
$cer = Join-Path $work 'displaylab-ci.cer'
Export-Certificate -Cert $cert -FilePath $cer | Out-Null
Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
"thumbprint $($cert.Thumbprint)"

"== Sign"
& $signtool sign /fd sha256 /sha1 $cert.Thumbprint /s My (Join-Path $pkg 'DisplayLabIdd.dll')
& $inf2cat "/driver:$pkg" '/os:10_x64,Server10_x64,10_NI_X64,10_GE_X64' /uselocaltime
& $signtool sign /fd sha256 /sha1 $cert.Thumbprint /s My (Join-Path $pkg 'DisplayLabIdd.cat')
Check ((Get-AuthenticodeSignature (Join-Path $pkg 'DisplayLabIdd.cat')).Status -eq 'Valid') 'the catalog is signed and trusted'

"== Install"
$devcon = Tool 'devcon.exe' 'x64'
if (-not $devcon) {
    # devcon creates the root device the INF binds to. The kit may not carry
    # it; the Virtual Display Driver release bundles Microsoft's.
    gh release download -R VirtualDrivers/Virtual-Display-Driver --dir "$work\vdd" --pattern '*.zip'
    Get-ChildItem "$work\vdd" -Filter *.zip | ForEach-Object { Expand-Archive -Force $_.FullName -DestinationPath "$work\vdd\$($_.BaseName)" }
    $devcon = (Get-ChildItem "$work\vdd" -Recurse -Filter devcon.exe | Select-Object -First 1).FullName
}
"devcon: $devcon"
$p = Start-Process -FilePath $devcon -ArgumentList 'install', (Join-Path $pkg 'DisplayLabIdd.inf'), 'Root\DisplayLabIdd' `
    -RedirectStandardOutput "$work\devcon.out" -RedirectStandardError "$work\devcon.err" -PassThru -NoNewWindow
if (-not $p.WaitForExit(180000)) {
    'devcon HUNG; windows with a title:'
    Get-Process | Where-Object { $_.MainWindowTitle } | Format-Table -AutoSize Id, ProcessName, MainWindowTitle
    Stop-Process -Id $p.Id -Force
}
Get-Content "$work\devcon.out", "$work\devcon.err" -ErrorAction SilentlyContinue
Get-PnpDevice -FriendlyName 'Display Lab Virtual Screens' -ErrorAction SilentlyContinue | Format-Table -AutoSize Status, Class, FriendlyName, InstanceId

# --- 3 and 4. Plug and unplug -------------------------------------------------

function Pipe([string]$line) {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'DisplayLabIdd', [System.IO.Pipes.PipeDirection]::InOut)
    try {
        $c.Connect(3000)
        $w = New-Object System.IO.StreamWriter($c)
        $w.AutoFlush = $true
        $w.Write("$line`n")
        (New-Object System.IO.StreamReader($c)).ReadLine()
    } catch {
        "NO PIPE: $($_.Exception.Message)"
    } finally {
        $c.Dispose()
    }
}

function Text($codes) { -join ($codes | Where-Object { $_ -ne 0 } | ForEach-Object { [char]$_ }) }

# Every monitor Windows knows is connected, by its EDID identity.
function Monitors {
    Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue | Where-Object Active | ForEach-Object {
        [pscustomobject]@{
            Maker = Text $_.ManufacturerName
            Product = Text $_.ProductCodeID
            Serial = Text $_.SerialNumberID
            Name = Text $_.UserFriendlyName
            Instance = $_.InstanceName
        }
    }
}

# What is on the desktop now, in real pixels: the screens apps can draw on.
# (System.Windows.Forms.Screen caches its list for the life of the process,
# and its bounds are scaled for DPI, so it is no use here.)
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class LabDesktop {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct DISPLAY_DEVICE {
        public int cb;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString;
        public int StateFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct DEVMODE {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
        public short dmSpecVersion, dmDriverVersion, dmSize, dmDriverExtra;
        public int dmFields, dmPositionX, dmPositionY, dmDisplayOrientation, dmDisplayFixedOutput;
        public short dmColor, dmDuplex, dmYResolution, dmTTOption, dmCollate;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
        public short dmLogPixels;
        public int dmBitsPerPel, dmPelsWidth, dmPelsHeight, dmDisplayFlags, dmDisplayFrequency;
        public int dmICMMethod, dmICMIntent, dmMediaType, dmDitherType, dmReserved1, dmReserved2, dmPanningWidth, dmPanningHeight;
    }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern bool EnumDisplayDevices(string lpDevice, int iDevNum, ref DISPLAY_DEVICE lpDisplayDevice, int dwFlags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern bool EnumDisplaySettings(string deviceName, int modeNum, ref DEVMODE devMode);
    public static List<string> Screens() {
        var list = new List<string>();
        for (int i = 0; ; i++) {
            var d = new DISPLAY_DEVICE(); d.cb = Marshal.SizeOf(d);
            if (!EnumDisplayDevices(null, i, ref d, 0)) break;
            if ((d.StateFlags & 1) == 0) continue; // not attached to the desktop
            var m = new DEVMODE(); m.dmSize = (short)Marshal.SizeOf(m);
            if (!EnumDisplaySettings(d.DeviceName, -1, ref m)) continue;
            list.Add(string.Format("{0}|{1},{2}|{3}x{4}|{5}", d.DeviceName, m.dmPositionX, m.dmPositionY, m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency));
        }
        return list;
    }
}
'@

function Desktop {
    [LabDesktop]::Screens() | ForEach-Object {
        $f = $_ -split '\|'
        [pscustomobject]@{ Device = $f[0]; Origin = $f[1]; Size = $f[2]; Hz = $f[3] }
    }
}

function Show([string]$label) {
    "-- $label"
    Monitors | Format-Table -AutoSize | Out-String -Width 200
    Desktop | Format-Table -AutoSize | Out-String -Width 200
}

function WaitFor([scriptblock]$cond, [int]$seconds = 15) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $cond) { return $true }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

function PlugArgs([string]$name, [string]$mode = '') {
    if ($mode) { & $Displaylab __edid $name $mode } else { & $Displaylab __edid $name }
}

"== Pipe"
$up = WaitFor { (Pipe 'PING') -like 'OK*' } 30
Check $up 'the driver answers on its pipe'
Pipe 'PING'
Show 'before'
$baseline = @(Monitors).Count

$proj = Pipe "PLUG 0 $(PlugArgs hd-projector)"
"PLUG 0 hd-projector: $proj"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '2001').Count -eq 1 }) 'a plugged projector appears with its serial (2001)'
Show 'after plugging the projector'
$p0 = Monitors | Where-Object Serial -eq '2001'
Check ($p0.Name -eq 'Lab HD Projec' -and $p0.Product -match '^0*500$') "it carries the profile's name and product ($($p0.Name), $($p0.Product))"
Check (WaitFor { @(Desktop | Where-Object Size -eq '1920x1080').Count -eq 1 }) 'it joins the desktop at its 1920x1080'


$stage = Pipe "PLUG 1 $(PlugArgs stage-monitor)"
"PLUG 1 stage-monitor: $stage"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '4001').Count -eq 1 }) 'a second screen appears with its own identity (4001)'
Show 'after plugging the stage monitor'
$stageBefore = Monitors | Where-Object Serial -eq '4001'

"UNPLUG 0: $(Pipe 'UNPLUG 0')"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '2001').Count -eq 0 }) 'unplugging the projector removes it'
Show 'after unplugging the projector'
$stageAfter = Monitors | Where-Object Serial -eq '4001'
Check ($stageAfter -and $stageAfter.Instance -eq $stageBefore.Instance) 'the stage monitor stays, as the same device'

"PLUG 0 hd-projector-spare: $(Pipe "PLUG 0 $(PlugArgs hd-projector-spare)")"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '2002').Count -eq 1 }) 'the spare unit of the same model shows its own serial (2002)'
Show 'after plugging the spare'

"PLUG 2 hd-projector 1280x720: $(Pipe "PLUG 2 $(PlugArgs hd-projector 1280x720)")"
Start-Sleep -Seconds 3
Show 'after plugging the projector at 1280x720'
Check (@(Desktop | Where-Object Size -eq '1280x720').Count -eq 1) 'a screen comes up in the mode asked for (1280x720)'

"LIST: $(Pipe 'LIST')"
Check ((Pipe 'PLUG 1 00 1x1@1') -like 'ERR*') 'a bad request is refused, not crashed on'
Check ((Pipe 'PING') -like 'OK*') 'the driver still answers after a bad request'

foreach ($slot in 0, 1, 2) { "UNPLUG ${slot}: $(Pipe "UNPLUG $slot")" }
Check (WaitFor { @(Monitors).Count -eq $baseline -and @(Desktop).Count -eq 1 }) 'unplugging everything leaves the runner as it was'
Show 'at the end'

"== Driver package"
Copy-Item $cer $pkg
Get-ChildItem $pkg | ForEach-Object { $_.FullName }

"== Summary"
if ($failures.Count -eq 0) { 'every check passed'; exit 0 }
$failures | ForEach-Object { "FAIL  $_" }
exit 1
