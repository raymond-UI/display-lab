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
$kits = "${env:ProgramFiles(x86)}\Windows Kits\10"
$iddcx = Get-ChildItem "$kits\Include" -Recurse -Filter iddcx.h -ErrorAction SilentlyContinue | Select-Object -First 1
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
$inf = Get-ChildItem -Recurse $out -Filter DisplayLabIdd.inf |
    Where-Object { -not (Select-String -Quiet -SimpleMatch '$UMDFVERSION$' $_.FullName) } | Select-Object -First 1
if (-not $dll -or -not $inf) {
    Check $false 'the build produced DisplayLabIdd.dll and a stamped DisplayLabIdd.inf'
    "== Summary"; $failures | ForEach-Object { "FAIL  $_" }; exit 1
}
"stamped INF:"
Get-Content $inf.FullName | Select-String 'DriverVer|UmdfLibraryVersion'

# --- 2. Sign with our own certificate, trust it, install ----------------------

$pkg = Join-Path $work 'package'
New-Item -ItemType Directory -Force -Path $pkg | Out-Null
Copy-Item $dll.FullName, $inf.FullName $pkg

$bin = Get-ChildItem "$kits\bin" -Directory | Where-Object { $_.Name -like '10.*' } | Sort-Object Name -Descending | Select-Object -First 1
$signtool = Join-Path $bin.FullName 'x64\signtool.exe'
$inf2cat = Join-Path $bin.FullName 'x86\inf2cat.exe'
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
$devcon = Get-ChildItem "$kits\Tools" -Recurse -Filter devcon.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -like '*x64*' } | Select-Object -First 1
"devcon: $($devcon.FullName)"
$p = Start-Process -FilePath $devcon.FullName -ArgumentList 'install', (Join-Path $pkg 'DisplayLabIdd.inf'), 'Root\DisplayLabIdd' `
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

# What is on the desktop: the screens apps can draw on.
function Desktop {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.Screen]::AllScreens | ForEach-Object {
        [pscustomobject]@{ Device = $_.DeviceName; Bounds = "$($_.Bounds.X),$($_.Bounds.Y) $($_.Bounds.Width)x$($_.Bounds.Height)"; Primary = $_.Primary }
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

$stage = Pipe "PLUG 1 $(PlugArgs stage-monitor)"
"PLUG 1 stage-monitor: $stage"
Check (WaitFor { @(Monitors | Where-Object Serial -eq '4001').Count -eq 1 }) 'a second screen appears with its own identity (4001)'
Show 'after plugging the stage monitor'
$stageBefore = Monitors | Where-Object Serial -eq '4001'
$desktopBefore = @(Desktop).Count

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
Check (@(Desktop | Where-Object Bounds -like '* 1280x720').Count -ge 1) 'a screen comes up in the mode asked for (1280x720)'

"LIST: $(Pipe 'LIST')"
Check ((Pipe 'PLUG 1 00 1x1@1') -like 'ERR*') 'a bad request is refused, not crashed on'
Check ((Pipe 'PING') -like 'OK*') 'the driver still answers after a bad request'

foreach ($slot in 0, 1, 2) { "UNPLUG ${slot}: $(Pipe "UNPLUG $slot")" }
Check (WaitFor { @(Monitors).Count -eq $baseline }) 'unplugging everything leaves the runner as it was'
Show 'at the end'

"== Driver package"
Copy-Item $cer $pkg
Get-ChildItem $pkg | ForEach-Object { $_.FullName }

"== Summary"
if ($failures.Count -eq 0) { 'every check passed'; exit 0 }
$failures | ForEach-Object { "FAIL  $_" }
exit 1
