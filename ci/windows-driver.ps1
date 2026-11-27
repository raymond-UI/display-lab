# Build, sign, install and probe the Display Lab display driver on a hosted
# Windows runner.
#
# What it proves:
#   1. The driver builds with what the runner image has.
#   2. A driver signed with our own certificate installs with
#      `displaylab driver install`: no Microsoft signing, no test mode.
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

# --- 1 and 2. Build, sign, install with displaylab's own installer ----------

$pkg = Join-Path $work 'package'
& (Join-Path $PSScriptRoot 'build-windows-driver.ps1') -Out $pkg
Check ($LASTEXITCODE -eq 0) 'the driver builds and is signed'

"== Install"
& $Displaylab driver install --from $pkg
Check ($LASTEXITCODE -eq 0) 'displaylab driver install installs it'
Check ((Get-AuthenticodeSignature (Join-Path $pkg 'DisplayLabIdd.cat')).Status -eq 'Valid') 'the catalog is signed and now trusted'
"status: $(& $Displaylab driver status)"
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
    [DllImport("user32.dll")]
    static extern int SetDisplayConfig(uint paths, IntPtr pathArray, uint modes, IntPtr modeArray, uint flags);
    // SDC_APPLY | SDC_TOPOLOGY_EXTEND: what Win+P, Extend does.
    public static int Extend() { return SetDisplayConfig(0, IntPtr.Zero, 0, IntPtr.Zero, 0x80 | 0x4); }
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
$desktopBaseline = @(Desktop).Count

# A screen of this size on the desktop as itself: a device that was not
# there before. A mirrored screen shares a device with one already there.
function OwnScreen([string]$size, $before) {
    @(Desktop | Where-Object { $_.Size -eq $size -and $_.Device -notin $before }).Count -eq 1
}
$before = @(Desktop | ForEach-Object Device)

$proj = Pipe "PLUG 0 $(PlugArgs hd-projector)"
"PLUG 0 hd-projector: $proj"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '2001').Count -eq 1 }) 'a plugged projector appears with its serial (2001)'
Show 'after plugging the projector'
$p0 = Monitors | Where-Object Serial -eq '2001'
Check ($p0.Name -eq 'Lab HD Projec' -and $p0.Product -match '^0*500$') "it carries the profile's name and product ($($p0.Name), $($p0.Product))"
$own = WaitFor { OwnScreen '1920x1080' $before } 5
if (-not $own) {
    # A laptop mirrors its first external screen; the backend then asks
    # Windows to extend, and so does this.
    "Windows mirrors the projector; asking it to extend (SetDisplayConfig: $([LabDesktop]::Extend()))"
    $own = WaitFor { OwnScreen '1920x1080' $before }
    Show 'after asking Windows to extend'
}
Check $own 'it joins the desktop as its own 1920x1080 screen'


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

$before = @(Desktop | ForEach-Object Device)
"PLUG 2 hd-projector 1280x720: $(Pipe "PLUG 2 $(PlugArgs hd-projector 1280x720)")"
$own = WaitFor { OwnScreen '1280x720' $before }
Show 'after plugging the projector at 1280x720'
Check $own 'a screen comes up in the mode asked for (1280x720)'

"LIST: $(Pipe 'LIST')"
Check ((Pipe 'PLUG 1 00 1x1@1') -like 'ERR*') 'a bad request is refused, not crashed on'
Check ((Pipe 'PING') -like 'OK*') 'the driver still answers after a bad request'

foreach ($slot in 0, 1, 2) { "UNPLUG ${slot}: $(Pipe "UNPLUG $slot")" }
Check (WaitFor { @(Monitors).Count -eq $baseline -and @(Desktop).Count -eq $desktopBaseline }) 'unplugging everything leaves the machine as it was'
Show 'at the end'

"== Driver package"
Get-ChildItem $pkg | ForEach-Object { $_.FullName }

"== Summary"
if ($failures.Count -eq 0) { 'every check passed'; exit 0 }
$failures | ForEach-Object { "FAIL  $_" }
exit 1
