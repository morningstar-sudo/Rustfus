[CmdletBinding()]
param(
    # Must equal the workspace version when given; defaults to it.
    [string]$Version,
    [string]$Target = "x86_64-pc-windows-msvc"
)

# Assembles the portable release: Rustfus.exe (WPF front end), rustfus_ffi.dll (Rust core) and
# rustfus-worker.exe (elevated writer), plus license files, as a ZIP with a SHA-256 sidecar.
# Run after `cargo build --release --locked --target <Target> -p rustfus-ffi -p rustfus-worker`.
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

$workspaceManifest = Get-Content -LiteralPath (Join-Path $root "Cargo.toml") -Raw
$versionMatch = [regex]::Match($workspaceManifest, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw "Could not read the workspace version from Cargo.toml" }
$workspaceVersion = $versionMatch.Groups[1].Value
if (-not $Version) { $Version = $workspaceVersion }
if ($Version -ne $workspaceVersion) {
    throw "Package version $Version does not match workspace version $workspaceVersion"
}

$release = Join-Path $root "target\$Target\release"
$dist = Join-Path $root "dist"
$packageName = "Rustfus-$Version-windows-x64"
$package = [IO.Path]::GetFullPath((Join-Path $dist $packageName))
$archive = Join-Path $dist "$packageName.zip"
$checksum = "$archive.sha256"

if (-not $package.StartsWith([IO.Path]::GetFullPath($dist) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to package outside the repository dist directory"
}

$worker = Join-Path $release "rustfus-worker.exe"
$core = Join-Path $release "rustfus_ffi.dll"
foreach ($required in $worker, $core) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "$required is missing. Build the Windows target before packaging."
    }
}

$guiDirectory = Join-Path $root "target\gui"
& (Join-Path $PSScriptRoot "build-gui.ps1") -Version $Version -Output $guiDirectory
$gui = Join-Path $guiDirectory "Rustfus.exe"
if (-not (Test-Path -LiteralPath $gui)) { throw "The front end was not built" }

New-Item -ItemType Directory -Force -Path $dist | Out-Null
foreach ($old in @($package, $archive, $checksum)) {
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force }
}
New-Item -ItemType Directory -Path $package | Out-Null

Copy-Item -LiteralPath $gui -Destination $package
Copy-Item -LiteralPath $core -Destination $package
Copy-Item -LiteralPath $worker -Destination $package
foreach ($document in "LICENSE", "README.md", "CHANGELOG.md", "SECURITY.md") {
    Copy-Item -LiteralPath (Join-Path $root $document) -Destination $package
}
& (Join-Path $PSScriptRoot "third-party-notices.ps1") `
    -Output (Join-Path $package "THIRD-PARTY-NOTICES.txt") -Target $Target

Compress-Archive -LiteralPath $package -DestinationPath $archive -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $packageName.zip" | Set-Content -LiteralPath $checksum -Encoding ascii

Write-Host "Created $archive ($([int]((Get-Item -LiteralPath $archive).Length / 1KB)) KB)"
Write-Host "SHA-256 $hash"
