param(
    [ValidateSet('arm', 'pause', 'resume', 'clear', 'status', 'watch')]
    [string]$Command = 'status'
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'opencode.ps1')

$session = $env:OPENCODE_SESSION_ID
if (-not $session) {
    throw "OPENCODE_SESSION_ID is not set; run this from an OpenCode session."
}

$root = Split-Path $PSScriptRoot -Parent
$objectivePath = Join-Path $root 'docs\GOAL_OBJECTIVE.txt'
if (-not (Test-Path $objectivePath)) {
    throw "Missing goal objective: $objectivePath"
}
$objective = (Get-Content $objectivePath -Raw).Trim()
if ($objective.Length -gt 4000) {
    throw "Goal objective is $($objective.Length) characters; limit is 4000."
}

function Send-GoalCommand {
    param([Parameter(Mandatory = $true)][string]$Text)
    $body = @{ name = 'goal'; text = $Text } | ConvertTo-Json -Compress
    return Invoke-OpenCodeApi -Method post -Path "/api/session/$session/command" -Body $body
}

function Get-GoalState {
    $body = @{ input = @{ sessionID = $session } } | ConvertTo-Json -Compress
    $raw = Invoke-OpenCodeApi -Method post -Path '/api/rpc/opencode.goal/get' -Body $body
    if (-not $raw) { return $null }
    try {
        $parsed = $raw | ConvertFrom-Json
        if ($parsed.output) { return $parsed.output }
        return $parsed
    } catch {
        return $null
    }
}

function Show-GoalState {
    $state = Get-GoalState
    if (-not $state) {
        Write-Output 'NO GOAL'
        return
    }
    $state | ConvertTo-Json -Depth 8
}

switch ($Command) {
    'arm' {
        Send-GoalCommand -Text $objective | Out-Null
        Write-Output 'GOAL ARMED'
        Show-GoalState
    }
    'pause' {
        Send-GoalCommand -Text 'pause' | Out-Null
        Write-Output 'GOAL PAUSED'
    }
    'resume' {
        Send-GoalCommand -Text 'resume' | Out-Null
        Write-Output 'GOAL RESUMED'
        Show-GoalState
    }
    'clear' {
        Send-GoalCommand -Text 'clear' | Out-Null
        Write-Output 'GOAL CLEARED'
    }
    'status' {
        Show-GoalState
    }
    'watch' {
        $cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) {
            'cargo'
        } else {
            Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        }
        $sameReason = 0
        $lastReason = ''
        while ($true) {
            $state = Get-GoalState
            if (-not $state) {
                Write-Output 'ARMING'
                Send-GoalCommand -Text $objective | Out-Null
                Start-Sleep -Seconds 15
                continue
            }
            Write-Output ("STATE {0} rounds={1} reason={2}" -f $state.status, $state.rounds, $state.reason)
            if ($state.status -eq 'complete') {
                Push-Location $root
                try {
                    & $cargo xtask goal-verify --all
                    if ($LASTEXITCODE -eq 0) {
                        Write-Output 'GOAL CONTROLLER: complete'
                        return
                    }
                    $failed = "The previous run was marked complete but goal-verify failed. Fix the failures, show the passing output, then continue. $($state.reason)"
                    Send-GoalCommand -Text ($objective + "`n`n" + $failed) | Out-Null
                } finally {
                    Pop-Location
                }
            } elseif ($state.status -eq 'paused') {
                $reason = [string]$state.reason
                if ($reason -eq $lastReason) { $sameReason++ } else { $sameReason = 0; $lastReason = $reason }
                if ($sameReason -ge 3) {
                    $blocker = Join-Path $root 'orchestration\blockers\goal-controller.md'
                    New-Item -ItemType Directory -Force -Path (Split-Path $blocker) | Out-Null
                    "Goal paused repeatedly: $reason" | Set-Content $blocker -Encoding utf8
                    Write-Output "BLOCKED: $reason (wrote $blocker)"
                    return
                }
                if ($reason -match 'Reached the limit|server restarted|Nothing ran|Not making progress|goal stopped after an error') {
                    Send-GoalCommand -Text 'resume' | Out-Null
                    Write-Output 'RESUMED'
                } else {
                    Write-Output "PAUSED: $reason (not auto-resuming)"
                    return
                }
            }
            Start-Sleep -Seconds 30
        }
    }
}
