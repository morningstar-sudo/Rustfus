[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string[]]$Path
)

# Authenticode-signs the given files with a self-signed certificate. A self-signed signature gives
# the files a publisher name ("Rustfus") and makes tampering detectable, but Windows still shows
# "Unknown Publisher" / SmartScreen until the user trusts the certificate.
#
# CODESIGN_PFX_BASE64 / CODESIGN_PFX_PASSWORD (optional) hold a persistent certificate made by
# scripts/new-signing-cert.ps1, so every build carries the same identity and a user can trust it
# once. Without them a throwaway certificate is created for this run.
$ErrorActionPreference = "Stop"

if ($env:CODESIGN_PFX_BASE64) {
    $bytes = [Convert]::FromBase64String($env:CODESIGN_PFX_BASE64)
    $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($bytes, $env:CODESIGN_PFX_PASSWORD)
}
else {
    Write-Warning "CODESIGN_PFX_BASE64 is not set; signing with a throwaway self-signed certificate."
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=Rustfus" `
        -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(5)
}

foreach ($file in $Path) {
    $result = Set-AuthenticodeSignature -LiteralPath $file -Certificate $cert -HashAlgorithm SHA256 `
        -TimestampServer http://timestamp.digicert.com
    # "UnknownError" just means the self-signed root is not trusted on this machine.
    if ($result.Status -eq "NotSigned" -or $result.SignerCertificate.Thumbprint -ne $cert.Thumbprint) {
        throw "Signing $file failed: $($result.StatusMessage)"
    }
    Write-Host "signed $(Split-Path -Leaf $file)"
}
