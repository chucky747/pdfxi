# Serve the acceptance evidence over plain HTTP so it can be opened in a
# browser with a normal link.
#
# Usage:
#   powershell -NoProfile -File scripts/serve-evidence.ps1
#   powershell -NoProfile -File scripts/serve-evidence.ps1 -Port 8099 -Root .
#
# A tiny TCP/HTTP/1.1 server is used instead of HttpListener so no URL ACL or
# administrator rights are needed. It serves files below -Root only, refuses
# path traversal, and answers GET/HEAD. "/" redirects to the evidence index.
# Stop it with Ctrl+C or by killing the process listening on the port.

param(
    [int]$Port = 8099,
    [string]$Root = '',
    [string]$IndexPath = 'docs/evidence/index.html',
    [string]$BindAddress = '127.0.0.1'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent

# Default to the repository root so the index can link to docs/, orchestration/
# and the evidence tree with ordinary relative paths.
$rootSource = if ($Root) { $Root } else { $repoRoot }
$rootPath = if ([System.IO.Path]::IsPathRooted($rootSource)) { $rootSource } else { Join-Path $repoRoot $rootSource }
$rootPath = [System.IO.Path]::GetFullPath($rootPath).TrimEnd('\', '/')
if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) {
    throw "evidence root not found: $rootPath"
}

$homeUrl = '/' + $IndexPath.TrimStart('/').Replace('\', '/')

$mime = @{
    '.html' = 'text/html; charset=utf-8'
    '.htm'  = 'text/html; charset=utf-8'
    '.css'  = 'text/css; charset=utf-8'
    '.js'   = 'text/javascript; charset=utf-8'
    '.json' = 'application/json; charset=utf-8'
    '.pdf'  = 'application/pdf'
    '.png'  = 'image/png'
    '.jpg'  = 'image/jpeg'
    '.jpeg' = 'image/jpeg'
    '.svg'  = 'image/svg+xml'
    '.md'   = 'text/plain; charset=utf-8'
    '.txt'  = 'text/plain; charset=utf-8'
    '.toml' = 'text/plain; charset=utf-8'
    '.log'  = 'text/plain; charset=utf-8'
}

$listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Parse($BindAddress), $Port)
$listener.Start()

Write-Output ("EVIDENCE SERVER http://{0}:{1}{2}  root={3}" -f $BindAddress, $Port, $homeUrl, $rootPath)
Write-Output ("PID {0}" -f $PID)
Write-Output ("PDF http://{0}:{1}/docs/evidence/PDFXI-acceptance-review.pdf" -f $BindAddress, $Port)
Write-Output 'Press Ctrl+C to stop.'

function Send-Response {
    param(
        [System.Net.Sockets.NetworkStream]$Stream,
        [int]$Status,
        [string]$Reason,
        [string]$ContentType,
        [byte[]]$Body,
        [int]$DeclaredLength = -1,
        [string]$Location = ''
    )
    $length = if ($DeclaredLength -ge 0) { $DeclaredLength } else { $Body.Length }
    $header = "HTTP/1.1 $Status $Reason`r`n" +
        "Content-Type: $ContentType`r`n" +
        "Content-Length: $length`r`n" +
        "Cache-Control: no-store`r`n"
    if ($Location) { $header += "Location: $Location`r`n" }
    $header += "Connection: close`r`n`r`n"
    $headerBytes = [System.Text.Encoding]::ASCII.GetBytes($header)
    $Stream.Write($headerBytes, 0, $headerBytes.Length)
    if (-not $Location -and $Body.Length -gt 0) { $Stream.Write($Body, 0, $Body.Length) }
    $Stream.Flush()
}

function Send-Text {
    param([System.Net.Sockets.NetworkStream]$Stream, [int]$Status, [string]$Reason, [string]$Text)
    Send-Response -Stream $Stream -Status $Status -Reason $Reason -ContentType 'text/plain; charset=utf-8' -Body ([System.Text.Encoding]::UTF8.GetBytes($Text))
}

while ($true) {
    $client = $listener.AcceptTcpClient()
    try {
        $client.ReceiveTimeout = 15000
        $stream = $client.GetStream()
        $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::ASCII, $false, 8192, $true)
        $requestLine = $reader.ReadLine()
        if (-not $requestLine) { continue }
        $parts = $requestLine -split ' '
        if ($parts.Length -lt 2) { continue }
        $method = $parts[0].ToUpperInvariant()
        $target = $parts[1]

        # Drain headers.
        while ($true) {
            $line = $reader.ReadLine()
            if ($null -eq $line -or $line -eq '') { break }
        }

        if ($method -ne 'GET' -and $method -ne 'HEAD') {
            Send-Text -Stream $stream -Status 405 -Reason 'Method Not Allowed' -Text 'only GET and HEAD are served'
            continue
        }

        $path = $target.Split('?')[0]
        $path = [System.Uri]::UnescapeDataString($path)
        if ($path -eq '/' -or $path -eq '') {
            Send-Response -Stream $stream -Status 302 -Reason 'Found' -ContentType 'text/plain; charset=utf-8' -Body ([byte[]]@()) -Location $homeUrl
            continue
        }

        $relative = $path.TrimStart('/').Replace('/', [System.IO.Path]::DirectorySeparatorChar)
        $full = [System.IO.Path]::GetFullPath((Join-Path $rootPath $relative))

        if (-not ($full -eq $rootPath -or $full.StartsWith($rootPath + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase))) {
            Send-Text -Stream $stream -Status 403 -Reason 'Forbidden' -Text 'path outside the evidence root'
            continue
        }
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
            Send-Text -Stream $stream -Status 404 -Reason 'Not Found' -Text ("not found: " + $path)
            continue
        }

        $extension = [System.IO.Path]::GetExtension($full).ToLowerInvariant()
        $contentType = if ($mime.ContainsKey($extension)) { $mime[$extension] } else { 'application/octet-stream' }
        $bytes = [System.IO.File]::ReadAllBytes($full)
        if ($method -eq 'HEAD') {
            Send-Response -Stream $stream -Status 200 -Reason 'OK' -ContentType $contentType -Body ([byte[]]@()) -DeclaredLength $bytes.Length
        } else {
            Send-Response -Stream $stream -Status 200 -Reason 'OK' -ContentType $contentType -Body $bytes
        }
        Write-Output ("{0} {1} -> {2} ({3} bytes)" -f $method, $path, $contentType, $bytes.Length)
    } catch {
        Write-Output ("REQUEST ERROR: " + $_.Exception.Message)
    } finally {
        try { $client.Close() } catch { }
    }
}
