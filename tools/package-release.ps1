<#
    Packages a built app into a release zip:

        dist\EthosBtJoystick-v<version>-win-x64.zip
            EthosBtJoystick.exe
            README.txt
            LICENSE.txt                   (MIT, this project)
            LICENSE-driver-MS-PL.txt      (the embedded driver)
            THIRD-PARTY-LICENSES.txt      (Rust crates and fonts built into the exe)

    Run build.ps1 first. The third-party list is generated from `cargo tree`
    and the license files that ship inside each crate.

    Usage:  powershell -ExecutionPolicy Bypass -File tools\package-release.ps1
#>
[CmdletBinding()]
param(
    [string]$Version = ''
)

$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$app  = Join-Path $root 'app'
$exe  = Join-Path $app 'target\release\EthosBtJoystick.exe'
if (-not (Test-Path $exe)) { throw "The app is not built yet - run build.ps1 first ($exe is missing)." }

if (-not $Version) {
    $match = Select-String -Path (Join-Path $app 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    $Version = $match.Matches[0].Groups[1].Value
}
$name = "EthosBtJoystick-v$Version-win-x64"

# --- third-party components ------------------------------------------------
# (run through cmd so cargo's progress output on stderr cannot become an error)
Push-Location $app
try {
    # make sure every crate's source (with its license files) is unpacked in
    # the cargo registry - after a cached build it may not be
    & cmd.exe /c 'cargo fetch --locked --target x86_64-pc-windows-msvc 2>nul'
    if ($LASTEXITCODE -ne 0) { throw 'cargo fetch failed.' }
    $tree = & cmd.exe /c 'cargo tree --target x86_64-pc-windows-msvc --edges normal --prefix none --format "{p}|{l}|{r}" 2>nul'
    if ($LASTEXITCODE -ne 0) { throw 'cargo tree failed.' }
}
finally { Pop-Location }

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$registries = @(Get-ChildItem (Join-Path $cargoHome 'registry\src') -Directory -ErrorAction SilentlyContinue)

$skipExtensions = '.rs', '.toml', '.json', '.yml', '.yaml', '.py', '.sh', '.html', '.lock'
$sha = [System.Security.Cryptography.SHA256]::Create()
$texts = [ordered]@{}        # hash -> @{ Text; Users }
$components = New-Object System.Collections.Generic.List[object]
$withoutText = New-Object System.Collections.Generic.List[string]

# License texts for crates whose published package does not contain one.
$extraDir = Join-Path $here 'third-party-extra'
$extra = @{
    'egui-LICENSE-MIT.txt'      = 'ecolor', 'eframe', 'egui', 'egui_glow', 'egui-winit', 'emath', 'epaint'
    'clipboard-win-LICENSE.txt' = @('clipboard-win')
    'profiling-LICENSE-MIT.txt' = @('profiling')
}

function Add-LicenseText([string]$Text, [string]$User) {
    $normalized = ($Text -replace "`r`n", "`n" -replace "`r", "`n").Trim()
    if (-not $normalized) { return }
    $hash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($normalized)))
    if (-not $texts.Contains($hash)) {
        $texts[$hash] = @{ Text = $normalized; Users = New-Object System.Collections.Generic.List[string] }
    }
    $texts[$hash].Users.Add($User)
}

foreach ($line in ($tree | ForEach-Object { $_ -replace ' \(\*\)$', '' } | Sort-Object -Unique)) {
    if ($line -notmatch '^(?<name>\S+) v(?<ver>\S+)(?<rest>[^|]*)\|(?<lic>[^|]*)\|(?<repo>.*)$') { continue }
    $crate = $Matches['name']; $ver = $Matches['ver']
    if ($Matches['rest'] -match ':\\|/') { continue }      # a local path = this project itself

    $components.Add([pscustomobject]@{ Name = $crate; Version = $ver; License = $Matches['lic']; Repo = $Matches['repo'] })

    $dir = $registries | ForEach-Object { Join-Path $_.FullName "$crate-$ver" } | Where-Object { Test-Path $_ } | Select-Object -First 1
    $found = 0
    if ($dir) {
        $files = Get-ChildItem $dir -File -Recurse -Depth 2 | Where-Object {
            $_.Length -lt 200KB -and $skipExtensions -notcontains $_.Extension.ToLower() -and (
                $_.Name -match '(?i)licen[sc]e|^copying|^copyright|^notice|^unlicense|^OFL|^UFL' -or
                ($_.Directory.Name -eq 'fonts' -and $_.Extension -eq '.txt'))
        } | Sort-Object FullName
        foreach ($file in $files) {
            $relative = $file.FullName.Substring($dir.Length + 1)
            Add-LicenseText (Get-Content $file.FullName -Raw -Encoding UTF8) "$crate $ver ($relative)"
            $found++
        }
    }
    if ($found -eq 0) {
        # A few crates are published without their license file; the texts are
        # kept in tools\third-party-extra (copied from the upstream repositories).
        $extraFile = $extra.Keys | Where-Object { $extra[$_] -contains $crate } | Select-Object -First 1
        if ($extraFile) {
            Add-LicenseText (Get-Content (Join-Path $extraDir $extraFile) -Raw -Encoding UTF8) "$crate $ver ($extraFile, from the upstream repository)"
        }
        else {
            $withoutText.Add("$crate $ver")
        }
    }
}

# The Rust standard library is linked into the exe as well.
Add-LicenseText (Get-Content (Join-Path $extraDir 'rust-LICENSE-MIT.txt') -Raw -Encoding UTF8) 'Rust standard library (LICENSE-MIT, from the upstream repository)'

$out = New-Object System.Collections.Generic.List[string]
$rule = '=' * 80
$out.Add('THIRD-PARTY LICENSES')
$out.Add('')
$out.Add("Ethos BT Joystick $Version is built from the open-source components listed below;")
$out.Add('their license texts follow in section 2. The Rust standard library')
$out.Add('(https://github.com/rust-lang/rust, MIT OR Apache-2.0) is linked into the program too.')
$out.Add('')
$out.Add('The virtual joystick driver embedded in the program is derived from Microsoft''s')
$out.Add('"vhidmini2" driver sample and is covered by LICENSE-driver-MS-PL.txt.')
$out.Add('')
$out.Add('1. COMPONENTS')
$out.Add('')
foreach ($c in $components) {
    $out.Add(('{0} {1}  -  {2}  -  {3}' -f $c.Name, $c.Version, $c.License, $c.Repo))
}
$out.Add('')
$out.Add('2. LICENSE TEXTS')
foreach ($entry in $texts.Values) {
    $out.Add('')
    $out.Add($rule)
    $out.Add('Applies to:')
    foreach ($user in $entry.Users) { $out.Add("  $user") }
    $out.Add($rule)
    $out.Add('')
    foreach ($textLine in ($entry.Text -split "`n")) { $out.Add($textLine.TrimEnd()) }
}

# --- stage + zip -----------------------------------------------------------
$dist  = Join-Path $root 'dist'
$stage = Join-Path $dist "release\$name"
if (Test-Path (Join-Path $dist 'release')) { Remove-Item (Join-Path $dist 'release') -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null

$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $stage 'THIRD-PARTY-LICENSES.txt'), ($out -join "`r`n") + "`r`n", $utf8)

$readme = @"
Ethos BT Joystick $Version
==========================

Use a FrSky Ethos radio as a wireless Bluetooth joystick for simulators on Windows.
Requires Windows 11 (64-bit) with Bluetooth.

1. On the radio: Model > Trainer, add a Bluetooth link, enable it and select Slave.
2. Start EthosBtJoystick.exe. Windows may warn about an unknown publisher the
   first time, because the program is not code signed.
3. Click "Install driver ..." (first time only) and approve the UAC prompt.
4. Click "Search for radio", select the radio and click "Connect".
5. Select "Ethos BT Joystick" as the controller in the simulator.

Documentation, source code and issues:
https://github.com/bliatun-code/EthosBtJoystick

Licenses: LICENSE.txt (this program), LICENSE-driver-MS-PL.txt (the embedded
driver) and THIRD-PARTY-LICENSES.txt (components built into the program).
"@
[IO.File]::WriteAllText((Join-Path $stage 'README.txt'), ($readme -replace "`r?`n", "`r`n") + "`r`n", $utf8)

Copy-Item $exe $stage
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
Copy-Item (Join-Path $root 'driver\LICENSE') (Join-Path $stage 'LICENSE-driver-MS-PL.txt')

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
# Windows PowerShell 5.1 would write backslashes into the zip (Compress-Archive
# always, ZipFile unless this switch is turned off before its first use).
[System.AppContext]::SetSwitch('Switch.System.IO.Compression.ZipFile.UseBackslash', $false)
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory((Join-Path $dist 'release'), $zip, [IO.Compression.CompressionLevel]::Optimal, $false)

Write-Host ''
Write-Host ("Components: {0}, distinct license texts: {1}" -f $components.Count, $texts.Count)
if ($withoutText.Count -gt 0) {
    # never ship a zip with an incomplete license file
    Remove-Item $zip -Force
    throw ("No license text found for: " + ($withoutText -join ', ') + " - add it to tools\third-party-extra.")
}
Write-Host ("Zip:    {0}  ({1:N0} bytes)" -f $zip, (Get-Item $zip).Length) -ForegroundColor Green
Write-Host ("SHA256: {0}" -f (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower())
