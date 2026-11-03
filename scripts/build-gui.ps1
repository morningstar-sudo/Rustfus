[CmdletBinding()]
param(
    # Build stamp shown in the log and file properties; defaults to the current UTC time.
    [string]$Stamp = (Get-Date).ToUniversalTime().ToString("yyyyMMdd.HHmmss"),
    # Defaults to target\gui.
    [string]$Output
)

# Builds the WPF front end (Rustfus.exe) with the C# compiler that ships with Windows: no SDK, no
# NuGet, no installer. The XAML window is embedded in the executable.
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$source = Join-Path $root "apps\rustfus-gui"

if (-not $Output) { $Output = Join-Path $root "target\gui" }

$framework = Join-Path $env:WINDIR "Microsoft.NET\Framework64\v4.0.30319"
$csc = Join-Path $framework "csc.exe"
$wpf = Join-Path $framework "WPF"
if (-not (Test-Path -LiteralPath $csc)) {
    throw "The .NET Framework 4 C# compiler was not found at $csc"
}

New-Item -ItemType Directory -Force -Path $Output | Out-Null
$assemblyInfo = Join-Path $Output "AssemblyInfo.g.cs"
@"
using System.Reflection;
[assembly: AssemblyTitle("Rustfus")]
[assembly: AssemblyProduct("Rustfus")]
[assembly: AssemblyCopyright("Licensed under the Apache License 2.0")]
[assembly: AssemblyInformationalVersion("$Stamp")]
"@ | Set-Content -LiteralPath $assemblyInfo -Encoding UTF8

$exe = Join-Path $Output "Rustfus.exe"
$sources = @(Get-ChildItem -LiteralPath $source -Filter "*.cs" | Select-Object -ExpandProperty FullName) + $assemblyInfo
& $csc -nologo -target:winexe -platform:x64 -optimize+ -debug- -warn:4 -warnaserror+ `
    "-out:$exe" `
    "-win32manifest:$(Join-Path $source 'app.manifest')" `
    "-resource:$(Join-Path $source 'MainWindow.xaml'),Rustfus.MainWindow.xaml" `
    "-r:$wpf\PresentationFramework.dll" "-r:$wpf\PresentationCore.dll" "-r:$wpf\WindowsBase.dll" `
    "-r:System.Xaml.dll" "-r:System.Xml.dll" `
    $sources
if ($LASTEXITCODE -ne 0) { throw "The C# compiler failed with exit code $LASTEXITCODE" }

Write-Host "Built $exe ($([int]((Get-Item -LiteralPath $exe).Length / 1KB)) KB, build $Stamp)"
