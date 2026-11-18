<#
    Downloads Microsoft's Windows Driver Kit as a NuGet package (about 111 MB)
    and extracts it to tools\wdk-nuget\pkg. Nothing is installed on the system;
    the driver build only uses the UMDF headers, WdfDriverStubUm.lib, hidport.h
    and Inf2Cat.exe from it.

    Usage:  powershell -ExecutionPolicy Bypass -File tools\get-wdk.ps1
#>
[CmdletBinding()]
param(
    [string]$Version = '10.0.26100.6584'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dir = Join-Path $here 'wdk-nuget'
$pkg = Join-Path $dir 'pkg'
$nupkg = Join-Path $dir 'wdk.nupkg'
$url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/$Version/microsoft.windows.wdk.x64.$Version.nupkg"

New-Item -ItemType Directory -Force $dir | Out-Null
if (-not (Test-Path $nupkg)) {
    Write-Host "Downloading $url ..."
    Invoke-WebRequest -Uri $url -OutFile $nupkg
}

if (Test-Path $pkg) { Remove-Item $pkg -Recurse -Force }
New-Item -ItemType Directory -Force $pkg | Out-Null

# a .nupkg is a zip archive; Windows' own tar.exe (bsdtar) can extract it
& (Join-Path $env:SystemRoot 'System32\tar.exe') -xf $nupkg -C $pkg
if ($LASTEXITCODE -ne 0) { throw 'Extracting the WDK package failed.' }

Write-Host "WDK $Version extracted to $pkg" -ForegroundColor Green
