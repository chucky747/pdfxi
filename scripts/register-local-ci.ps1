<#
.SYNOPSIS
    Register or remove the opt-in nightly local CI job.

.DESCRIPTION
    GitHub CI is not used by this project. `cargo xtask ci` is the verified
    local gate; this script lets a developer schedule it with Windows Task
    Scheduler for the nightly tier. Nothing is registered unless this script is
    run explicitly.

    The job runs as the current user, only while that user is logged on, with a
    six-hour limit. It uses the resolved `cargo.exe` path and the repository
    root, so no machine-specific path is stored in the repository.

.PARAMETER TaskName
    Scheduled task name. Default: "PDFXI local CI".

.PARAMETER At
    Daily start time, 24-hour "HH:mm". Default: "02:30".

.PARAMETER Remove
    Unregister the task instead of creating it.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/register-local-ci.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/register-local-ci.ps1 -Remove
#>
[CmdletBinding()]
param(
    [string]$TaskName = 'PDFXI local CI',
    [string]$At = '02:30',
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

if ($Remove) {
    $existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($existing) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Output "REMOVED scheduled task '$TaskName'"
    } else {
        Write-Output "NOT FOUND scheduled task '$TaskName'"
    }
    exit 0
}

$cargo = (Get-Command cargo -ErrorAction Stop).Source
if (-not (Test-Path -LiteralPath $cargo -PathType Leaf)) {
    throw "cargo not found: $cargo"
}
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $cargo -Argument 'xtask ci' -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -Daily -At $At
$settings = New-ScheduledTaskSettingsSet `
    -StartWhenAvailable `
    -ExecutionTimeLimit (New-TimeSpan -Hours 6)
$principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited

Register-ScheduledTask `
    -TaskName $TaskName `
    -Action $action `
    -Trigger $trigger `
    -Settings $settings `
    -Principal $principal `
    -Description 'PDFXI local CI: cargo xtask ci (Windows, local only)' `
    -Force | Out-Null

Write-Output "REGISTERED scheduled task '$TaskName' daily at $At"
Write-Output "  command: $cargo xtask ci"
Write-Output "  workdir: $root"
Write-Output "  report:  $root\target\ci\ci-report.md"
