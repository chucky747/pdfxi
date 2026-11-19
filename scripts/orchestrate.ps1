param(
    [ValidateSet('claim', 'release', 'board', 'status')]
    [Parameter(Mandatory = $true)][string]$Command,
    [string]$Task,
    [string]$Lane
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) {
    'cargo'
} else {
    Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
}

Push-Location $root
try {
    switch ($Command) {
        'claim' {
            if (-not $Task -or -not $Lane) { throw 'claim needs -Task and -Lane' }
            & $cargo xtask claim $Task --lane $Lane
        }
        'release' {
            if (-not $Task) { throw 'release needs -Task' }
            & $cargo xtask release $Task
        }
        'board' {
            & $cargo xtask board
        }
        'status' {
            & $cargo xtask board
            Get-Content (Join-Path $root 'orchestration\BOARD.md') -Raw
        }
    }
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
