<#
.SYNOPSIS
    Build and run the T-0260 end-to-end acceptance scenario.

.DESCRIPTION
    Compiles `T-0260-scenario.rs` against the lane's release artifacts of the
    shipping crates, runs it on `fixtures/realworld/**`, then reopens the signed
    output with PDFium through `T-0260-pdfium-reopen.rs`.

    Fixtures are opened read-only. Every output file goes to the scratch
    directory under `target/tmp`.
#>
[CmdletBinding()]
param(
    [string]$Lane = 'qa',
    [string]$Scratch = 'target/tmp/t0260'
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
Push-Location $root
try {
    . (Join-Path $root 'scripts\env.ps1') -Lane $Lane | Out-Null
    $temp = Join-Path $root 'target\tmp\temp'
    New-Item -ItemType Directory -Path $temp -Force | Out-Null
    $env:TEMP = $temp
    $env:TMP = $temp

    Write-Output "== building release libraries (lane=$Lane)"
    & cargo build --release -p pdfxi-core -p pdfxi-document -p pdfxi-edit -p pdfxi-forms -p pdfxi-convert -p pdfxi-security
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    & cargo build --release -p pdfxi-render --features pdfium
    if ($LASTEXITCODE -ne 0) { throw "cargo build (pdfium) failed with exit code $LASTEXITCODE" }

    $release = Join-Path $env:CARGO_TARGET_DIR 'release'
    $outDir = Join-Path $root 'target\tmp\t0260'
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null

    $searchDirs = Get-ChildItem -Path $release -Recurse -File |
        Where-Object { $_.Extension -in @('.rlib', '.dll', '.a', '.lib') } |
        ForEach-Object { $_.DirectoryName } |
        Sort-Object -Unique
    if (-not $searchDirs) { throw "no rlibs found under $release" }

    function Compile-Program {
        param(
            [string]$Source,
            [string]$CrateName,
            [string]$Exe,
            [string[]]$Crates
        )
        $rsp = Join-Path $env:TEMP ("t0260-rustc-{0}.rsp" -f $CrateName)
        $argsList = @(
            '--edition', '2024',
            '--crate-name', $CrateName,
            '-C', 'opt-level=3',
            '-C', 'debug-assertions=off',
            '-C', 'codegen-units=1',
            '-C', 'lto=thin',
            '-C', 'strip=symbols',
            '-o', $Exe
        )
        foreach ($dir in $searchDirs) { $argsList += "-Ldependency=$dir" }
        foreach ($crate in $Crates) {
            $rlib = Join-Path $release ("lib{0}.rlib" -f $crate)
            if (-not (Test-Path $rlib)) { throw "missing release artifact $rlib" }
            $argsList += "--extern=$crate=$rlib"
        }
        Set-Content -Path $rsp -Value $argsList -Encoding ascii
        & rustc "@$rsp" $Source
        if ($LASTEXITCODE -ne 0) { throw "rustc failed for $CrateName with exit code $LASTEXITCODE" }
    }

    Write-Output "== compiling the scenario program"
    Compile-Program -Source (Join-Path $PSScriptRoot 'T-0260-scenario.rs') `
        -CrateName 't0260_scenario' -Exe (Join-Path $outDir 'T-0260-scenario.exe') `
        -Crates @('pdfxi_core', 'pdfxi_document', 'pdfxi_edit', 'pdfxi_forms', 'pdfxi_convert', 'pdfxi_security')

    Write-Output "== compiling the PDFium reopen program"
    Compile-Program -Source (Join-Path $PSScriptRoot 'T-0260-pdfium-reopen.rs') `
        -CrateName 't0260_pdfium_reopen' -Exe (Join-Path $outDir 'T-0260-pdfium-reopen.exe') `
        -Crates @('pdfxi_render', 'pdfxi_text')

    Write-Output "== running the acceptance scenario"
    & (Join-Path $outDir 'T-0260-scenario.exe') $root (Join-Path $root $Scratch)
    $scenarioExit = $LASTEXITCODE
    Write-Output "scenario_exit_code=$scenarioExit"

    # A release build loads PDFium only from the executable directory (by
    # design, so a planted DLL elsewhere cannot load). Copy the locally
    # installed library beside the reopen executable.
    $dllSource = if ($env:PDFIUM_PATH -and (Test-Path $env:PDFIUM_PATH)) {
        $env:PDFIUM_PATH
    } else {
        Join-Path $env:LOCALAPPDATA 'pdfxi\pdfium\bin\pdfium.dll'
    }
    if (Test-Path $dllSource) {
        Copy-Item -LiteralPath $dllSource -Destination (Join-Path $outDir 'pdfium.dll') -Force
    } else {
        Write-Output "pdfium library not found at $dllSource"
    }

    $signed = Join-Path $root (Join-Path $Scratch 't0260-signed.pdf')
    if (Test-Path $signed) {
        Write-Output "== reopening the signed output in PDFium"
        & (Join-Path $outDir 'T-0260-pdfium-reopen.exe') $signed
        if ($LASTEXITCODE -ne 0) { throw "PDFium reopen failed with exit code $LASTEXITCODE" }
    } else {
        Write-Output "signed output missing; PDFium reopen skipped"
    }
    exit $scenarioExit
}
finally {
    Pop-Location
}