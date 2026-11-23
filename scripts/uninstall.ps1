# Remove the per-user PDFXI installation (Windows).
#
# This is the supported recovery path when an older per-user installer left a
# broken uninstall entry, and a normal way to remove a portable install. It
# only touches the explicit per-user target, the matching HKCU uninstall key
# and shortcuts that point into that target. User settings under
# %APPDATA%\PDFXI are kept unless -RemoveSettings is given.
#
# Usage:
#   powershell -NoProfile -File scripts/uninstall.ps1
#   powershell -NoProfile -File scripts/uninstall.ps1 -RemoveSettings

[CmdletBinding()]
param(
    [string]$TargetDir = '',
    [switch]$RemoveSettings
)

$ErrorActionPreference = 'Stop'
if (-not $TargetDir) { $TargetDir = Join-Path $env:LOCALAPPDATA 'Programs\PDFXI' }
$target = [System.IO.Path]::GetFullPath($TargetDir)
$expected = [System.IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs\PDFXI'))
if (-not $target.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "refusing to remove '$target'; the per-user target is '$expected'"
}
if (-not (Test-Path -LiteralPath (Join-Path $target 'pdfxi-ui.exe') -PathType Leaf)) {
    throw "refusing to remove '$target'; it does not contain pdfxi-ui.exe"
}

$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{4C1F0A46-8E5B-4D2A-9C77-2B6A1E9F3D10}_is1'
if (Test-Path $key) {
    $entry = Get-ItemProperty -LiteralPath $key -ErrorAction SilentlyContinue
    $entryTarget = if ($entry.InstallLocation) {
        ([System.IO.Path]::GetFullPath($entry.InstallLocation)).TrimEnd('\', '/')
    } else {
        ''
    }
    if ($entryTarget -and -not $entryTarget.Equals($target.TrimEnd('\', '/'), [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to remove an uninstall entry that points at $($entry.InstallLocation)"
    }
    Remove-Item -LiteralPath $key -Recurse -Force
    "REMOVED $key"
}

$desktop = [Environment]::GetFolderPath('Desktop')
$programs = [Environment]::GetFolderPath('Programs')
$shell = New-Object -ComObject WScript.Shell
foreach ($link in @(
    (Join-Path $desktop 'PDFXI.lnk'),
    (Join-Path $programs 'PDFXI.lnk'),
    (Join-Path $programs 'PDFXI CLI.lnk'),
    (Join-Path $programs 'PDFXI\PDFXI.lnk'),
    (Join-Path $programs 'PDFXI\PDFXI CLI.lnk')
)) {
    if (-not (Test-Path -LiteralPath $link -PathType Leaf)) { continue }
    $shortcut = $shell.CreateShortcut($link)
    $linkTarget = if ($shortcut.TargetPath) { [System.IO.Path]::GetFullPath($shortcut.TargetPath) } else { '' }
    if ($linkTarget.StartsWith($target, [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $link -Force
        "REMOVED $link"
    }
}
foreach ($group in @((Join-Path $programs 'PDFXI'), (Join-Path $programs 'PDFXI Smoke'))) {
    if ((Test-Path -LiteralPath $group -PathType Container) -and
        ((Get-ChildItem -LiteralPath $group -File -ErrorAction SilentlyContinue | Measure-Object).Count -eq 0)) {
        Remove-Item -LiteralPath $group -Force
        "REMOVED $group"
    }
}

if (Test-Path -LiteralPath $target) {
    Remove-Item -LiteralPath $target -Recurse -Force
    "REMOVED $target"
}

if ($RemoveSettings) {
    $settings = Join-Path $env:APPDATA 'PDFXI'
    if (Test-Path -LiteralPath $settings) {
        Remove-Item -LiteralPath $settings -Recurse -Force
        "REMOVED $settings"
    }
}
"UNINSTALL OK"
