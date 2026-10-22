# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the Windows installer with NSIS. Run from the repository root.
#   build-installer.ps1 -Version 0.5.0 -BinDir <dir with helia.exe, helia-gui.exe> -MetaDir <dir with THIRD-PARTY-LICENSES.txt> -OutDir dist
# Writes <OutDir>\helia-<version>-x86_64-pc-windows-msvc-setup.exe and its .sha256.
#
# NSIS is taken from PATH or the usual install folders; failing that, the pinned
# 3.11 zip is downloaded once into a cache folder and checked against its SHA-256.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$BinDir,
    [Parameter(Mandatory = $true)][string]$MetaDir,
    [Parameter(Mandatory = $true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
$NsisVersion = '3.11'
$NsisZipSha256 = 'c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1'
$NsisUrl = "https://downloads.sourceforge.net/project/nsis/NSIS%203/$NsisVersion/nsis-$NsisVersion.zip"

function Find-Makensis {
    $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($d in @("${env:ProgramFiles(x86)}\NSIS", "$env:ProgramFiles\NSIS")) {
        if (Test-Path "$d\makensis.exe") { return "$d\makensis.exe" }
    }
    $cacheRoot = if (Test-Path 'W:\') { 'W:\tools' } else { Join-Path $env:LOCALAPPDATA 'helia-ci' }
    $dir = Join-Path $cacheRoot "nsis-$NsisVersion"
    if (-not (Test-Path "$dir\makensis.exe")) {
        New-Item -ItemType Directory -Force $cacheRoot | Out-Null
        $zip = Join-Path $cacheRoot "nsis-$NsisVersion.zip"
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        # curl.exe, not Invoke-WebRequest: SourceForge answers a PowerShell user agent with an HTML page.
        curl.exe --fail --silent --show-error --location --ssl-no-revoke --retry 3 -o $zip $NsisUrl
        if ($LASTEXITCODE -ne 0) { throw "download of NSIS failed ($LASTEXITCODE)" }
        $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($got -ne $NsisZipSha256) { $len = (Get-Item $zip).Length; Remove-Item -Force $zip; throw "NSIS zip checksum mismatch: $got ($len bytes)" }
        $tmp = Join-Path $cacheRoot "nsis-extract-$PID"
        Expand-Archive -Path $zip -DestinationPath $tmp
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
        Move-Item (Join-Path $tmp "nsis-$NsisVersion") $dir
        Remove-Item -Recurse -Force $tmp
    }
    return "$dir\makensis.exe"
}

$note = 'installer-build-error.txt'
Remove-Item -Force -ErrorAction SilentlyContinue $note
try {
$makensis = Find-Makensis
"makensis: $makensis"

$numeric = ($Version -split '-')[0]
if ($numeric -notmatch '^\d+\.\d+\.\d+$') { throw "version '$Version' is not x.y.z[-suffix]" }
$version4 = "$numeric.0"

$stage = Join-Path $env:RUNNER_TEMP 'helia-installer-stage'
if (-not $env:RUNNER_TEMP) { $stage = Join-Path ([IO.Path]::GetTempPath()) 'helia-installer-stage' }
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory $stage | Out-Null
Copy-Item "$BinDir\helia.exe", "$BinDir\helia-gui.exe" $stage
Copy-Item README.md, CHANGELOG.md, LOGO-LICENCE.md $stage
Copy-Item "$MetaDir\THIRD-PARTY-LICENSES.txt" $stage
Copy-Item assets\branding\helia.ico $stage
# The licence pages are rich-edit boxes: give them Windows line endings.
$utf8 = New-Object System.Text.UTF8Encoding $false
foreach ($pair in @(@('LICENSE', 'LICENSE.txt'), @('ADDITIONAL-TERMS.md', 'ADDITIONAL-TERMS.txt'))) {
    $text = [IO.File]::ReadAllText((Join-Path (Get-Location) $pair[0]), $utf8) -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText((Join-Path $stage $pair[1]), $text, (New-Object System.Text.UTF8Encoding $true))
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$name = "helia-$Version-x86_64-pc-windows-msvc-setup.exe"
$out = Join-Path (Resolve-Path $OutDir) $name
$ErrorActionPreference = 'Continue'
$log = & $makensis /V2 "/DVERSION=$Version" "/DVERSION4=$version4" "/DSTAGE=$stage" "/DOUTFILE=$out" packaging\windows\helia.nsi 2>&1 | Out-String
$log
if ($LASTEXITCODE -ne 0) { throw ('makensis failed with ' + $LASTEXITCODE + ' : ' + (($log -split '\r?\n' | Where-Object { $_ } | Select-Object -Last 3) -join ' / ')) }
$ErrorActionPreference = 'Stop'
$hash = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLower()
[IO.File]::WriteAllText("$out.sha256", "$hash  $name`n", $utf8)
Get-Item $out | Format-Table Name, Length
} catch {
    [IO.File]::WriteAllText((Join-Path (Get-Location) $note), $_.Exception.Message, (New-Object System.Text.UTF8Encoding $false))
    throw
}
