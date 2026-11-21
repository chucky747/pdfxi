# Build a labelled PDF contact sheet from the installed-build captures.
#
# Usage:
#   powershell -NoProfile -File scripts/captures-to-pdf.ps1
#   powershell -NoProfile -File scripts/captures-to-pdf.ps1 -Source docs/evidence/ui/final -Out docs/evidence/ui/PDFXI-installed-captures.pdf
#
# The sheet groups files by their name prefix (light-windows, light-macos,
# dark-windows, dark-macos, extras), puts four captures on each A4-landscape
# page with the file name as a caption, and prints it with Microsoft Edge in
# headless mode. No network access and no external tools beyond Edge.

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

$edgeCandidates = @(
    (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe')
)
$edge = $edgeCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $edge) {
    throw 'Microsoft Edge was not found; it is required for headless PDF printing'
}

$images = Get-ChildItem -LiteralPath $sourceDir -File -Filter '*.png' | Sort-Object Name
if ($images.Count -eq 0) {
    throw "no PNG captures in $sourceDir"
}

# Keep the scratch HTML and the Edge profile inside the repository target
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

foreach ($group in $groups.Keys) {
    $items = $groups[$group]
    if (-not $items -or $items.Count -eq 0) { continue }
    for ($i = 0; $i -lt $items.Count; $i += 4) {
        [void]$builder.AppendLine('<div class="page">')
        [void]$builder.AppendLine("<h2>$group - page $([int]($i / 4) + 1)</h2><div class='grid'>")
        for ($j = $i; $j -lt [Math]::Min($i + 4, $items.Count); $j++) {
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
# A dedicated profile avoids colliding with a running Edge instance; Edge often
# exits non-zero after a successful print, so the file check below decides.
$profile = Join-Path $scratch 'edge-profile'
New-Item -ItemType Directory -Force -Path $profile | Out-Null
$edgeArgs = @(
    '--headless=new',
    '--disable-gpu',
    '--no-first-run',
    '--no-pdf-header-footer',
    '--allow-file-access-from-files',
    "--user-data-dir=$profile",
    "--print-to-pdf=$outPath",
    $url
)
$edgeProcess = Start-Process -FilePath $edge -ArgumentList $edgeArgs -Wait -PassThru -NoNewWindow
if ($edgeProcess.ExitCode -ne 0) {
    Write-Output "EDGE exit $($edgeProcess.ExitCode) (continuing; the file check decides)"
}

if (-not (Test-Path -LiteralPath $outPath -PathType Leaf)) {
    throw "Edge did not produce $outPath"
}

$info = Get-Item -LiteralPath $outPath
Write-Output ("CAPTURES PDF {0} images={1} bytes={2}" -f $outPath, $images.Count, $info.Length)
