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
    [switch]$NoShortcut,
    [switch]$DesktopShortcut
)

$ErrorActionPreference = 'Stop'

$source = $PSScriptRoot
$exe = Join-Path $source 'pdfxi.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "portable payload is incomplete: $exe is missing"
}
$uiExe = Join-Path $source 'pdfxi-ui.exe'

$target = [System.IO.Path]::GetFullPath($TargetDir)
New-Item -ItemType Directory -Force -Path $target | Out-Null

# Explicit allowlist: install only what the portable archive ships.
$payload = @(
    'pdfxi.exe', 'pdfxi-ui.exe', 'LICENSE', 'NOTICE', 'README.md',
    'THIRD_PARTY.md', 'deny.toml', 'manifest.json'
)
foreach ($name in $payload) {
    $src = Join-Path $source $name
    if (Test-Path -LiteralPath $src -PathType Leaf) {
        Copy-Item -LiteralPath $src -Destination (Join-Path $target $name) -Force
    }
}

function New-ShellLink {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string]$Description
    )
    if (-not (Test-Path -LiteralPath $Directory)) {
        New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    }
    $link = Join-Path $Directory $FileName
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($link)
    $shortcut.TargetPath = (Resolve-Path -LiteralPath $Executable).Path
    $shortcut.WorkingDirectory = (Resolve-Path -LiteralPath $target).Path
    $shortcut.Description = $Description
    $shortcut.Save()
    Write-Output "SHORTCUT $link"
}

if (-not $NoShortcut) {
    # Environment.SpecialFolder.Programs is the per-user Start Menu Programs
    # folder, so no administrator token is required.
    $programs = [Environment]::GetFolderPath([Environment+SpecialFolder]::Programs)
    $installedUi = Join-Path $target 'pdfxi-ui.exe'
    if (Test-Path -LiteralPath $installedUi -PathType Leaf) {
        New-ShellLink -Directory $programs -FileName 'PDFXI.lnk' `
            -Executable $installedUi -Description 'PDFXI viewer'
    } else {
        New-ShellLink -Directory $programs -FileName 'PDFXI.lnk' `
            -Executable (Join-Path $target 'pdfxi.exe') -Description 'PDFXI portable CLI'
    }
    New-ShellLink -Directory $programs -FileName 'PDFXI CLI.lnk' `
        -Executable (Join-Path $target 'pdfxi.exe') -Description 'PDFXI portable CLI'

    if ($DesktopShortcut) {
        $desktop = [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)
        if (Test-Path -LiteralPath $installedUi -PathType Leaf) {
            New-ShellLink -Directory $desktop -FileName 'PDFXI.lnk' `
                -Executable $installedUi -Description 'PDFXI viewer'
        }
    }
}

Write-Output "INSTALLED $target"