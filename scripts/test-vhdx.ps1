$ErrorActionPreference = "Stop"

# Runs the disposable-VHDX acceptance tests of rustfus-windows. Each test creates its own VHDX in a
# temporary directory, takes the physical disk number only from that VHD handle, and never opens a
# real disk. Needs an elevated Windows session:
#   - disposable_vhdx_formats_mounts_and_passes_chkdsk   partition + FAT32 + probe file + chkdsk
#   - disposable_vhdx_raw_write_round_trips_and_verifies raw image write, read-back, separate re-read

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$administrator = [Security.Principal.WindowsBuiltInRole]::Administrator
if (-not $principal.IsInRole($administrator)) {
    throw "The disposable VHDX integration tests require an elevated Windows runner."
}

$testArguments = @("disposable_vhdx_", "--ignored", "--nocapture", "--test-threads=1")

Push-Location (Join-Path $PSScriptRoot "..")
try {
    $cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if ($cargo) {
        & $cargo.Source test -p rustfus-windows --target x86_64-pc-windows-msvc -- @testArguments
    }
    else {
        $testRoot = [IO.Path]::GetFullPath((Join-Path (Get-Location) "target\x86_64-pc-windows-msvc\debug\deps"))
        $test = Get-ChildItem -LiteralPath $testRoot -Filter "rustfus_windows-*.exe" -File |
            Sort-Object LastWriteTimeUtc -Descending |
            Select-Object -First 1
        if (-not $test -or -not $test.FullName.StartsWith($testRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "No Windows-native rustfus-windows test binary was found. Build it with cargo-xwin first."
        }
        & $test.FullName @testArguments
    }
    if ($LASTEXITCODE -ne 0) {
        throw "The disposable VHDX integration tests failed."
    }
}
finally {
    Pop-Location
}
