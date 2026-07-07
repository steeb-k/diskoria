# Runs the Rust app from diskoria/. `cargo run` always recompiles when sources (or
# Cargo.toml) change, then executes the fresh binary. It does not rebuild for unrelated file edits.
# Optional: $env:DISKORIA_FORCE_CLEAN = "1" to `cargo clean -p diskoria` before run (slow).
$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path $PSScriptRoot -Parent
$CargoDir = Join-Path $RepoRoot "diskoria"
if (-not (Test-Path (Join-Path $CargoDir "Cargo.toml"))) {
    Write-Error "Expected Cargo.toml at $CargoDir"
}
Push-Location $CargoDir
try {
    if ($env:DISKORIA_FORCE_CLEAN -eq "1") {
        Write-Host "[run-dev] DISKORIA_FORCE_CLEAN=1: cargo clean -p diskoria" -ForegroundColor Yellow
        cargo clean -p diskoria
    }
    if (-not $env:RUST_LOG) {
        $env:RUST_LOG = "diskoria=debug,warn"
    }
    Write-Host "[run-dev] RUST_LOG=$($env:RUST_LOG)" -ForegroundColor Cyan
    Write-Host "[run-dev] $CargoDir -> cargo run (rebuilds when Rust sources change)" -ForegroundColor DarkGray
    # Forward any flags through to the binary via `cargo run -- <args>`.
    cargo run -- @args
} finally {
    Pop-Location
}
