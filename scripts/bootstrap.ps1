<#
.SYNOPSIS
Bootstrap PDFXI on Windows without interactive decisions (T-0132 / AC-01).

.DESCRIPTION
Three modes, all of which avoid network access unless a tool is genuinely
absent and an install is explicitly requested:

  (default)   Install or locate Git, Rust and a working linker, install the
              pinned GNU toolchain, configure git identity and print the
              board. Idempotent: safe to run repeatedly. This is the only
              mode that may download, install or modify the machine.

  -Check      Non-destructive dry run. Discover Git, Rust (rustup/cargo/rustc),
              the linker (gcc) and the pinned toolchain; print resolved paths
              and versions; exit non-zero with clear MISSING diagnostics when
              something is absent. Never installs, downloads, edits PATH,
              touches git config, creates directories or alters the machine.

  -SelfTest   Deterministic offline tests of the check logic (path resolution,
              missing-tool detection, version output, check-mode no-op and the
              success path). Uses a temporary sandbox and no network/GUI.

Check-mode discovery honours three optional override environment variables,
which exist so -SelfTest can inject a sandbox without mutating the machine:

  PDFXI_BOOTSTRAP_CARGO_BIN   default: %USERPROFILE%\.cargo\bin
  PDFXI_BOOTSTRAP_MINGIT      default: %LOCALAPPDATA%\Programs\MinGit
  PDFXI_BOOTSTRAP_WINLIBS     default: %LOCALAPPDATA%\Programs\winlibs

.EXAMPLE
powershell -NoProfile -File scripts/bootstrap.ps1 -Check
.EXAMPLE
powershell -NoProfile -File scripts/bootstrap.ps1 -SelfTest
.EXAMPLE
powershell -NoProfile -File scripts/bootstrap.ps1
#>
[CmdletBinding(DefaultParameterSetName = 'Install')]
param(
    [Parameter(ParameterSetName = 'Check')][switch]$Check,
    [Parameter(ParameterSetName = 'SelfTest')][switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$script:Toolchain = 'stable-x86_64-pc-windows-gnu'

# Test-injectable roots; defaults mirror the install-mode PATH additions.
$cargoBin = if ($env:PDFXI_BOOTSTRAP_CARGO_BIN) { $env:PDFXI_BOOTSTRAP_CARGO_BIN } else { Join-Path $env:USERPROFILE '.cargo\bin' }
$minGit   = if ($env:PDFXI_BOOTSTRAP_MINGIT) { $env:PDFXI_BOOTSTRAP_MINGIT } else { Join-Path $env:LOCALAPPDATA 'Programs\MinGit' }
$winlibs  = if ($env:PDFXI_BOOTSTRAP_WINLIBS) { $env:PDFXI_BOOTSTRAP_WINLIBS } else { Join-Path $env:LOCALAPPDATA 'Programs\winlibs' }

# ---------------------------------------------------------------------------
# Read-only discovery helpers. These never mutate PATH, the filesystem or the
# registry, so they are safe in -Check and reused by -SelfTest.
# ---------------------------------------------------------------------------
function Resolve-Tool {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [string[]]$CandidatePaths = @(),
        [AllowEmptyString()][string]$SearchPath = $env:Path
    )
    # 1) Look along the supplied search path without changing $env:Path.
    if ($SearchPath) {
        foreach ($dir in ($SearchPath -split ';' | Where-Object { $_ })) {
            foreach ($exe in @("$Name.exe", "$Name")) {
                $candidate = Join-Path $dir $exe
                if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                    return [pscustomobject]@{ Name = $Name; Found = $true; Path = $candidate; Source = 'PATH' }
                }
            }
        }
    }
    # 2) Fall back to the locations the bootstrap itself would add to PATH.
    foreach ($candidate in $CandidatePaths) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return [pscustomobject]@{ Name = $Name; Found = $true; Path = $candidate; Source = 'candidate' }
        }
    }
    return [pscustomobject]@{ Name = $Name; Found = $false; Path = ''; Source = '' }
}

function Invoke-ToolCapture {
    param([Parameter(Mandatory = $true)][string]$Path, [string[]]$Arguments = @())
    # PowerShell 5.1 turns native stderr into a terminating error when
    # $ErrorActionPreference is 'Stop'. Scope it to 'Continue' while probing
    # and merge stderr into stdout so informational notes never abort -Check.
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        return @(& $Path @Arguments 2>&1)
    } finally {
        $ErrorActionPreference = $oldEap
    }
}

function Get-VersionLine {
    param([AllowNull()][string[]]$VersionOutput)
    if (-not $VersionOutput) { return '' }
    $line = $VersionOutput | ForEach-Object { [string]$_ } |
            Where-Object { $_.Trim() } | Select-Object -First 1
    if ($line) { return $line.Trim() }
    return ''
}

function Get-ToolVersion {
    param([string]$Path)
    if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '' }
    try {
        $output = Invoke-ToolCapture -Path $Path -Arguments @('--version')
        return (Get-VersionLine -VersionOutput $output)
    } catch {
        return ''
    }
}

function Get-BootstrapPrereqResult {
    param(
        [string]$CargoBin = $cargoBin,
        [string]$MinGit = $minGit,
        [string]$Winlibs = $winlibs,
        [AllowEmptyString()][string]$SearchPath = $env:Path
    )
    $defs = @(
        @{ Key = 'git';    Name = 'git';    Candidates = @((Join-Path $MinGit 'cmd\git.exe')) },
        @{ Key = 'rustup'; Name = 'rustup'; Candidates = @((Join-Path $CargoBin 'rustup.exe')) },
        @{ Key = 'cargo';  Name = 'cargo';  Candidates = @((Join-Path $CargoBin 'cargo.exe')) },
        @{ Key = 'rustc';  Name = 'rustc';  Candidates = @((Join-Path $CargoBin 'rustc.exe')) },
        @{ Key = 'gcc';    Name = 'gcc';    Candidates = @((Join-Path $Winlibs 'bin\gcc.exe')) }
    )
    $tools = [ordered]@{}
    foreach ($def in $defs) {
        $r = Resolve-Tool -Name $def.Name -CandidatePaths $def.Candidates -SearchPath $SearchPath
        $tools[$def.Key] = [pscustomobject][ordered]@{
            Name     = $def.Name
            Found    = $r.Found
            Path     = $r.Path
            Source   = $r.Source
            Expected = ($def.Candidates -join ', ')
            Version  = (Get-ToolVersion -Path $r.Path)
        }
    }
    $toolchainInstalled = $false
    $rustupPath = $tools['rustup'].Path
    if ($rustupPath) {
        foreach ($line in (Invoke-ToolCapture -Path $rustupPath -Arguments @('toolchain', 'list'))) {
            if ([string]$line -like "$($script:Toolchain)*") { $toolchainInstalled = $true; break }
        }
    }
    $missing = [System.Collections.ArrayList]::new()
    foreach ($key in $tools.Keys) {
        if (-not $tools[$key].Found) { [void]$missing.Add($key) }
    }
    if (-not $toolchainInstalled) { [void]$missing.Add("toolchain:$($script:Toolchain)") }
    return [pscustomobject]@{
        Tools              = $tools
        Toolchain          = $script:Toolchain
        ToolchainInstalled = $toolchainInstalled
        Missing            = $missing.ToArray()
        Ok                 = ($missing.Count -eq 0)
    }
}

function Write-Utf8NoBom {
    param([Parameter(Mandatory = $true)][string]$Path,
          [Parameter(Mandatory = $true)][string]$Text)
    $enc = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Text, $enc)
}

# ---------------------------------------------------------------------------
# -Check: print versions/resolved paths, fail loudly when anything is missing.
# ---------------------------------------------------------------------------
function Invoke-Check {
    Write-Output "BOOTSTRAP CHECK (non-destructive) root=$root"
    Write-Output "CANDIDATE cargo=$cargoBin git=$minGit\cmd linker=$winlibs\bin"
    $result = Get-BootstrapPrereqResult
    foreach ($key in $result.Tools.Keys) {
        $tool = $result.Tools[$key]
        if ($tool.Found) {
            Write-Output ("TOOL {0} found=True path={1} version={2}" -f $key, $tool.Path, $tool.Version)
        } else {
            Write-Output ("TOOL {0} found=False path=<none> version=<none> expected_at={1}" -f $key, $tool.Expected)
        }
    }
    Write-Output ("TOOLCHAIN {0} installed={1}" -f $result.Toolchain, $result.ToolchainInstalled)
    if ($result.Ok) {
        Write-Output ("CHECK result=PASS missing=0 verified_tools={0}" -f $result.Tools.Count)
        $script:ExitCode = 0
    } else {
        foreach ($m in $result.Missing) { Write-Output ("MISSING {0}" -f $m) }
        Write-Output ("CHECK result=FAIL missing={0} diagnostics=install or locate the missing items with scripts/bootstrap.ps1 (default mode)" -f $result.Missing.Count)
        $script:ExitCode = 1
    }
}

# ---------------------------------------------------------------------------
# Deterministic offline self-tests of the check logic.
# ---------------------------------------------------------------------------
function Invoke-SelfTest {
    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ('pdfxi-bootstrap-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
    $passed = 0
    $failed = 0
    $psExe = [System.Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
    $origCargo = $env:PDFXI_BOOTSTRAP_CARGO_BIN
    $origGit = $env:PDFXI_BOOTSTRAP_MINGIT
    $origWinlibs = $env:PDFXI_BOOTSTRAP_WINLIBS
    try {
        # TEST 1: path resolution (candidate fallback and explicit search path).
        $fake = Join-Path $sandbox 'fake\tool.exe'
        New-Item -ItemType Directory -Force -Path (Split-Path $fake -Parent) | Out-Null
        Write-Utf8NoBom $fake 'stub'
        $r1a = Resolve-Tool -Name 'tool' -CandidatePaths @($fake) -SearchPath ''
        $pathDir = Join-Path $sandbox 'pathdir'
        New-Item -ItemType Directory -Force -Path $pathDir | Out-Null
        $pathExe = Join-Path $pathDir 'pathtool.exe'
        Write-Utf8NoBom $pathExe 'stub'
        $r1b = Resolve-Tool -Name 'pathtool' -CandidatePaths @() -SearchPath $pathDir
        if ($r1a.Found -and $r1a.Path -eq $fake -and $r1a.Source -eq 'candidate' -and
            $r1b.Found -and $r1b.Path -eq $pathExe -and $r1b.Source -eq 'PATH') {
            Write-Output 'TEST 1 path_resolution PASS'; $passed++
        } else { Write-Output 'TEST 1 path_resolution FAIL'; $failed++ }

        # TEST 2: missing-tool detection (no PATH, no candidates).
        $r2a = Resolve-Tool -Name 'definitely_missing_tool' -CandidatePaths @((Join-Path $sandbox 'nope\tool.exe')) -SearchPath ''
        $empty = Join-Path $sandbox 'empty'
        New-Item -ItemType Directory -Force -Path $empty | Out-Null
        $r2b = Get-BootstrapPrereqResult -CargoBin $empty -MinGit $empty -Winlibs $empty -SearchPath ''
        if ((-not $r2a.Found) -and $r2a.Path -eq '' -and (-not $r2b.Ok) -and
            ($r2b.Missing -contains 'git') -and ($r2b.Missing -contains 'gcc')) {
            Write-Output ("TEST 2 missing_tool_detection PASS missing={0}" -f $r2b.Missing.Count); $passed++
        } else { Write-Output 'TEST 2 missing_tool_detection FAIL'; $failed++ }

        # TEST 3: version output (pure parser + real local tool probe, no network).
        $v3 = Get-VersionLine -VersionOutput @('', 'git version 2.56.0.windows.4', 'trailing text')
        $realGit = Resolve-Tool -Name 'git' -CandidatePaths @((Join-Path $minGit 'cmd\git.exe'))
        $realVer = Get-ToolVersion -Path $realGit.Path
        if ($v3 -eq 'git version 2.56.0.windows.4' -and $realGit.Found -and
            $realVer -like 'git version*') {
            Write-Output ("TEST 3 version_output PASS version={0}" -f $realVer); $passed++
        } else { Write-Output 'TEST 3 version_output FAIL'; $failed++ }

        # TEST 4: check-mode no-op. Run the real script as a child with all
        # candidate roots pointed at an empty sandbox and PATH emptied. -Check
        # must report FAIL (missing) yet create/change nothing.
        $sandboxCargo = Join-Path $sandbox 'cargo'
        $sandboxGit = Join-Path $sandbox 'mingit'
        $sandboxWin = Join-Path $sandbox 'winlibs'
        $emptyBin = Join-Path $sandbox 'emptybin'
        foreach ($d in @($sandboxCargo, $sandboxGit, $sandboxWin, $emptyBin)) {
            New-Item -ItemType Directory -Force -Path $d | Out-Null
        }
        $sentinel = Join-Path $sandbox 'sentinel.txt'
        Write-Utf8NoBom $sentinel 'DO-NOT-CHANGE'
        $snapshot = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object { "$($_.FullName)|$($_.Length)" } | Sort-Object) -join "`n"
        $oldPath = $env:Path
        try {
            $env:PDFXI_BOOTSTRAP_CARGO_BIN = $sandboxCargo
            $env:PDFXI_BOOTSTRAP_MINGIT = $sandboxGit
            $env:PDFXI_BOOTSTRAP_WINLIBS = $sandboxWin
            $env:Path = $emptyBin
            $out4 = & $psExe -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Check 2>&1
            $code4 = $LASTEXITCODE
        } finally {
            $env:Path = $oldPath
        }
        $snapshotAfter = @(Get-ChildItem -LiteralPath $sandbox -Recurse -Force | ForEach-Object { "$($_.FullName)|$($_.Length)" } | Sort-Object) -join "`n"
        $unchanged = ($snapshot -ceq $snapshotAfter) -and ((Get-Content -LiteralPath $sentinel -Raw) -eq 'DO-NOT-CHANGE')
        $installMarkers = @('Installing', 'git init', 'rustup toolchain install', 'Bootstrap complete', 'Configuring git')
        $leaked = $false
        foreach ($marker in $installMarkers) { if ("$out4" -match [regex]::Escape($marker)) { $leaked = $true } }
        if ($code4 -eq 1 -and $unchanged -and (-not $leaked) -and ("$out4" -match 'BOOTSTRAP CHECK')) {
            Write-Output 'TEST 4 check_mode_no_op PASS exit=1 unchanged=True install_markers=0'; $passed++
        } else { Write-Output "TEST 4 check_mode_no_op FAIL exit=$code4 unchanged=$unchanged leaked=$leaked"; $failed++ }

        # TEST 5: success path through the real script entry point (this machine).
        $env:PDFXI_BOOTSTRAP_CARGO_BIN = $origCargo
        $env:PDFXI_BOOTSTRAP_MINGIT = $origGit
        $env:PDFXI_BOOTSTRAP_WINLIBS = $origWinlibs
        if (-not $origCargo) { Remove-Item Env:PDFXI_BOOTSTRAP_CARGO_BIN -ErrorAction SilentlyContinue }
        if (-not $origGit) { Remove-Item Env:PDFXI_BOOTSTRAP_MINGIT -ErrorAction SilentlyContinue }
        if (-not $origWinlibs) { Remove-Item Env:PDFXI_BOOTSTRAP_WINLIBS -ErrorAction SilentlyContinue }
        $out5 = & $psExe -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Check 2>&1
        $code5 = $LASTEXITCODE
        if ($code5 -eq 0 -and ("$out5" -match 'CHECK result=PASS') -and ("$out5" -match 'TOOL git found=True')) {
            Write-Output 'TEST 5 check_success_path PASS exit=0'; $passed++
        } else { Write-Output "TEST 5 check_success_path FAIL exit=$code5"; $failed++ }
    } finally {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Output ("SELFTEST passed={0} failed={1}" -f $passed, $failed)
    $script:ExitCode = $(if ($failed -eq 0 -and $passed -ge 4) { 0 } else { 1 })
}

# ---------------------------------------------------------------------------
# Install mode (original, mutating behaviour). Only reached with no switch.
# ---------------------------------------------------------------------------
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

function Invoke-Install {
    $env:Path = "$cargoBin;$winlibs\bin;$minGit\cmd;$env:Path"
    $env:WINGET_DISABLE_INTERACTIVITY = '1'

    Ensure-MinGit
    Ensure-Rust
    Ensure-Mingw

    $rustup = Join-Path $cargoBin 'rustup.exe'
    & $rustup toolchain install $script:Toolchain --profile minimal
    & $rustup component add rustfmt clippy --toolchain $script:Toolchain
    & $rustup default $script:Toolchain

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
}

# --- dispatch --------------------------------------------------------------
$script:ExitCode = 1
try {
    if ($Check) { Invoke-Check; exit $script:ExitCode }
    if ($SelfTest) { Invoke-SelfTest; exit $script:ExitCode }
    Invoke-Install
    exit 0
} catch {
    Write-Output "BOOTSTRAP ERROR: $($_.Exception.Message)"
    exit 1
}