# Exercise the PDFXI virtual printer end to end: install the queue, print a real
# document from Notepad through the Windows spooler, capture the finished job with
# `pdfxi printer watch`, verify the captured PDF, then remove the queue.
#
# Usage:
#   powershell -NoProfile -File scripts/test-virtual-printer.ps1
#   powershell -NoProfile -File scripts/test-virtual-printer.ps1 -Printer "PDFXI PDF" -Keep
#
# The script needs no elevation. It exits 0 only when every assertion passes and
# leaves the queue removed unless -Keep is given.

param(
    [string]$Printer = 'PDFXI PDF',
    [switch]$Keep
)

$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$tmp = Join-Path $root 'target\tmp\virtual-printer'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

$candidates = @(
    (Join-Path $root 'target\lanes\core\debug\pdfxi.exe'),
    (Join-Path $root 'target\integration\debug\pdfxi.exe'),
    (Join-Path $root 'target\debug\pdfxi.exe')
)
$binary = $null
foreach ($candidate in $candidates) {
    if (-not (Test-Path -LiteralPath $candidate)) { continue }
    $help = & $candidate --help 2>&1
    if (($help -join "`n") -match 'printer') { $binary = $candidate; break }
}
if (-not $binary) {
    throw "no pdfxi binary with the printer command found; build it with: cargo build -p pdfxi-cli"
}

$assertions = 0
function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "FAILED: $Message" }
    $script:assertions++
    Write-Output "ok $script:assertions - $Message"
}

$captured = Join-Path $tmp 'captured.pdf'
Remove-Item -LiteralPath $captured -ErrorAction SilentlyContinue

try {
    $installJson = & $binary printer install --name $Printer --json | ConvertFrom-Json
    Assert-True ($installJson.installed -eq $true) "install reports the queue installed"
    $port = [string]$installJson.port
    Assert-True ([System.IO.Path]::IsPathRooted($port)) "the capture port is an absolute file path"

    $status = & $binary printer status --name $Printer --json | ConvertFrom-Json
    Assert-True ($status.installed -eq $true) "status sees the queue in the spooler"
    Assert-True ($status.driver -eq 'Microsoft Print To PDF') "the queue uses the Windows PDF driver"

    $watchOut = Join-Path $tmp 'watch.out'
    $watchErr = Join-Path $tmp 'watch.err'
    $watchArgs = 'printer watch --name="{0}" --out="{1}" --timeout 90' -f $Printer, $captured
    $watch = Start-Process -FilePath $binary `
        -ArgumentList $watchArgs `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $watchOut -RedirectStandardError $watchErr
    Start-Sleep -Seconds 2

    # Print from a real application through the Windows spooler. Word is used
    # because the Windows 11 Notepad has no /pt verb; any application that can
    # print works the same way.
    $word = New-Object -ComObject Word.Application
    try {
        $word.Visible = $false
        $document = $word.Documents.Add()
        $document.Content.Text = 'PDFXI virtual printer test document, printed from Microsoft Word.'
        $document.PrintOut([ref]$false, [ref]$false, [ref]0, [ref]'', [ref]$Printer)
        $document.Close([ref]$false)
    } finally {
        $word.Quit()
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($word) | Out-Null
    }

    if (-not $watch.WaitForExit(120000)) {
        $watch | Stop-Process -Force
        throw 'FAILED: printer watch did not finish within 120 s'
    }
    # Windows PowerShell does not expose ExitCode for a redirected -PassThru
    # process, so the command's own report is the signal that it succeeded.
    $watchReport = Get-Content -LiteralPath $watchOut -Raw -ErrorAction SilentlyContinue
    Assert-True ($watchReport -match 'captured:') "printer watch reports the captured job"
    Assert-True (Test-Path -LiteralPath $captured) "the captured job exists"

    $info = & $binary info $captured
    Assert-True ($LASTEXITCODE -eq 0) "the captured file parses as a PDF"
    Assert-True (($info -join "`n") -match 'pages: [1-9]') "the captured PDF has at least one page"
    Assert-True ((Get-Item -LiteralPath $captured).Length -gt 1000) "the captured PDF is not empty"

    $second = & $binary printer status --name $Printer --json | ConvertFrom-Json
    Assert-True ($second.port_file_exists -eq $false) "the port file is cleared for the next job"
} finally {
    if (-not $Keep) {
        & $binary printer remove --name $Printer | Out-Null
    }
}

if (-not $Keep) {
    $after = & $binary printer status --name $Printer --json | ConvertFrom-Json
    Assert-True ($after.installed -eq $false) "remove takes the queue out of the spooler"
}

Write-Output "VIRTUAL PRINTER OK: $assertions assertions"
