[CmdletBinding()]
param(
    # Defaults to the workspace version in Cargo.toml.
    [string]$Version,
    # Defaults to target\gui.
    [string]$Output
)

# Builds the WPF front end (Rustfus.exe) with the C# compiler that ships with Windows: no SDK, no
# NuGet, no installer. The XAML window is embedded in the executable.
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$source = Join-Path $root "apps\rustfus-gui"

if (-not $Version) {
    $manifest = Get-Content -LiteralPath (Join-Path $root "Cargo.toml") -Raw
    $match = [regex]::Match($manifest, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
    if (-not $match.Success) { throw "Could not read the workspace version from Cargo.toml" }
    $Version = $match.Groups[1].Value
}
if (-not $Output) { $Output = Join-Path $root "target\gui" }

$framework = Join-Path $env:WINDIR "Microsoft.NET\Framework64\v4.0.30319"
$csc = Join-Path $framework "csc.exe"
$wpf = Join-Path $framework "WPF"
if (-not (Test-Path -LiteralPath $csc)) {
    throw "The .NET Framework 4 C# compiler was not found at $csc"
}

# 1.2.3-rc.1 -> assembly version 1.2.3.0, informational version 1.2.3-rc.1
$numeric = ($Version -split '[-+]')[0].Split('.')
while ($numeric.Count -lt 4) { $numeric += "0" }
$assemblyVersion = ($numeric[0..3]) -join "."

New-Item -ItemType Directory -Force -Path $Output | Out-Null
$assemblyInfo = Join-Path $Output "AssemblyInfo.g.cs"
@"
using System.Reflection;
[assembly: AssemblyTitle("Rustfus")]
[assembly: AssemblyProduct("Rustfus")]
[assembly: AssemblyCopyright("Licensed under the Apache License 2.0")]
[assembly: AssemblyVersion("$assemblyVersion")]
[assembly: AssemblyFileVersion("$assemblyVersion")]
[assembly: AssemblyInformationalVersion("$Version")]
"@ | Set-Content -LiteralPath $assemblyInfo -Encoding UTF8

$exe = Join-Path $Output "Rustfus.exe"
$sources = @(Get-ChildItem -LiteralPath $source -Filter "*.cs" | Select-Object -ExpandProperty FullName) + $assemblyInfo
& $csc -nologo -target:winexe -platform:x64 -optimize+ -debug- -warn:4 -warnaserror+ `
    "-out:$exe" `
    "-win32manifest:$(Join-Path $source 'app.manifest')" `
    "-resource:$(Join-Path $source 'MainWindow.xaml'),Rustfus.MainWindow.xaml" `
    "-r:$wpf\PresentationFramework.dll" "-r:$wpf\PresentationCore.dll" "-r:$wpf\WindowsBase.dll" `
    "-r:System.Xaml.dll" `
    $sources
if ($LASTEXITCODE -ne 0) { throw "The C# compiler failed with exit code $LASTEXITCODE" }

Write-Host "Built $exe ($([int]((Get-Item -LiteralPath $exe).Length / 1KB)) KB, version $Version)"
