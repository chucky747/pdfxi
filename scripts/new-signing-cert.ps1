<#
.SYNOPSIS
    Create (or reuse) a local code-signing certificate for PDFXI builds.

.DESCRIPTION
    Free, machine-local signing. The certificate is created in the current
    user's personal certificate store with the Code Signing EKU and an
    exportable private key. Nothing is committed and no public CA is involved.

    This is not a publicly trusted publisher: other machines will not trust the
    signature unless they install the exported certificate. See
    scripts/trust-signing-cert.ps1 for the local trust step.

.PARAMETER Subject
    Certificate subject. Default: "CN=PDFXI Local Build, O=PDFXI".

.PARAMETER PfxPath
    Optional PFX backup path outside the repository. The private key password
    is read from -PfxPassword or prompted as a secure string.

.PARAMETER Force
    Create a new certificate even when one with the same subject exists.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/new-signing-cert.ps1

.EXAMPLE
    $env:PDFXI_SIGN_THUMBPRINT = '<thumbprint>'
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1
#>
[CmdletBinding()]
param(
    [string]$Subject = 'CN=PDFXI Local Build, O=PDFXI',
    [string]$PfxPath = '',
    [System.Security.SecureString]$PfxPassword,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$existing = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert -ErrorAction SilentlyContinue |
    Where-Object { $_.Subject -eq $Subject } |
    Sort-Object NotAfter -Descending |
    Select-Object -First 1

if ($existing -and -not $Force) {
    $cert = $existing
    Write-Output "REUSED code-signing certificate thumbprint=$($cert.Thumbprint) not_after=$($cert.NotAfter.ToString('yyyy-MM-dd'))"
} else {
    $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $Subject `
        -CertStoreLocation Cert:\CurrentUser\My `
        -KeyAlgorithm RSA `
        -KeyLength 3072 `
        -HashAlgorithm SHA256 `
        -KeyExportPolicy Exportable `
        -NotAfter (Get-Date).AddYears(3)
    Write-Output "CREATED code-signing certificate thumbprint=$($cert.Thumbprint) not_after=$($cert.NotAfter.ToString('yyyy-MM-dd'))"
}

if ($PfxPath) {
    $resolved = [System.IO.Path]::GetFullPath($PfxPath)
    $root = Split-Path $PSScriptRoot -Parent
    if ($resolved.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to write the private-key backup inside the repository: $resolved"
    }
    if (-not $PfxPassword) {
        $PfxPassword = Read-Host -AsSecureString -Prompt 'PFX password'
    }
    $directory = Split-Path $resolved -Parent
    if ($directory -and -not (Test-Path -LiteralPath $directory)) {
        New-Item -ItemType Directory -Force -Path $directory | Out-Null
    }
    Export-PfxCertificate -Cert $cert -FilePath $resolved -Password $PfxPassword | Out-Null
    Write-Output "PFX $resolved"
}

Write-Output "THUMBPRINT=$($cert.Thumbprint)"
Write-Output "NEXT set `$env:PDFXI_SIGN_THUMBPRINT='$($cert.Thumbprint)' and run scripts/trust-signing-cert.ps1"
