<#
.SYNOPSIS
    Trust (or untrust) the local PDFXI self-signed code-signing certificate.

.DESCRIPTION
    A self-signed certificate verifies only on machines that trust it. This
    script adds the public certificate to the current user's Root and
    TrustedPublisher stores through the .NET certificate store API, which does
    not open a UI prompt, so it works unattended.

    -Remove deletes the certificate from both trust stores. It never touches the
    private key in the personal store.

.PARAMETER Thumbprint
    Certificate thumbprint. Defaults to $env:PDFXI_SIGN_THUMBPRINT.

.PARAMETER Remove
    Remove the public certificate from the trust stores.

.EXAMPLE
    $env:PDFXI_SIGN_THUMBPRINT = '<thumbprint>'
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/trust-signing-cert.ps1
#>
[CmdletBinding()]
param(
    [string]$Thumbprint = '',
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'
if (-not $Thumbprint) { $Thumbprint = $env:PDFXI_SIGN_THUMBPRINT }
if (-not $Thumbprint) { throw 'thumbprint required: pass -Thumbprint or set PDFXI_SIGN_THUMBPRINT' }
$normalized = ($Thumbprint -replace '[^0-9A-Fa-f]', '').ToUpperInvariant()

function Open-UserStore {
    param([string]$Name)
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store(
        $Name,
        [System.Security.Cryptography.X509Certificates.StoreLocation]::CurrentUser)
    $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
    return $store
}

if ($Remove) {
    foreach ($name in @('Root', 'TrustedPublisher')) {
        $store = Open-UserStore $name
        try {
            $matches = @($store.Certificates | Where-Object { $_.Thumbprint -eq $normalized })
            foreach ($found in $matches) { $store.Remove($found) }
        } finally {
            $store.Close()
        }
    }
    Write-Output "UNTRUSTED thumbprint=$normalized (CurrentUser Root + TrustedPublisher)"
    exit 0
}

$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert -ErrorAction SilentlyContinue |
    Where-Object { $_.Thumbprint -eq $normalized } |
    Select-Object -First 1
if (-not $cert) { throw "code-signing certificate not found in Cert:\CurrentUser\My: $normalized" }

foreach ($name in @('Root', 'TrustedPublisher')) {
    $store = Open-UserStore $name
    try {
        $existing = @($store.Certificates | Where-Object { $_.Thumbprint -eq $normalized })
        if (-not $existing) { $store.Add($cert) }
    } finally {
        $store.Close()
    }
}
Write-Output "TRUSTED thumbprint=$normalized (CurrentUser Root + TrustedPublisher)"
