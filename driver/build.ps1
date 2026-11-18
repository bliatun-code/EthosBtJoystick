<#
    Builds the Ethos BT Joystick UMDF 2 driver package:

        driver\out\package\ethosjoy.dll
        driver\out\package\ethosjoy.inf
        driver\out\package\ethosjoy.cat   (unsigned - it is signed on the target PC at install time)

    Requirements:
      * Visual Studio 2022 (or Build Tools) with the C++ x64 toolset and a Windows 10/11 SDK
      * The WDK NuGet package (Microsoft.Windows.WDK.x64) extracted to tools\wdk-nuget\pkg
        (only the UMDF headers, WdfDriverStubUm.lib, hidport.h and Inf2Cat.exe are used)
#>
[CmdletBinding()]
param(
    [string]$WdkRoot = '',
    [string]$WdkVersion = '10.0.26100.0',
    [string]$UmdfVersion = '2.15'
)

$ErrorActionPreference = 'Stop'

function Fail($msg) { throw $msg }

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $WdkRoot) { $WdkRoot = Join-Path $here '..\tools\wdk-nuget\pkg\c' }

# --- locate tools ---------------------------------------------------------
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) { Fail 'vswhere.exe not found - install Visual Studio Build Tools (C++ workload).' }
$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) { Fail 'No Visual Studio installation with the C++ x64 toolset was found.' }
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) { Fail "vcvars64.bat not found at $vcvars" }

$WdkRoot = (Resolve-Path $WdkRoot).Path
$umdfInc = Join-Path $WdkRoot "Include\wdf\umdf\$UmdfVersion"
$umdfLib = Join-Path $WdkRoot "Lib\wdf\umdf\x64\$UmdfVersion\WdfDriverStubUm.lib"
$hidport = Join-Path $WdkRoot "Include\$WdkVersion\km\hidport.h"
$inf2cat = Join-Path $WdkRoot "bin\$WdkVersion\x86\Inf2Cat.exe"
foreach ($p in $umdfInc, $umdfLib, $hidport, $inf2cat) {
    if (-not (Test-Path $p)) { Fail "WDK file not found: $p" }
}

# --- prepare output dirs --------------------------------------------------
$out = Join-Path $here 'out'
$obj = Join-Path $out 'obj'
$inc = Join-Path $out 'inc'
$pkg = Join-Path $out 'package'
foreach ($d in $obj, $inc, $pkg) {
    if (Test-Path $d) { Remove-Item $d -Recurse -Force }
    New-Item -ItemType Directory -Force $d | Out-Null
}

# hidport.h lives in the kernel-mode include dir; copy just that header so the
# rest of the km headers cannot shadow the user-mode SDK headers.
Copy-Item $hidport $inc

# --- compile + link -------------------------------------------------------
$minor = $UmdfVersion.Split('.')[1]
$defines = @(
    '/DUNICODE', '/D_UNICODE', '/DWIN32_LEAN_AND_MEAN=1',
    '/D_WIN32_WINNT=0x0A00', '/DWINVER=0x0A00', '/DWINNT=1', '/DNTDDI_VERSION=0x0A000000',
    '/DUMDF_VERSION_MAJOR=2', "/DUMDF_VERSION_MINOR=$minor"
) -join ' '

$src = Join-Path $here 'ethosjoy.c'
$rc  = Join-Path $here 'ethosjoy.rc'
$dll = Join-Path $pkg 'ethosjoy.dll'

# /wd4324: "structure was padded due to alignment specifier" comes from the WDF headers themselves
$cl   = "cl /nologo /c /W4 /WX /wd4324 /O2 /MT /GS /guard:cf /Zi /Fd`"$obj\ethosjoy_c.pdb`" $defines /I`"$umdfInc`" /I`"$inc`" /Fo`"$obj\ethosjoy.obj`" `"$src`""
$rcc  = "rc /nologo /fo`"$obj\ethosjoy.res`" `"$rc`""
$link = "link /nologo /DLL /OUT:`"$dll`" /PDB:`"$obj\ethosjoy.pdb`" /DEBUG /RELEASE /OPT:REF /OPT:ICF /SUBSYSTEM:WINDOWS /DYNAMICBASE /NXCOMPAT /guard:cf /MACHINE:X64 `"$obj\ethosjoy.obj`" `"$obj\ethosjoy.res`" `"$umdfLib`" ntdll.lib kernel32.lib"

$script = Join-Path $obj 'build.cmd'
@"
@echo off
call "$vcvars" >nul 2>nul || exit /b 1
$cl || exit /b 1
$rcc || exit /b 1
$link || exit /b 1
"@ | Set-Content -Encoding ascii $script

& cmd.exe /c "`"$script`""
if ($LASTEXITCODE -ne 0) { Fail "Driver build failed (exit code $LASTEXITCODE)." }

# the import library / export file are not part of the driver package
Get-ChildItem $pkg -Include *.lib, *.exp -Recurse | Remove-Item -Force

# --- package: INF + catalog ----------------------------------------------
Copy-Item (Join-Path $here 'ethosjoy.inf') $pkg

& $inf2cat "/driver:$pkg" '/os:10_X64' '/uselocaltime'
if ($LASTEXITCODE -ne 0) { Fail "Inf2Cat failed (exit code $LASTEXITCODE)." }

Write-Host ''
Write-Host 'Driver package:' -ForegroundColor Green
Get-ChildItem $pkg | ForEach-Object { '  {0,-16} {1,8} bytes' -f $_.Name, $_.Length }
