<#
.SYNOPSIS
    Build and run the T-0260B real-corpus open / edit / save measurement.

.DESCRIPTION
    Compiles `T-0260B-measure.rs` against the lane's release artifacts of the
    shipping crates (`pdfxi-core`, `pdfxi-document`, `pdfxi-edit`) and runs it on
    `fixtures/realworld/**`. Nothing in the workspace is edited and no committed
    fixture is written: saves go to the scratch directory under `target/tmp`.

    The runner exists because `pdfxi-testkit` does not depend on
    `pdfxi-document`, and adding a benchmark binary to a shipping crate would
    change its manifest for a measurement-only task.

.PARAMETER Lane
    Lane target directory (`scripts/env.ps1 -Lane <name>`). Defaults to `qa`.

.PARAMETER Fixtures
    Real-world corpus directory. Defaults to `fixtures/realworld`.

.PARAMETER Scratch
    Writable scratch directory for the save case. Defaults to
    `target/tmp/t0260b-save`.

.EXAMPLE
    pwsh -File docs/evidence/acceptance/T-0260B-measure.ps1
#>
[CmdletBinding()]
param(
    [string]$Lane = 'qa',
    [string]$Fixtures = 'fixtures/realworld',
    [string]$Scratch = 'target/tmp/t0260b-save'
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
Push-Location $root
try {
    . (Join-Path $root 'scripts\env.ps1') -Lane $Lane | Out-Null
    $release = Join-Path $env:CARGO_TARGET_DIR 'release'
    Write-Output "== building release libraries (lane=$Lane)"
    & cargo build --release -p pdfxi-core -p pdfxi-document -p pdfxi-edit
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $outDir = Join-Path $root 'target\tmp\t0260b'
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    $exe = Join-Path $outDir 'T-0260B-measure.exe'

    # Every directory that holds a linkable artifact is a search path: rlibs for
    # libraries, proc-macro DLLs for derives, and static libraries for native
    # build scripts. Cargo 1.100 keeps dependencies under
    # `release/build/<pkg>/<hash>/out`, while older layouts use `release/deps`;
    # collecting directories covers both.
    $searchDirs = Get-ChildItem -Path $release -Recurse -File |
        Where-Object { $_.Extension -in @('.rlib', '.dll', '.a', '.lib') } |
        ForEach-Object { $_.DirectoryName } |
        Sort-Object -Unique
    if (-not $searchDirs) { throw "no rlibs found under $release" }

    $rsp = Join-Path $env:TEMP ("opencode\t0260b-rustc-{0}.rsp" -f $PID)
    # rustc reads one argument per line from a response file and performs no
    # quote stripping, so each line below is a complete argument.
    $argsList = @(
        '--edition', '2024',
        '--crate-name', 't0260b_measure',
        '-C', 'opt-level=3',
        '-C', 'debug-assertions=off',
        '-C', 'codegen-units=1',
        # The workspace release profile uses thin LTO, so dependency rlibs carry
        # LLVM bitcode rather than machine code; the consumer must run the same
        # LTO mode or GNU ld rejects the archive.
        '-C', 'lto=thin',
        '-C', 'strip=symbols',
        '-o', $exe
    )
    foreach ($dir in $searchDirs) { $argsList += "-Ldependency=$dir" }
    foreach ($crate in @('pdfxi_core', 'pdfxi_document', 'pdfxi_edit')) {
        $rlib = Join-Path $release ("lib{0}.rlib" -f $crate)
        if (-not (Test-Path $rlib)) { throw "missing release artifact $rlib" }
        $argsList += "--extern=$crate=$rlib"
    }
    Set-Content -Path $rsp -Value $argsList -Encoding ascii

    Write-Output "== compiling $exe (search_paths=$($searchDirs.Count))"
    & rustc "@$rsp" (Join-Path $PSScriptRoot 'T-0260B-measure.rs')
    if ($LASTEXITCODE -ne 0) { throw "rustc failed with exit code $LASTEXITCODE" }

    Write-Output "== measuring $Fixtures"
    & $exe (Join-Path $root $Fixtures) (Join-Path $root $Scratch)
    $code = $LASTEXITCODE
    Write-Output "measurement_exit_code=$code"
    exit $code
}
finally {
    Pop-Location
}
