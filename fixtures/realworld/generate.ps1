<#
.SYNOPSIS
    Regenerate the real-producer PDF corpus under fixtures/realworld/.

.DESCRIPTION
    Produces the corpus files that this host can actually author:

      powerpoint-slides.pdf        Microsoft PowerPoint 16.0 COM, 3 slides, 16:9
      browser-print.pdf            Microsoft Edge headless --print-to-pdf
      pdfxi-forms-selfmade.pdf     project testkit (labelled self-produced)
      pdfxi-encrypted-selfmade.pdf project encryption writer (password pdfxi-test)

    All document content is original text written for this repository. Nothing
    is downloaded. The script writes intermediate files under target/tmp and
    copies the finished files into fixtures/realworld/.

    Microsoft Word COM export is currently blocked on the reference host
    (SaveAs2/ExportAsFixedFormat hang). The Word slots are recorded as a gap in
    orchestration/blockers/T-0215-word-com.md and in README.md.

.PARAMETER OutputDir
    Destination directory. Defaults to the directory holding this script.

.PARAMETER TempDir
    Scratch directory. Defaults to target/tmp/realworld under the repository
    root.
#>
[CmdletBinding()]
param(
    [string]$OutputDir,
    [string]$TempDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $OutputDir) { $OutputDir = $PSScriptRoot }
if (-not $TempDir) { $TempDir = Join-Path $repoRoot 'target\tmp\realworld' }
New-Item -ItemType Directory -Force -Path $OutputDir, $TempDir | Out-Null
$env:TEMP = $TempDir
$env:TMP = $TempDir
function New-PowerPointSlides {
    param([string]$OutPath)
    $pp = New-Object -ComObject PowerPoint.Application
    $pp.Visible = -1
    $pres = $null
    try {
        $pres = $pp.Presentations.Add()
        $pres.PageSetup.SlideSize = 15  # ppSlideSizeOnScreen16x9
        $titles = @('Lantern Slide', 'Harbor Slide', 'Meadow Slide')
        $texts = @(
            'The lantern slide marker is the first landscape page.',
            'The harbor slide marker is the second landscape page.',
            'The meadow slide marker is the third and final landscape page.'
        )
        for ($i = 0; $i -lt 3; $i++) {
            $slide = $pres.Slides.Add($i + 1, 2)  # ppLayoutText
            $slide.Shapes.Title.TextFrame.TextRange.Text = $titles[$i]
            $slide.Shapes.Item(2).TextFrame.TextRange.Text = $texts[$i]
        }
        $pres.SaveAs($OutPath, 32)  # ppSaveAsPDF
    }
    finally {
        if ($null -ne $pres) { $pres.Close() }
        $pp.Quit()
    }
}

function New-BrowserPrint {
    param([string]$OutPath, [string]$WorkDir)
    $edge = @(
        (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe')
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $edge) { throw 'Microsoft Edge is not installed' }
    $htmlPath = Join-Path $WorkDir 'browser-print.html'
    $lines = @(
        '<!DOCTYPE html>',
        '<html lang="en"><head><meta charset="utf-8"><title>PDFXI Browser Print Sample</title></head>',
        '<body>',
        '<h1>PDFXI Browser Print Sample</h1>',
        '<p>The kestrel browser marker opens this print-to-PDF sample. It was written locally for the pdfxi corpus and contains no third-party content.</p>',
        '<p>External link: <a href="https://example.com">example.com</a></p>',
        '<p>Internal link: <a href="#section-two">jump to section two</a></p>',
        '<h2 id="section-two">Section Two</h2>',
        '<p>The kestrel anchor marker lives in the second section, which the internal link targets.</p>',
        '</body></html>'
    )
    Set-Content -Path $htmlPath -Value ($lines -join "`n") -Encoding utf8
    $uri = [Uri]::new($htmlPath).AbsoluteUri
    & $edge --headless --disable-gpu --no-pdf-header-footer `
        "--user-data-dir=$(Join-Path $WorkDir 'edge-profile')" `
        "--print-to-pdf=$OutPath" $uri | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Edge print-to-pdf failed with exit code $LASTEXITCODE" }
}
$pptOut = Join-Path $TempDir 'powerpoint-slides.pdf'
$pdfOut = Join-Path $TempDir 'browser-print.pdf'
Write-Host 'PowerPoint: 3 slides, 16:9'
New-PowerPointSlides -OutPath $pptOut
Write-Host 'Edge: headless print-to-pdf'
New-BrowserPrint -OutPath $pdfOut -WorkDir $TempDir
Copy-Item -LiteralPath $pptOut -Destination (Join-Path $OutputDir 'powerpoint-slides.pdf') -Force
Copy-Item -LiteralPath $pdfOut -Destination (Join-Path $OutputDir 'browser-print.pdf') -Force

# Self-produced slots: no installed external producer can emit AcroForm fields
# or a genuinely password-protected PDF on the reference host.
Write-Host 'pdfxi-testkit: self-produced forms and encrypted PDFs'
Push-Location $repoRoot
try {
    & cargo run -q -p pdfxi-testkit --example generate_realworld_selfmade
    if ($LASTEXITCODE -ne 0) { throw "generate_realworld_selfmade failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

Write-Host "Real-world corpus written to $OutputDir"
Get-ChildItem -LiteralPath $OutputDir -Filter '*.pdf' | ForEach-Object {
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "{0} {1} {2}" -f $_.Name, $_.Length, $hash
}
