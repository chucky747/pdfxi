<#
.SYNOPSIS
Automated test for the per-user Inno Setup installer.

.DESCRIPTION
Builds the Smoke-identity installer (a separate AppId and AppName, so a real
installation is never touched), installs it silently into a folder under
target\tmp, asserts the payload, the HKCU uninstall entry and the running viewer
(GUI subsystem, pdfium.dll loaded from the install directory), then uninstalls
and asserts that the directory, the registry key and the smoke shortcuts are
gone while the real installation and the real desktop shortcut are untouched.

When Inno Setup (ISCC.exe) is not installed the script prints a loud
SKIPPED line and exits 0, so a CI host without Inno Setup does not fail.

Exit codes: 0 pass or skip, 1 failed assertion.

.PARAMETER TargetDir
Cargo target directory holding release\pdfxi.exe and release\pdfxi-ui.exe.
Defaults to CARGO_TARGET_DIR, then target\integration.

.PARAMETER SkipBuild
Reuse the existing release binaries instead of running a release build.

.PARAMETER StagingDir
Existing package staging directory to install. When omitted, scripts/package.ps1
produces a fresh one under target\tmp\installer-test\package.

.PARAMETER PdfiumDir
PDFium payload directory (bin\pdfium.dll). Defaults to the package.ps1 lookup.

.PARAMETER WorkDir
Scratch root. Defaults to target\tmp\installer-test. Everything the test creates
lives below it, except the smoke identity registry entry and shortcuts, which
the uninstall must remove.
#>
[CmdletBinding()]
param(
    [string]$TargetDir = '',
    [string]$StagingDir = '',
    [string]$PdfiumDir = '',
    [string]$WorkDir = '',
    [switch]$SkipBuild,
    [int]$ViewerSeconds = 5
)

$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$script:Passes = 0
$script:Failures = 0

function Write-Pass {
    param([string]$Message)
    $script:Passes++
    Write-Output "PASS $Message"
}

function Write-Fail {
    param([string]$Message)
    $script:Failures++
    Write-Output "FAIL $Message"
}

function Resolve-Iscc {
    if ($env:ISCC_PATH -and (Test-Path -LiteralPath $env:ISCC_PATH -PathType Leaf)) {
        return $env:ISCC_PATH
    }
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $bases = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:LOCALAPPDATA) | Where-Object { $_ }
    foreach ($base in $bases) {
        foreach ($relative in @('Inno Setup 6\ISCC.exe', 'Programs\Inno Setup 6\ISCC.exe', 'Inno Setup 5\ISCC.exe')) {
            $candidate = Join-Path $base $relative
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
        }
    }
    return ''
}

# PE optional-header Subsystem: 2 is WINDOWS_GUI, 3 is CONSOLE.
function Get-PeSubsystem {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $reader = New-Object System.IO.BinaryReader($stream)
        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) { return -1 }
        $stream.Position = $peOffset + 92
        return [int]$reader.ReadUInt16()
    } finally {
        $stream.Dispose()
    }
}

# Identity of a directory tree: relative path, size and last-write time per file.
function Get-TreeFingerprint {
    param([Parameter(Mandatory = $true)][string]$Directory)
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { return 'absent' }
    $base = (Resolve-Path -LiteralPath $Directory).Path.TrimEnd([char[]]@('\', '/'))
    $lines = Get-ChildItem -LiteralPath $base -Recurse -File |
        Sort-Object -Property FullName |
        ForEach-Object { "{0}|{1}|{2}" -f $_.FullName.Substring($base.Length), $_.Length, $_.LastWriteTimeUtc.Ticks }
    return ($lines -join "`n")
}

function Get-PathFingerprint {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return 'absent' }
    $item = Get-Item -LiteralPath $Path
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    return ("{0}|{1}|{2}" -f $item.Length, $item.LastWriteTimeUtc.Ticks, $hash)
}

function Test-RegistryKey {
    param([Parameter(Mandatory = $true)][string]$Key)
    return Test-Path -LiteralPath ("Registry::HKEY_CURRENT_USER\" + $Key)
}

function Remove-RegistryKeyIfPresent {
    param([Parameter(Mandatory = $true)][string]$Key)
    $path = "Registry::HKEY_CURRENT_USER\" + $Key
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
}

function Wait-Until {
    param([Parameter(Mandatory = $true)][scriptblock]$Condition,
          [int]$Seconds = 60)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return [bool](& $Condition)
}

# --- preflight -------------------------------------------------------------
$iscc = Resolve-Iscc
if (-not $iscc) {
    Write-Output ('*' * 78)
    Write-Output 'SKIPPED: Inno Setup (ISCC.exe) not found; the installer was not tested.'
    Write-Output 'SKIPPED: install Inno Setup 6 (winget install --id JRSoftware.InnoSetup -e)'
    Write-Output 'SKIPPED: or set ISCC_PATH, then re-run cargo xtask installer-test.'
    Write-Output ('*' * 78)
    exit 0
}
Write-Output "ISCC $iscc"

$resolvedTarget = if ($TargetDir) {
    [System.IO.Path]::GetFullPath($TargetDir)
} elseif ($env:CARGO_TARGET_DIR) {
    [System.IO.Path]::GetFullPath((Join-Path $root $env:CARGO_TARGET_DIR))
} else {
    Join-Path $root 'target\integration'
}
Write-Output "TARGET $resolvedTarget"

$work = if ($WorkDir) { [System.IO.Path]::GetFullPath($WorkDir) } else { Join-Path $root 'target\tmp\installer-test' }
$temp = Join-Path $work 'temp'
$packageRoot = Join-Path $work 'package'
$smokeRoot = Join-Path $work 'smoke'
$installDir = Join-Path $work 'install'
New-Item -ItemType Directory -Force -Path $temp, $packageRoot, $smokeRoot | Out-Null
# Restricted agent sessions cannot execute helpers from the default temp folder;
# every child process inherits these.
$env:TEMP = $temp
$env:TMP = $temp
Write-Output "TEMP $env:TEMP"

$fixture = Join-Path $root 'fixtures\realworld\microsoft-excel-2019-sample.pdf'
$realInstall = Join-Path $env:LOCALAPPDATA 'Programs\PDFXI'
$desktop = [Environment]::GetFolderPath('Desktop')
$programs = [Environment]::GetFolderPath('Programs')
$realShortcut = Join-Path $desktop 'PDFXI.lnk'
$smokeShortcut = Join-Path $desktop 'PDFXI Smoke.lnk'
$smokeGroup = Join-Path $programs 'PDFXI Smoke'
$smokeKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{9E1B7C52-6B4A-4E3D-9F27-9A6D0C1C7A21}_is1'

if (-not (Test-Path -LiteralPath $fixture -PathType Leaf)) {
    Write-Fail "fixture missing: $fixture"
    Write-Output 'INSTALLER TEST FAILED (preflight)'
    exit 1
}

# A leftover smoke installation from an aborted run is ours to clear.
if (Test-Path -LiteralPath $installDir) { Remove-Item -LiteralPath $installDir -Recurse -Force }
Remove-RegistryKeyIfPresent $smokeKey
foreach ($stale in @($smokeShortcut, $smokeGroup)) {
    if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Recurse -Force }
}

$realInstallBefore = Get-TreeFingerprint $realInstall
$realShortcutBefore = Get-PathFingerprint $realShortcut
Write-Output ("REAL install fingerprint before: {0} bytes of listing" -f $realInstallBefore.Length)
Write-Output ("REAL shortcut fingerprint before: {0}" -f $realShortcutBefore)
# --- staging ---------------------------------------------------------------
if ($StagingDir) {
    $staging = [System.IO.Path]::GetFullPath($StagingDir)
    Write-Output "STAGING (given) $staging"
} else {
    $packageArgs = @('-TargetDir', $resolvedTarget, '-OutputDir', $packageRoot)
    if ($SkipBuild) { $packageArgs += '-SkipBuild' }
    if ($PdfiumDir) { $packageArgs += @('-PdfiumDir', $PdfiumDir) }
    Write-Output ("PACKAGE scripts/package.ps1 {0}" -f ($packageArgs -join ' '))
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts\package.ps1') @packageArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "scripts/package.ps1 exited $LASTEXITCODE; no staging to test"
        Write-Output 'INSTALLER TEST FAILED (packaging)'
        exit 1
    }
    $staging = Join-Path $packageRoot 'staging'
}
foreach ($required in @('pdfxi-ui.exe', 'pdfium.dll', 'manifest.json')) {
    if (-not (Test-Path -LiteralPath (Join-Path $staging $required) -PathType Leaf)) {
        Write-Fail "staging is missing $required : $staging"
        Write-Output 'INSTALLER TEST FAILED (staging)'
        exit 1
    }
}
$version = (Get-Content -LiteralPath (Join-Path $staging 'manifest.json') -Raw | ConvertFrom-Json).version
Write-Output "STAGING $staging version=$version"

# --- smoke-identity installer ---------------------------------------------
$iss = Join-Path $root 'installer\pdfxi.iss'
$icon = Join-Path $root 'crates\pdfxi-app\assets\pdfxi.ico'
$isccArgs = @(
    "/DAppVersion=$version",
    "/DStageDir=$staging",
    "/DOutputDir=$smokeRoot",
    "/DSourceIcon=$icon",
    '/DSmoke=1',
    $iss
)
Write-Output ("ISCC {0} {1}" -f $iscc, ($isccArgs -join ' '))
& $iscc @isccArgs
if ($LASTEXITCODE -ne 0) {
    Write-Fail "ISCC exited $LASTEXITCODE; the smoke installer was not built"
    Write-Output 'INSTALLER TEST FAILED (build)'
    exit 1
}
$setup = Join-Path $smokeRoot ("pdfxi-setup-{0}-windows-x86_64.exe" -f $version)
if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
    Write-Fail "ISCC reported success but $setup is missing"
    Write-Output 'INSTALLER TEST FAILED (build)'
    exit 1
}
$setupHash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash
Write-Output "SETUP $setup"
Write-Output "SETUP SHA256 $setupHash"

# --- silent install --------------------------------------------------------
$installLog = Join-Path $work 'install.log'
$installArgs = @(
    '/VERYSILENT',
    '/SUPPRESSMSGBOXES',
    '/CURRENTUSER',
    '/NORESTART',
    "/DIR=$installDir",
    '/TASKS=desktopicon',
    "/LOG=$installLog"
)
Write-Output ("INSTALL {0} {1}" -f $setup, ($installArgs -join ' '))
$setupProcess = Start-Process -FilePath $setup -ArgumentList $installArgs -Wait -PassThru
if ($setupProcess.ExitCode -ne 0) {
    Write-Fail "setup exited $($setupProcess.ExitCode) (log: $installLog)"
} else {
    Write-Pass "setup exited 0"
}

foreach ($relative in @(
    'pdfxi-ui.exe',
    'pdfxi.exe',
    'pdfium.dll',
    'LICENSE',
    'NOTICE',
    'THIRD_PARTY.md',
    'pdfium-licenses\LICENSE.txt',
    'unins000.exe'
)) {
    $full = Join-Path $installDir $relative
    if (Test-Path -LiteralPath $full -PathType Leaf) {
        Write-Pass "installed $relative"
    } else {
        Write-Fail "installed payload missing $relative"
    }
}

$installedViewer = Join-Path $installDir 'pdfxi-ui.exe'
if (Test-Path -LiteralPath $installedViewer -PathType Leaf) {
    $subsystem = Get-PeSubsystem $installedViewer
    if ($subsystem -eq 2) {
        Write-Pass 'installed pdfxi-ui.exe is a GUI-subsystem image (subsystem 2)'
    } else {
        Write-Fail "installed pdfxi-ui.exe has PE subsystem $subsystem, expected 2"
    }
    $installedHash = (Get-FileHash -LiteralPath $installedViewer -Algorithm SHA256).Hash
    $stagedHash = (Get-FileHash -LiteralPath (Join-Path $staging 'pdfxi-ui.exe') -Algorithm SHA256).Hash
    if ($installedHash -eq $stagedHash) {
        Write-Pass 'installed viewer bytes equal the staged viewer'
    } else {
        Write-Fail "installed viewer SHA256 $installedHash != staged $stagedHash"
    }
}

if (Test-RegistryKey $smokeKey) {
    $displayName = (Get-ItemProperty -LiteralPath ("Registry::HKEY_CURRENT_USER\" + $smokeKey) -Name DisplayName -ErrorAction SilentlyContinue).DisplayName
    if ($displayName -eq "PDFXI Smoke $version") {
        Write-Pass "HKCU uninstall entry present (DisplayName=PDFXI Smoke $version)"
    } else {
        Write-Fail "HKCU uninstall entry has DisplayName '$displayName', expected 'PDFXI Smoke $version'"
    }
} else {
    Write-Fail "HKCU uninstall entry missing: $smokeKey"
}

if (Test-Path -LiteralPath $smokeShortcut -PathType Leaf) {
    Write-Pass "smoke desktop shortcut created: $smokeShortcut"
} else {
    Write-Fail "smoke desktop shortcut missing: $smokeShortcut"
}

# --- launch the installed viewer ------------------------------------------
if (Test-Path -LiteralPath $installedViewer -PathType Leaf) {
    $viewerProcess = Start-Process -FilePath $installedViewer -ArgumentList @("`"$fixture`"") -WorkingDirectory $installDir -PassThru
    Start-Sleep -Seconds $ViewerSeconds
    $viewerProcess.Refresh()
    if ($viewerProcess.HasExited) {
        Write-Fail "installed viewer exited after ${ViewerSeconds}s with code $($viewerProcess.ExitCode)"
    } else {
        Write-Pass "installed viewer alive after ${ViewerSeconds}s with a document open"
        $loaded = @($viewerProcess.Modules | Where-Object { $_.ModuleName -eq 'pdfium.dll' })
        if ($loaded.Count -eq 0) {
            Write-Fail 'installed viewer has no pdfium.dll module loaded'
        } else {
            $loadedPath = [System.IO.Path]::GetFullPath($loaded[0].FileName)
            $expectedPath = [System.IO.Path]::GetFullPath((Join-Path $installDir 'pdfium.dll'))
            if ($loadedPath.Equals($expectedPath, [System.StringComparison]::OrdinalIgnoreCase)) {
                Write-Pass "pdfium.dll loaded from the install directory: $loadedPath"
            } else {
                Write-Fail "pdfium.dll loaded from $loadedPath, expected $expectedPath"
            }
        }
    }
    if (-not $viewerProcess.HasExited) {
        $viewerProcess.Kill()
        $viewerProcess.WaitForExit(10000) | Out-Null
    }
}

# --- silent uninstall ------------------------------------------------------
$uninstaller = Join-Path $installDir 'unins000.exe'
if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
    Write-Output "UNINSTALL $uninstaller /VERYSILENT /SUPPRESSMSGBOXES /NORESTART"
    $uninstallProcess = Start-Process -FilePath $uninstaller -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru
    if ($uninstallProcess.ExitCode -ne 0) {
        Write-Fail "uninstaller exited $($uninstallProcess.ExitCode)"
    } else {
        Write-Pass 'uninstaller exited 0'
    }
} else {
    Write-Fail 'unins000.exe not found; cannot exercise uninstall'
}
if (-not (Wait-Until -Condition { -not (Test-RegistryKey $smokeKey) } -Seconds 60)) {
    Write-Fail "HKCU uninstall entry still present after 60s: $smokeKey"
}

if (Test-Path -LiteralPath $installDir) {
    $left = @(Get-ChildItem -LiteralPath $installDir -Recurse -File -ErrorAction SilentlyContinue)
    if ($left.Count -eq 0 -and -not (Wait-Until -Condition { -not (Test-Path -LiteralPath $installDir) } -Seconds 20)) {
        Write-Fail "install directory still present after uninstall: $installDir"
    } elseif ($left.Count -gt 0) {
        Write-Fail ("install directory still holds {0} file(s): {1}" -f $left.Count, (($left | Select-Object -First 5 | ForEach-Object { $_.FullName }) -join '; '))
    } else {
        Write-Pass 'install directory removed'
    }
} else {
    Write-Pass 'install directory removed'
}

if (Test-RegistryKey $smokeKey) {
    Write-Fail "HKCU uninstall entry survived uninstall: $smokeKey"
} else {
    Write-Pass 'HKCU uninstall entry removed'
}

if (Test-Path -LiteralPath $smokeShortcut) {
    Write-Fail "smoke desktop shortcut survived uninstall: $smokeShortcut"
} else {
    Write-Pass 'smoke desktop shortcut removed'
}

if (Test-Path -LiteralPath $smokeGroup) {
    Write-Fail "smoke Start Menu group survived uninstall: $smokeGroup"
} else {
    Write-Pass 'smoke Start Menu group removed'
}

# --- the real installation must be untouched ------------------------------
$realInstallAfter = Get-TreeFingerprint $realInstall
$realShortcutAfter = Get-PathFingerprint $realShortcut
if ($realInstallBefore -eq $realInstallAfter) {
    Write-Pass "real installation untouched: $realInstall"
} else {
    Write-Fail "real installation changed: $realInstall"
}
if ($realShortcutBefore -eq $realShortcutAfter) {
    Write-Pass "real desktop shortcut untouched: $realShortcut"
} else {
    Write-Fail "real desktop shortcut changed: $realShortcut"
}

Write-Output ("ASSERTIONS passed={0} failed={1}" -f $script:Passes, $script:Failures)
if ($script:Failures -gt 0) {
    Write-Output ("INSTALLER TEST FAILED: {0} assertion(s)" -f $script:Failures)
    exit 1
}
Write-Output 'INSTALLER TEST PASS'
exit 0