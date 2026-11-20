<#
.SYNOPSIS
Fetch the pinned prebuilt PDFium binary for Windows x64 into a local cache.

.DESCRIPTION
One release tag and one archive SHA-256 are pinned. The archive is hashed
before anything is extracted; a mismatch fails closed and leaves any existing
cache untouched. There is no `releases/latest`, no hash-less download, and no
change to the user's `Path` or any other persistent environment variable.

Release builds load `pdfium.dll` from the directory of the running executable
only (see `crates/pdfxi-render/src/pdfium_backend.rs`). This cache is for
developers and tests, which additionally honour `PDFIUM_PATH` and
`%LOCALAPPDATA%\pdfxi\pdfium\bin`.

.EXAMPLE
powershell -NoProfile -File scripts/fetch-pdfium.ps1
.EXAMPLE
powershell -NoProfile -File scripts/fetch-pdfium.ps1 -Destination "$env:LOCALAPPDATA\pdfium-cache" -Force
#>
[CmdletBinding()]
param(
    [string]$Destination = '',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

# --- pinned inputs ---------------------------------------------------------
$PdfiumTag = 'chromium/8165'
$PdfiumAsset = 'pdfium-win-x64.tgz'
$PdfiumSha256 = 'ca7338ae7779b8b1b82f4580305c4d762d214f8df1d54b47647fe3cc75989ded'
$PdfiumBuild = '8165'
$PdfiumUrl = 'https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F8165/pdfium-win-x64.tgz'

# The cache lives under the per-user local app data directory unless the caller
# names another location; no machine-specific path is hard-coded.
$base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { $env:USERPROFILE }
$dir = if ($Destination) {
    [System.IO.Path]::GetFullPath($Destination)
} else {
    Join-Path $base 'pdfxi\pdfium'
}
$dll = Join-Path $dir 'bin\pdfium.dll'

if (-not $Force -and (Test-Path $dll) -and (Test-Path (Join-Path $dir 'LICENSE')) -and (Test-Path (Join-Path $dir 'licenses'))) {
    $version = if (Test-Path (Join-Path $dir 'VERSION')) { (Get-Content (Join-Path $dir 'VERSION') -Raw).Trim() } else { 'unknown' }
    Write-Output "PDFium already cached: $dll"
    Write-Output "Release $PdfiumTag ($version); use -Force to re-download."
    return
}
Write-Output "Downloading pinned PDFium $PdfiumTag ($PdfiumAsset)..."
$download = Join-Path ([System.IO.Path]::GetTempPath()) ("pdfium-$PdfiumBuild-" + [guid]::NewGuid().ToString('N') + '.tgz')
try {
    Invoke-WebRequest -Uri $PdfiumUrl -OutFile $download -UseBasicParsing

    $actual = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $PdfiumSha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch for $PdfiumAsset from ${PdfiumTag}: expected $PdfiumSha256, got $actual. Nothing was extracted."
    }

    # Extract into a staging directory, validate it, then replace the cache.
    $staging = Join-Path $dir ('..\' + 'pdfium-staging-' + [guid]::NewGuid().ToString('N'))
    $staging = [System.IO.Path]::GetFullPath($staging)
    try {
        New-Item -ItemType Directory -Force -Path $staging | Out-Null
        & tar -xzf $download -C $staging
        if ($LASTEXITCODE -ne 0) { throw "tar failed with exit $LASTEXITCODE" }

        $stagedDll = Join-Path $staging 'bin\pdfium.dll'
        if (-not (Test-Path $stagedDll)) { throw 'archive does not contain bin\pdfium.dll' }
        if (-not (Test-Path (Join-Path $staging 'LICENSE'))) { throw 'archive does not contain LICENSE' }
        if (-not (Test-Path (Join-Path $staging 'licenses'))) { throw 'archive does not contain licenses\' }

        $versionFile = Join-Path $staging 'VERSION'
        if ((Test-Path $versionFile) -and ((Get-Content -LiteralPath $versionFile -Raw) -notmatch "BUILD=$PdfiumBuild")) {
            throw "archive VERSION does not report BUILD=$PdfiumBuild"
        }

        if (Test-Path $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
        New-Item -ItemType Directory -Force -Path (Split-Path $dir -Parent) | Out-Null
        Move-Item -LiteralPath $staging -Destination $dir
        $staging = ''
    } finally {
        if ($staging -and (Test-Path $staging)) { Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue }
    }
} finally {
    if (Test-Path $download) { Remove-Item -LiteralPath $download -Force -ErrorAction SilentlyContinue }
}

if (-not (Test-Path $dll)) { throw "PDFium DLL missing at $dll after extraction" }
$DllSha256 = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "PDFium ready: $dll"
Write-Output "Pinned release: $PdfiumTag"
Write-Output "Archive SHA-256: $PdfiumSha256"
Write-Output "DLL SHA-256: $DllSha256"
Write-Output 'Your Path was not modified. Release builds load pdfium.dll from the executable directory.'