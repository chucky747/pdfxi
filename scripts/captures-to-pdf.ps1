# Build a labelled PDF contact sheet from the installed-build captures.
#
# Usage:
#   powershell -NoProfile -File scripts/captures-to-pdf.ps1
#   powershell -NoProfile -File scripts/captures-to-pdf.ps1 -Source docs/evidence/ui/final -Out docs/evidence/ui/PDFXI-installed-captures.pdf
#
# The sheet groups files by their name prefix (light-windows, light-macos,
# dark-windows, dark-macos, extras), opens with a one-page overview of every
# capture, then a numbered index, then six captures per A4-landscape page with
# the file name as a caption. It prints with Chrome or Edge in headless mode.
# No network access and no external tools beyond the browser.

param(
    [string]$Source = 'docs/evidence/ui/final',
    [string]$Out = 'docs/evidence/ui/PDFXI-installed-captures.pdf',
    [string]$Title = 'PDFXI - installed build captures',
    [string]$Subtitle = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$sourceDir = if ([System.IO.Path]::IsPathRooted($Source)) { $Source } else { Join-Path $root $Source }
$outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }

if (-not (Test-Path -LiteralPath $sourceDir -PathType Container)) {
    throw "capture directory not found: $sourceDir"
}

$images = Get-ChildItem -LiteralPath $sourceDir -File -Filter '*.png' | Sort-Object Name
if ($images.Count -eq 0) {
    throw "no PNG captures in $sourceDir"
}

# Keep the scratch HTML and the browser profile inside the repository target
# directory: some locked-down hosts deny writes or execution in %TEMP%.
$scratch = Join-Path $root 'target\tmp\captures-pdf'
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$htmlPath = Join-Path $scratch 'captures-gallery.html'
$builder = New-Object System.Text.StringBuilder
[void]$builder.AppendLine('<html><head><meta charset="utf-8"><style>')
[void]$builder.AppendLine('body{font-family:Segoe UI,sans-serif;margin:0}h1{font-size:20px;margin:14px}')
[void]$builder.AppendLine('h2{font-size:13px;margin:10px 0 6px;color:#444}')
[void]$builder.AppendLine('.page{page-break-after:always;padding:10px 14px;box-sizing:border-box}')
[void]$builder.AppendLine('.grid{display:grid;grid-template-columns:1fr 1fr;gap:10px}')
[void]$builder.AppendLine('.overview{display:grid;grid-template-columns:repeat(8,1fr);gap:5px}')
[void]$builder.AppendLine('.thumb{border:1px solid #ddd;border-radius:4px;padding:3px}')
[void]$builder.AppendLine('.thumb img{width:100%;display:block}')
[void]$builder.AppendLine('.thumb .cap{font-size:6px;line-height:1.1;word-break:break-all}')
[void]$builder.AppendLine('.index{column-count:3;column-gap:18px;font-size:11px}')
[void]$builder.AppendLine('.index div{font-family:Consolas,monospace;margin-bottom:3px;break-inside:avoid}')
[void]$builder.AppendLine('.cell{border:1px solid #ddd;border-radius:6px;padding:6px}')
[void]$builder.AppendLine('.cell img{width:100%;display:block}')
[void]$builder.AppendLine('.cap{font-size:10px;color:#555;margin-top:3px;font-family:Consolas,monospace}')
[void]$builder.AppendLine('.meta{font-size:11px;color:#666;margin:2px 0 10px}')
[void]$builder.AppendLine('</style></head><body>')
[void]$builder.AppendLine("<h1>$Title</h1>")
if ($Subtitle) { [void]$builder.AppendLine("<div class='meta'>$Subtitle</div>") }

$groups = [ordered]@{
    'light-windows' = @()
    'light-macos'   = @()
    'dark-windows'  = @()
    'dark-macos'    = @()
    'extras'        = @()
}
foreach ($image in $images) {
    $group = 'extras'
    foreach ($key in 'light-windows', 'light-macos', 'dark-windows', 'dark-macos') {
        if ($image.BaseName.StartsWith("$key-")) { $group = $key; break }
    }
    $groups[$group] += $image
}

[void]$builder.AppendLine("<div class='page'><h2>Overview - all $($images.Count) captures on one sheet</h2><div class='overview'>")
$number = 0
foreach ($group in $groups.Keys) {
    foreach ($item in $groups[$group]) {
        $number++
        $url = 'file:///' + ($item.FullName -replace '\\', '/')
        [void]$builder.AppendLine("<div class='thumb'><img src='$url'><div class='cap'>$number. $($item.Name)</div></div>")
    }
}
[void]$builder.AppendLine('</div></div>')

[void]$builder.AppendLine("<div class='page'><h2>Capture index - $($images.Count) files</h2><div class='index'>")
$number = 0
foreach ($group in $groups.Keys) {
    foreach ($item in $groups[$group]) {
        $number++
        [void]$builder.AppendLine("<div>$number. $($item.Name)</div>")
    }
}
[void]$builder.AppendLine('</div></div>')

foreach ($group in $groups.Keys) {
    $items = $groups[$group]
    if (-not $items -or $items.Count -eq 0) { continue }
    $pageCount = [int][Math]::Ceiling($items.Count / 6)
    for ($i = 0; $i -lt $items.Count; $i += 6) {
        $pageNo = [int]($i / 6) + 1
        [void]$builder.AppendLine('<div class="page">')
        [void]$builder.AppendLine("<h2>$group - $($items.Count) captures - page $pageNo of $pageCount</h2><div class='grid'>")
        for ($j = $i; $j -lt [Math]::Min($i + 6, $items.Count); $j++) {
            $item = $items[$j]
            $url = 'file:///' + ($item.FullName -replace '\\', '/')
            [void]$builder.AppendLine("<div class='cell'><img src='$url'><div class='cap'>$($item.Name)</div></div>")
        }
        [void]$builder.AppendLine('</div></div>')
    }
}
[void]$builder.AppendLine('</body></html>')
Set-Content -LiteralPath $htmlPath -Value $builder.ToString() -Encoding utf8

$outDir = Split-Path $outPath -Parent
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
Remove-Item -LiteralPath $outPath -ErrorAction SilentlyContinue

$url = 'file:///' + ($htmlPath -replace '\\', '/')
# Chrome first, then Edge; a fresh profile per run avoids singleton locks and
# the watchdog stops a stuck headless print from hanging the build.
$browserCandidates = New-Object System.Collections.Generic.List[string]
$browserCandidates.Add((Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'))
$browserCandidates.Add((Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'))
if (${env:ProgramFiles(x86)}) {
    $browserCandidates.Add((Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe'))
    $browserCandidates.Add((Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'))
}
$browser = $browserCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $browser) { throw 'Neither Chrome nor Edge was found; one is required for headless PDF printing' }

$profile = Join-Path $scratch ('browser-profile-' + (Get-Date -Format 'HHmmssfff'))
New-Item -ItemType Directory -Force -Path $profile | Out-Null
$browserArgs = @(
    '--headless=new',
    '--disable-gpu',
    '--no-first-run',
    '--no-pdf-header-footer',
    '--allow-file-access-from-files',
    "--user-data-dir=$profile",
    "--print-to-pdf=$outPath",
    $url
)
$process = Start-Process -FilePath $browser -ArgumentList $browserArgs -PassThru -NoNewWindow
if (-not $process.WaitForExit(240000)) {
    Get-Process chrome, msedge -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    throw 'headless print did not finish within 240 s; the browser was stopped'
}

if (-not (Test-Path -LiteralPath $outPath -PathType Leaf)) {
    throw "$browser did not produce $outPath (exit $($process.ExitCode))"
}

$info = Get-Item -LiteralPath $outPath
Write-Output ("CAPTURES PDF {0} images={1} bytes={2}" -f $outPath, $images.Count, $info.Length)
