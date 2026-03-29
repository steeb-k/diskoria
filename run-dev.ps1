# Runs the Rust app from jf-storage-tester/. `cargo run` always recompiles when sources (or
# Cargo.toml) change, then executes the fresh binary. It does not rebuild for unrelated file edits.
# Optional: $env:JFST_FORCE_CLEAN = "1" to `cargo clean -p jf-storage-tester` before run (slow).
$ErrorActionPreference = "Stop"
$CargoDir = Join-Path $PSScriptRoot "jf-storage-tester"
if (-not (Test-Path (Join-Path $CargoDir "Cargo.toml"))) {
    Write-Error "Expected Cargo.toml at $CargoDir"
}
Push-Location $CargoDir
try {
    if ($env:JFST_FORCE_CLEAN -eq "1") {
        Write-Host "[run-dev] JFST_FORCE_CLEAN=1: cargo clean -p jf-storage-tester" -ForegroundColor Yellow
        cargo clean -p jf-storage-tester
    }
    if (-not $env:RUST_LOG) {
        $env:RUST_LOG = "jf_surface=debug,warn"
    }
    Write-Host "[run-dev] RUST_LOG=$($env:RUST_LOG)" -ForegroundColor Cyan
    Write-Host "[run-dev] $CargoDir -> cargo run (rebuilds when Rust sources change)" -ForegroundColor DarkGray
    cargo run
} finally {
    Pop-Location
}
