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
  staging/LICENSE          MIT license text
  staging/NOTICE           clean-room / trademark notice
  staging/README.md        project readme
  staging/THIRD_PARTY.md   third-party component notices
  staging/deny.toml        license/cargo-deny allowlist
  staging/install.ps1      user-scope installer (no elevation, no network)
  staging/manifest.json    SHA-256 manifest of every other payload file
  pdfxi-portable-<version>-windows-x86_64.zip
  manifest.json            copy of the staged manifest for inspection
  <zipname>.sha256         sidecar hash of the archive

No PDFium DLL is bundled (only the notice in THIRD_PARTY.md). No Adobe asset
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
    [string]$OutputDir = ''
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
    'pdfxi.exe', 'pdfxi-ui.exe', 'LICENSE', 'NOTICE', 'README.md', 'THIRD_PARTY.md',
    'deny.toml', 'install.ps1', 'manifest.json'
)
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

function Get-PayloadMap {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)
    $map = [ordered]@{}
    $map['pdfxi.exe']      = Join-Path $ReleaseDir 'pdfxi.exe'
    $map['pdfxi-ui.exe']   = Join-Path $ReleaseDir 'pdfxi-ui.exe'
    $map['LICENSE']        = Join-Path $root 'LICENSE'
    $map['NOTICE']         = Join-Path $root 'NOTICE'
    $map['README.md']      = Join-Path $root 'README.md'
    $map['THIRD_PARTY.md'] = Join-Path $root 'docs\THIRD_PARTY.md'
    $map['deny.toml']      = Join-Path $root 'deny.toml'
    $map['install.ps1']    = Join-Path $PSScriptRoot 'install.ps1'
    return $map
}

function Resolve-TargetDir {
    param([string]$Override)
    if ($Override) { return [System.IO.Path]::GetFullPath($Override) }
    if ($env:CARGO_TARGET_DIR) { return $env:CARGO_TARGET_DIR }
    $lane = Join-Path $root 'target\lanes\qa'
    if (Test-Path -LiteralPath $lane -PathType Container) { return $lane }
    return (Join-Path $root 'target')
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
        if ($script:AllowList -notcontains $rel) {
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
    return $result
}

function Test-SourceLayout {
    param([Parameter(Mandatory = $true)][string]$ReleaseDir)
    $result = New-ValidationResult
    $map = Get-PayloadMap -ReleaseDir $ReleaseDir
    foreach ($name in $map.Keys) {
        foreach ($pat in $script:SecretPatterns) {
            if ($name -match $pat) {
                Add-ValidationError $result 'secret' "declared payload name rejected: $name"
                break
            }
        }
        if ($name -eq 'pdfxi.exe' -or $name -eq 'pdfxi-ui.exe') {
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
    foreach ($f in @(Get-ChildItem -LiteralPath $StagingDir -File | Sort-Object -Property Name)) {
        if ($f.Name -eq 'manifest.json') { continue }
        [void]$files.Add([ordered]@{
            path   = $f.Name
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
        binaries          = @('pdfxi.exe')
        # Intentionally no wall-clock timestamp: identical inputs produce a
        # byte-identical manifest, which is what `-SelfTest` asserts.
        files             = $files.ToArray()
        legal             = 'No PDFium DLL, Adobe asset, secret, or private document is bundled. PDFium is notice-only; see THIRD_PARTY.md.'
    }
    $json = $manifest | ConvertTo-Json -Depth 8
    Write-Utf8NoBom (Join-Path $StagingDir 'manifest.json') ($json + "`n")
    return $json
}

# --- packaging -------------------------------------------------------------
function New-Staging {
    param([Parameter(Mandatory = $true)][string]$StagingDir,
          [Parameter(Mandatory = $true)][string]$ReleaseDir)
    if (Test-Path -LiteralPath $StagingDir) { Remove-Item -LiteralPath $StagingDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
    $map = Get-PayloadMap -ReleaseDir $ReleaseDir
    foreach ($name in $map.Keys) {
        $src = $map[$name]
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
            throw "missing payload source for '$name': $src"
        }
        Copy-Item -LiteralPath $src -Destination (Join-Path $StagingDir $name) -Force
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
          [switch]$WithArchive)
    New-Item -ItemType Directory -Force -Path $PackageRoot | Out-Null
    $staging = Join-Path $PackageRoot 'staging'
    New-Staging -StagingDir $staging -ReleaseDir $ReleaseDir
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
    if ($DoBuild) {
        Write-Output 'BUILD cargo build --workspace --release'
        Push-Location $root
        try {
            & cargo build --workspace --release
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit $LASTEXITCODE" }
            Write-Output 'BUILD cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium'
            & cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
            if ($LASTEXITCODE -ne 0) { throw "UI build failed with exit $LASTEXITCODE" }
        } finally { Pop-Location }
    } else {
        Write-Output 'SKIPBUILD using existing release binaries'
    }
    $releaseDir = Join-Path (Resolve-TargetDir $TargetDir) 'release'
    foreach ($binary in @('pdfxi.exe', 'pdfxi-ui.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir $binary) -PathType Leaf)) {
            throw "release binary missing: $(Join-Path $releaseDir $binary) (omit -SkipBuild to build it)"
        }
    }
    $artifacts = New-PackageArtifacts -ReleaseDir $releaseDir -PackageRoot $script:PackageRoot -WithArchive
    Format-Result -Result (Test-PayloadSafety -StagingDir $artifacts.Staging) -Label 'SAFETY'
    Format-Result -Result (Test-Manifest -ManifestPath (Join-Path $script:PackageRoot 'manifest.json') -StagingDir $artifacts.Staging) -Label 'MANIFEST'
    Write-Output ("PACKAGE OK zip={0}" -f $artifacts.Zip)
    Write-Output ("PACKAGE SHA256 {0}" -f (Get-Sha256 $artifacts.Zip))
    Write-Output ("PACKAGE ROOT {0}" -f $script:PackageRoot)
    $script:ExitCode = 0
}

# --- -Check ----------------------------------------------------------------
function Invoke-Check {
    $releaseDir = Join-Path (Resolve-TargetDir $TargetDir) 'release'
    $sourceResult = Test-SourceLayout -ReleaseDir $releaseDir
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
    } else {
        Write-Output "CHECK ui_binary=absent (expected: -Check never builds) $uiBin"
    }
    $allOk = $sourceResult.Ok
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

function Invoke-SelfTest {
    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ('pdfxi-package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
    $passed = 0
    $failed = 0
    try {
        $rel = New-FakeReleaseDir -Sandbox $sandbox
        $out1 = Join-Path $sandbox 'out1'
        $art1 = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out1

        # 1. layout validation: a complete staging tree must pass.
        $r1 = Test-PayloadSafety -StagingDir $art1.Staging
        if ($r1.Ok -and $r1.LayoutOk -and $r1.NoticesOk) {
            Write-Output 'TEST 1 layout_validation PASS'; $passed++
        } else { Format-Result $r1 'TEST 1'; Write-Output 'TEST 1 layout_validation FAIL'; $failed++ }

        # 2. missing-notice failure: removing NOTICE must be rejected.
        $out2 = Join-Path $sandbox 'out2'
        $null = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out2
        Remove-Item -LiteralPath (Join-Path $out2 'staging\NOTICE') -Force
        $r2 = Test-PayloadSafety -StagingDir (Join-Path $out2 'staging')
        if ((-not $r2.Ok) -and (-not $r2.NoticesOk)) {
            Write-Output 'TEST 2 missing_notice_failure PASS'; $passed++
        } else { Format-Result $r2 'TEST 2'; Write-Output 'TEST 2 missing_notice_failure FAIL'; $failed++ }

        # 3. hash manifest determinism: two runs, same inputs, same bytes.
        $out3 = Join-Path $sandbox 'out3'
        $art3 = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out3
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
            -SkipBuild -TargetDir (Split-Path $rel -Parent) -OutputDir $skipOut | Out-Null
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
            -SkipBuild -TargetDir $emptyTarget -OutputDir $negOut 2>&1 | Out-Null
        $negCode = $LASTEXITCODE
        if ($negCode -ne 0) {
            Write-Output "TEST 4b skipbuild_missing_binary_rejected PASS exit=$negCode"; $passed++
        } else { Write-Output 'TEST 4b skipbuild_missing_binary_rejected FAIL'; $failed++ }

        # 5. secret/private-file rejection: planted secret.pem must be refused.
        $out5 = Join-Path $sandbox 'out5'
        $null = New-PackageArtifacts -ReleaseDir $rel -PackageRoot $out5
        Write-Utf8NoBom (Join-Path $out5 'staging\secret.pem') 'not a real key'
        $r5 = Test-PayloadSafety -StagingDir (Join-Path $out5 'staging')
        if ((-not $r5.Ok) -and (-not $r5.SecretFree)) {
            Write-Output 'TEST 5 secret_private_file_rejection PASS'; $passed++
        } else { Format-Result $r5 'TEST 5'; Write-Output 'TEST 5 secret_private_file_rejection FAIL'; $failed++ }

        # 6. source inputs validate cleanly on the real repository.
        $r6 = Test-SourceLayout -ReleaseDir (Join-Path (Resolve-TargetDir $TargetDir) 'release')
        if ($r6.Ok) {
            Write-Output 'TEST 6 source_layout_and_notices PASS'; $passed++
        } else { Format-Result $r6 'TEST 6'; Write-Output 'TEST 6 source_layout_and_notices FAIL'; $failed++ }

        # 7. installer copies the payload without touching the Start Menu.
        $installed = Join-Path $sandbox 'installed'
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $art1.Staging 'install.ps1') `
            -TargetDir $installed -NoShortcut | Out-Null
        $installCode = $LASTEXITCODE
        if ($installCode -eq 0 -and
            (Test-Path -LiteralPath (Join-Path $installed 'pdfxi.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $installed 'NOTICE') -PathType Leaf)) {
            Write-Output "TEST 7 installer_copy PASS exit=$installCode"; $passed++
        } else { Write-Output "TEST 7 installer_copy FAIL exit=$installCode"; $failed++ }
    } finally {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Output ("SELFTEST passed={0} failed={1}" -f $passed, $failed)
    $script:ExitCode = $(if ($failed -eq 0 -and $passed -ge 5) { 0 } else { 1 })
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