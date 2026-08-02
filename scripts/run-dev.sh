#!/usr/bin/env bash
# Linux counterpart of run-dev.ps1. `cargo run` always recompiles when sources
# (or Cargo.toml) change, then executes the fresh binary.
# Optional: DISKORIA_FORCE_CLEAN=1 to `cargo clean -p diskoria` before run (slow).
#
# Builds as the invoking user, then the app itself relaunches via pkexec when it
# needs root (or run this script under `sudo -E` to skip the polkit prompt).
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_dir="$repo_root/diskoria"
if [[ ! -f "$cargo_dir/Cargo.toml" ]]; then
    echo "Expected Cargo.toml at $cargo_dir" >&2
    exit 1
fi

cd "$cargo_dir"
if [[ "${DISKORIA_FORCE_CLEAN:-}" == "1" ]]; then
    echo "[run-dev] DISKORIA_FORCE_CLEAN=1: cargo clean -p diskoria"
    cargo clean -p diskoria
fi
export RUST_LOG="${RUST_LOG:-diskoria=debug,warn}"
echo "[run-dev] RUST_LOG=$RUST_LOG"
echo "[run-dev] $cargo_dir -> cargo run (rebuilds when Rust sources change)"
# Forward any flags through to the binary via `cargo run -- <args>`.
exec cargo run -- "$@"
