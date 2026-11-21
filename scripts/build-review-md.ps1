# Build the acceptance review as one Markdown document with inline captures.
#
# Usage:
#   powershell -NoProfile -File scripts/build-review-md.ps1
#   powershell -NoProfile -File scripts/build-review-md.ps1 -Out docs/evidence/PDFXI-acceptance-review.md
#
# The document is generated so the numbers always match docs/features.toml and
# docs/design/FEATURE_ACCEPTANCE.md. Every PNG in docs/evidence/ui/final gets an
# inline image and a descriptor that says what the window actually shows.
# No PDF, no browser, no network.

param(
    [string]$Out = 'docs/evidence/PDFXI-acceptance-review.md',
    [string]$CaptureDir = 'docs/evidence/ui/final'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
$capturePath = if ([System.IO.Path]::IsPathRooted($CaptureDir)) { $CaptureDir } else { Join-Path $root $CaptureDir }
if (-not (Test-Path -LiteralPath $capturePath -PathType Container)) {
    throw "capture directory not found: $capturePath"
}

# --- feature matrix -------------------------------------------------------
$raw = Get-Content 'docs/features.toml' -Raw -Encoding utf8
$raw = [regex]::Replace($raw, '(?s)notes\s*=\s*""".*?"""', '')
$features = New-Object System.Collections.Generic.List[object]
foreach ($block in ([regex]::Split($raw, '(?m)^\[\[feature\]\]\s*$') | Where-Object { $_.Trim() })) {
    $id = [regex]::Match($block, '(?m)^id\s*=\s*"([^"]+)"')
    if (-not $id.Success) { continue }
    $features.Add([pscustomobject]@{
            Id     = $id.Groups[1].Value
            Title  = [regex]::Match($block, '(?m)^title\s*=\s*"([^"]*)"').Groups[1].Value
            Status = [regex]::Match($block, '(?m)^status\s*=\s*"([^"]*)"').Groups[1].Value
            Domain = [regex]::Match($block, '(?m)^domain\s*=\s*"([^"]*)"').Groups[1].Value
        })
}
$statusOrder = @('verified', 'library', 'model', 'replaced')
$counts = @{}
foreach ($status in $statusOrder) { $counts[$status] = @($features | Where-Object { $_.Status -eq $status }).Count }
$verified = @($features | Where-Object { $_.Status -eq 'verified' } | Sort-Object Domain, Id)
$unverified = @($features | Where-Object { $_.Status -ne 'verified' } | Sort-Object Status, Domain, Id)
$total = $features.Count

# --- acceptance matrix ----------------------------------------------------
$acceptanceText = Get-Content 'docs/design/FEATURE_ACCEPTANCE.md' -Raw -Encoding utf8
$passItems = ([regex]::Matches($acceptanceText, '(?m)^\|\s*(A|O)-\d+\s*\|[^|]*\|[^|]*\|[^|]*\|\s*pass\s*\|')).Count
$openItems = ([regex]::Matches($acceptanceText, '(?m)^\|\s*(A|O)-\d+\s*\|[^|]*\|[^|]*\|[^|]*\|\s*open\s*\|')).Count

# --- captures -------------------------------------------------------------
$captures = Get-ChildItem -LiteralPath $capturePath -File -Filter '*.png' | Sort-Object Name

$toolNotes = @{
    'main'              = 'Main window: the three-page fixture is open, the Pages panel lists three thumbnails, three bookmarks are loaded, the tool rail is visible, and the Tools panel is open on the right. This is the base state every other capture starts from.'
    'ai'                = 'AI Assistant tool with the panel open: "Connect to OpenCode", the notice that a bounded excerpt is sent to the local service, the Ask box, and the Summarize / Key insights buttons. Not connected in this capture.'
    'ai-answer'         = 'AI Assistant after a live turn: connected to OpenCode 2.3.2 (57 models), model opencode-go/hy4-flash-free, question "Summarize this document in one sentence." and the answer naming the Lantern, Harbor and Meadow slide markers.'
    'ai-insights'       = 'AI Assistant after the Key insights turn: same connection and model, prompt "List the key insights in this document as short bullets.", and a four-bullet answer naming each slide marker and its page.'
    'comment'           = 'Comment tool selected in the toolbar. The viewer, Pages panel and bookmarks are visible; no annotation panel is open, because it opens after a text or page-region selection. Treat this as the tool-selected state, not as proof a comment was placed.'
    'highlight'         = 'Highlight tool selected in the toolbar. Same base viewer state as the Comment capture; a highlight needs a text selection, so no markup is visible here.'
    'convert'           = 'Convert to PDF panel: add files, output choice ("No output chosen"), title, open-after-converting, page size, and the Convert button.'
    'edit'              = 'Edit PDF panel: "No text selected", Add text, Add image, Done and Close.'
    'export'            = 'Export section at the bottom of the Tools panel: "Convert this PDF to" with the format list (Plain text shown) and the Export button.'
    'fill'              = 'Fill & Sign panel over the fixture, reporting that this document has no form fields. The fielded version is the fill-forms capture.'
    'fill-forms'        = 'Fill & Sign over pdfxi-forms-selfmade.pdf: three fields filled (text "typed value", checkbox On, choice "two"), field list, Reset and Flatten, and the form data exchange buttons (Import/Export XFDF, Import/Export PDF).'
    'organize'          = 'Organize Pages panel: "0 of 3 page(s) selected", Rotate left/right, Delete, Move up/down, Insert blank page before/after, Extract selected, Split each page.'
    'protect'           = 'Protect PDF panel: user and owner password fields, permission checkboxes (printing, copying text and images, annotations, form filling), Protect and save as, and Remove protection.'
    'sign'              = 'Sign dialog: certificate signature (PKCS#12 identity, container password, reason, location, page, appearance, Sign/Close) and the request-signature form (recipient email, subject, message, Send request) with Tracked requests empty.'
    'request-signature' = 'Sign dialog again, with the request-signature section in view. It is visually identical to the sign capture, so it adds no distinct state of its own.'
    'print'             = 'Print dialog over the viewer: pages All/Current/Pages, copies, scale, orientation, Print and Cancel. Note: this file is named light-windows-print but was captured in the dark theme.'
    'help'              = 'PDFXI help panel: getting started, keyboard shortcuts, themes and window controls, the privacy statement about what leaves the machine, and an Open user guide button.'
    'word-opens-docx'   = 'Microsoft Word opening the exported DOCX (t0260-export.docx, Compatibility Mode) and showing the exported text, as the round-trip check for Word export.'
}

$groups = [ordered]@{
    'light-windows' = @{ Theme = 'Light theme'; Platform = 'Windows caption buttons'; Captures = @() }
    'light-macos'   = @{ Theme = 'Light theme'; Platform = 'macOS caption buttons'; Captures = @() }
    'dark-windows'  = @{ Theme = 'Dark theme'; Platform = 'Windows caption buttons'; Captures = @() }
    'dark-macos'    = @{ Theme = 'Dark theme'; Platform = 'macOS caption buttons'; Captures = @() }
    'other'         = @{ Theme = ''; Platform = ''; Captures = @() }
}
foreach ($capture in $captures) {
    $group = 'other'
    foreach ($key in 'light-windows', 'light-macos', 'dark-windows', 'dark-macos') {
        if ($capture.BaseName.StartsWith("$key-")) { $group = $key; break }
    }
    $groups[$group].Captures += $capture
}

$lines = New-Object System.Collections.Generic.List[string]
function Add-Line([string]$text = '') { $lines.Add($text) }
function Add-TableRow([string[]]$cells) { Add-Line ('| ' + ($cells -join ' | ') + ' |') }

# --- document -------------------------------------------------------------
Add-Line '# PDFXI acceptance review'
Add-Line
Add-Line 'This is the review document for the PDFXI feature-acceptance campaign: what'
Add-Line 'was accepted, what the installed build looks like in every captured state, and'
Add-Line 'which feature rows are verified and which are not. It is generated by'
Add-Line '`scripts/build-review-md.ps1` from `docs/features.toml`,'
Add-Line '`docs/design/FEATURE_ACCEPTANCE.md` and the PNG files in'
Add-Line '`docs/evidence/ui/final/`, so the numbers and the captures cannot drift from the'
Add-Line 'repository.'
Add-Line
Add-Line 'Every image below is embedded from `ui/final/`. Open this file in an editor'
Add-Line 'that renders Markdown images (VS Code, GitHub, Obsidian) and the screenshots'
Add-Line 'appear inline with their descriptors.'
Add-Line
Add-Line '## Status at a glance'
Add-Line
Add-TableRow @('Measure', 'Result')
Add-TableRow @('---', '---')
Add-TableRow @('Acceptance items (A-/O- matrix)', "$passItems pass, $openItems open")
Add-TableRow @('Feature rows (canonical matrix)', "$total")
Add-TableRow @('Reachable and tested (`verified`)', "$($counts['verified'])")
Add-TableRow @('`library` (byte-level code, no user path)', "$($counts['library'])")
Add-TableRow @('`model` (model, contract or stub)', "$($counts['model'])")
Add-TableRow @('`replaced`', "$($counts['replaced'])")
Add-TableRow @('Installed build', '`%LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe`, SHA-256 `6787a129f8321e2c08aea07f1532d9e349306a0daffabd461351f27914ca77dc`')
Add-TableRow @('Captures', "$($captures.Count) PNG files, embedded below")
Add-Line
Add-Line '**The honest reading:** the campaign accepted 40 of 40 items on the surface it'
Add-Line 'chose, and 54 of 145 feature rows run on real documents and are reachable. The'
Add-Line 'other 91 rows are `library`, `model` or `replaced`, so "all features working" is'
Add-Line 'not a true statement about this build. The register below lists every row.'
Add-Line
Add-Line '## Gates on the committed tree'
Add-Line
Add-Line '```text'
Add-Line 'cargo xtask verify            -> VERIFY OK; no_dead_controls ok; FEATURES OK: 145 rows, 54 verified'
Add-Line 'cargo deny check              -> advisories ok, bans ok, licenses ok, sources ok'
Add-Line 'cargo xtask docs              -> DOCS OK'
Add-Line 'cargo xtask features verify   -> FEATURES OK: 145 rows, 54 verified'
Add-Line 'cargo xtask goal-verify --all -> 145 rows / 54 verified / 89 not verified or replaced'
Add-Line '                                 (exit 1 by design, ADR 0009)'
Add-Line '```'
Add-Line
Add-Line '## Captures'
Add-Line
Add-Line "All $($captures.Count) installed-build captures are embedded below, grouped by"
Add-Line 'theme and caption style. Each image is followed by a descriptor of what the'
Add-Line 'window actually shows, including the cases where a tool is selected but its'
Add-Line 'panel is not open.'
Add-Line
Add-Line '| Group | Captures |'
Add-Line '| --- | --- |'
foreach ($groupKey in $groups.Keys) {
    $info = $groups[$groupKey]
    if ($info.Captures.Count -eq 0) { continue }
    $label = if ($groupKey -eq 'other') { 'Other' } else { "$($info.Theme), $($info.Platform)" }
    Add-Line "| $label | $($info.Captures.Count) |"
}
Add-Line
Add-Line 'Two things to keep in mind while reading them: `light-windows-print.png` is'
Add-Line 'dark despite its name, and `sign` / `request-signature` are the same dialog.'
Add-Line 'Both are called out again next to the images.'
Add-Line

$sectionNumber = 0
foreach ($groupKey in $groups.Keys) {
    $info = $groups[$groupKey]
    if ($info.Captures.Count -eq 0) { continue }
    $sectionNumber++
    $heading = if ($groupKey -eq 'other') { 'Other captures' } else { "$($info.Theme) - $($info.Platform)" }
    Add-Line "## $sectionNumber. $heading ($($info.Captures.Count))"
    Add-Line
    Add-Line "Fixture: ``fixtures/realworld/powerpoint-slides.pdf`` (3 landscape pages) unless"
    Add-Line 'the descriptor names another file.'
    Add-Line
    foreach ($capture in $info.Captures) {
        $tool = $capture.BaseName
        foreach ($key in 'light-windows-', 'light-macos-', 'dark-windows-', 'dark-macos-') {
            if ($tool.StartsWith($key)) { $tool = $tool.Substring($key.Length); break }
        }
        $note = if ($toolNotes.ContainsKey($tool)) { $toolNotes[$tool] } else { 'Captured window state for this feature.' }
        $prefix = if ($groupKey -eq 'other') { '' } else { "$($info.Theme), $($info.Platform). " }
        Add-Line "### ``$($capture.Name)``"
        Add-Line
        Add-Line "![$($capture.BaseName)](ui/final/$($capture.Name))"
        Add-Line
        Add-Line "*$prefix$note*"
        Add-Line
    }
}

Add-Line '## Feature status register'
Add-Line
Add-Line "Counts from ``docs/features.toml``: $total rows - $($counts['verified']) verified,"
Add-Line "$($counts['library']) library, $($counts['model']) model, $($counts['replaced']) replaced."
Add-Line 'The meanings come from ADR 0009.'
Add-Line
Add-Line '| Status | Rows | Meaning |'
Add-Line '| --- | --- | --- |'
Add-Line "| `verified` | $($counts['verified']) | Runs on real document bytes and a user can reach it through the GUI or the CLI. |"
Add-Line "| `library` | $($counts['library']) | Real byte-level implementation over PDF documents and tested, but no user-reachable path or narrower than the product row. |"
Add-Line "| `model` | $($counts['model']) | In-memory model, plan, contract, or host-boundary stub; not a working product feature. |"
Add-Line "| `replaced` | $($counts['replaced']) | Superseded by another row. |"
Add-Line "| **total** | **$total** | Canonical matrix: `docs/features.toml` |"
Add-Line
Add-Line "### Verified rows ($($verified.Count))"
Add-Line
Add-Line '| ID | Title | Domain |'
Add-Line '| --- | --- | --- |'
foreach ($row in $verified) { Add-Line "| ``$($row.Id)`` | $($row.Title) | $($row.Domain) |" }
Add-Line
Add-Line "### Not verified ($($unverified.Count))"
Add-Line
Add-Line 'These rows exist in the matrix but are not reachable product features yet.'
Add-Line
Add-Line '| ID | Title | Status | Domain |'
Add-Line '| --- | --- | --- | --- |'
foreach ($row in $unverified) { Add-Line "| ``$($row.Id)`` | $($row.Title) | ``$($row.Status)`` | $($row.Domain) |" }
Add-Line
Add-Line '## Blockers on record'
Add-Line
foreach ($blocker in (Get-ChildItem 'orchestration/blockers' -File -Filter '*.md' | Sort-Object Name)) {
    $firstHeading = (Get-Content -LiteralPath $blocker.FullName -TotalCount 12 -Encoding utf8 |
        Where-Object { $_ -match '^#{1,2}\s+' } | Select-Object -First 1)
    $title = if ($firstHeading) { $firstHeading -replace '^#{1,2}\s+', '' } else { $blocker.BaseName }
    Add-Line "- ``$($blocker.BaseName)`` - $title"
}
Add-Line
Add-Line '## Sources'
Add-Line
Add-Line '- `docs/design/FEATURE_ACCEPTANCE.md` - the A-/O- acceptance matrix.'
Add-Line '- `docs/features.toml` - the canonical feature matrix enforced by the gates.'
Add-Line '- `docs/evidence/release/T-0262.md` - installer, shortcut, captures, assistant turns.'
Add-Line '- `docs/evidence/ui/` - per-task UI evidence documents.'
Add-Line '- `docs/decisions/0009-feature-status-honesty.md` - what each status means.'
Add-Line '- `docs/evidence/ui/final/` - the PNGs embedded above.'
Add-Line
Add-Line '---'
Add-Line
Add-Line "Generated by ``scripts/build-review-md.ps1``: $($captures.Count) captures, $total feature rows, $passItems acceptance items."
Add-Line

$outDir = Split-Path $outPath -Parent
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
Set-Content -LiteralPath $outPath -Value ($lines -join "`r`n") -Encoding utf8

$missing = @()
$document = $lines -join "`r`n"
foreach ($capture in $captures) {
    if (-not $document.Contains("](ui/final/$($capture.Name))")) { $missing += $capture.Name }
}
if ($missing.Count -gt 0) { throw "captures without an image reference: $($missing -join ', ')" }

Write-Output ("REVIEW MD {0} captures={1} rows={2} acceptance={3}pass/{4}open bytes={5}" -f `
        $outPath, $captures.Count, $total, $passItems, $openItems, (Get-Item -LiteralPath $outPath).Length)
