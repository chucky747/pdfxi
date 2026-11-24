# Real screen-reader smoke for PDFXI (Windows, NVDA).
#
# Launches a real NVDA session with its Speech Viewer, starts the viewer on a
# real AcroForm fixture, drives keyboard focus through the shell, and reads the
# speech log back through UI Automation. It fails closed when NVDA is absent:
# there is no silent skip.
#
# Usage:
#   powershell -NoProfile -File scripts/screen-reader-smoke.ps1 `
#     -Exe "$env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe"
#
# NVDA resolution order: -NvdaPath, $env:PDFXI_NVDA, the portable copy under
# target\tools\nvda-portable, the installed Program Files locations, PATH.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$FixtureDir = '',
    [string]$OutDir = '',
    [string]$NvdaPath = '',
    [int]$WaitMs = 5000
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $FixtureDir) { $FixtureDir = Join-Path $root 'fixtures\realworld' }
if (-not $OutDir) { $OutDir = Join-Path $root 'target\tmp\screen-reader' }
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$fixture = Join-Path $FixtureDir 'pdfxi-forms-selfmade.pdf'
if (-not (Test-Path -LiteralPath $fixture -PathType Leaf)) { throw "fixture not found: $fixture" }

$log = Join-Path $OutDir 'screen-reader.log'
Remove-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue
$script:Passes = 0
$script:Failures = 0
function Write-Pass([string]$m) { $script:Passes++; $line = "PASS $m"; Write-Output $line; Add-Content -LiteralPath $log -Value $line }
function Write-Fail([string]$m) { $script:Failures++; $line = "FAIL $m"; Write-Output $line; Add-Content -LiteralPath $log -Value $line }

function Resolve-Nvda {
    param([string]$Override)
    $candidates = @()
    if ($Override) { $candidates += $Override }
    if ($env:PDFXI_NVDA) { $candidates += $env:PDFXI_NVDA }
    $candidates += @(
        (Join-Path $root 'target\tools\nvda-portable\nvda.exe'),
        (Join-Path $env:ProgramFiles 'NVDA\nvda.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'NVDA\nvda.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\NVDA\nvda.exe')
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) { return (Resolve-Path -LiteralPath $candidate).Path }
    }
    $command = Get-Command nvda.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

$nvda = Resolve-Nvda $NvdaPath
if (-not $nvda) {
    Write-Fail 'NVDA not found: install NVDA or set PDFXI_NVDA / -NvdaPath'
    Write-Output 'SCREEN READER SMOKE BLOCKED: NVDA missing'
    exit 2
}
Write-Output "NVDA $nvda"

# NVDA keeps its configuration under --config-path. A dedicated directory keeps
# the developer's real configuration untouched and pins the Speech Viewer on.
$configDir = Join-Path $OutDir 'nvda-config'
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
$ini = @"
schemaVersion = 25
[speechViewer]
showSpeechViewerAtStartup = true
[general]
showWelcomeDialogAtStartup = false
[update]
autoCheck = false
startupNotification = false
allowUsageStats = false
askedAllowUsageStats = true
"@
[System.IO.File]::WriteAllText((Join-Path $configDir 'nvda.ini'), $ini, (New-Object System.Text.UTF8Encoding($false)))

# Child processes write under OutDir, avoiding host %TEMP% restrictions.
$env:TEMP = Join-Path $OutDir 'temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null

# A second NVDA instance cannot share the speech viewer. Stop only an NVDA
# process launched from the same portable directory; fail for any other one.
$nvdaDir = Split-Path $nvda -Parent
Get-Process -Name nvda -ErrorAction SilentlyContinue | ForEach-Object {
    if ($_.Path -and $_.Path.StartsWith($nvdaDir, [System.StringComparison]::OrdinalIgnoreCase)) {
        Stop-Process -Id $_.Id -Force
    } else {
        throw "another NVDA instance is running ($($_.Path)); close it before the smoke"
    }
}
Start-Sleep -Milliseconds 500

. (Join-Path $PSScriptRoot 'ui-harness.ps1')
$script:RvUi = $Exe
$script:RvDir = $OutDir
$script:PdfiumDll = Join-Path (Split-Path $Exe -Parent) 'pdfium.dll'

Add-Type -AssemblyName UIAutomationClient

function Get-SpeechViewer {
    $rootEl = [System.Windows.Automation.AutomationElement]::RootElement
    return $rootEl.FindFirst(
        [System.Windows.Automation.TreeScope]::Children,
        (New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'NVDA Speech Viewer')))
}

function Read-Speech {
    param([int]$Retries = 10)
    for ($attempt = 1; $attempt -le $Retries; $attempt++) {
        try {
            $viewer = Get-SpeechViewer
            if ($viewer) {
                $doc = $viewer.FindFirst(
                    [System.Windows.Automation.TreeScope]::Descendants,
                    (New-Object System.Windows.Automation.PropertyCondition(
                        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [System.Windows.Automation.ControlType]::Document)))
                if ($doc) {
                    $pattern = $null
                    if ($doc.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) {
                        return $pattern.Current.Value
                    }
                }
            }
        } catch { }
        Start-Sleep -Milliseconds 300
    }
    throw 'NVDA Speech Viewer document is not readable'
}

function Find-AppElement {
    param([Parameter(Mandatory = $true)][string]$Name)
    $condition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    return (Get-UiRoot).FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}

function Assert-Speech {
    param([string]$Label, [string]$Text, [string]$Pattern)
    $normalized = ($Text -replace '\s+', ' ')
    if ($normalized -match $Pattern) {
        Write-Pass $Label
    } else {
        Write-Fail "$Label (pattern '$Pattern' missing)"
    }
}

function Focus-Element {
    param([string]$Name)
    $element = Find-AppElement $Name
    if (-not $element) { Write-Fail "element not found in the accessibility tree: $Name"; return $false }
    try { $element.SetFocus() } catch { Write-Fail "cannot focus $Name : $($_.Exception.Message)"; return $false }
    Start-Sleep -Milliseconds 700
    return $true
}

$nvdaProc = $null
$viewerStarted = $false
try {
    $nvdaProc = Start-Process -FilePath $nvda -ArgumentList "--config-path=$configDir" -PassThru
    Start-Sleep -Seconds 8
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline -and -not (Get-SpeechViewer)) { Start-Sleep -Milliseconds 500 }
    if (-not (Get-SpeechViewer)) { throw 'NVDA Speech Viewer did not open' }
    Write-Pass 'NVDA Speech Viewer is running'

    Start-Ui -ArgList @($fixture) -WaitMs $WaitMs -Width 1600 -Height 1000 | Out-Null
    $viewerStarted = $true
    Start-Sleep -Seconds 4
    Write-Pass 'viewer opened the AcroForm fixture'

    $hwnd = Get-UiWin
    $started = Read-Speech
    $baseline = $started.Length

    if (-not (Enter-Real $hwnd)) { throw 'viewer did not become the foreground window' }
    for ($i = 1; $i -le 35; $i++) {
        [RvW]::Key([uint16]0x09, $true)
        Start-Sleep -Milliseconds 40
        [RvW]::Key([uint16]0x09, $false)
        Start-Sleep -Milliseconds 350
    }
    Start-Sleep -Seconds 1
    $toolbarSpeech = Read-Speech
    Exit-Real

    Assert-Speech 'NVDA announces the application window' $toolbarSpeech 'PDFXI\s+window'
    Assert-Speech 'NVDA announces the Open button' $toolbarSpeech 'Open\s+button'
    Assert-Speech 'NVDA announces the Print button' $toolbarSpeech 'Print\s+button'
    Assert-Speech 'NVDA announces the Fit button' $toolbarSpeech 'Fit\s+button'
    Assert-Speech 'NVDA announces the Pages rail' $toolbarSpeech 'Pages\s+button'
    Assert-Speech 'NVDA announces the comments rail' $toolbarSpeech 'Comments\s+button'

    if (Focus-Element 'Page number') {
        $pageSpeech = Read-Speech
        Assert-Speech 'NVDA reads the page number field' $pageSpeech 'Page number\s+edit\s+1'
        if (Focus-Element 'Next page') {
            Assert-Speech 'NVDA reads the next-page control' (Read-Speech) 'Next page\s+button'
        }
    }

    if (Focus-Element 'Fill & Sign') {
        [RvW]::Key([uint16]0x0D, $true)
        Start-Sleep -Milliseconds 60
        [RvW]::Key([uint16]0x0D, $false)
        Start-Sleep -Seconds 2
        $formSpeech = Read-Speech
        Assert-Speech 'NVDA announces the Fill & Sign control' $formSpeech 'Fill & Sign\s+button'
    }

    if (Focus-Element 'Form field text-field') {
        Assert-Speech 'NVDA reads the text field name and value' (Read-Speech) 'Form field text-field\s+edit\s+typed value'
    }
    if (Focus-Element 'Form field checkbox-field') {
        Assert-Speech 'NVDA reads the checkbox role and state' (Read-Speech) 'Form field checkbox-field\s+check box\s+checked'
    }
    if (Focus-Element 'Form field choice-field') {
        Assert-Speech 'NVDA reads the choice role and value' (Read-Speech) 'Form field choice-field\s+combo box\s+two'
    }

    $final = Read-Speech
    Add-Content -LiteralPath $log -Value "speech_bytes=$($final.Length) baseline_bytes=$baseline"
    Shot -Name 'screen-reader' | Out-Null
} catch {
    Write-Fail "screen-reader session: $($_.Exception.Message)"
} finally {
    if ($viewerStarted) { Stop-Ui -WaitMs 4000 | Out-Null }
    if ($nvdaProc -and -not $nvdaProc.HasExited) { Stop-Process -Id $nvdaProc.Id -Force -ErrorAction SilentlyContinue }
    Get-Process -Name nvda,nvda_slave -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($nvdaDir, [System.StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force -ErrorAction SilentlyContinue
}

if ($script:Failures -gt 0) {
    Write-Output "SCREEN READER SMOKE FAILED: passes=$($script:Passes) failures=$($script:Failures)"
    Write-Output "log: $log"
    exit 1
}
Write-Output "SCREEN READER SMOKE PASS: $($script:Passes) checks"
Write-Output "log: $log"
exit 0
