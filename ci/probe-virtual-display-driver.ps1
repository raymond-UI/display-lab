# Can this machine install the Virtual Display Driver and get a monitor from
# it? Prints everything it learns; failing steps are reported, not fatal, so
# one run answers as many questions as possible.
$ErrorActionPreference = 'Continue'

function Screens {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.Screen]::AllScreens | ForEach-Object {
        "{0}  {1}x{2}  primary={3}" -f $_.DeviceName, $_.Bounds.Width, $_.Bounds.Height, $_.Primary
    }
}

"== Windows"
[System.Environment]::OSVersion.VersionString
"== Session"
"session id: " + (Get-Process -Id $PID).SessionId
"== Screens before"
Screens

"== Latest driver release"
$repo = 'VirtualDrivers/Virtual-Display-Driver'
gh release view -R $repo --json tagName,assets -q '.tagName, (.assets[] | .name)'

$work = Join-Path $env:RUNNER_TEMP 'vdd'
New-Item -ItemType Directory -Force -Path $work | Out-Null
gh release download -R $repo --dir $work --pattern '*.zip'
Get-ChildItem $work -Filter *.zip | ForEach-Object {
    Expand-Archive -Force $_.FullName -DestinationPath (Join-Path $work $_.BaseName)
}
"== Files"
Get-ChildItem -Recurse $work -Include *.inf, *.cat, *.sys, *.ps1, *.exe, *.xml | ForEach-Object { $_.FullName }

"== Signature of the driver catalog"
Get-ChildItem -Recurse $work -Include *.cat | Select-Object -First 3 | ForEach-Object {
    (Get-AuthenticodeSignature $_.FullName) | Format-List Status, StatusMessage, SignerCertificate
}

"== Install with the project's own script, if the release ships one"
$script = Get-ChildItem -Recurse $work -Filter 'virtual-driver-manager.ps1' | Select-Object -First 1
if ($script) {
    & $script.FullName -Action install -Silent -Json
} else {
    "no virtual-driver-manager.ps1 in the release; trying pnputil on the INF"
    $inf = Get-ChildItem -Recurse $work -Filter *.inf | Select-Object -First 1
    if ($inf) {
        pnputil /add-driver $inf.FullName /install
        # An IddCx driver is root-enumerated: the device node has to be
        # created, which pnputil can do on recent Windows.
        pnputil /add-device "Root\MttVDD" 2>&1
    } else {
        "no INF found"
    }
}

Start-Sleep -Seconds 10
"== Display devices"
Get-PnpDevice -Class Display, Monitor | Format-Table -AutoSize Status, Class, FriendlyName, InstanceId
"== Screens after"
Screens
