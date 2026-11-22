<#
.SYNOPSIS
User-scope installer for the PDFXI portable package.

.DESCRIPTION
Copies the portable payload to a caller-supplied directory and registers a
Start Menu shortcut in the CURRENT USER's Programs folder. It never elevates,
never touches the network, and copies the bundled PDFium DLL only from the payload.
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

function Test-SamePath {
    param([string]$Left, [string]$Right)
    return [string]::Equals(
        [System.IO.Path]::GetFullPath($Left),
        [System.IO.Path]::GetFullPath($Right),
        [System.StringComparison]::OrdinalIgnoreCase)
}

# Explicit allowlist: install only what the portable archive ships. The guide
# and manifest are handled below; install.ps1 is part of the archive and must
# be copied too, or the installed tree fails its own manifest.
$payload = @(
    'pdfxi.exe', 'pdfxi-ui.exe', 'pdfium.dll', 'LICENSE', 'NOTICE', 'README.md',
    'THIRD_PARTY.md', 'deny.toml', 'install.ps1', 'manifest.json'
)
foreach ($name in $payload) {
    $src = Join-Path $source $name
    $dest = Join-Path $target $name
    if ((Test-Path -LiteralPath $src -PathType Leaf) -and -not (Test-SamePath $src $dest)) {
        Copy-Item -LiteralPath $src -Destination $dest -Force
    }
}
# PDFium's own notices ship in pdfium-licenses\ next to the binaries.
$licenceSource = Join-Path $source 'pdfium-licenses'
if (Test-Path -LiteralPath $licenceSource -PathType Container) {
    $licenceTarget = Join-Path $target 'pdfium-licenses'
    if (-not (Test-SamePath $licenceSource $licenceTarget)) {
        New-Item -ItemType Directory -Force -Path $licenceTarget | Out-Null
        foreach ($file in @(Get-ChildItem -LiteralPath $licenceSource -File)) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $licenceTarget $file.Name) -Force
        }
    }
}
# The user guide ships under docs\ so Help can open it from the install directory.
# Copy the contents (not the directory itself) so an upgrade merges over the
# previous guide instead of nesting a second docs\docs copy.
$docsSource = Join-Path $source 'docs'
if (Test-Path -LiteralPath $docsSource -PathType Container) {
    $docsTarget = Join-Path $target 'docs'
    if (-not (Test-SamePath $docsSource $docsTarget)) {
        New-Item -ItemType Directory -Force -Path $docsTarget | Out-Null
        Copy-Item -Path (Join-Path $docsSource '*') -Destination $docsTarget -Recurse -Force
    }
    # Older installers created a duplicate under docs\docs; remove it so the
    # viewer (which serves <install>\docs) always sees the current guide.
    $nested = Join-Path $docsTarget 'docs'
    if (Test-Path -LiteralPath $nested -PathType Container) {
        Remove-Item -LiteralPath $nested -Recurse -Force
    }
}

function New-ShellLink {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string]$Description
    )
    if (-not (Test-Path -LiteralPath $Directory)) {
        New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    }
    $link = Join-Path $Directory $FileName
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($link)
    $shortcut.TargetPath = (Resolve-Path -LiteralPath $Executable).Path
    $shortcut.WorkingDirectory = (Resolve-Path -LiteralPath $WorkingDirectory).Path
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
            -Executable $installedUi -WorkingDirectory $target -Description 'PDFXI viewer'
    } else {
        New-ShellLink -Directory $programs -FileName 'PDFXI.lnk' `
            -Executable (Join-Path $target 'pdfxi.exe') -WorkingDirectory $target -Description 'PDFXI portable CLI'
    }
    New-ShellLink -Directory $programs -FileName 'PDFXI CLI.lnk' `
        -Executable (Join-Path $target 'pdfxi.exe') -WorkingDirectory $target -Description 'PDFXI portable CLI'

    if ($DesktopShortcut) {
        $desktop = [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)
        if (Test-Path -LiteralPath $installedUi -PathType Leaf) {
            New-ShellLink -Directory $desktop -FileName 'PDFXI.lnk' `
                -Executable $installedUi -WorkingDirectory $target -Description 'PDFXI viewer'
        }
    }
}

Write-Output "INSTALLED $target"