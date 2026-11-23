# Installed-build UI smoke for PDFXI (Windows, UI Automation).
#
# Runs the installed viewer against deterministic fixtures and checks the
# behaviours that unit tests cannot prove: the window opens, the accessibility
# tree has no unnamed invokable controls, text search reaches real page text,
# password-protected files unlock, page rotation keeps bookmarks, a dirty
# window close prompts, a deep page tree does not crash, and form fields load.
#
# The script is self-contained: it generates the protected, rotated and deep
# fixtures with the installed CLI and uses the bundled PDFium checker as an
# independent reader. It writes screenshots and a log under -OutDir and exits
# non-zero on the first failed group.
#
# Usage:
#   powershell -NoProfile -File scripts/ui-smoke.ps1 `
#     -Exe "$env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe"
#
# The installed viewer and CLI must share a directory with pdfium.dll.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Cli = '',
    [string]$FixtureDir = '',
    [string]$OutDir = '',
    [string]$PdfiumPath = '',
    [int]$WaitMs = 3500
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $FixtureDir) { $FixtureDir = Join-Path $root 'fixtures\ui-smoke' }
if (-not $OutDir) { $OutDir = Join-Path $root 'target\tmp\ui-smoke' }
$installDir = Split-Path $Exe -Parent
if (-not $Cli) { $Cli = Join-Path $installDir 'pdfxi.exe' }
if (-not $PdfiumPath) { $PdfiumPath = Join-Path $installDir 'pdfium.dll' }
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "viewer not found: $Exe" }
if (-not (Test-Path -LiteralPath $Cli -PathType Leaf)) { throw "CLI not found: $Cli" }
if (-not (Test-Path -LiteralPath $PdfiumPath -PathType Leaf)) { throw "pdfium.dll not found: $PdfiumPath" }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$Cli = (Resolve-Path -LiteralPath $Cli).Path
$PdfiumPath = (Resolve-Path -LiteralPath $PdfiumPath).Path
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# The harness resolves its process by this path and writes screenshots here.
# Set both after dot-sourcing: the harness defaults them to the real install.
. (Join-Path $PSScriptRoot 'ui-harness.ps1')
$script:RvUi = $Exe
$script:RvDir = $OutDir

# pdfium-check.ps1 preserves a caller-supplied path and loads it on first use.
$script:PdfiumDll = $PdfiumPath
. (Join-Path $PSScriptRoot 'pdfium-check.ps1')

$script:Passes = 0
$script:Failures = 0
$script:Log = Join-Path $OutDir 'ui-smoke.log'
Remove-Item -LiteralPath $script:Log -Force -ErrorAction SilentlyContinue

function Write-SmokePass {
    param([string]$Message)
    $script:Passes++
    $line = "PASS $Message"
    Write-Output $line
    Add-Content -LiteralPath $script:Log -Value $line
}

function Write-SmokeFail {
    param([string]$Message)
    $script:Failures++
    $line = "FAIL $Message"
    Write-Output $line
    Add-Content -LiteralPath $script:Log -Value $line
}

function Test-UiAlive {
    @(Get-UiProc).Count -gt 0
}

function Wait-TreePattern {
    param([string]$Pattern, [int]$Seconds = 12)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (-not (Test-UiAlive)) { return $false }
        try {
            if (@(Tree -Match $Pattern).Count -gt 0) { return $true }
        } catch { }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

function Assert-TreePattern {
    param([string]$Label, [string]$Pattern, [int]$Seconds = 12)
    if (Wait-TreePattern -Pattern $Pattern -Seconds $Seconds) {
        Write-SmokePass $Label
        $hit = @(Tree -Match $Pattern | Select-Object -First 1)
        Add-Content -LiteralPath $script:Log -Value ("  {0}" -f $hit)
        return $true
    }
    Write-SmokeFail "$Label (missing /$Pattern/)"
    return $false
}

function Assert-Pdfium {
    param([string]$Label, [string]$Path, [string]$Pattern, [string]$Password = '')
    $report = if ($Password) {
        Pdf-Report -Path $Path -Password $Password -TextChars 0
    } else {
        Pdf-Report -Path $Path -TextChars 0
    }
    $text = ($report -join "`n")
    Add-Content -LiteralPath $script:Log -Value $text
    if ($text -match $Pattern) {
        Write-SmokePass $Label
        return $true
    }
    Write-SmokeFail "$Label (PDFium did not report /$Pattern/)"
    return $false
}

function Invoke-CliResult {
    param([string[]]$Arguments)
    $stdout = Join-Path $OutDir 'cli-stdout.txt'
    $stderr = Join-Path $OutDir 'cli-stderr.txt'
    $process = Start-Process -FilePath $Cli -ArgumentList $Arguments -Wait -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr -NoNewWindow
    $out = ''
    if (Test-Path -LiteralPath $stdout) { $out += (Get-Content -LiteralPath $stdout -Raw) }
    if (Test-Path -LiteralPath $stderr) { $out += (Get-Content -LiteralPath $stderr -Raw) }
    @{ ExitCode = $process.ExitCode; Text = $out }
}

function Assert-NoUnnamedControls {
    param([string]$Label)
    $root = Get-UiRoot
    $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
    $main = Get-UiWin
    $mainRect = New-Object RvW+RECT
    [void][RvW]::GetWindowRect($main, [ref]$mainRect)
    $bad = @()
    $elements = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($element in $elements) {
        try {
            $current = $element.Current
            if (-not [string]::IsNullOrWhiteSpace($current.Name)) { continue }
            $type = $current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            if ($type -notin @('Button', 'CheckBox', 'ComboBox', 'Custom', 'Edit', 'RadioButton')) { continue }
            $patterns = @($element.GetSupportedPatterns() | ForEach-Object {
                $_.ProgrammaticName -replace 'PatternIdentifiers\.Pattern$', '' -replace 'Pattern$', ''
            }) -join ','
            if ($patterns -notmatch 'Invoke|Toggle|Selection|Value|Text') { continue }
            if ($type -eq 'Custom') {
                # egui's built-in scrollbar and panel-resize chrome is a thin
                # bar with no app-authored accessible name. It is framework
                # chrome, not a button or field: the scroll area's content and
                # every app control are still checked below and above.
                $bounds = $current.BoundingRectangle
                if (-not $bounds.IsEmpty -and ($bounds.Width -le 4 -or $bounds.Height -le 4)) {
                    continue
                }
                # egui 0.33 gives a floating Window a private drag handle with no
                # separate accessible name; the Window node itself is named.
                # Skip only that chrome, not app-authored controls.
                $parent = $walker.GetParent($element)
                if ($parent) {
                    $parentCurrent = $parent.Current
                    $parentType = $parentCurrent.ControlType.ProgrammaticName -replace '^ControlType\.', ''
                    if ($parentType -eq 'Window') {
                        $parentRect = $parentCurrent.BoundingRectangle
                        if (-not $parentRect.IsEmpty -and
                            ($parentRect.Width -lt $mainRect.R - $mainRect.L -or
                             $parentRect.Height -lt $mainRect.B - $mainRect.T)) {
                            continue
                        }
                    }
                }
            }
            $bad += Format-El $element
        } catch { }
    }
    if ($bad.Count -eq 0) {
        Write-SmokePass "$Label has no unnamed invokable controls"
    } else {
        Write-SmokeFail "$Label has $($bad.Count) unnamed invokable control(s)"
        foreach ($line in $bad) { Add-Content -LiteralPath $script:Log -Value "  $line" }
    }
}

function Start-SmokeUi {
    param([string[]]$Arguments = @(), [string]$Tag)
    Stop-Ui -Force -WaitMs 1500 | Out-Null
    Start-Sleep -Milliseconds 300
    $line = Start-Ui -ArgList $Arguments -WaitMs $WaitMs -Width 1600 -Height 1000
    Add-Content -LiteralPath $script:Log -Value $line
    if (-not (Test-UiAlive)) { throw "viewer did not stay alive for $Tag" }
}

function Close-SmokeUi {
    param([string]$Tag)
    Stop-Ui -WaitMs 5000 | Out-Null
    if (Test-UiAlive) {
        Write-SmokeFail "$Tag did not close on WM_CLOSE"
        @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
    } else {
        Write-SmokePass "$Tag closed on WM_CLOSE"
    }
}

function New-DeepPagesPdf {
    param([Parameter(Mandatory = $true)][string]$Path, [int]$Depth = 20000)
    $objects = New-Object System.Collections.Generic.List[string]
    $objects.Add("<< /Type /Catalog /Pages 2 0 R >>")
    for ($i = 2; $i -le $Depth; $i++) {
        $objects.Add("<< /Type /Pages /Kids [$($i + 1) 0 R] /Count 1 >>")
    }
    $page = $Depth + 1
    $objects.Add("<< /Type /Pages /Kids [$($page + 1) 0 R] /Count 1 >>")
    $objects.Add("<< /Type /Page /Parent $page 0 R /MediaBox [0 0 612 792] >>")

    $body = New-Object System.Text.StringBuilder
    [void]$body.Append("%PDF-1.7`n%PDFXI deep page tree fixture`n")
    $offsets = New-Object System.Collections.Generic.List[long]
    $offsets.Add(0)
    for ($i = 1; $i -le $objects.Count; $i++) {
        $offsets.Add([long]$body.Length)
        [void]$body.Append("$i 0 obj`n$($objects[$i - 1])`nendobj`n")
    }
    $xref = [long]$body.Length
    [void]$body.Append("xref`n0 $($objects.Count + 1)`n")
    [void]$body.Append("0000000000 65535 f `n")
    for ($i = 1; $i -le $objects.Count; $i++) {
        [void]$body.Append(("{0:0000000000} 00000 n `n" -f $offsets[$i]))
    }
    [void]$body.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")
    [System.IO.File]::WriteAllText($Path, $body.ToString(), (New-Object System.Text.ASCIIEncoding))
}

# --- fixtures --------------------------------------------------------------
$excel = Join-Path $root 'fixtures\realworld\microsoft-excel-2019-sample.pdf'
$slides = Join-Path $root 'fixtures\realworld\powerpoint-slides.pdf'
$forms = Join-Path $FixtureDir 'ff-test-gui.pdf'
foreach ($required in @($excel, $slides, $forms)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        Write-SmokeFail "fixture missing: $required"
        Write-Output "UI SMOKE FAILED (fixtures)"
        exit 1
    }
}

$protected = Join-Path $OutDir 'protected.pdf'
$protectedResult = Invoke-CliResult -Arguments @('protect', '--out', $protected, '--password', 'test-password', '--owner', 'owner-password', $excel)
if ($protectedResult.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $protected)) {
    Write-SmokeFail "could not create protected fixture: $($protectedResult.Text.Trim())"
} else {
    Assert-Pdfium -Label 'protected fixture is encrypted' -Path $protected -Pattern 'securityRevision=[1-9]' -Password 'test-password' | Out-Null
}

$slidesCopy = Join-Path $OutDir 'slides-source.pdf'
$slidesRotated = Join-Path $OutDir 'slides-rotated.pdf'
Copy-Item -LiteralPath $slides -Destination $slidesCopy -Force
$rotateResult = Invoke-CliResult -Arguments @('pages', 'rotate', '--angle', '90', '--out', $slidesRotated, $slidesCopy)
if ($rotateResult.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $slidesRotated)) {
    Write-SmokeFail "could not rotate the slides fixture: $($rotateResult.Text.Trim())"
} else {
    Assert-Pdfium -Label 'rotation kept all three bookmarks' -Path $slidesRotated -Pattern 'bookmarks\(top\)=3' | Out-Null
    Assert-Pdfium -Label 'rotated fixture is at 90 degrees' -Path $slidesRotated -Pattern 'rotate=90' | Out-Null
}

$deep = Join-Path $OutDir 'deep-pages.pdf'
New-DeepPagesPdf -Path $deep -Depth 20000
$deepResult = Invoke-CliResult -Arguments @('info', $deep)
if ($deepResult.ExitCode -eq 0) {
    Write-SmokeFail 'deep page tree was accepted by the CLI'
} elseif ($deepResult.Text -notmatch 'nested deeper than 64') {
    Write-SmokeFail "deep page tree error was not typed: $($deepResult.Text.Trim())"
} else {
    Write-SmokePass 'CLI rejects the 20,000-deep page tree with a typed depth error'
}

# --- 1. shell and accessibility tree ---------------------------------------
try {
    Start-SmokeUi -Tag 'shell'
    Shot -Name '01-shell' | Out-Null
    Assert-NoUnnamedControls -Label 'shell'
    Close-SmokeUi -Tag 'shell'
} catch {
    Write-SmokeFail "shell session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- 2. text search over real page text ------------------------------------
try {
    Start-SmokeUi -Arguments @($excel) -Tag 'find'
    Assert-TreePattern -Label 'document opened for find' -Pattern 'microsoft-excel-2019-sample' | Out-Null
    Assert-NoUnnamedControls -Label 'document shell'
    Real-Chord -Vk 0x11, 0x46 | Out-Null
    if (Wait-TreePattern -Pattern 'Find:') {
        Real-Type -Text 'orchestra' | Out-Null
        Real-Chord -Vk 0x0D | Out-Null
        Assert-TreePattern -Label 'find reports one match for orchestra' -Pattern '1 of 1' | Out-Null
        Shot -Name '02-find' | Out-Null
    } else {
        Write-SmokeFail 'find bar did not open'
    }
    Close-SmokeUi -Tag 'find'
} catch {
    Write-SmokeFail "find session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- 3. password-protected document ----------------------------------------
try {
    Start-SmokeUi -Arguments @($protected) -Tag 'password'
    if (Assert-TreePattern -Label 'password prompt appeared' -Pattern 'Password required') {
        Real-Type -Text 'test-password' | Out-Null
        Real-Chord -Vk 0x0D | Out-Null
        Assert-TreePattern -Label 'correct password unlocks the document' -Pattern 'Document unlocked' | Out-Null
        Shot -Name '03-password' | Out-Null
    }
    Close-SmokeUi -Tag 'password'
} catch {
    Write-SmokeFail "password session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- 4. bookmarks survive rotation; dirty window close prompts -------------
try {
    Start-SmokeUi -Arguments @($slidesRotated) -Tag 'bookmarks'
    Assert-TreePattern -Label 'rotated document shows three bookmarks' -Pattern '3 bookmarks' | Out-Null
    Invoke-El -Name 'Organize Pages' -Type Button | Out-Null
    if (Wait-TreePattern -Pattern 'Organize Pages') {
        Invoke-El -Name 'Page 1' -Type Button | Out-Null
        Invoke-El -Name 'Rotate left' -Type Button | Out-Null
        Assert-TreePattern -Label 'rotation keeps three bookmarks in the panel' -Pattern '3 bookmarks' | Out-Null
        $handle = Get-UiWin
        [void][RvW]::PostMessage($handle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        if (Assert-TreePattern -Label 'dirty window close shows Unsaved changes' -Pattern 'Unsaved changes') {
            Shot -Name '04-close-prompt' | Out-Null
            Real-Chord -Vk 0x44 | Out-Null
            Start-Sleep -Milliseconds 1500
            if (Test-UiAlive) {
                Write-SmokeFail 'Discard did not close the viewer'
                @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
            } else {
                Write-SmokePass 'Discard closed the dirty viewer'
            }
        }
    } else {
        Write-SmokeFail 'Organize Pages window did not open'
    }
} catch {
    Write-SmokeFail "bookmarks session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- 5. deep page tree does not crash the viewer ---------------------------
try {
    Start-SmokeUi -Arguments @($deep) -Tag 'deep'
    Start-Sleep -Seconds 3
    if (Test-UiAlive) {
        Write-SmokePass 'viewer stays alive on the 20,000-deep page tree'
    } else {
        Write-SmokeFail 'viewer exited on the 20,000-deep page tree'
    }
    Shot -Name '05-deep' | Out-Null
    Close-SmokeUi -Tag 'deep'
} catch {
    Write-SmokeFail "deep session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- 6. form fields load and validate --------------------------------------
try {
    Start-SmokeUi -Arguments @($forms) -Tag 'forms'
    Assert-TreePattern -Label 'form fixture opened' -Pattern 'ff-test-gui' | Out-Null
    Invoke-El -Name 'Fill & Sign' -Type Button | Out-Null
    if (Assert-TreePattern -Label 'Fill & Sign lists readonly_ff1' -Pattern 'readonly_ff1') {
        Assert-TreePattern -Label 'Fill & Sign lists required_ff2' -Pattern 'required_ff2' | Out-Null
        Assert-TreePattern -Label 'Fill & Sign lists plain_ff0' -Pattern 'plain_ff0' | Out-Null
        Assert-NoUnnamedControls -Label 'forms shell'
        Shot -Name '06-forms' | Out-Null
    }
    $validate = Invoke-CliResult -Arguments @('form', 'validate', $forms)
    if ($validate.ExitCode -eq 2 -and $validate.Text -match "required field 'required_ff2' has no value") {
        Write-SmokePass 'form validate reads the Required bit on required_ff2'
    } else {
        Write-SmokeFail "form validate did not report required_ff2: $($validate.Text.Trim())"
    }
    Close-SmokeUi -Tag 'forms'
} catch {
    Write-SmokeFail "forms session: $($_.Exception.Message)"
    @(Get-UiProc) | Stop-Process -Force -ErrorAction SilentlyContinue
}

# --- summary ---------------------------------------------------------------
if ($script:Failures -gt 0) {
    Write-Output "UI SMOKE FAILED: passes=$($script:Passes) failures=$($script:Failures)"
    Write-Output "log: $($script:Log)"
    exit 1
}
Write-Output "UI SMOKE PASS: $($script:Passes) checks"
Write-Output "log: $($script:Log)"
exit 0
