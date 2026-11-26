# Build and sign Display Lab's display driver into a package folder:
# DisplayLabIdd.dll, .inf, .cat and the .cer it is signed with, which is what
# `displaylab driver install --from <folder>` takes.
#
#   ci/build-windows-driver.ps1 -Out dist\driver                     # throwaway certificate
#   ci/build-windows-driver.ps1 -Out dist\driver -Pfx cert.pfx -PfxPassword ...
#
# Needs Visual Studio (with its driver toolset) and nuget; the Windows Driver
# Kit itself is restored from NuGet.

param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Pfx = '',
    [string]$PfxPassword = ''
)

# (PowerShell names are not case-sensitive: nothing below may be called $out.)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path ([System.IO.Path]::GetTempPath()) 'displaylab-idd-build'
New-Item -ItemType Directory -Force -Path $work | Out-Null

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
$buildDir = Join-Path $work 'build\'
& $msbuild $proj /nologo /v:minimal /p:Configuration=Release /p:Platform=x64 "/p:OutDir=$buildDir" "/p:IntDir=$work\obj\"
if ($LASTEXITCODE -ne 0) { "the driver did not build"; exit 1 }
$dll = Get-ChildItem -Recurse $buildDir -Filter DisplayLabIdd.dll | Select-Object -First 1
if (-not $dll) { "the build produced no DisplayLabIdd.dll"; exit 1 }

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

New-Item -ItemType Directory -Force -Path $Out | Out-Null
Copy-Item $dll.FullName, $inf $Out

$signtool = Tool 'signtool.exe' 'x64'
$inf2cat = Tool 'inf2cat.exe' 'x86'
"signtool: $signtool"
"inf2cat: $inf2cat"

"== Certificate"
if ($Pfx) {
    # The release certificate. Its private key never leaves the secret store.
    $password = ConvertTo-SecureString $PfxPassword -AsPlainText -Force
    $cert = Import-PfxCertificate -FilePath $Pfx -Password $password -CertStoreLocation Cert:\CurrentUser\My
} else {
    # A throwaway for CI: good for two days, on this runner only.
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Display Lab driver (CI)' `
        -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature -NotAfter (Get-Date).AddDays(2)
}
Export-Certificate -Cert $cert -FilePath (Join-Path $Out 'DisplayLabIdd.cer') | Out-Null
"$($cert.Subject), thumbprint $($cert.Thumbprint)"

"== Sign"
& $signtool sign /fd sha256 /sha1 $cert.Thumbprint /s My (Join-Path $Out 'DisplayLabIdd.dll')
& $inf2cat "/driver:$Out" '/os:10_x64,Server10_x64,10_NI_X64,10_GE_X64' /uselocaltime
& $signtool sign /fd sha256 /sha1 $cert.Thumbprint /s My (Join-Path $Out 'DisplayLabIdd.cat')
if ($LASTEXITCODE -ne 0) { "signing failed"; exit 1 }

"== Package"
Get-ChildItem $Out | ForEach-Object { $_.Name }

exit 0
