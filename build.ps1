<#
    Builds everything:

        1. driver\out\package\   the virtual joystick driver (DLL + INF + catalog)
        2. dist\EthosBtJoystick.exe   the app, with the driver package embedded

    Requirements: Rust (stable, x86_64-pc-windows-msvc), Visual Studio 2022 Build
    Tools with the C++ toolset + Windows SDK, and the WDK NuGet package in
    tools\wdk-nuget (run tools\get-wdk.ps1 once to download it).

    Usage:  powershell -ExecutionPolicy Bypass -File build.ps1 [-SkipDriver] [-Test]
#>
[CmdletBinding()]
param(
    [switch]$SkipDriver,   # reuse the driver package from the last build
    [switch]$Test          # also run the unit tests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not $SkipDriver) {
    Write-Host '== Driver ==' -ForegroundColor Cyan
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'driver\build.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'The driver build failed.' }
}

Write-Host '== App ==' -ForegroundColor Cyan
Push-Location (Join-Path $root 'app')
try {
    if ($Test) {
        & cargo test
        if ($LASTEXITCODE -ne 0) { throw 'The tests failed.' }
    }
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'The app build failed.' }
}
finally {
    Pop-Location
}

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item (Join-Path $root 'app\target\release\EthosBtJoystick.exe') $dist -Force

Write-Host ''
Write-Host ('Done: ' + (Join-Path $dist 'EthosBtJoystick.exe')) -ForegroundColor Green
