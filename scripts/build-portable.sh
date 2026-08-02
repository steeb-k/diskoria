#!/usr/bin/env bash
# Linux release build: emits the two artifacts the self-updater and humans use
# (see docs/releasing.md):
#   releases/<ver>/diskoria-<ver>-linux-x86_64                  bare binary (updater target)
#   releases/<ver>/diskoria-<ver>-portable-linux-x86_64.tar.gz  binary + polkit policy + .desktop + README
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_dir="$repo_root/diskoria"

version="$(cargo metadata --no-deps --format-version 1 --manifest-path "$cargo_dir/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
arch="$(uname -m)"
out_dir="$repo_root/releases/$version"
bin_name="diskoria-$version-linux-$arch"
tar_name="diskoria-$version-portable-linux-$arch.tar.gz"

echo "[build-portable] building $version for $arch"
cargo build --release --locked --manifest-path "$cargo_dir/Cargo.toml"

mkdir -p "$out_dir"
cp "$cargo_dir/target/release/diskoria" "$out_dir/$bin_name"
strip "$out_dir/$bin_name" 2>/dev/null || true
chmod 755 "$out_dir/$bin_name"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/diskoria-$version"
cp "$out_dir/$bin_name" "$stage/diskoria-$version/diskoria"
cp "$repo_root/linux/com.diskoria.pkexec.policy" \
   "$repo_root/linux/diskoria.desktop" \
   "$repo_root/linux/README.md" \
   "$stage/diskoria-$version/"
tar -C "$stage" -czf "$out_dir/$tar_name" "diskoria-$version"

echo "[build-portable] wrote:"
echo "  $out_dir/$bin_name"
echo "  $out_dir/$tar_name"
