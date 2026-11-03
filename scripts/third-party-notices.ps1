[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Output,
    [string]$Target = "x86_64-pc-windows-msvc"
)

# Lists every third-party Rust crate linked into rustfus_ffi.dll, with its
# license expression and source, followed by the license files shipped inside each crate. Build-time
# only crates (proc-macros and what they pull in) are not linked and are left out. Uses only
# `cargo tree` and the local cargo registry, so no extra tool is needed.
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }

$seen = @{}
Push-Location $root
try {
    foreach ($crate in @("rustfus-ffi")) {
        $lines = & cargo tree -p $crate --target $Target --locked --prefix none `
            -e "normal,no-proc-macro" --format "{p}|{l}|{r}"
        if ($LASTEXITCODE -ne 0) { throw "cargo tree failed for $crate" }
        foreach ($line in $lines) {
            if ($line -match '^(?<name>[A-Za-z0-9_\-]+) v(?<version>[^ |]+)[^|]*\|(?<license>[^|]*)\|(?<repo>.*)$') {
                if ($Matches.name -like "rustfus-*") { continue }
                $seen["$($Matches.name) $($Matches.version)"] = [pscustomobject]@{
                    Name = $Matches.name; Version = $Matches.version
                    License = $Matches.license.Trim(); Repository = $Matches.repo.Trim()
                }
            }
        }
    }
}
finally { Pop-Location }
if ($seen.Count -eq 0) { throw "No third-party crates were found; refusing to write an empty notices file" }

$text = New-Object Text.StringBuilder
[void]$text.AppendLine("Rustfus is licensed under the Apache License 2.0 (see LICENSE).")
[void]$text.AppendLine("")
[void]$text.AppendLine("It links the following third-party Rust crates. Their licenses follow.")
[void]$text.AppendLine("")
foreach ($crate in $seen.Values | Sort-Object Name) {
    [void]$text.AppendLine(("{0} {1} - {2}" -f $crate.Name, $crate.Version, $crate.License))
    if ($crate.Repository) { [void]$text.AppendLine("    $($crate.Repository)") }
}
foreach ($crate in $seen.Values | Sort-Object Name) {
    $sourceDir = Get-ChildItem -Path (Join-Path $cargoHome "registry\src\*\$($crate.Name)-$($crate.Version)") `
        -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
    [void]$text.AppendLine("")
    [void]$text.AppendLine(("=" * 78))
    [void]$text.AppendLine("$($crate.Name) $($crate.Version) ($($crate.License))")
    [void]$text.AppendLine(("=" * 78))
    $licenseFiles = @()
    if ($sourceDir) {
        $licenseFiles = @(Get-ChildItem -LiteralPath $sourceDir.FullName -File |
            Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)' })
    }
    if ($licenseFiles.Count -eq 0) {
        [void]$text.AppendLine("(No license file is distributed inside this crate; see its repository and the license expression above.)")
    }
    foreach ($file in $licenseFiles) {
        [void]$text.AppendLine("--- $($file.Name)")
        [void]$text.AppendLine([IO.File]::ReadAllText($file.FullName).TrimEnd())
        [void]$text.AppendLine("")
    }
}
[IO.File]::WriteAllText($Output, $text.ToString(), (New-Object Text.UTF8Encoding($false)))
Write-Host "Wrote $Output ($($seen.Count) crates)"
