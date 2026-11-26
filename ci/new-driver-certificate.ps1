# Make the certificate Display Lab's Windows driver is signed with in
# releases, and print the two repository secrets to store it as.
#
# Run once, on your own machine, in PowerShell:
#   ./ci/new-driver-certificate.ps1
# then follow what it prints. The private key stays in the secret (and in
# the .pfx, which you should keep somewhere safe or delete).
#
# `displaylab driver install` trusts this certificate for code signing on
# the machines it installs on, so guard the private key like a password.

param([int]$Years = 5)

$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Display Lab driver' `
    -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears($Years)
$password = -join ((48..57) + (65..90) + (97..122) | Get-Random -Count 32 | ForEach-Object { [char]$_ })
$pfx = Join-Path (Get-Location) 'display-lab-driver.pfx'
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"

"Made $pfx ($($cert.Subject), thumbprint $($cert.Thumbprint), until $($cert.NotAfter.ToString('yyyy-MM-dd')))."
""
"Store it as two secrets of the GitHub repository (Settings > Secrets and variables > Actions):"
"  WINDOWS_DRIVER_PFX           the file, in base64"
"  WINDOWS_DRIVER_PFX_PASSWORD  $password"
""
"With the GitHub CLI:"
"  gh secret set WINDOWS_DRIVER_PFX --body ([Convert]::ToBase64String([IO.File]::ReadAllBytes('$pfx')))"
"  gh secret set WINDOWS_DRIVER_PFX_PASSWORD --body '$password'"
