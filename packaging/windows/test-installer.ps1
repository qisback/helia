# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Prove the Windows installer on a real Windows machine: silent per-user install,
# then shortcuts, file association, programs, Apps & Features entry and PATH are
# checked, then the registered silent uninstall leaves nothing behind. A second
# round checks the opt-out switches.
#   test-installer.ps1 -Installer <setup.exe> -Version 0.5.0 -Report <file>
# Every check appends PASS or FAIL to the report (the runner shows no job logs, so the
# workflow posts the report as a commit status). Exits 1 if any check failed.
param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Report = 'installer-test.txt'
)
$ErrorActionPreference = 'Stop'
$script:failed = 0
Set-Content -Path $Report -Value '' -Encoding ascii

function Check([string]$name, [scriptblock]$test) {
    $ok = $false
    $note = ''
    try { $ok = [bool](& $test) } catch { $note = ' (' + $_.Exception.Message + ')' }
    $line = $(if ($ok) { 'PASS ' } else { 'FAIL ' }) + $name + $note
    if (-not $ok) { $script:failed++ }
    Add-Content -Path $Report -Value $line -Encoding ascii
    Write-Output $line
}

$dir = Join-Path $env:RUNNER_TEMP 'Helia Install Test'   # a space on purpose
$arpKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Helia'
$programs = [Environment]::GetFolderPath('Programs')
$desktop = [Environment]::GetFolderPath('Desktop')
$startLnk = Join-Path $programs 'Helia.lnk'
$deskLnk = Join-Path $desktop 'Helia.lnk'

function Run-Install([string[]]$switches) {
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    $args = @('/S') + $switches + @("/D=$dir")   # /D= must come last and takes no quotes
    $p = Start-Process -FilePath $Installer -ArgumentList $args -Wait -PassThru
    return $p.ExitCode
}
function Link-Target([string]$lnk) { (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath }
function User-Path { (Get-ItemProperty 'HKCU:\Environment').Path }
function Subsystem([string]$exe) {
    $b = [IO.File]::ReadAllBytes($exe)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    [BitConverter]::ToUInt16($b, $pe + 24 + 68)   # 2 = GUI, 3 = console
}
function Silent-Uninstall {
    $q = (Get-ItemProperty $arpKey).QuietUninstallString
    if ($q -notmatch '^"([^"]+)"\s*(.*)$') { throw "odd QuietUninstallString: $q" }
    Start-Process -FilePath $Matches[1] -ArgumentList $Matches[2] -Wait | Out-Null
    # The uninstaller copies itself to %TEMP% and returns at once: wait for the folder to go.
    for ($i = 0; $i -lt 60 -and (Test-Path $dir); $i++) { Start-Sleep -Seconds 1 }
}
function Assert-Clean {
    Check 'uninstall removed the install folder' { -not (Test-Path $dir) }
    Check 'uninstall removed the Start menu shortcut' { -not (Test-Path $startLnk) }
    Check 'uninstall removed the desktop shortcut' { -not (Test-Path $deskLnk) }
    Check 'uninstall removed Apps and Features entry' { -not (Test-Path $arpKey) }
    Check 'uninstall removed Software\Helia' { -not (Test-Path 'HKCU:\Software\Helia') }
    Check 'uninstall removed the ProgID' { -not (Test-Path 'HKCU:\Software\Classes\Helia.Capture') }
    Check 'uninstall removed the Applications key' { -not (Test-Path 'HKCU:\Software\Classes\Applications\helia-gui.exe') }
    foreach ($e in '.ser', '.avi', '.fits') {
        Check "uninstall removed Open with for $e" {
            $k = "HKCU:\Software\Classes\$e\OpenWithProgids"
            (-not (Test-Path $k)) -or ($null -eq (Get-ItemProperty $k).'Helia.Capture')
        }
    }
    Check 'uninstall took helia off PATH' { -not (($(User-Path) -split ';') -contains $dir) }
}

# Leftovers from an earlier run on this machine must not decide the result.
if (Test-Path $arpKey) { Silent-Uninstall }

# Round 1: defaults plus a desktop shortcut and PATH.
Check 'installer runs silently and exits 0' { (Run-Install @('/DESKTOP', '/ADDPATH')) -eq 0 }
Check 'helia-gui.exe installed' { Test-Path "$dir\helia-gui.exe" }
Check 'helia.exe installed' { Test-Path "$dir\helia.exe" }
Check 'uninstall.exe installed' { Test-Path "$dir\uninstall.exe" }
foreach ($f in 'LICENSE.txt', 'ADDITIONAL-TERMS.txt', 'LOGO-LICENCE.md', 'THIRD-PARTY-LICENSES.txt', 'README.md', 'CHANGELOG.md', 'helia.ico') {
    Check "$f installed" { Test-Path "$dir\$f" }
}
Check 'installed in the per-user folder, not Program Files' { (Get-ItemProperty $arpKey).InstallLocation -eq $dir }
Check 'helia.exe --version reports the version' {
    $first = ((& "$dir\helia.exe" --version | Out-String).Trim() -split "\r?\n")[0]
    $first -eq "helia $Version"
}
Check 'helia-gui.exe --version reports the version' {
    $first = ((& "$dir\helia-gui.exe" --version | Out-String).Trim() -split "\r?\n")[0]
    $first -eq "helia-gui $Version"
}
Check 'helia-gui.exe is a GUI program (no console window)' { (Subsystem "$dir\helia-gui.exe") -eq 2 }
Check 'helia.exe is a console program' { (Subsystem "$dir\helia.exe") -eq 3 }
Check 'exe VersionInfo: product, copyright and version' {
    $vi = (Get-Item "$dir\helia-gui.exe").VersionInfo
    ($vi.ProductName -eq 'Helia') -and ($vi.LegalCopyright -eq 'Copyright (C) 2026 Laurence Horrocks-Barlow') -and $vi.FileVersion.StartsWith($Version)
}
Check 'installer VersionInfo carries the publisher' {
    $vi = (Get-Item $Installer).VersionInfo
    ($vi.CompanyName -eq 'Laurence Horrocks-Barlow') -and ($vi.ProductVersion -eq $Version)
}
Check 'Start menu shortcut points at helia-gui.exe' { (Link-Target $startLnk) -eq "$dir\helia-gui.exe" }
Check 'desktop shortcut points at helia-gui.exe' { (Link-Target $deskLnk) -eq "$dir\helia-gui.exe" }
foreach ($e in '.ser', '.avi', '.fits') {
    Check "Open with Helia is listed for $e" { $null -ne (Get-ItemProperty "HKCU:\Software\Classes\$e\OpenWithProgids").'Helia.Capture' }
}
Check '.ser default program is not taken over' { -not (Test-Path 'HKCU:\Software\Classes\.ser') -or ($null -eq (Get-ItemProperty 'HKCU:\Software\Classes\.ser').'(default)') }
Check 'ProgID opens files with helia-gui.exe "%1"' {
    (Get-ItemProperty 'HKCU:\Software\Classes\Helia.Capture\shell\open\command').'(default)' -eq ('"' + $dir + '\helia-gui.exe" "%1"')
}
Check 'Apps and Features: name, version, publisher, uninstaller' {
    $k = Get-ItemProperty $arpKey
    ($k.DisplayName -eq 'Helia') -and ($k.DisplayVersion -eq $Version) -and ($k.Publisher -eq 'Laurence Horrocks-Barlow') -and ($k.UninstallString -like '*uninstall.exe*') -and ($k.DisplayIcon -like '*helia-gui.exe*')
}
Check 'helia is on the user PATH' { ($(User-Path) -split ';') -contains $dir }
Silent-Uninstall
Assert-Clean

# Round 2: the opt-out switches.
Check 'installer runs silently with /NOSTART /NOASSOC' { (Run-Install @('/NOSTART', '/NOASSOC')) -eq 0 }
Check 'no Start menu shortcut with /NOSTART' { -not (Test-Path $startLnk) }
Check 'no desktop shortcut by default' { -not (Test-Path $deskLnk) }
Check 'no file association with /NOASSOC' { -not (Test-Path 'HKCU:\Software\Classes\Helia.Capture') }
Check 'PATH untouched by default' { -not (($(User-Path) -split ';') -contains $dir) }
Silent-Uninstall
Assert-Clean

if (Test-Path $dir) { Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue }
Add-Content -Path $Report -Value ("RESULT failed=" + $script:failed) -Encoding ascii
if ($script:failed -gt 0) { exit 1 }
