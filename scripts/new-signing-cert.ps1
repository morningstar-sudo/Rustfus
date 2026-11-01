[CmdletBinding()]
param(
    [string]$Output
)

# Creates a persistent self-signed code-signing certificate (valid 10 years). Store
# `codesign-pfx-base64.txt` as the repository secret CODESIGN_PFX_BASE64 and the printed password
# as CODESIGN_PFX_PASSWORD. Publish Rustfus.cer; a user who installs it into "Trusted Root
# Certification Authorities" and "Trusted Publishers" stops seeing the warning. Keep the .pfx private.
$ErrorActionPreference = "Stop"
# $PSScriptRoot is empty inside a param() default in Windows PowerShell 5.1, so default here.
if (-not $Output) { $Output = Join-Path $PSScriptRoot "..\target\signing" }
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path -LiteralPath $Output).Path

$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=Rustfus" `
    -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(10)
$random = [byte[]]::new(18)
$generator = [Security.Cryptography.RandomNumberGenerator]::Create()
$generator.GetBytes($random)
$generator.Dispose()
$password = [Convert]::ToBase64String($random)
$pfx = Join-Path $Output "Rustfus.pfx"
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null
Export-Certificate -Cert $cert -FilePath (Join-Path $Output "Rustfus.cer") | Out-Null
[Convert]::ToBase64String([IO.File]::ReadAllBytes($pfx)) | Set-Content (Join-Path $Output "codesign-pfx-base64.txt") -Encoding ascii
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"

Write-Host "Files in $Output"
Write-Host "CODESIGN_PFX_PASSWORD = $password"
