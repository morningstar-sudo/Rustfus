[CmdletBinding()]
param(
    # UTC build stamp (yyyyMMdd.HHmmss) that names the package; defaults to now.
    [string]$Stamp = (Get-Date).ToUniversalTime().ToString("yyyyMMdd.HHmmss"),
    [string]$Target = "x86_64-pc-windows-msvc"
)

# Assembles the portable release: Rustfus.exe (WPF front end) and rustfus_ffi.dll (Rust core and
# media writer), plus non-Markdown license notices, as a ZIP with a SHA-256 sidecar. Repository
# Markdown files are intentionally not shipped. Signs both binaries first (scripts/sign.ps1).
# Run after `cargo build --release --locked --target <Target> -p rustfus-ffi`.
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

$release = Join-Path $root "target\$Target\release"
$dist = Join-Path $root "dist"
$packageName = "Rustfus-$Stamp-windows-x64"
$package = [IO.Path]::GetFullPath((Join-Path $dist $packageName))
$archive = Join-Path $dist "$packageName.zip"
$checksum = "$archive.sha256"

if (-not $package.StartsWith([IO.Path]::GetFullPath($dist) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to package outside the repository dist directory"
}

$core = Join-Path $release "rustfus_ffi.dll"
foreach ($required in @($core)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "$required is missing. Build the Windows target before packaging."
    }
}

$guiDirectory = Join-Path $root "target\gui"
& (Join-Path $PSScriptRoot "build-gui.ps1") -Stamp $Stamp -Output $guiDirectory
$gui = Join-Path $guiDirectory "Rustfus.exe"
if (-not (Test-Path -LiteralPath $gui)) { throw "The front end was not built" }

# Sign before zipping; the package gets the signed copies. Signing the cargo outputs in place is
# fine: the build tree is disposable.
& (Join-Path $PSScriptRoot "sign.ps1") -Path $gui, $core

New-Item -ItemType Directory -Force -Path $dist | Out-Null
foreach ($old in @($package, $archive, $checksum)) {
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force }
}
New-Item -ItemType Directory -Path $package | Out-Null

Copy-Item -LiteralPath $gui -Destination $package
Copy-Item -LiteralPath $core -Destination $package
Copy-Item -LiteralPath (Join-Path $root "LICENSE") -Destination $package
& (Join-Path $PSScriptRoot "third-party-notices.ps1") `
    -Output (Join-Path $package "THIRD-PARTY-NOTICES.txt") -Target $Target

$markdown = @(Get-ChildItem -LiteralPath $package -Recurse -File -Filter "*.md")
if ($markdown.Count -ne 0) {
    throw "Markdown files must not be included in the release package: $($markdown.FullName -join ', ')"
}

Compress-Archive -LiteralPath $package -DestinationPath $archive -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $packageName.zip" | Set-Content -LiteralPath $checksum -Encoding ascii

Write-Host "Created $archive ($([int]((Get-Item -LiteralPath $archive).Length / 1KB)) KB)"
Write-Host "SHA-256 $hash"
