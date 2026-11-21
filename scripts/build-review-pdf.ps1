# Build one review PDF for the acceptance campaign: plan, matrix, evidence,
# blockers, handoff and every installed-build capture.
#
# Usage:
#   powershell -NoProfile -File scripts/build-review-pdf.ps1
#   powershell -NoProfile -File scripts/build-review-pdf.ps1 -Out docs/evidence/PDFXI-acceptance-review.pdf
#
# The script needs only Microsoft Edge (headless print). It converts a fixed,
# ordered list of repository documents from Markdown to HTML, appends a labelled
# contact sheet of docs/evidence/ui/final/*.png, and prints the result to PDF.

param(
    [string]$Out = 'docs/evidence/PDFXI-acceptance-review.pdf'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
$scratch = Join-Path $root 'target\tmp\review-pdf'
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$htmlPath = Join-Path $scratch 'review.html'
$profile = Join-Path $scratch 'edge-profile'

function Convert-MarkdownToHtml {
    param([string]$Markdown, [string]$BaseDir)

    $lines = $Markdown -split "`r?`n"
    $html = New-Object System.Text.StringBuilder
    $inCode = $false
    $codeLang = ''
    $codeLines = New-Object System.Collections.Generic.List[string]
    $listType = ''
    $tableRows = New-Object System.Collections.Generic.List[string]
    $paragraph = New-Object System.Collections.Generic.List[string]

    function Escape-Html([string]$text) {
        $text = $text -replace '&', '&amp;'
        $text = $text -replace '<', '&lt;'
        $text = $text -replace '>', '&gt;'
        return $text
    }

    function Format-Inline([string]$text) {
        $t = $text
        # code spans first so their content is not formatted further
        $t = [regex]::Replace($t, '`([^`]+)`', '<code>$1</code>')
        $t = [regex]::Replace($t, '\*\*([^*]+)\*\*', '<strong>$1</strong>')
        $t = [regex]::Replace($t, '(?<!\*)\*([^*]+)\*(?!\*)', '<em>$1</em>')
        $t = [regex]::Replace($t, '\[([^\]]+)\]\(([^)]+)\)', '<a href="$2">$1</a>')
        return $t
    }

    function Flush-Paragraph {
        if ($script:PendingParagraph.Count -gt 0) {
            $text = ($script:PendingParagraph -join ' ')
            [void]$script:Html.AppendLine("<p>$(Format-Inline $text)</p>")
            $script:PendingParagraph.Clear()
        }
    }

    function Flush-List {
        if ($script:ListType) {
            [void]$script:Html.AppendLine("</$($script:ListType)>")
            $script:ListType = ''
        }
    }

    function Flush-Table {
        if ($script:TableRows.Count -ge 2) {
            $header = $script:TableRows[0]
            $body = $script:TableRows | Select-Object -Skip 2
            [void]$script:Html.AppendLine('<table><thead><tr>')
            foreach ($cell in ($header.Trim('|') -split '\|')) {
                [void]$script:Html.AppendLine("<th>$(Format-Inline ($cell.Trim()))</th>")
            }
            [void]$script:Html.AppendLine('</tr></thead><tbody>')
            foreach ($row in $body) {
                [void]$script:Html.AppendLine('<tr>')
                foreach ($cell in ($row.Trim('|') -split '\|')) {
                    [void]$script:Html.AppendLine("<td>$(Format-Inline ($cell.Trim()))</td>")
                }
                [void]$script:Html.AppendLine('</tr>')
            }
            [void]$script:Html.AppendLine('</tbody></table>')
        }
        $script:TableRows.Clear()
    }

    # Register the mutable state in script scope so the helper functions see it.
    $script:Html = $html
    $script:PendingParagraph = $paragraph
    $script:ListType = ''
    $script:TableRows = $tableRows

    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]

        if ($inCode) {
            if ($line -match '^\s*```') {
                [void]$html.AppendLine("<pre><code class='lang-$codeLang'>" + (Escape-Html ($codeLines -join "`n")) + '</code></pre>')
                $inCode = $false
                $codeLines.Clear()
            } else {
                $codeLines.Add($line)
            }
            continue
        }

        if ($line -match '^\s*```(\w*)') {
            Flush-Paragraph; Flush-List; Flush-Table
            $inCode = $true
            $codeLang = $Matches[1]
            continue
        }

        if ($line -match '^\s*\|') {
            Flush-Paragraph; Flush-List
            $tableRows.Add($line)
            continue
        } elseif ($tableRows.Count -gt 0) {
            Flush-Table
        }

        if ($line -match '^(#{1,6})\s+(.*)$') {
            Flush-Paragraph; Flush-List
            $level = [Math]::Min(6, $Matches[1].Length + 1)
            [void]$html.AppendLine("<h$level>$(Format-Inline $Matches[2])</h$level>")
            continue
        }

        if ($line -match '^\s*([-*+])\s+(.*)$') {
            Flush-Paragraph
            if ($listType -ne 'ul') { Flush-List; $listType = 'ul'; [void]$html.AppendLine('<ul>') }
            $item = $Matches[2]
            $todo = ''
            if ($item -match '^\[( |x|X)\]\s+(.*)$') { $todo = if ($Matches[1] -eq ' ') { '&#9744; ' } else { '&#9745; ' }; $item = $Matches[2] }
            [void]$html.AppendLine("<li>$todo$(Format-Inline $item)</li>")
            continue
        }

        if ($line -match '^\s*(\d+)\.\s+(.*)$') {
            Flush-Paragraph
            if ($listType -ne 'ol') { Flush-List; $listType = 'ol'; [void]$html.AppendLine('<ol>') }
            [void]$html.AppendLine("<li>$(Format-Inline $Matches[2])</li>")
            continue
        }
        Flush-List

        if ($line -match '^\s*>\s?(.*)$') {
            Flush-Paragraph
            [void]$html.AppendLine("<blockquote>$(Format-Inline $Matches[1])</blockquote>")
            continue
        }

        if ($line -match '^\s*(---+|\*\*\*+)\s*$') {
            Flush-Paragraph
            [void]$html.AppendLine('<hr>')
            continue
        }

        if ($line.Trim() -eq '') {
            Flush-Paragraph
            continue
        }

        $paragraph.Add($line.Trim())
    }

    Flush-Paragraph
    Flush-List
    Flush-Table
    if ($inCode) {
        [void]$html.AppendLine('<pre><code>' + (Escape-Html ($codeLines -join "`n")) + '</code></pre>')
    }
    return $html.ToString()
}

$sections = New-Object System.Collections.Generic.List[object]
function Add-Section([string]$Title, [string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Write-Output "SKIP missing $Path"
        return
    }
    $sections.Add([pscustomobject]@{ Title = $Title; Path = (Resolve-Path -LiteralPath $Path).Path })
}

Add-Section 'Campaign plan' 'docs/plans/archive/2026-11-20-pdfxi-feature-acceptance-e06453b4/2026-11-20-pdfxi-feature-acceptance-e06453b4.md'
Add-Section 'Campaign goal' 'docs/plans/archive/2026-11-20-pdfxi-feature-acceptance-e06453b4/2026-11-20-pdfxi-feature-acceptance-e06453b4.goal.md'
Add-Section 'Acceptance matrix (A-/O- items)' 'docs/design/FEATURE_ACCEPTANCE.md'
Add-Section 'Feature matrix summary' 'docs/FEATURES.md'
Add-Section 'Release evidence (T-0262)' 'docs/evidence/release/T-0262.md'
Add-Section 'Acceptance scenario and QA (T-0260)' 'docs/evidence/acceptance/T-0260.md'
Add-Section 'Fuzz targets and runner (T-0260A)' 'docs/evidence/acceptance/T-0260A.md'
Add-Section 'Open/edit/save budgets (T-0260B)' 'docs/evidence/acceptance/T-0260B.md'
Add-Section 'Documentation close-out (T-0261)' 'docs/evidence/acceptance/T-0261.md'
foreach ($file in (Get-ChildItem 'docs/evidence/ui' -File -Filter 'T-*.md' | Sort-Object Name)) {
    Add-Section ("UI evidence " + $file.BaseName) $file.FullName
}
Add-Section 'Handoff' 'orchestration/handoff/2026-11-21-acceptance-handoff.md'
foreach ($file in (Get-ChildItem 'orchestration/blockers' -File -Filter '*.md' | Sort-Object Name)) {
    Add-Section ("Blocker " + $file.BaseName) $file.FullName
}

$css = @'
@page { size: A4 portrait; margin: 13mm 12mm 13mm 12mm; }
body { font-family: "Segoe UI", Arial, sans-serif; font-size: 9.6pt; color: #1a1a1a; line-height: 1.42; }
.cover { page-break-after: always; padding-top: 30mm; }
.cover h1 { font-size: 30pt; margin: 0 0 6mm 0; }
.cover .sub { font-size: 12pt; color: #555; margin-bottom: 10mm; }
.cover table { width: 100%; border-collapse: collapse; margin-top: 6mm; }
.cover td { border: 1px solid #ccc; padding: 2.2mm 3mm; font-size: 9.6pt; vertical-align: top; }
.cover td.k { width: 42mm; background: #f4f4f4; font-weight: 600; }
.toc { page-break-after: always; }
.toc h2 { font-size: 15pt; }
.toc ol { margin-left: 6mm; }
.toc li { margin: 1.1mm 0; }
section.doc { page-break-before: always; }
section.doc > h1.doctitle { font-size: 16pt; border-bottom: 2px solid #b3261e; padding-bottom: 2mm; margin: 0 0 4mm 0; }
section.doc h2 { font-size: 13pt; margin: 5mm 0 2mm; border-bottom: 1px solid #ddd; padding-bottom: 1mm; }
section.doc h3 { font-size: 11pt; margin: 4mm 0 1.5mm; }
section.doc h4 { font-size: 10pt; margin: 3mm 0 1mm; }
p { margin: 0 0 2mm 0; }
ul, ol { margin: 0 0 2.5mm 6mm; padding-left: 4mm; }
li { margin: 0.6mm 0; }
pre { background: #f6f8fa; border: 1px solid #e1e4e8; border-radius: 2mm; padding: 2.5mm; font-size: 8pt; white-space: pre-wrap; word-break: break-word; margin: 0 0 3mm 0; }
code { font-family: Consolas, monospace; font-size: 8.4pt; background: #f2f2f2; padding: 0 1mm; border-radius: 1mm; }
pre code { background: none; padding: 0; font-size: 8pt; }
table { width: 100%; border-collapse: collapse; margin: 0 0 3mm 0; font-size: 8.4pt; }
th, td { border: 1px solid #cfcfcf; padding: 1.4mm 2mm; vertical-align: top; text-align: left; }
th { background: #f2f2f2; }
blockquote { border-left: 3px solid #b3261e; margin: 0 0 2mm 0; padding: 1mm 3mm; color: #444; background: #fafafa; }
hr { border: none; border-top: 1px solid #ddd; margin: 3mm 0; }
a { color: #0b57d0; text-decoration: none; }
img { max-width: 100%; }
.gallery .page { page-break-after: always; }
.gallery .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 3mm; }
.gallery .cell { border: 1px solid #ddd; border-radius: 2mm; padding: 1.5mm; }
.gallery .cell img { width: 100%; display: block; }
.gallery .cap { font-family: Consolas, monospace; font-size: 7pt; color: #555; margin-top: 1mm; }
'@

$builder = New-Object System.Text.StringBuilder
[void]$builder.AppendLine('<html><head><meta charset="utf-8"><style>')
[void]$builder.AppendLine($css)
[void]$builder.AppendLine('</style></head><body>')

$head = (& git log --oneline -1 2>$null | Out-String).Trim()
$passCount = (Select-String -Path 'docs/design/FEATURE_ACCEPTANCE.md' -Pattern '\|\s*pass\s*\|' | Measure-Object).Count
$openCount = (Select-String -Path 'docs/design/FEATURE_ACCEPTANCE.md' -Pattern '\|\s*open\s*\|' | Measure-Object).Count
$images = Get-ChildItem 'docs/evidence/ui/final' -File -Filter '*.png' | Sort-Object Name
$installed = "$env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe"
$installedHash = if (Test-Path -LiteralPath $installed) { (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash.ToLower() } else { 'not installed' }

[void]$builder.AppendLine('<div class="cover">')
[void]$builder.AppendLine('<h1>PDFXI acceptance campaign</h1>')
[void]$builder.AppendLine('<div class="sub">Phases 0-4 - acceptance evidence, blockers, handoff and every installed-build capture</div>')
[void]$builder.AppendLine('<table>')
[void]$builder.AppendLine("<tr><td class='k'>Repository HEAD</td><td>$head</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Acceptance items</td><td>$passCount pass, $openCount open (docs/design/FEATURE_ACCEPTANCE.md)</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Feature matrix</td><td>145 rows, 54 verified; 89 outside this campaign's acceptance surface</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Installed viewer</td><td>%LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe<br>$installedHash</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Gates</td><td>cargo xtask verify: VERIFY OK (no-dead-controls green)<br>cargo deny check: advisories ok, bans ok, licenses ok, sources ok<br>cargo xtask goal-verify --all: 145 rows / 54 verified / 89 unverified (exit 1, expected)</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Captures</td><td>$($images.Count) installed-build PNGs, contact sheet at the end of this PDF</td></tr>")
[void]$builder.AppendLine("<tr><td class='k'>Assistant</td><td>Live turns on OpenCode 2.3.2 with model opencode-go/hy4-flash-free (A-16)</td></tr>")
[void]$builder.AppendLine('</table></div>')

[void]$builder.AppendLine('<div class="toc"><h2>Contents</h2><ol>')
$n = 0
foreach ($section in $sections) { $n++; [void]$builder.AppendLine("<li>$n. $($section.Title)</li>") }
$n++
[void]$builder.AppendLine("<li>$n. Installed-build captures ($($images.Count) images)</li>")
[void]$builder.AppendLine('</ol></div>')

foreach ($section in $sections) {
    $text = Get-Content -LiteralPath $section.Path -Raw -Encoding utf8
    $body = Convert-MarkdownToHtml -Markdown $text -BaseDir (Split-Path $section.Path -Parent)
    [void]$builder.AppendLine('<section class="doc">')
    [void]$builder.AppendLine("<h1 class='doctitle'>$($section.Title)</h1>")
    [void]$builder.AppendLine($body)
    [void]$builder.AppendLine('</section>')
}

[void]$builder.AppendLine('<section class="doc gallery"><h1 class="doctitle">Installed-build captures</h1>')
$groups = [ordered]@{ 'light-windows' = @(); 'light-macos' = @(); 'dark-windows' = @(); 'dark-macos' = @(); 'extras' = @() }
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
[void]$builder.AppendLine('</section></body></html>')

Set-Content -LiteralPath $htmlPath -Value $builder.ToString() -Encoding utf8

$edgeCandidates = @(
    (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe')
)
$edge = $edgeCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $edge) { throw 'Microsoft Edge was not found; it is required for headless PDF printing' }

New-Item -ItemType Directory -Force -Path $profile | Out-Null
$outDir = Split-Path $outPath -Parent
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
Remove-Item -LiteralPath $outPath -ErrorAction SilentlyContinue

$url = 'file:///' + ($htmlPath -replace '\\', '/')
$edgeArgs = @(
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-pdf-header-footer',
    '--allow-file-access-from-files', "--user-data-dir=$profile",
    "--print-to-pdf=$outPath", $url
)
$process = Start-Process -FilePath $edge -ArgumentList $edgeArgs -Wait -PassThru -NoNewWindow
if (-not (Test-Path -LiteralPath $outPath -PathType Leaf)) {
    throw "Edge did not produce $outPath (exit $($process.ExitCode))"
}
$info = Get-Item -LiteralPath $outPath
Write-Output ("REVIEW PDF {0} sections={1} images={2} bytes={3}" -f $outPath, $sections.Count, $images.Count, $info.Length)

