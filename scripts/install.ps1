<#
.SYNOPSIS
User-scope installer for the PDFXI portable package.

.DESCRIPTION
Copies the portable payload to a caller-supplied directory and registers a
Start Menu shortcut in the CURRENT USER's Programs folder. It never elevates,
never touches the network, and never copies an Adobe asset or a PDFium DLL.
Run it from the extracted portable archive.

.EXAMPLE
powershell -NoProfile -File install.ps1 -TargetDir "$env:LOCALAPPDATA\Programs\PDFXI"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TargetDir,
    [switch]$NoShortcut
)

$ErrorActionPreference = 'Stop'

$source = $PSScriptRoot
$exe = Join-Path $source 'pdfxi.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "portable payload is incomplete: $exe is missing"
}

$target = [System.IO.Path]::GetFullPath($TargetDir)
New-Item -ItemType Directory -Force -Path $target | Out-Null

# Explicit allowlist: install only what the portable archive ships.
$payload = @(
    'pdfxi.exe', 'LICENSE', 'NOTICE', 'README.md',
    'THIRD_PARTY.md', 'deny.toml', 'manifest.json'
)
foreach ($name in $payload) {
    $src = Join-Path $source $name
    if (Test-Path -LiteralPath $src -PathType Leaf) {
        Copy-Item -LiteralPath $src -Destination (Join-Path $target $name) -Force
    }
}

if (-not $NoShortcut) {
    # Environment.SpecialFolder.Programs is the per-user Start Menu Programs
    # folder, so no administrator token is required.
    $programs = [Environment]::GetFolderPath([Environment+SpecialFolder]::Programs)
    if (-not (Test-Path -LiteralPath $programs)) {
        New-Item -ItemType Directory -Force -Path $programs | Out-Null
    }
    $link = Join-Path $programs 'PDFXI.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($link)
    $shortcut.TargetPath = (Resolve-Path -LiteralPath (Join-Path $target 'pdfxi.exe')).Path
    $shortcut.WorkingDirectory = (Resolve-Path -LiteralPath $target).Path
    $shortcut.Description = 'PDFXI portable CLI'
    $shortcut.Save()
    Write-Output "SHORTCUT $link"
}

Write-Output "INSTALLED $target"