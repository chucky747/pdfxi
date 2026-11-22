<#
.SYNOPSIS
Hermetic regression test for the portable install.ps1 contract.

.DESCRIPTION
Builds a synthetic portable payload (the real scripts/install.ps1 plus stub
binaries, licences, guide pages and a schema-1 SHA-256 manifest) under
target\tmp\portable-install-test, then asserts:

  1. a fresh install copies every manifest-listed file, including install.ps1,
     into a target path that contains a space;
  2. the installed tree verifies against its own manifest;
  3. a second install over an existing tree merges the guide and picks up a new
     page without creating docs\docs;
  4. a legacy docs\docs tree is removed on reinstall;
  5. running the installed script with TargetDir equal to its own directory is
     a safe, idempotent no-op.

No network, no elevation, no Start Menu changes (-NoShortcut). The sandbox is
removed on success and kept for inspection on failure.

.EXAMPLE
powershell -NoProfile -File scripts/test-portable-install.ps1
#>
[CmdletBinding()]
param(
    [string]$WorkDir = '',
    # Path to the installer under test. Defaults to scripts\install.ps1; the
    # regression test passes an older revision here to prove it fails.
    [string]$InstallerPath = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $WorkDir) {
    $WorkDir = Join-Path $root 'target\tmp\portable-install-test'
}
$work = [System.IO.Path]::GetFullPath($WorkDir)

$script:Passed = 0
$script:Failed = 0

function Write-Utf8NoBom {
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$Text)
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Assert-That {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][bool]$Condition,
        [string]$Detail = ''
    )
    if ($Condition) {
        Write-Output "PASS $Name"
        $script:Passed++
    } else {
        Write-Output "FAIL $Name $Detail"
        $script:Failed++
    }
}

function New-TestManifest {
    param([Parameter(Mandatory = $true)][string]$Payload)
    $payloadRoot = (Resolve-Path -LiteralPath $Payload).Path.TrimEnd([char[]]@('\', '/'))
    $files = [System.Collections.ArrayList]::new()
    foreach ($file in @(Get-ChildItem -LiteralPath $payloadRoot -Recurse -File | Sort-Object -Property FullName)) {
        $relative = $file.FullName.Substring($payloadRoot.Length).TrimStart([char[]]@('\', '/'))
        if ($relative -eq 'manifest.json') { continue }
        [void]$files.Add([ordered]@{
            path   = $relative
            sha256 = Get-Sha256 $file.FullName
            bytes  = [long]$file.Length
        })
    }
    $manifest = [ordered]@{
        schema  = 1
        package = 'pdfxi-portable'
        version = '0.0.0-portable-test'
        files   = $files.ToArray()
    }
    $json = $manifest | ConvertTo-Json -Depth 5
    Write-Utf8NoBom (Join-Path $payloadRoot 'manifest.json') ($json + "`n")
}

function Test-InstalledManifest {
    param([Parameter(Mandatory = $true)][string]$Directory)
    $problems = [System.Collections.ArrayList]::new()
    $manifestPath = Join-Path $Directory 'manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        [void]$problems.Add('manifest.json missing')
        return $problems.ToArray()
    }
    $m = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($m.schema -ne 1) {
        [void]$problems.Add("unexpected schema $($m.schema)")
    }
    foreach ($entry in $m.files) {
        $path = Join-Path $Directory $entry.path
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            [void]$problems.Add("missing $($entry.path)")
        } elseif ((Get-Sha256 $path) -ne $entry.sha256) {
            [void]$problems.Add("hash $($entry.path)")
        } elseif ((Get-Item -LiteralPath $path).Length -ne [long]$entry.bytes) {
            [void]$problems.Add("bytes $($entry.path)")
        }
    }
    return $problems.ToArray()
}

function Invoke-Installer {
    param(
        [Parameter(Mandatory = $true)][string]$Script,
        [Parameter(Mandatory = $true)][string]$Target
    )
    if (-not (Test-Path -LiteralPath $Script -PathType Leaf)) {
        return [pscustomobject]@{ ExitCode = 1; Output = @("installer missing: $Script") }
    }
    $output = & powershell -NoProfile -ExecutionPolicy Bypass -File $Script -TargetDir $Target -NoShortcut 2>&1
    return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = @($output) }
}

if (Test-Path -LiteralPath $work) {
    Remove-Item -LiteralPath $work -Recurse -Force
}
$payload = Join-Path $work 'payload'
foreach ($dir in @('docs\tools', 'pdfium-licenses')) {
    New-Item -ItemType Directory -Force -Path (Join-Path $payload $dir) | Out-Null
}
$installerSource = if ($InstallerPath) {
    [System.IO.Path]::GetFullPath($InstallerPath)
} else {
    Join-Path $root 'scripts\install.ps1'
}
if (-not (Test-Path -LiteralPath $installerSource -PathType Leaf)) {
    Write-Output "FAIL installer_source missing $installerSource"
    exit 1
}
Copy-Item -LiteralPath $installerSource -Destination (Join-Path $payload 'install.ps1') -Force
Write-Utf8NoBom (Join-Path $payload 'pdfxi.exe') "MZ portable-test CLI`n"
Write-Utf8NoBom (Join-Path $payload 'pdfxi-ui.exe') "MZ portable-test UI`n"
Write-Utf8NoBom (Join-Path $payload 'pdfium.dll') "MZ portable-test PDFium`n"
foreach ($name in @('LICENSE', 'NOTICE', 'README.md', 'THIRD_PARTY.md', 'deny.toml')) {
    Write-Utf8NoBom (Join-Path $payload $name) "portable test $name`n"
}
Write-Utf8NoBom (Join-Path $payload 'pdfium-licenses\LICENSE') "PDFium BSD-3-Clause stub`n"
Write-Utf8NoBom (Join-Path $payload 'pdfium-licenses\pdfium.txt') "PDFium third-party notice stub`n"
Write-Utf8NoBom (Join-Path $payload 'docs\README.md') "# PDFXI guide`n"
Write-Utf8NoBom (Join-Path $payload 'docs\SUMMARY.md') "# Summary`n"
Write-Utf8NoBom (Join-Path $payload 'docs\security.md') "# Security`n"
Write-Utf8NoBom (Join-Path $payload 'docs\privacy.md') "# Privacy`n"
Write-Utf8NoBom (Join-Path $payload 'docs\tools\convert-to-pdf.md') "# Convert`n`n## Retired capture features`n"
New-TestManifest -Payload $payload

$installed = Join-Path $work 'install with space'
$run1 = Invoke-Installer -Script (Join-Path $payload 'install.ps1') -Target $installed
Assert-That 'fresh_install_exit' ($run1.ExitCode -eq 0) "exit=$($run1.ExitCode)"
$installedScript = Join-Path $installed 'install.ps1'
Assert-That 'fresh_install_copies_installer' (Test-Path -LiteralPath $installedScript -PathType Leaf)
Assert-That 'fresh_install_installer_hash' (
    (Test-Path -LiteralPath $installedScript -PathType Leaf) -and
    ((Get-Sha256 $installedScript) -eq (Get-Sha256 (Join-Path $payload 'install.ps1')))
)
$problems = @(Test-InstalledManifest -Directory $installed)
Assert-That 'fresh_install_manifest' ($problems.Count -eq 0) ($problems -join '; ')
Assert-That 'fresh_install_no_nested_docs' (-not (Test-Path -LiteralPath (Join-Path $installed 'docs\docs')))
Assert-That 'fresh_install_guide_section' (
    (Get-Content -LiteralPath (Join-Path $installed 'docs\tools\convert-to-pdf.md') -Raw).Contains('Retired capture features')
)

# Upgrade: change a guide page, add a page, and reinstall over the existing tree.
Add-Content -LiteralPath (Join-Path $payload 'docs\tools\convert-to-pdf.md') -Value '<!-- upgrade marker -->'
Write-Utf8NoBom (Join-Path $payload 'docs\new-page.md') "# New page`n"
New-TestManifest -Payload $payload
$run2 = Invoke-Installer -Script (Join-Path $payload 'install.ps1') -Target $installed
Assert-That 'upgrade_install_exit' ($run2.ExitCode -eq 0) "exit=$($run2.ExitCode)"
Assert-That 'upgrade_merges_changed_page' (
    (Get-Content -LiteralPath (Join-Path $installed 'docs\tools\convert-to-pdf.md') -Raw).Contains('upgrade marker')
)
Assert-That 'upgrade_adds_new_page' (Test-Path -LiteralPath (Join-Path $installed 'docs\new-page.md') -PathType Leaf)
$problems = @(Test-InstalledManifest -Directory $installed)
Assert-That 'upgrade_manifest' ($problems.Count -eq 0) ($problems -join '; ')
Assert-That 'upgrade_no_nested_docs' (-not (Test-Path -LiteralPath (Join-Path $installed 'docs\docs')))

# Legacy layout: an old installer left docs\docs. A reinstall must remove it.
New-Item -ItemType Directory -Force -Path (Join-Path $installed 'docs\docs\tools') | Out-Null
Write-Utf8NoBom (Join-Path $installed 'docs\docs\tools\stale.md') 'legacy nested copy'
$run3 = Invoke-Installer -Script (Join-Path $payload 'install.ps1') -Target $installed
Assert-That 'legacy_install_exit' ($run3.ExitCode -eq 0) "exit=$($run3.ExitCode)"
Assert-That 'legacy_nested_docs_removed' (-not (Test-Path -LiteralPath (Join-Path $installed 'docs\docs')))
$problems = @(Test-InstalledManifest -Directory $installed)
Assert-That 'legacy_manifest' ($problems.Count -eq 0) ($problems -join '; ')

# In place: run the installed script against its own directory.
$payloadHash = Get-Sha256 (Join-Path $installed 'pdfxi.exe')
$run4 = Invoke-Installer -Script $installedScript -Target $installed
Assert-That 'in_place_exit' ($run4.ExitCode -eq 0) "exit=$($run4.ExitCode)"
Assert-That 'in_place_payload_intact' (
    (Test-Path -LiteralPath (Join-Path $installed 'pdfxi.exe') -PathType Leaf) -and
    ((Get-Sha256 (Join-Path $installed 'pdfxi.exe')) -eq $payloadHash)
)
$problems = @(Test-InstalledManifest -Directory $installed)
Assert-That 'in_place_manifest' ($problems.Count -eq 0) ($problems -join '; ')
Assert-That 'in_place_no_nested_docs' (-not (Test-Path -LiteralPath (Join-Path $installed 'docs\docs')))

Write-Output ("PORTABLE_INSTALL_TEST passed={0} failed={1}" -f $script:Passed, $script:Failed)
if ($script:Failed -gt 0) {
    Write-Output "PORTABLE_INSTALL_TEST workdir=$work"
    exit 1
}
Remove-Item -LiteralPath $work -Recurse -Force
exit 0