# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# End-to-end test of `helia update` on Windows (the NSIS installer path). Run on the
# Windows build machine, from the repository root:
#   tools\update-e2e.ps1 [-Work <folder>] [-KeepWork]
#
# It builds two versions of Helia that trust a throwaway key and fetch updates from a
# local server (HELIA_UPDATE_BASE_URL=http://127.0.0.1:PORT, both host lists
# 127.0.0.1, HELIA_UPDATE_PINNED_KEYS=<throwaway public key>), makes an NSIS
# installer for each, installs the old one silently per user, serves a signed
# manifest and the new installer, and then runs the installed old helia.exe:
#   update check -> exit 10; update apply (no --yes, no terminal) -> exit 3;
#   update apply --yes -> the helper waits for helia.exe to end, runs the new
#   installer with /S /D=<folder>, and the installed version becomes the new one;
#   settings under %APPDATA%\helia are untouched; update rollback --yes puts the
#   old programs back.
# Each check appends PASS or FAIL to the report; exit 1 if any failed. It needs
# cargo, and NSIS (packaging\windows finds or
# downloads it). Written for the helia-win-ci runner; it has not been run by its
# author on Windows when this file was committed.
param(
    [string]$Work = (Join-Path ([IO.Path]::GetTempPath()) ("helia-update-e2e-" + [guid]::NewGuid().ToString('N').Substring(0, 8))),
    [switch]$KeepWork,
    [string]$NewVersion = ''   # rehearsal: for example 0.8.0
)
$ErrorActionPreference = 'Stop'
$script:failed = 0
New-Item -ItemType Directory -Force $Work | Out-Null
if (-not $env:RUNNER_TEMP) { $env:RUNNER_TEMP = $Work }   # build-installer.ps1 stages under it
$report = Join-Path $Work 'update-e2e.txt'
Set-Content -Path $report -Value '' -Encoding ascii

function Check([string]$name, [scriptblock]$test) {
    $ok = $false
    $note = ''
    try { $ok = [bool](& $test) } catch { $note = ' (' + $_.Exception.Message + ')' }
    $line = $(if ($ok) { 'PASS ' } else { 'FAIL ' }) + $name + $note
    if (-not $ok) { $script:failed++ }
    Add-Content -Path $report -Value $line -Encoding ascii
    Write-Output $line
}

$root = (Get-Location).Path
if (-not (Test-Path (Join-Path $root 'packaging\windows\helia.nsi'))) { throw 'run this from the repository root' }
$target = Join-Path $Work 'target'
$env:CARGO_TARGET_DIR = $target

# Keys.
cargo build -q -p helia-update --features sign --bin helia-manifest
if ($LASTEXITCODE -ne 0) { throw 'building helia-manifest failed' }
$tool = Join-Path $target 'debug\helia-manifest.exe'
& $tool keygen-for-tests --out-dir (Join-Path $Work 'keys') | Out-Null
& $tool keygen-for-tests --out-dir (Join-Path $Work 'other-keys') | Out-Null
$pub = (Get-Content (Join-Path $Work 'keys\test.pub'))[-1].Trim()

# Port.
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
$base = "http://127.0.0.1:$port"
$env:HELIA_UPDATE_BASE_URL = $base
$env:HELIA_UPDATE_MANIFEST_HOSTS = '127.0.0.1'
$env:HELIA_UPDATE_ASSET_HOSTS = '127.0.0.1'
$env:HELIA_UPDATE_PINNED_KEYS = $pub

function Build-Version([string]$srcDir, [string]$outDir) {
    Push-Location $srcDir
    try {
        cargo build -q -p helia-cli -p helia-gui
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        $bin = Join-Path $target 'debug'
        $ver = ((Get-Content Cargo.toml | Select-String '^version = "(\d+\.\d+\.\d+)"' | Select-Object -First 1).Matches[0].Groups[1].Value)
        $meta = Join-Path $Work "meta-$ver"
        New-Item -ItemType Directory -Force $meta | Out-Null
        Set-Content -Path (Join-Path $meta 'THIRD-PARTY-LICENSES.txt') -Value 'test' -Encoding ascii
        & (Join-Path $root 'packaging\windows\build-installer.ps1') -Version $ver -BinDir $bin -MetaDir $meta -OutDir $outDir | Out-Null
        return $ver
    } finally { Pop-Location }
}

$distOld = Join-Path $Work 'dist-old'
$oldVersion = Build-Version $root $distOld

# The new version: a copy of the tree with the patch number raised.
$newSrc = Join-Path $Work 'src-new'
robocopy $root $newSrc /E /XD target .git site _site node_modules /NFL /NDL /NJH /NJS /NP | Out-Null
$m = [regex]::Match((Get-Content (Join-Path $newSrc 'Cargo.toml') -Raw), '(?m)^version = "(\d+)\.(\d+)\.(\d+)"')
$toVersion = "$($m.Groups[1].Value).$($m.Groups[2].Value).$([int]$m.Groups[3].Value + 1)"
if ($NewVersion) { $toVersion = $NewVersion }
# Only the workspace version and the version requirements of its own path dependencies.
$rootToml = Join-Path $newSrc 'Cargo.toml'
$rt = [regex]::new('(?m)^version = "' + [regex]::Escape($oldVersion) + '"')
Set-Content -Path $rootToml -Value $rt.Replace((Get-Content $rootToml -Raw), "version = `"$toVersion`"", 1) -NoNewline
@(Get-ChildItem (Join-Path $newSrc 'crates') -Recurse -Filter Cargo.toml) + @(Get-Item (Join-Path $newSrc 'xtask\Cargo.toml')) | ForEach-Object {
    $t = Get-Content $_.FullName -Raw
    $t = [regex]::Replace($t, '(?m)^(helia[a-z-]* = \{ version = ")' + [regex]::Escape($oldVersion) + '(", path)', ('${1}' + $toVersion + '${2}'))
    Set-Content -Path $_.FullName -Value $t -NoNewline
}
$distNew = Join-Path $Work 'dist-new'
$built = Build-Version $newSrc $distNew
Check "the new build is $toVersion" { $built -eq $toVersion }

# Serve.
$serve = Join-Path $Work 'serve'
$filesDir = Join-Path $serve "helia\$toVersion"
New-Item -ItemType Directory -Force $filesDir, (Join-Path $serve 'helia\stable') | Out-Null
$installerNew = Get-ChildItem $distNew -Filter '*-setup.exe' | Select-Object -First 1
Copy-Item $installerNew.FullName $filesDir
$manifest = Join-Path $serve 'helia\stable\manifest.json'
& $tool build --app helia --channel stable --version $toVersion --sequence 1 --base-url "$base/helia/$toVersion/" --expires-days 30 --asset "windows-x86_64:nsis:$($installerNew.FullName)" --out $manifest --allow-loopback-http
& $tool sign --manifest $manifest --secret-key-file (Join-Path $Work 'keys\test.key')
# A small static file server in a background job (no Python needed on the Windows runner).
$server = Start-Job -ArgumentList $serve, $port -ScriptBlock {
    param($root, $port)
    $l = [Net.HttpListener]::new()
    $l.Prefixes.Add("http://127.0.0.1:$port/")
    $l.Start()
    while ($l.IsListening) {
        $c = $l.GetContext()
        try {
            $rel = [Uri]::UnescapeDataString($c.Request.Url.AbsolutePath).TrimStart('/').Replace('/', '\')
            $p = Join-Path $root $rel
            if ($rel -notmatch '\.\.' -and (Test-Path -LiteralPath $p -PathType Leaf)) {
                $b = [IO.File]::ReadAllBytes($p)
                $c.Response.ContentLength64 = $b.Length
                $c.Response.OutputStream.Write($b, 0, $b.Length)
            } else { $c.Response.StatusCode = 404 }
        } catch { $c.Response.StatusCode = 500 }
        $c.Response.Close()
    }
}

try {
    Start-Sleep -Seconds 2
    # Install the old version silently, per user, into a folder with a space.
    $dir = Join-Path $Work 'Helia Install'
    $oldInstaller = Get-ChildItem $distOld -Filter '*-setup.exe' | Select-Object -First 1
    $p = Start-Process -FilePath $oldInstaller.FullName -ArgumentList @('/S', "/D=$dir") -Wait -PassThru
    Check 'the old version installs silently' { $p.ExitCode -eq 0 -and (Test-Path "$dir\helia.exe") }
    $exe = Join-Path $dir 'helia.exe'
    function Version-Of { ((& $exe -V) -split ' ')[-1].Trim() }
    Check "installed version is $oldVersion" { (Version-Of) -eq $oldVersion }

    $env:HELIA_UPDATE_DIR = Join-Path $Work 'state'
    # The settings the update must not touch.
    $settings = Join-Path $env:APPDATA 'helia\gui.conf'
    $settingsBefore = if (Test-Path $settings) { (Get-FileHash $settings).Hash } else { $null }

    # Native commands that exit non-zero by design write to stderr: do not stop on that.
    $ErrorActionPreference = 'Continue'
    & $exe update check | Out-Null
    $rc = $LASTEXITCODE
    Check 'update check finds the update (exit 10)' { $rc -eq 10 }
    $null | & $exe update apply | Out-Null
    $rc = $LASTEXITCODE
    Check 'update apply without --yes does nothing (exit 3)' { $rc -eq 3 -and (Version-Of) -eq $oldVersion }

    $applyOut = & $exe update apply --yes 2>&1 | Out-String
    $rc = $LASTEXITCODE
    Write-Output "update apply --yes said: $applyOut"
    Check 'update apply --yes schedules the install (exit 0)' { $rc -eq 0 }
    # The helper waits for this helia.exe to be gone (it is) and runs the installer.
    $deadline = (Get-Date).AddSeconds(180)
    while ((Get-Date) -lt $deadline -and (Version-Of) -ne $toVersion) { Start-Sleep -Seconds 2 }
    Check "the installed version becomes $toVersion" { (Version-Of) -eq $toVersion }
    if ((Version-Of) -ne $toVersion) {
        # Say what the helper did: its own log, and what is still running.
        Get-ChildItem -Path (Join-Path $env:HELIA_UPDATE_DIR 'previous') -Recurse -Filter helper.log -ErrorAction SilentlyContinue |
            ForEach-Object { Write-Output "---- $($_.FullName)"; Get-Content $_.FullName }
        Get-Process powershell, *setup*, helia* -ErrorAction SilentlyContinue | Format-Table Id, ProcessName, StartTime -AutoSize | Out-String | Write-Output
    }
    Check 'the settings file is untouched' { ($null -eq $settingsBefore) -or ((Get-FileHash $settings).Hash -eq $settingsBefore) }
    Check 'the old programs were kept for rollback' { Test-Path (Join-Path $env:HELIA_UPDATE_DIR "previous\$oldVersion\helia.exe") }

    & $exe update rollback --yes | Out-Null
    $rc = $LASTEXITCODE
    Check 'update rollback --yes schedules the restore (exit 0)' { $rc -eq 0 }
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline -and (Version-Of) -ne $oldVersion) { Start-Sleep -Seconds 2 }
    Check "rollback puts $oldVersion back" { (Version-Of) -eq $oldVersion }

    $uninstall = Join-Path $dir 'uninstall.exe'
    if (Test-Path $uninstall) { Start-Process -FilePath $uninstall -ArgumentList '/S' -Wait | Out-Null }
} finally {
    if ($server) { Stop-Job $server -ErrorAction SilentlyContinue; Remove-Job $server -Force -ErrorAction SilentlyContinue }
    Remove-Item Env:\HELIA_UPDATE_BASE_URL, Env:\HELIA_UPDATE_MANIFEST_HOSTS, Env:\HELIA_UPDATE_ASSET_HOSTS, Env:\HELIA_UPDATE_PINNED_KEYS, Env:\HELIA_UPDATE_DIR -ErrorAction SilentlyContinue
}

Write-Output "report: $report"
if ($script:failed -gt 0) { Write-Output "$script:failed check(s) FAILED"; exit 1 }
Write-Output 'all checks passed'
if (-not $KeepWork) { Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue }
