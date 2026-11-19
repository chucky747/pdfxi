# Bootstrap PDFXI on Windows without interactive decisions.
# Idempotent: safe to run repeatedly.

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$minGit = Join-Path $env:LOCALAPPDATA 'Programs\MinGit'
$winlibs = Join-Path $env:LOCALAPPDATA 'Programs\winlibs'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

$env:Path = "$cargoBin;$winlibs\bin;$minGit\cmd;$env:Path"
$env:WINGET_DISABLE_INTERACTIVITY = '1'

function Ensure-MinGit {
    if (Get-Command git -ErrorAction SilentlyContinue) { return }
    if (-not (Test-Path (Join-Path $minGit 'cmd\git.exe'))) {
        Write-Output 'Installing user-local MinGit...'
        $zip = Join-Path $env:TEMP 'MinGit.zip'
        Invoke-WebRequest -Uri 'https://github.com/git-for-windows/git/releases/latest/download/MinGit-2.56.0-64-bit.zip' -OutFile $zip -UseBasicParsing
        New-Item -ItemType Directory -Force -Path $minGit | Out-Null
        Expand-Archive -Path $zip -DestinationPath $minGit -Force
    }
    $env:Path = "$minGit\cmd;$env:Path"
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -notlike "*$minGit\cmd*") {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$minGit\cmd", 'User')
    }
}

function Ensure-Rust {
    if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
        Write-Output 'Installing Rustup...'
        try {
            winget install --id Rustlang.Rustup -e --source winget --accept-package-agreements --accept-source-agreements --silent | Out-Null
        } catch {
            $init = Join-Path $env:TEMP 'rustup-init.exe'
            Invoke-WebRequest -Uri 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile $init -UseBasicParsing
            & $init -y --default-toolchain none
        }
        $env:Path = "$cargoBin;$env:Path"
    }
}

function Ensure-Mingw {
    if (Get-Command gcc -ErrorAction SilentlyContinue) { return }
    if (-not (Test-Path (Join-Path $winlibs 'bin\gcc.exe'))) {
        Write-Output 'Installing user-local WinLibs MinGW...'
        $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/brechtsanders/winlibs_mingw/releases/latest' -Headers @{ 'User-Agent' = 'pdfxi-bootstrap' }
        $asset = $release.assets | Where-Object { $_.name -match 'x86_64.*posix.*seh.*ucrt.*\.zip$' } | Select-Object -First 1
        if (-not $asset) { throw 'No WinLibs asset found' }
        $zip = Join-Path $env:TEMP 'winlibs.zip'
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing
        New-Item -ItemType Directory -Force -Path $winlibs | Out-Null
        Expand-Archive -Path $zip -DestinationPath $winlibs -Force
        $nested = Get-ChildItem $winlibs -Directory | Where-Object { Test-Path (Join-Path $_.FullName 'bin\gcc.exe') } | Select-Object -First 1
        if ($nested) {
            Get-ChildItem $nested.FullName | Move-Item -Destination $winlibs -Force
            Remove-Item $nested.FullName -Recurse -Force
        }
    }
    $env:Path = "$winlibs\bin;$env:Path"
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -notlike "*$winlibs\bin*") {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$winlibs\bin", 'User')
    }
}

Ensure-MinGit
Ensure-Rust
Ensure-Mingw

$rustup = Join-Path $cargoBin 'rustup.exe'
& $rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
& $rustup component add rustfmt clippy --toolchain stable-x86_64-pc-windows-gnu
& $rustup default stable-x86_64-pc-windows-gnu

if (-not (Test-Path (Join-Path $root '.git'))) {
    git init -b main $root
}
$name = git config --global user.name
if (-not $name) { git config --global user.name 'PDFXI Bot' }
$email = git config --global user.email
if (-not $email) { git config --global user.email 'pdfxi@localhost' }

Push-Location $root
try {
    cargo xtask board
    Write-Output ''
    Write-Output 'Bootstrap complete.'
    Write-Output 'Next: cargo xtask goal-verify --milestone M0'
} finally {
    Pop-Location
}
