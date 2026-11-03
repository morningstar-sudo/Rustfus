[CmdletBinding()]
param(
    # Path of the release ZIP; its .sha256 sidecar must sit next to it.
    [Parameter(Mandatory)] [string]$Archive
)

# Checks a release package the way a user receives it: verify the checksum, unpack into a clean
# directory, make sure nothing links the Visual C++ runtime, run the front end's --self-test, and
# start the window in --demo mode (fake data, no disk access) to see that it stays up.
$ErrorActionPreference = "Stop"
$Archive = (Resolve-Path -LiteralPath $Archive).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("rustfus-smoke-" + [Guid]::NewGuid().ToString("N"))
try {
    $sidecar = "$Archive.sha256"
    $expected = ((Get-Content -LiteralPath $sidecar -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
    if ($expected -ne $actual) { throw "SHA-256 mismatch: sidecar $expected, archive $actual" }
    Write-Host "checksum ok"

    Expand-Archive -LiteralPath $Archive -DestinationPath $work
    $app = @(Get-ChildItem -LiteralPath $work -Directory)[0].FullName
    $required = "Rustfus.exe", "rustfus_ffi.dll", "LICENSE",
        "THIRD-PARTY-NOTICES.txt"
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $app $name))) { throw "The package is missing $name" }
    }
    if (Test-Path -LiteralPath (Join-Path $app "rustfus-worker.exe")) {
        throw "The legacy rustfus-worker.exe must not be included in the package"
    }
    $markdown = @(Get-ChildItem -LiteralPath $app -Recurse -File -Filter "*.md")
    if ($markdown.Count -ne 0) {
        throw "The package contains Markdown files: $($markdown.FullName -join ', ')"
    }
    Write-Host "package contents ok"

    # Without the static CRT the app fails on machines that lack the Visual C++ Redistributable.
    foreach ($name in @("rustfus_ffi.dll")) {
        $bytes = [IO.File]::ReadAllBytes((Join-Path $app $name))
        $text = [Text.Encoding]::ASCII.GetString($bytes).ToLowerInvariant()
        foreach ($forbidden in "vcruntime140", "api-ms-win-crt") {
            if ($text.Contains($forbidden)) { throw "$name imports $forbidden (the C runtime must be linked statically)" }
        }
    }
    Write-Host "no C runtime imports"

    $self = Start-Process -FilePath (Join-Path $app "Rustfus.exe") -ArgumentList "--self-test" -Wait -PassThru
    if ($self.ExitCode -ne 0) {
        throw "Rustfus.exe --self-test failed with exit code $($self.ExitCode) (10 library, 11 USB scan)"
    }
    Write-Host "self-test ok"

    $window = Start-Process -FilePath (Join-Path $app "Rustfus.exe") -ArgumentList "--demo" `
        -WorkingDirectory $app -PassThru
    Start-Sleep -Seconds 5
    $window.Refresh()
    if ($window.HasExited) { throw "The window exited early with code $($window.ExitCode)" }
    Stop-Process -Id $window.Id -Force
    Write-Host "window stays up"
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
