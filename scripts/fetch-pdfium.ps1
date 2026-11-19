$ErrorActionPreference = 'Stop'

$dir = Join-Path $env:LOCALAPPDATA 'pdfxi\pdfium'
$dll = Join-Path $dir 'bin\pdfium.dll'

if (-not (Test-Path $dll)) {
    Write-Output 'Fetching PDFium for Windows x64...'
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/bblanchon/pdfium-binaries/releases/latest' -Headers @{ 'User-Agent' = 'pdfxi-bootstrap' }
    $asset = $release.assets | Where-Object { $_.name -eq 'pdfium-win-x64.tgz' } | Select-Object -First 1
    if (-not $asset) { throw 'pdfium-win-x64.tgz not found in the latest release' }
    $tgz = Join-Path $env:TEMP 'pdfium-win-x64.tgz'
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tgz -UseBasicParsing
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    tar -xzf $tgz -C $dir
}

if (-not (Test-Path $dll)) { throw "PDFium DLL missing at $dll" }

$env:PDFIUM_PATH = $dll
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dir\bin*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir\bin", 'User')
}
Write-Output "PDFium ready: $dll"
Write-Output "Version: $(Get-Content (Join-Path $dir 'VERSION') -Raw)"
