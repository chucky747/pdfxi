<#
.SYNOPSIS
    Verify Authenticode signatures on installed PDFXI binaries.

.DESCRIPTION
    Used by `cargo xtask ci` after the installed-build gates. With
    PDFXI_SIGN_THUMBPRINT set, both binaries must have a Valid signature from
    that certificate and the command fails otherwise. Without a thumbprint the
    check reports the unsigned state and passes unless -Require was passed;
    signed builds are never inferred.

.PARAMETER Viewer
    Installed pdfxi-ui.exe. Defaults to the per-user install location.

.PARAMETER Cli
    Installed pdfxi.exe. Defaults to the per-user install location.

.PARAMETER Require
    Fail when no thumbprint is configured or a binary is missing.

.EXAMPLE
    $env:PDFXI_SIGN_THUMBPRINT = '<thumbprint>'
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-signatures.ps1 -Require
#>
[CmdletBinding()]
param(
    [string]$Viewer = '',
    [string]$Cli = '',
    [switch]$Require
)

$ErrorActionPreference = 'Stop'
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\PDFXI'
if (-not $Viewer) { $Viewer = Join-Path $installDir 'pdfxi-ui.exe' }
if (-not $Cli) { $Cli = Join-Path $installDir 'pdfxi.exe' }

$thumb = $env:PDFXI_SIGN_THUMBPRINT
if (-not $thumb) {
    $message = 'SIGNATURE CHECK skipped: PDFXI_SIGN_THUMBPRINT not set (unsigned build)'
    if ($Require) { throw $message }
    Write-Output $message
    exit 0
}
$normalized = ($thumb -replace '[^0-9A-Fa-f]', '').ToUpperInvariant()

$failed = 0
foreach ($path in @($Viewer, $Cli)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Write-Output "FAIL signature target missing: $path"
        $failed++
        continue
    }
    $sig = Get-AuthenticodeSignature -LiteralPath $path
    $actual = if ($sig.SignerCertificate) { $sig.SignerCertificate.Thumbprint } else { '' }
    if ($sig.Status -eq 'Valid' -and $actual -eq $normalized) {
        Write-Output "PASS signed and valid: $path"
    } else {
        Write-Output "FAIL signature: $path status=$($sig.Status) signer=$actual"
        $failed++
    }
}

if ($failed -gt 0) {
    Write-Output "SIGNATURE CHECK FAILED: $failed target(s)"
    exit 1
}
Write-Output "SIGNATURE CHECK PASS: thumbprint=$normalized"
exit 0
