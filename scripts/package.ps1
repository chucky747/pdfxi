<#
.SYNOPSIS
Produce a reproducible Windows package for PDFXI (T-0123 / AC-10).

.DESCRIPTION
Three modes, all offline and elevation-free:

  (default)   cargo build --workspace --release, then package.
  -SkipBuild  package the binaries already present in the release target dir.
  -Check      validate layout, notices, allowlist and any existing manifest
              WITHOUT building or producing a package.

The package is written under target/package/ and contains:

  staging/pdfxi.exe        CLI binary (workspace crate pdfxi-cli -> bin "pdfxi")
  staging/pdfxi-ui.exe     desktop viewer (pdfxi-app bin "pdfxi-ui", ui+pdfium)
  staging/pdfium.dll       pinned PDFium (chromium/8165); loaded from the exe directory
  staging/pdfium-licenses\ PDFium BSD-3-Clause licence and third-party notices
  staging/LICENSE          MIT license text
  staging/NOTICE           clean-room / trademark notice
  staging/README.md        project readme
  staging/THIRD_PARTY.md   third-party component notices
  staging/deny.toml        license/cargo-deny allowlist
  staging/install.ps1      user-scope installer (no elevation, no network)
  staging/docs\            user guide (contents of docs\book\src) for Help
  staging/manifest.json    SHA-256 manifest of every other payload file
  pdfxi-portable-<version>-windows-x86_64.zip
  manifest.json            copy of the staged manifest for inspection
  <zipname>.sha256         sidecar hash of the archive

The pinned PDFium DLL is bundled with its licence and third-party notices,
and its SHA-256 is verified before staging. No Adobe asset
is copied. Only an explicit allowlist is staged, and every staged file is
checked against secret/private-file patterns before an archive is produced.

.EXAMPLE
powershell -NoProfile -File scripts/package.ps1 -Check
.EXAMPLE
powershell -NoProfile -File scripts/package.ps1 -SkipBuild
.EXAMPLE
powershell -NoProfile -File scripts/package.ps1 -SelfTest
#>
[CmdletBinding(DefaultParameterSetName = 'Build')]
param(
    [Parameter(ParameterSetName = 'Check')][switch]$Check,
    [Parameter(ParameterSetName = 'SkipBuild')][switch]$SkipBuild,
    [Parameter(ParameterSetName = 'SelfTest')][switch]$SelfTest,
    [string]$TargetDir = '',
    [string]$OutputDir = '',
    [string]$PdfiumDir = '',
    [string]$PdfiumSha256 = '',
    # Self-test only: the deterministic stub binaries are not real PE images.
    [switch]$SkipViewerCheck,
    # Build the Inno Setup installer (also done automatically when ISCC exists).
    [switch]$Installer
)

$ErrorActionPreference = 'Stop'

# --- bootstrap PATH / environment (matches scripts/env.ps1 fallback) --------
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$mingwBin = Join-Path $env:LOCALAPPDATA 'Programs\winlibs\bin'
$env:Path = "$cargoBin;$mingwBin;$env:Path"
$env:CARGO_TERM_COLOR = 'never'

$root = Split-Path $PSScriptRoot -Parent
$script:PackageRoot = if ($OutputDir) {
    [System.IO.Path]::GetFullPath($OutputDir)
} else {
    Join-Path $root 'target\package'
}

# --- payload definition ----------------------------------------------------
# Only these files may ever enter the archive. manifest.json is generated and
# is therefore in the allowlist but not in the source map.
$script:AllowList = @(
    'pdfxi.exe', 'pdfxi-ui.exe', 'pdfium.dll', 'LICENSE', 'NOTICE', 'README.md',
    'THIRD_PARTY.md', 'deny.toml', 'install.ps1', 'manifest.json'
)
# PDFium's own licence texts ship in a subdirectory so the notice is complete.
# The user guide ships under docs\ so the installed viewer's Help window can open
# it from `exe\docs` (bundled_docs_dir()).
$script:AllowListPrefixes = @('pdfium-licenses\', 'docs\')
# Help needs at least these guide pages; -Check fails when one is missing.
$script:RequiredDocs = @('docs\README.md', 'docs\SUMMARY.md', 'docs\security.md', 'docs\privacy.md')
# Pinned PDFium release; scripts/fetch-pdfium.ps1 downloads exactly this build.
$script:PdfiumTag = 'chromium/8165'
$script:PdfiumDllSha256 = '31d249b8535dd5f351ae7f4b714dc4bb1d76b6069b321a309bc99271dd65f566'
$script:NoticeFiles = @('LICENSE', 'NOTICE', 'README.md', 'THIRD_PARTY.md', 'deny.toml')
$script:SecretPatterns = @(
    '(?i)(^|[\\/])id_(rsa|dsa|ecdsa|ed25519)(\.|$)',
    '(?i)\.(pem|p12|pfx|jks|keystore|kdbx|gpg|pgp|asc|env)$',
    '(?i)(^|[\\/])\.env(\.|$)',
    '(?i)(^|[\\/])(secrets?|credentials?|tokens?|api[_-]?keys?|private[_-]?keys?|passwords?)([\\/.]|$)',
    '(?i)(^|[\\/])(corpus[\\/]private|corpus[\\/]cache|private)([\\/]|$)',
    '(?i)\.(docx?|xlsx?|pptx?|pdf|msg|eml|one)$',
    '(?i)(customer|confidential|payroll|invoice)'
)

# Locate Inno Setup's command-line compiler without hard-coding a drive letter.
function Resolve-Iscc {
    if ($env:ISCC_PATH -and (Test-Path -LiteralPath $env:ISCC_PATH -PathType Leaf)) { return $env:ISCC_PATH }
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

# True when the file carries at least one RT_GROUP_ICON (icon group) resource.
function Test-IconGroupResource {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not ('PdfxiPeResources' -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class PdfxiPeResources
{
    private delegate bool EnumResNameProc(IntPtr m, IntPtr t, IntPtr n, IntPtr p);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryExW(string file, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll")]
    private static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern bool EnumResourceNamesW(IntPtr module, IntPtr type, EnumResNameProc proc, IntPtr param);
    public static string[] Names(string path, int type)
    {
        List<string> names = new List<string>();
        IntPtr module = LoadLibraryExW(path, IntPtr.Zero, 2);
        if (module == IntPtr.Zero) { return names.ToArray(); }
        try
        {
            EnumResourceNamesW(module, (IntPtr)type,
                delegate(IntPtr m, IntPtr t, IntPtr n, IntPtr p) { names.Add(((long)n).ToString()); return true; },
                IntPtr.Zero);
        }
        finally { FreeLibrary(module); }
        return names.ToArray();
    }
}
"@
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    $groups = @([PdfxiPeResources]::Names($Path, 14))
    return ($groups.Count -ge 1)
}

# Build the per-user installer next to the portable zip.
function New-Installer {
    param([Parameter(Mandatory = $true)][string]$Iscc,
          [Parameter(Mandatory = $true)][string]$Version,
          [Parameter(Mandatory = $true)][string]$StagingDir,
          [Parameter(Mandatory = $true)][string]$PackageRoot,
          [switch]$Smoke)
    $iss = Join-Path $root 'installer\pdfxi.iss'
    if (-not (Test-Path -LiteralPath $iss -PathType Leaf)) { throw "installer script missing: $iss" }
    $icon = Join-Path $root 'crates\pdfxi-app\assets\pdfxi.ico'
    $isccArgs = @(
        "/DAppVersion=$Version",
        "/DStageDir=$StagingDir",
        "/DOutputDir=$PackageRoot",
        "/DSourceIcon=$icon"
    )
    if ($Smoke) { $isccArgs += '/DSmoke=1' }
    # The script path is the only positional argument; defines come first.
    $isccArgs += $iss
    Write-Host "INSTALLER iscc=$Iscc smoke=$([bool]$Smoke)"
    # Keep ISCC's chatter off the function's return pipeline.
    $isccOutput = & $Iscc @isccArgs 2>&1
    foreach ($line in @($isccOutput)) { if ("$line".Trim()) { Write-Host "ISCC $line" } }
    $exit = $LASTEXITCODE
    $setup = Join-Path $PackageRoot "pdfxi-setup-$Version-windows-x86_64.exe"
    if ($exit -ne 0) { throw "ISCC exited $exit; no installer produced" }
    if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) { throw "ISCC reported success but $setup is missing" }
    Write-Utf8NoBom "$setup.sha256" ("$((Get-Sha256 $setup))  $(Split-Path -Leaf $setup)`n")
    return $setup
}

# Default PDFium source: the per-user developer cache written by
# scripts/fetch-pdfium.ps1. Never a machine-specific path.
function Resolve-PdfiumDir {
    param([string]$Override)
    if ($Override) { return [System.IO.Path]::GetFullPath($Override) }
    if ($env:PDFIUM_DIR) { return [System.IO.Path]::GetFullPath($env:PDFIUM_DIR) }
    $base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { $env:USERPROFILE }
    return (Join-Path $base 'pdfxi\pdfium')
}

# Verify the bundled PDFium payload: DLL hash, LICENSE, and licenses\ directory.
function Assert-PdfiumPayload {
    param([Parameter(Mandatory = $true)][string]$Directory,
          [Parameter(Mandatory = $true)][string]$ExpectedSha256)
    $dll = Join-Path $Directory 'bin\pdfium.dll'
    if (-not (Test-Path -LiteralPath $dll -PathType Leaf)) {
        throw "PDFium DLL missing: $dll (run scripts/fetch-pdfium.ps1)"
    }
    $actual = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $ExpectedSha256.ToLowerInvariant()) {
        throw "refusing to package PDFium: $dll has SHA-256 $actual, expected $ExpectedSha256"
    }
    foreach ($required in @('LICENSE', 'licenses')) {
        if (-not (Test-Path -LiteralPath (Join-Path $Directory $required))) {
            throw "PDFium payload is incomplete: $required missing under $Directory"
        }
    }
}

function Get-PdfiumSha256 {
    param([string]$Override)
    if ($Override) { return $Override }
    return $script:PdfiumDllSha256
}

function Copy-PdfiumLicences {
    param([Parameter(Mandatory = $true)][string]$SourceDir,
          [Parameter(Mandatory = $true)][string]$StagingDir)
    $licenceDir = Join-Path $StagingDir 'pdfium-licenses'
    New-Item -ItemType Directory -Force -Path $licenceDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $SourceDir 'LICENSE') -Destination (Join-Path $licenceDir 'LICENSE.txt') -Force
    foreach ($file in @(Get-ChildItem -LiteralPath (Join-Path $SourceDir 'licenses') -File)) {
        Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $licenceDir $file.Name) -Force
    }
}

function Get-PayloadMap {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir,
          [string]$PdfiumDir = '')
    $map = [ordered]@{}
    $map['pdfxi.exe']      = Join-Path $ReleaseDir 'pdfxi.exe'
    $map['pdfxi-ui.exe']   = Join-Path $ReleaseDir 'pdfxi-ui.exe'
    $map['pdfium.dll']     = Join-Path (Resolve-PdfiumDir $PdfiumDir) 'bin\pdfium.dll'
    $map['LICENSE']        = Join-Path $root 'LICENSE'
    $map['NOTICE']         = Join-Path $root 'NOTICE'
    $map['README.md']      = Join-Path $root 'README.md'
    $map['THIRD_PARTY.md'] = Join-Path $root 'docs\THIRD_PARTY.md'
    $map['deny.toml']      = Join-Path $root 'deny.toml'
    $map['install.ps1']    = Join-Path $PSScriptRoot 'install.ps1'
    return $map
}

# Resolve the Cargo target directory for packaging. A lane directory is never a
# default: packaging stale lane binaries was a real defect. An explicit
# -TargetDir or CARGO_TARGET_DIR always wins; otherwise target\integration.
function Resolve-TargetDir {
    param([string]$Override)
    if ($Override) { return [System.IO.Path]::GetFullPath($Override) }
    if ($env:CARGO_TARGET_DIR) { return [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) }
    return (Join-Path $root 'target\integration')
}

# --- small helpers ---------------------------------------------------------
function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-Utf8NoBom {
    param([Parameter(Mandatory = $true)][string]$Path,
          [Parameter(Mandatory = $true)][string]$Text)
    $enc = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Text, $enc)
}

function New-ValidationResult {
    $r = [ordered]@{
        Ok         = $true
        LayoutOk   = $true
        NoticesOk  = $true
        SecretFree = $true
        Verified   = 0
        Errors     = [System.Collections.ArrayList]::new()
    }
    return $r
}

function Add-ValidationError {
    param($Result, [string]$Category, [string]$Message)
    $Result.Ok = $false
    if ($Category -eq 'layout') { $Result.LayoutOk = $false }
    if ($Category -eq 'notice') { $Result.NoticesOk = $false }
    if ($Category -eq 'secret') { $Result.SecretFree = $false; $Result.LayoutOk = $false }
    [void]$Result.Errors.Add("[$Category] $Message")
}

function Format-Result {
    param($Result, [string]$Label)
    foreach ($e in $Result.Errors) { Write-Output "$Label $e" }
    Write-Output ("{0} ok={1} layout={2} notices={3} secret_free={4} hashes_verified={5}" -f `
        $Label, $Result.Ok, $Result.LayoutOk, $Result.NoticesOk, $Result.SecretFree, $Result.Verified)
}

# --- validation ------------------------------------------------------------
function Test-PayloadSafety {
    param([Parameter(Mandatory = $true)][string]$StagingDir)
    $result = New-ValidationResult
    if (-not (Test-Path -LiteralPath $StagingDir -PathType Container)) {
        Add-ValidationError $result 'layout' "staging directory missing: $StagingDir"
        return $result
    }
    $sd = (Resolve-Path -LiteralPath $StagingDir).Path.TrimEnd([char[]]@('\', '/'))
    $present = @()
    foreach ($f in @(Get-ChildItem -LiteralPath $sd -Recurse -File)) {
        $rel = $f.FullName.Substring($sd.Length).TrimStart([char[]]@('\', '/'))
        $present += $rel
        $allowed = ($script:AllowList -contains $rel)
        foreach ($prefix in $script:AllowListPrefixes) {
            if ($rel.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) { $allowed = $true }
        }
        if (-not $allowed) {
            Add-ValidationError $result 'layout' "unexpected payload file: $rel"
        }
        foreach ($pat in $script:SecretPatterns) {
            if ($rel -match $pat) {
                Add-ValidationError $result 'secret' "secret/private-file rejected: $rel (pattern: $pat)"
                break
            }
        }
    }
    foreach ($required in $script:AllowList) {
        if ($present -notcontains $required) {
            Add-ValidationError $result 'layout' "missing payload file: $required"
        }
    }
    foreach ($n in $script:NoticeFiles) {
        $p = Join-Path $sd $n
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
            Add-ValidationError $result 'notice' "missing notice: $n"
        } elseif ((Get-Item -LiteralPath $p).Length -le 0) {
            Add-ValidationError $result 'notice' "empty notice: $n"
        }
    }
    foreach ($n in $script:RequiredDocs) {
        $p = Join-Path $sd $n
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
            Add-ValidationError $result 'layout' "missing user-guide page: $n"
        } elseif ((Get-Item -LiteralPath $p).Length -le 0) {
            Add-ValidationError $result 'layout' "empty user-guide page: $n"
        }
    }
    return $result
}

function Test-UiBinary {
    param([Parameter(Mandatory = $true)][string]$Path)
    $result = New-ValidationResult
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Add-ValidationError $result 'layout' "missing viewer binary: $Path"
        return $result
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x100) {
        Add-ValidationError $result 'layout' "viewer binary is too small to be a PE image: $Path"
        return $result
    }
    $pe = [System.BitConverter]::ToInt32($bytes, 0x3c)
    if ($pe -le 0 -or ($pe + 0x60) -ge $bytes.Length) {
        Add-ValidationError $result 'layout' "viewer binary has no valid PE header: $Path"
        return $result
    }
    $subsystem = [System.BitConverter]::ToUInt16($bytes, $pe + 24 + 0x44)
    if ($subsystem -ne 2) {
        Add-ValidationError $result 'layout' "pdfxi-ui.exe subsystem is $subsystem, expected 2 (Windows GUI); the viewer would open a console window"
    }
    $info = (Get-Item -LiteralPath $Path).VersionInfo
    if ($info.ProductName -ne 'PDFXI') {
        Add-ValidationError $result 'layout' "pdfxi-ui.exe has no PDFXI version resource (ProductName='$($info.ProductName)')"
    }
    if (-not $info.FileVersion) {
        Add-ValidationError $result 'layout' "pdfxi-ui.exe has no FileVersion resource"
    }
    if (-not (Test-IconGroupResource -Path $Path)) {
        Add-ValidationError $result 'layout' "pdfxi-ui.exe has no icon group resource (RT_GROUP_ICON); it would show the default application icon"
    }
    return $result
}

function Test-SourceLayout {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir,
          [string]$PdfiumDir = '')
    $result = New-ValidationResult
    $map = Get-PayloadMap -ReleaseDir $ReleaseDir -PdfiumDir $PdfiumDir
    foreach ($name in $map.Keys) {
        foreach ($pat in $script:SecretPatterns) {
            if ($name -match $pat) {
                Add-ValidationError $result 'secret' "declared payload name rejected: $name"
                break
            }
        }
        if ($name -eq 'pdfxi.exe' -or $name -eq 'pdfxi-ui.exe' -or $name -eq 'pdfium.dll') {
            # Build artifacts; -Check intentionally does not require a build.
            continue
        }
        $src = $map[$name]
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
            Add-ValidationError $result 'layout' "missing source payload file: $name -> $src"
        } elseif ((Get-Item -LiteralPath $src).Length -le 0) {
            Add-ValidationError $result 'layout' "empty source payload file: $name"
        }
    }
    foreach ($n in $script:NoticeFiles) {
        if ($map.Keys -notcontains $n) {
            Add-ValidationError $result 'notice' "notice not declared in payload: $n"
        }
    }
    $installer = Join-Path $PSScriptRoot 'install.ps1'
    if (Test-Path -LiteralPath $installer -PathType Leaf) {
        $itext = Get-Content -LiteralPath $installer -Raw
        if ($itext -match 'Invoke-WebRequest|Invoke-RestMethod|WebClient|RunAs|Start-Process') {
            Add-ValidationError $result 'layout' 'install.ps1 contains a forbidden network/elevation API'
        }
    }
    return $result
}

function Test-Manifest {
    param([Parameter(Mandatory = $true)][string]$ManifestPath,
          [Parameter(Mandatory = $true)][string]$StagingDir)
    $result = New-ValidationResult
    try {
        $m = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    } catch {
        Add-ValidationError $result 'layout' "manifest is not valid JSON: $($_.Exception.Message)"
        return $result
    }
    if ($m.schema -ne 1) {
        Add-ValidationError $result 'layout' "unexpected manifest schema: $($m.schema)"
    }
    foreach ($entry in $m.files) {
        $p = Join-Path $StagingDir $entry.path
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
            Add-ValidationError $result 'layout' "manifest lists missing staged file: $($entry.path)"
        } elseif ((Get-Sha256 $p) -ne $entry.sha256) {
            Add-ValidationError $result 'layout' "hash mismatch for staged file: $($entry.path)"
        } else {
            $result.Verified++
        }
    }
    return $result
}

# --- reproducibility inputs ------------------------------------------------
function Get-ToolVersion {
    param([string]$Exe)
    try {
        $cmd = Get-Command $Exe -ErrorAction Stop
        return ((& $cmd.Source --version 2>$null) | Select-Object -First 1)
    } catch {
        return "$Exe unavailable"
    }
}

function Get-WorkspaceField {
    param([string]$File, [string]$Section, [string]$Key, [string]$Fallback)
    $path = Join-Path $root $File
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $Fallback }
    $txt = Get-Content -LiteralPath $path -Raw
    $pattern = '(?ms)^\s*\[' + [regex]::Escape($Section) + '\]\s*$.*?^\s*' +
               [regex]::Escape($Key) + '\s*=\s*"([^"]+)"'
    if ($txt -match $pattern) { return $Matches[1] }
    return $Fallback
}

function Get-CrateList {
    $lock = Join-Path $root 'Cargo.lock'
    $list = [System.Collections.ArrayList]::new()
    if (-not (Test-Path -LiteralPath $lock -PathType Leaf)) { return @() }
    $inPackage = $false
    $name = ''
    $version = ''
    foreach ($line in [System.IO.File]::ReadAllLines($lock)) {
        if ($line -match '^\[\[package\]\]') {
            if ($inPackage -and $name) { [void]$list.Add("$name@$version") }
            $inPackage = $true; $name = ''; $version = ''
            continue
        }
        if ($line -match '^\[') {
            if ($inPackage -and $name) { [void]$list.Add("$name@$version") }
            $inPackage = $false
            continue
        }
        if ($inPackage -and $line -match '^name\s*=\s*"([^"]+)"') {
            $name = $Matches[1]
        } elseif ($inPackage -and $line -match '^version\s*=\s*"([^"]+)"') {
            $version = $Matches[1]
        }
    }
    if ($inPackage -and $name) { [void]$list.Add("$name@$version") }
    return $list.ToArray()
}

function New-ManifestFile {
    param([Parameter(Mandatory = $true)][string]$StagingDir)
    $files = [System.Collections.ArrayList]::new()
    $stagingRoot = (Resolve-Path -LiteralPath $StagingDir).Path.TrimEnd([char[]]@('\', '/'))
    foreach ($f in @(Get-ChildItem -LiteralPath $StagingDir -Recurse -File | Sort-Object -Property FullName)) {
        $relative = $f.FullName.Substring($stagingRoot.Length).TrimStart([char[]]@('\', '/'))
        if ($relative -eq 'manifest.json') { continue }
        [void]$files.Add([ordered]@{
            path   = $relative
            sha256 = (Get-Sha256 $f.FullName)
            bytes  = [long]$f.Length
        })
    }
    $lock = Join-Path $root 'Cargo.lock'
    $lockHash = if (Test-Path -LiteralPath $lock -PathType Leaf) { Get-Sha256 $lock } else { 'unavailable' }
    $manifest = [ordered]@{
        schema            = 1
        package           = 'pdfxi-portable'
        version           = Get-WorkspaceField -File 'Cargo.toml' -Section 'workspace.package' -Key 'version' -Fallback '0.0.0'
        host              = 'windows/x86_64'
        toolchain         = [ordered]@{
            rustc          = Get-ToolVersion -Exe 'rustc'
            cargo          = Get-ToolVersion -Exe 'cargo'
            channel        = Get-WorkspaceField -File 'rust-toolchain.toml' -Section 'toolchain' -Key 'channel' -Fallback 'stable'
            edition        = Get-WorkspaceField -File 'Cargo.toml' -Section 'workspace.package' -Key 'edition' -Fallback '2021'
            rust_version   = Get-WorkspaceField -File 'Cargo.toml' -Section 'workspace.package' -Key 'rust-version' -Fallback '1.88'
        }
        cargo_lock_sha256 = $lockHash
        crates            = @(Get-CrateList)
        binaries          = @('pdfxi.exe', 'pdfxi-ui.exe', 'pdfium.dll')
        # Intentionally no wall-clock timestamp: identical inputs produce a
        # byte-identical manifest, which is what `-SelfTest` asserts.
        files             = $files.ToArray()
        pdfium            = [ordered]@{
            tag      = $script:PdfiumTag
            dll      = 'pdfium.dll'
            sha256   = (Get-PdfiumSha256 $PdfiumSha256).ToLowerInvariant()
            licences = 'pdfium-licenses\'
        }
        legal             = 'Bundles the pinned PDFium binary with its BSD-3-Clause licence and third-party notices under pdfium-licenses\; no Adobe asset, secret, or private document is included.'
    }
    $json = $manifest | ConvertTo-Json -Depth 8
    Write-Utf8NoBom (Join-Path $StagingDir 'manifest.json') ($json + "`n")
    return $json
}

# --- packaging -------------------------------------------------------------
function New-Staging {
    param([Parameter(Mandatory = $true)][string]$StagingDir,
          [Parameter(Mandatory = $true)][string]$ReleaseDir,
          [string]$PdfiumDir = '')
    if (Test-Path -LiteralPath $StagingDir) { Remove-Item -LiteralPath $StagingDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
    $map = Get-PayloadMap -ReleaseDir $ReleaseDir -PdfiumDir $PdfiumDir
    foreach ($name in $map.Keys) {
        $src = $map[$name]
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
            throw "missing payload source for '$name': $src"
        }
        Copy-Item -LiteralPath $src -Destination (Join-Path $StagingDir $name) -Force
    }
    Copy-PdfiumLicences -SourceDir (Resolve-PdfiumDir $PdfiumDir) -StagingDir $StagingDir
    # Help reads the guide from `exe\docs`; stage the mdBook sources flat, without
    # unrelated docs/evidence or docs/decisions trees.
    $guideSrc = Join-Path $root 'docs\book\src'
    if (-not (Test-Path -LiteralPath $guideSrc -PathType Container)) {
        throw "user guide source missing: $guideSrc"
    }
    $guideDst = Join-Path $StagingDir 'docs'
    New-Item -ItemType Directory -Force -Path $guideDst | Out-Null
    foreach ($file in @(Get-ChildItem -LiteralPath $guideSrc -Recurse -File)) {
        $rel = $file.FullName.Substring((Resolve-Path -LiteralPath $guideSrc).Path.Length).TrimStart([char[]]@('\', '/'))
        $dst = Join-Path $guideDst $rel
        $dstDir = Split-Path -Parent $dst
        if (-not (Test-Path -LiteralPath $dstDir -PathType Container)) {
            New-Item -ItemType Directory -Force -Path $dstDir | Out-Null
        }
        Copy-Item -LiteralPath $file.FullName -Destination $dst -Force
    }
}

function New-Archive {
    param([Parameter(Mandatory = $true)][string]$StagingDir,
          [Parameter(Mandatory = $true)][string]$PackageRoot,
          [Parameter(Mandatory = $true)][string]$Version)
    # Normalize entry timestamps so the zip bytes are a pure function of input.
    # Use a local-kind 1980-01-02 stamp: .NET ZipArchive rejects a UTC epoch
    # that maps before 1980 in the host timezone.
    $fixed = [datetime]::new(1980, 1, 2, 0, 0, 0, [DateTimeKind]::Local)
    foreach ($f in @(Get-ChildItem -LiteralPath $StagingDir -Recurse)) {
        $f.LastWriteTime = $fixed
        $f.CreationTime = $fixed
    }
    $name = "pdfxi-portable-$Version-windows-x86_64.zip"
    $zip = Join-Path $PackageRoot $name
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
    Compress-Archive -Path (Join-Path $StagingDir '*') -DestinationPath $zip -CompressionLevel Optimal
    Write-Utf8NoBom "$zip.sha256" ("$((Get-Sha256 $zip))  $name`n")
    return $zip
}

function New-PackageArtifacts {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir,
          [Parameter(Mandatory = $true)][string]$PackageRoot,
          [string]$PdfiumDir = '',
          [string]$PdfiumSha256 = '',
          [switch]$SkipViewerCheck,
          [switch]$WithArchive)
    Assert-PdfiumPayload -Directory (Resolve-PdfiumDir $PdfiumDir) -ExpectedSha256 (Get-PdfiumSha256 $PdfiumSha256)
    if (-not $SkipViewerCheck) {
        $uiCheck = Test-UiBinary -Path (Join-Path $ReleaseDir 'pdfxi-ui.exe')
        if (-not $uiCheck.Ok) {
            Format-Result -Result $uiCheck -Label 'VIEWER'
            throw 'pdfxi-ui.exe failed the GUI-subsystem/version-resource check; package not produced'
        }
    }
    New-Item -ItemType Directory -Force -Path $PackageRoot | Out-Null
    $staging = Join-Path $PackageRoot 'staging'
    New-Staging -StagingDir $staging -ReleaseDir $ReleaseDir -PdfiumDir $PdfiumDir
    $json = New-ManifestFile -StagingDir $staging
    $safety = Test-PayloadSafety -StagingDir $staging
    if (-not $safety.Ok) {
        Format-Result -Result $safety -Label 'SAFETY'
        throw 'payload failed layout/notice/secret validation; archive not produced'
    }
    Copy-Item -LiteralPath (Join-Path $staging 'manifest.json') `
              -Destination (Join-Path $PackageRoot 'manifest.json') -Force
    $zip = ''
    if ($WithArchive) {
        $version = (Get-Content -LiteralPath (Join-Path $staging 'manifest.json') -Raw | ConvertFrom-Json).version
        $zip = New-Archive -StagingDir $staging -PackageRoot $PackageRoot -Version $version
    }
    return [pscustomobject]@{ ManifestJson = $json; Staging = $staging; Zip = $zip }
}

function Invoke-Package {
    param([bool]$DoBuild)
    $resolvedTarget = Resolve-TargetDir $TargetDir
    Write-Output "TARGET $resolvedTarget"
    if ($DoBuild) {
        Write-Output 'BUILD cargo build --workspace --release'
        Push-Location $root
        try {
            # Build into the directory that is about to be packaged.
            $env:CARGO_TARGET_DIR = $resolvedTarget
            & cargo build --workspace --release
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit $LASTEXITCODE" }
            Write-Output 'BUILD cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium'
            & cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
            if ($LASTEXITCODE -ne 0) { throw "UI build failed with exit $LASTEXITCODE" }
        } finally { Pop-Location }
    } else {
        Write-Output 'SKIPBUILD using existing release binaries'
    }
    $releaseDir = Join-Path $resolvedTarget 'release'
    foreach ($binary in @('pdfxi.exe', 'pdfxi-ui.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir $binary) -PathType Leaf)) {
            throw "release binary missing: $(Join-Path $releaseDir $binary) (omit -SkipBuild to build it)"
        }
    }
    Write-Output "PDFIUM $(Resolve-PdfiumDir $PdfiumDir) sha256=$(Get-PdfiumSha256 $PdfiumSha256)"
    $artifacts = New-PackageArtifacts -ReleaseDir $releaseDir -PackageRoot $script:PackageRoot `
        -PdfiumDir $PdfiumDir -PdfiumSha256 $PdfiumSha256 -SkipViewerCheck:$SkipViewerCheck -WithArchive
    Format-Result -Result (Test-PayloadSafety -StagingDir $artifacts.Staging) -Label 'SAFETY'
    Format-Result -Result (Test-Manifest -ManifestPath (Join-Path $script:PackageRoot 'manifest.json') -StagingDir $artifacts.Staging) -Label 'MANIFEST'
    Write-Output ("PACKAGE OK zip={0}" -f $artifacts.Zip)
    Write-Output ("PACKAGE SHA256 {0}" -f (Get-Sha256 $artifacts.Zip))
    Write-Output ("PACKAGE ROOT {0}" -f $script:PackageRoot)
    $iscc = Resolve-Iscc
    if (-not $iscc) {
        if ($Installer) {
            throw 'ISCC.exe not found: install Inno Setup 6 (`winget install --id JRSoftware.InnoSetup -e`) or set ISCC_PATH, then re-run with -Installer'
        }
        Write-Output 'INSTALLER skipped: ISCC.exe not found (set ISCC_PATH or install JRSoftware.InnoSetup to build it)'
    } else {
        $setupVersion = (Get-Content -LiteralPath (Join-Path $artifacts.Staging 'manifest.json') -Raw | ConvertFrom-Json).version
        $setup = New-Installer -Iscc $iscc -Version $setupVersion -StagingDir $artifacts.Staging -PackageRoot $script:PackageRoot
        Write-Output ("INSTALLER {0}" -f $setup)
        Write-Output ("INSTALLER SHA256 {0}" -f (Get-Sha256 $setup))
    }
    $script:ExitCode = 0
}

# --- -Check ----------------------------------------------------------------
function Invoke-Check {
    $releaseDir = Join-Path (Resolve-TargetDir $TargetDir) 'release'
    $sourceResult = Test-SourceLayout -ReleaseDir $releaseDir -PdfiumDir $PdfiumDir
    Format-Result -Result $sourceResult -Label 'SOURCE'
    $bin = Join-Path $releaseDir 'pdfxi.exe'
    if (Test-Path -LiteralPath $bin -PathType Leaf) {
        Write-Output "CHECK binary=present $bin"
    } else {
        Write-Output "CHECK binary=absent (expected: -Check never builds) $bin"
    }
    $uiBin = Join-Path $releaseDir 'pdfxi-ui.exe'
    if (Test-Path -LiteralPath $uiBin -PathType Leaf) {
        Write-Output "CHECK ui_binary=present $uiBin"
        $uiResult = Test-UiBinary -Path $uiBin
        Format-Result -Result $uiResult -Label 'VIEWER'
        $allOkViewer = $uiResult.Ok
    } else {
        Write-Output "CHECK ui_binary=absent (expected: -Check never builds) $uiBin"
        $allOkViewer = $true
    }
    $pdfiumResolved = Resolve-PdfiumDir $PdfiumDir
    try {
        Assert-PdfiumPayload -Directory $pdfiumResolved -ExpectedSha256 (Get-PdfiumSha256 $PdfiumSha256)
        Write-Output "CHECK pdfium=ok $pdfiumResolved"
        $pdfiumOk = $true
    } catch {
        Write-Output "CHECK pdfium=failed $($_.Exception.Message)"
        $pdfiumOk = $false
    }
    $allOk = $sourceResult.Ok -and $allOkViewer -and $pdfiumOk
    $staging = Join-Path $script:PackageRoot 'staging'
    if (Test-Path -LiteralPath $staging -PathType Container) {
        $stagedResult = Test-PayloadSafety -StagingDir $staging
        Format-Result -Result $stagedResult -Label 'STAGING'
        $allOk = $allOk -and $stagedResult.Ok
    } else {
        Write-Output 'STAGING absent (no package built yet)'
    }
    $manifestPath = Join-Path $script:PackageRoot 'manifest.json'
    if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
        $manifestResult = Test-Manifest -ManifestPath $manifestPath -StagingDir $staging
        Format-Result -Result $manifestResult -Label 'MANIFEST'
        $allOk = $allOk -and $manifestResult.Ok
    } else {
        Write-Output 'MANIFEST absent (no package built yet)'
    }
    Write-Output ("CHECK layout_and_notices={0}" -f $(if ($allOk) { 'PASS' } else { 'FAIL' }))
    $script:ExitCode = $(if ($allOk) { 0 } else { 1 })
}

# --- -SelfTest: deterministic checks ---------------------------------------
function New-FakeReleaseDir {
    param([string]$Sandbox)
    $rel = Join-Path $Sandbox 'fake-target\release'
    New-Item -ItemType Directory -Force -Path $rel | Out-Null
    # Tiny deterministic stubs standing in for the compiled binaries.
    Write-Utf8NoBom (Join-Path $rel 'pdfxi.exe') "MZ stub for package self-test`n"
    Write-Utf8NoBom (Join-Path $rel 'pdfxi-ui.exe') "MZ ui stub for package self-test`n"
    return $rel
}

function New-FakePdfiumDir {
    param([string]$Sandbox)
    $dir = Join-Path $Sandbox 'fake-pdfium'
    New-Item -ItemType Directory -Force -Path (Join-Path $dir 'bin') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $dir 'licenses') | Out-Null
    Write-Utf8NoBom (Join-Path $dir 'bin\pdfium.dll') "MZ pdfium stub for package self-test`n"
    Write-Utf8NoBom (Join-Path $dir 'LICENSE') "PDFium BSD-3-Clause stub licence`n"
    Write-Utf8NoBom (Join-Path $dir 'licenses\pdfium.txt') "PDFium third-party notice stub`n"
    $sha = (Get-Sha256 (Join-Path $dir 'bin\pdfium.dll'))
    return [pscustomobject]@{ Directory = $dir; Sha256 = $sha }
}

function Invoke-SelfTest {
    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ('pdfxi-package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
    $passed = 0
    $failed = 0
    try {
        $rel = New-FakeReleaseDir -Sandbox $sandbox
        $pdf = New-FakePdfiumDir -Sandbox $sandbox
        $out1 = Join-Path $sandbox 'out1'
        $art1 = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out1 `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck

        # 1. layout validation: a complete staging tree must pass.
        $r1 = Test-PayloadSafety -StagingDir $art1.Staging
        if ($r1.Ok -and $r1.LayoutOk -and $r1.NoticesOk) {
            Write-Output 'TEST 1 layout_validation PASS'; $passed++
        } else { Format-Result $r1 'TEST 1'; Write-Output 'TEST 1 layout_validation FAIL'; $failed++ }

        # 2. missing-notice failure: removing NOTICE must be rejected.
        $out2 = Join-Path $sandbox 'out2'
        $null = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out2 `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck
        Remove-Item -LiteralPath (Join-Path $out2 'staging\NOTICE') -Force
        $r2 = Test-PayloadSafety -StagingDir (Join-Path $out2 'staging')
        if ((-not $r2.Ok) -and (-not $r2.NoticesOk)) {
            Write-Output 'TEST 2 missing_notice_failure PASS'; $passed++
        } else { Format-Result $r2 'TEST 2'; Write-Output 'TEST 2 missing_notice_failure FAIL'; $failed++ }

        # 3. hash manifest determinism: two runs, same inputs, same bytes.
        $out3 = Join-Path $sandbox 'out3'
        $art3 = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out3 `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck
        $a = [System.IO.File]::ReadAllText((Join-Path $out1 'staging\manifest.json'))
        $b = [System.IO.File]::ReadAllText((Join-Path $out3 'staging\manifest.json'))
        $ha = (Get-Sha256 (Join-Path $out1 'staging\manifest.json'))
        $hb = (Get-Sha256 (Join-Path $out3 'staging\manifest.json'))
        if ($a -ceq $b -and $ha -eq $hb) {
            Write-Output "TEST 3 manifest_determinism PASS sha256=$ha"; $passed++
        } else { Write-Output "TEST 3 manifest_determinism FAIL ($ha vs $hb)"; $failed++ }

        # 4. -SkipBuild path: real child process, fake release target present.
        $skipOut = Join-Path $sandbox 'skipbuild'
        & powershell -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath `
            -SkipBuild -TargetDir (Split-Path $rel -Parent) -OutputDir $skipOut `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck | Out-Null
        $skipCode = $LASTEXITCODE
        $zip = Get-ChildItem -LiteralPath $skipOut -Filter '*.zip' -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($skipCode -eq 0 -and $zip) {
            Write-Output "TEST 4 skipbuild_path PASS exit=$skipCode zip=$($zip.Name)"; $passed++
        } else { Write-Output "TEST 4 skipbuild_path FAIL exit=$skipCode"; $failed++ }

        # 4b. -SkipBuild without binaries must fail cleanly.
        $emptyTarget = Join-Path $sandbox 'empty-target'
        New-Item -ItemType Directory -Force -Path $emptyTarget | Out-Null
        $negOut = Join-Path $sandbox 'neg-out'
        & powershell -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath `
            -SkipBuild -TargetDir $emptyTarget -OutputDir $negOut `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 2>&1 | Out-Null
        $negCode = $LASTEXITCODE
        if ($negCode -ne 0) {
            Write-Output "TEST 4b skipbuild_missing_binary_rejected PASS exit=$negCode"; $passed++
        } else { Write-Output 'TEST 4b skipbuild_missing_binary_rejected FAIL'; $failed++ }

        # 5. secret/private-file rejection: planted secret.pem must be refused.
        $out5 = Join-Path $sandbox 'out5'
        $null = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out5 `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck
        Write-Utf8NoBom (Join-Path $out5 'staging\secret.pem') 'not a real key'
        $r5 = Test-PayloadSafety -StagingDir (Join-Path $out5 'staging')
        if ((-not $r5.Ok) -and (-not $r5.SecretFree)) {
            Write-Output 'TEST 5 secret_private_file_rejection PASS'; $passed++
        } else { Format-Result $r5 'TEST 5'; Write-Output 'TEST 5 secret_private_file_rejection FAIL'; $failed++ }

        # 6. source inputs validate cleanly on the real repository.
        $r6 = Test-SourceLayout -ReleaseDir (Join-Path (Resolve-TargetDir $TargetDir) 'release') -PdfiumDir $pdf.Directory
        if ($r6.Ok) {
            Write-Output 'TEST 6 source_layout_and_notices PASS'; $passed++
        } else { Format-Result $r6 'TEST 6'; Write-Output 'TEST 6 source_layout_and_notices FAIL'; $failed++ }

        # 7. installer copies the payload (including itself) without touching
        # the Start Menu; the installed tree must verify against its manifest,
        # and a reinstall over a legacy docs\docs layout must remove the nest.
        $installed = Join-Path $sandbox 'installed'
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $art1.Staging 'install.ps1') `
            -TargetDir $installed -NoShortcut | Out-Null
        $installCode = $LASTEXITCODE
        $installedScript = Join-Path $installed 'install.ps1'
        $scriptCopied = (Test-Path -LiteralPath $installedScript -PathType Leaf) -and
            ((Get-Sha256 $installedScript) -eq (Get-Sha256 (Join-Path $art1.Staging 'install.ps1')))
        $installedManifest = Test-Manifest -ManifestPath (Join-Path $installed 'manifest.json') -StagingDir $installed
        New-Item -ItemType Directory -Force -Path (Join-Path $installed 'docs\docs') | Out-Null
        Write-Utf8NoBom (Join-Path $installed 'docs\docs\stale.md') 'legacy nested copy'
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $art1.Staging 'install.ps1') `
            -TargetDir $installed -NoShortcut | Out-Null
        $upgradeCode = $LASTEXITCODE
        $nestedGone = -not (Test-Path -LiteralPath (Join-Path $installed 'docs\docs'))
        if ($installCode -eq 0 -and $upgradeCode -eq 0 -and $scriptCopied -and
            $installedManifest.Ok -and $nestedGone -and
            (Test-Path -LiteralPath (Join-Path $installed 'pdfxi.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $installed 'NOTICE') -PathType Leaf)) {
            Write-Output "TEST 7 installer_copy PASS exit=$installCode upgrade=$upgradeCode verified=$($installedManifest.Verified) installer_copied=1 nested_removed=1"; $passed++
        } else {
            Format-Result $installedManifest 'TEST 7'
            Write-Output "TEST 7 installer_copy FAIL exit=$installCode upgrade=$upgradeCode installer_copied=$scriptCopied manifest_ok=$($installedManifest.Ok) nested_removed=$nestedGone"; $failed++
        }

        # 8. the archive ships the viewer, the pinned PDFium DLL, and its licences.
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $out8 = Join-Path $sandbox 'out8'
        $art8 = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out8 -WithArchive `
            -PdfiumDir $pdf.Directory -PdfiumSha256 $pdf.Sha256 -SkipViewerCheck
        $entries = @()
        $archive = [System.IO.Compression.ZipFile]::OpenRead($art8.Zip)
        try { $entries = @($archive.Entries | ForEach-Object { $_.FullName.Replace('/', '\') }) }
        finally { $archive.Dispose() }
        $hasUi = $entries -contains 'pdfxi-ui.exe'
        $hasDll = $entries -contains 'pdfium.dll'
        $hasLicence = @($entries | Where-Object { $_ -like 'pdfium-licenses\*' }).Count -ge 2
        if ($hasUi -and $hasDll -and $hasLicence) {
            Write-Output "TEST 8 payload_bundle PASS ui=$hasUi pdfium=$hasDll licences=$hasLicence entries=$($entries.Count)"; $passed++
        } else {
            Write-Output "TEST 8 payload_bundle FAIL ui=$hasUi pdfium=$hasDll licences=$hasLicence"; $failed++
        }

        # 9. the viewer check rejects a stub and accepts the real release binary.
        $stubCheck = Test-UiBinary -Path (Join-Path $rel 'pdfxi-ui.exe')
        $realUi = Join-Path (Resolve-TargetDir $TargetDir) 'release\pdfxi-ui.exe'
        if ((Test-Path -LiteralPath $realUi -PathType Leaf)) {
            $realCheck = Test-UiBinary -Path $realUi
            if ((-not $stubCheck.Ok) -and $realCheck.Ok) {
                Write-Output "TEST 9 viewer_binary_check PASS stub_rejected=1 real_accepted=1"; $passed++
            } else {
                Format-Result $realCheck 'TEST 9'
                Write-Output "TEST 9 viewer_binary_check FAIL stub_ok=$($stubCheck.Ok) real_ok=$($realCheck.Ok)"; $failed++
            }
        } else {
            if ($stubCheck.Ok) {
                Write-Output 'TEST 9 viewer_binary_check FAIL stub accepted'; $failed++
            } else {
                Write-Output "TEST 9 viewer_binary_check PARTIAL stub_rejected=1 real_binary=absent ($realUi)"; $passed++
            }
        }
    } finally {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Output ("SELFTEST passed={0} failed={1}" -f $passed, $failed)
    $script:ExitCode = $(if ($failed -eq 0 -and $passed -ge 8) { 0 } else { 1 })
}

# --- dispatch --------------------------------------------------------------
$script:ExitCode = 1
try {
    if ($SelfTest) { Invoke-SelfTest; exit $script:ExitCode }
    if ($Check)    { Invoke-Check; exit $script:ExitCode }
    if ($SkipBuild) { Invoke-Package -DoBuild $false; exit 0 }
    Invoke-Package -DoBuild $true; exit 0
} catch {
    Write-Output "PACKAGE ERROR: $($_.Exception.Message)"
    exit 1
}