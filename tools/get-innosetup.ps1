<#
    Downloads Inno Setup (about 11 MB) and unpacks it to tools\innosetup in its
    portable mode: no administrator rights, no entry in Windows' list of apps,
    no shortcuts and no file association. Only ISCC.exe, the command line
    compiler, is used - by tools\package-release.ps1 to build the installer.

    The download comes from the Inno Setup project's own releases on GitHub. It
    must match the SHA-256 below and carry a valid signature before it is run.

    GitHub's Windows build image has this version preinstalled, so CI does not
    need this script. Inno Setup is free for non-commercial use, see
    https://jrsoftware.org/isinfo.php

    Usage:  powershell -ExecutionPolicy Bypass -File tools\get-innosetup.ps1
#>
[CmdletBinding()]
param(
    [string]$Version = '6.7.1',
    [string]$Sha256 = '4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dir = Join-Path $here 'innosetup'
$download = Join-Path $here 'innosetup-download'
$setup = Join-Path $download "innosetup-$Version.exe"
$tag = 'is-' + ($Version -replace '\.', '_')
$url = "https://github.com/jrsoftware/issrc/releases/download/$tag/innosetup-$Version.exe"

if (Test-Path $download) { Remove-Item $download -Recurse -Force }
New-Item -ItemType Directory -Force $download | Out-Null
try {
    Write-Host "Downloading $url ..."
    Invoke-WebRequest -Uri $url -OutFile $setup

    $hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
    if ($hash -ne $Sha256.ToLower()) { throw "The download has SHA-256 $hash, expected $Sha256." }
    $signature = Get-AuthenticodeSignature -FilePath $setup
    if ($signature.Status -ne 'Valid') { throw "The download is not validly signed: $($signature.StatusMessage)" }
    Write-Host "Checked: SHA-256 matches, signed by $($signature.SignerCertificate.Subject)"

    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $arguments = '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', '/PORTABLE=1', "/DIR=`"$dir`""
    $process = Start-Process -FilePath $setup -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Unpacking Inno Setup failed (exit code $($process.ExitCode))." }
}
finally {
    Remove-Item $download -Recurse -Force -ErrorAction SilentlyContinue
}

$iscc = Join-Path $dir 'ISCC.exe'
if (-not (Test-Path $iscc)) { throw "ISCC.exe is missing in $dir." }
Write-Host "Inno Setup $Version unpacked to $dir" -ForegroundColor Green
