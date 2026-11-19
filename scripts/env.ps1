param(
    [string]$Lane = 'orchestrator'
)

$root = Split-Path $PSScriptRoot -Parent
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:LOCALAPPDATA\Programs\winlibs\bin;$env:LOCALAPPDATA\Programs\MinGit\cmd;$env:Path"
$env:CARGO_INCREMENTAL = '1'
$env:CARGO_TERM_COLOR = 'never'
if ($Lane -and $Lane -ne 'orchestrator' -and $Lane -ne 'integrator') {
    $env:CARGO_TARGET_DIR = Join-Path $root "target\lanes\$Lane"
} else {
    $env:CARGO_TARGET_DIR = Join-Path $root 'target\integration'
}

Write-Output "PDFXI lane=$Lane"
Write-Output "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"
