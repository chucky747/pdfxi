# Open the acceptance evidence in the default browser.
#
# Usage:
#   powershell -NoProfile -File scripts/open-evidence.ps1            # start (if needed) and open
#   powershell -NoProfile -File scripts/open-evidence.ps1 -NoBrowser # only ensure the server runs
#   powershell -NoProfile -File scripts/open-evidence.ps1 -Stop      # stop the server
#
# The server is detached and hidden, so the link keeps working after the
# terminal that started it is closed. Nothing is installed and the listener is
# bound to 127.0.0.1 only.

param(
    [int]$Port = 8099,
    [switch]$NoBrowser,
    [switch]$Stop
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
$serverScript = Join-Path $PSScriptRoot 'serve-evidence.ps1'
$indexFile = Join-Path $repoRoot 'docs\evidence\index.html'
$url = "http://127.0.0.1:$Port/"

function Test-Port {
    param([int]$Number)
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $async = $client.BeginConnect('127.0.0.1', $Number, $null, $null)
        if (-not $async.AsyncWaitHandle.WaitOne(500)) { return $false }
        $client.EndConnect($async)
        return $true
    } catch {
        return $false
    } finally {
        try { $client.Close() } catch { }
    }
}

if ($Stop) {
    # Get-NetTCPConnection is Windows-only; fall back to netstat elsewhere.
    $ownerIds = @()
    $connections = @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue -ErrorVariable netTcpError)
    if ($connections.Count -gt 0) {
        $ownerIds = @($connections | Select-Object -ExpandProperty OwningProcess -Unique)
    } elseif (Get-Command netstat -ErrorAction SilentlyContinue) {
        $ownerIds = @(
            netstat -ano |
                Select-String -Pattern "LISTENING\s+(\d+)\s*$" |
                Where-Object { $_.Line -match "[:.]$Port\s" } |
                ForEach-Object { [int]$Matches[1] } |
                Select-Object -Unique
        )
    }
    if ($ownerIds.Count -eq 0) {
        Write-Output "no server listening on 127.0.0.1:$Port"
        return
    }
    foreach ($ownerId in $ownerIds) {
        Stop-Process -Id $ownerId -Force -ErrorAction SilentlyContinue
        Write-Output "stopped PID $ownerId"
    }
    return
}

if (-not (Test-Path -LiteralPath $indexFile -PathType Leaf)) {
    throw "evidence index not found: $indexFile"
}

if (-not (Test-Port -Number $Port)) {
    # Log under target/tmp so a failed start can be diagnosed without a console.
    $logDir = Join-Path $repoRoot 'target\tmp'
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    $log = Join-Path $logDir 'evidence-server.log'
    $errLog = Join-Path $logDir 'evidence-server.err.log'
    $arguments = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', "`"$serverScript`"",
        '-Port', $Port
    )
    $server = Start-Process -FilePath 'powershell' -ArgumentList ($arguments -join ' ') -WindowStyle Hidden -PassThru -RedirectStandardOutput $log -RedirectStandardError $errLog
    Write-Output ("started server PID {0} on port {1} (log {2})" -f $server.Id, $Port, $log)

    $ready = $false
    for ($i = 0; $i -lt 40; $i++) {
        if (Test-Port -Number $Port) { $ready = $true; break }
        Start-Sleep -Milliseconds 250
    }
    if (-not $ready) {
        $detail = if (Test-Path -LiteralPath $errLog) { (Get-Content -LiteralPath $errLog -Raw).Trim() } else { '' }
        throw "server did not start listening on 127.0.0.1:$Port. $detail"
    }
} else {
    Write-Output ("server already listening on 127.0.0.1:{0}" -f $Port)
}

Write-Output ("LINK {0}" -f $url)
Write-Output ("FILE {0}" -f ('file:///' + ($indexFile -replace '\\', '/')))
Write-Output ("STOP powershell -NoProfile -File scripts/serve-evidence.ps1  (or open-evidence.ps1 -Stop)")

if (-not $NoBrowser) {
    Start-Process $url
    Write-Output 'opened the link in the default browser'
}
