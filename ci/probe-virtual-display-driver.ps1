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

# Every install attempt runs in a child process with a deadline. The first
# run of this probe hung with no output: something in the install waited on
# a prompt nobody can answer. A deadline turns that into an answer.
function Run-WithDeadline([string]$label, [string]$command, [int]$seconds) {
    "-- $label (deadline $seconds s)"
    $out = Join-Path $work "$label.out.txt"
    $err = Join-Path $work "$label.err.txt"
    $p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile', '-NonInteractive', '-Command', $command `
        -RedirectStandardOutput $out -RedirectStandardError $err -PassThru -NoNewWindow
    if (-not $p.WaitForExit($seconds * 1000)) {
        "HUNG after $seconds s. Windows with a title at that moment:"
        Get-Process | Where-Object { $_.MainWindowTitle } | Format-Table -AutoSize Id, ProcessName, MainWindowTitle
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    } else {
        "exit code $($p.ExitCode)"
    }
    Get-Content $out -ErrorAction SilentlyContinue
    Get-Content $err -ErrorAction SilentlyContinue
}

# The x64 build is the folder the release calls "x86".
$driverDir = Get-ChildItem -Recurse -Directory $work -Filter 'VirtualDisplayDriver' |
    Where-Object { $_.FullName -like '*x86.Driver.Only*' } | Select-Object -First 1
$inf = Get-ChildItem $driverDir.FullName -Filter *.inf | Select-Object -First 1
$cat = Get-ChildItem $driverDir.FullName -Filter *.cat | Select-Object -First 1
$devcon = Get-ChildItem -Recurse $work -Filter devcon.exe | Select-Object -First 1
"INF: $($inf.FullName)"
"devcon: $($devcon.FullName)"

"== Driver settings shipped with the release (monitor count, EDID, modes)"
Get-Content (Join-Path $driverDir.FullName 'vdd_settings.xml')

"== Trust the driver's publisher"
# The first run hung inside pnputil: a driver signed by anyone other than
# Microsoft asks "Would you like to install this device software?" unless
# its publisher is already trusted. Trusting it first removes the prompt.
$signer = (Get-AuthenticodeSignature $cat.FullName).SignerCertificate
$certFile = Join-Path $work 'vdd-signer.cer'
Export-Certificate -Cert $signer -FilePath $certFile | Out-Null
Import-Certificate -FilePath $certFile -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Format-List Subject, Thumbprint

"== Install"
Run-WithDeadline 'devcon-install' "& '$($devcon.FullName)' install '$($inf.FullName)' 'Root\MttVDD'" 180

"== Where the driver reads its settings"
foreach ($path in @('C:\VirtualDisplayDriver\vdd_settings.xml', 'C:\IddSampleDriver\option.txt')) {
    if (Test-Path $path) { "found $path"; Get-Content $path }
}
Get-ItemProperty -Path 'HKLM:\SOFTWARE\MikeTheTech\VirtualDisplayDriver' -ErrorAction SilentlyContinue | Format-List

Start-Sleep -Seconds 10
"== Display devices"
Get-PnpDevice -Class Display, Monitor | Format-Table -AutoSize Status, Class, FriendlyName, InstanceId
"== Screens after"
Screens
