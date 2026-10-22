# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Post one commit status. The runner shows no job logs, so a Windows test reports
# its verdict (and the first failing checks) this way.
#   post-status.ps1 -Context 'windows / installer' -ReportFile installer-test.txt
# Environment: FORGEJO_TOKEN, FORGEJO_SERVER, GITHUB_REPOSITORY, STATUS_SHA. The token
# is never printed.
param(
    [Parameter(Mandatory = $true)][string]$Context,
    [Parameter(Mandatory = $true)][string]$ReportFile,
    [string]$TargetUrl = ''
)
$ErrorActionPreference = 'Continue'
$lines = @()
if (Test-Path $ReportFile) { $lines = @(Get-Content $ReportFile | Where-Object { $_ }) }
$fails = @($lines | Where-Object { $_ -like 'FAIL *' })
$passes = @($lines | Where-Object { $_ -like 'PASS *' })
$state = 'success'
if ($fails.Count -gt 0 -or $lines.Count -eq 0) { $state = 'failure' }
$desc = "$($passes.Count) passed, $($fails.Count) failed"
if ($lines.Count -eq 0) { $desc = 'no report: the test did not run' }
foreach ($f in $fails) { $desc += '; ' + ($f -replace '^FAIL ', '') }
if ($desc.Length -gt 250) { $desc = $desc.Substring(0, 247) + '...' }
$body = @{ state = $state; context = $Context; description = $desc }
if ($TargetUrl) { $body.target_url = $TargetUrl }
$uri = "$($env:FORGEJO_SERVER)/api/v1/repos/$($env:GITHUB_REPOSITORY)/statuses/$($env:STATUS_SHA)"
$headers = @{ Authorization = "token $($env:FORGEJO_TOKEN)"; 'User-Agent' = 'helia-installer-test/1' }
try {
    Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -ContentType 'application/json' -Body ($body | ConvertTo-Json) | Out-Null
    "posted $Context : $state ($desc)"
} catch {
    "could not post the status: $($_.Exception.Message)"
}
exit 0
