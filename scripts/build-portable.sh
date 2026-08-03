#!/usr/bin/env bash
# Linux release build: emits the two artifacts the self-updater and humans use
# (see docs/releasing.md):
#   releases/<ver>/diskoria-<ver>-linux-<arch>                  bare binary (updater target)
#   releases/<ver>/diskoria-<ver>-portable-linux-<arch>.tar.gz  binary + polkit policy + .desktop
#                                                               + systemd units + install-service.sh + README
#
# Usage:
#   ./scripts/build-portable.sh                                   native build
#   ./scripts/build-portable.sh --target aarch64-unknown-linux-gnu  ARM64 cross build
#
# Linux has no x64-on-ARM emulation, so each architecture needs its own binary
# and the updater refuses an asset built for a different one (KI-46). Ship both
# to the same release.
#
# The ARM64 cross build needs:
#   rustup target add aarch64-unknown-linux-gnu
#   the aarch64-linux-gnu-gcc cross toolchain  (rusqlite bundles SQLite, which
#   is C, so `cc` needs a compiler for the target)
set -euo pipefail

target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:?--target needs a triple}"; shift 2 ;;
    --target=*) target="${1#--target=}"; shift ;;
    *) echo "usage: $0 [--target <triple>]" >&2; exit 2 ;;
  esac
done

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_dir="$repo_root/diskoria"

version="$(cargo metadata --no-deps --format-version 1 --manifest-path "$cargo_dir/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"

if [ -n "$target" ]; then
  arch="${target%%-*}"
  build_dir="$cargo_dir/target/$target/release"
  target_args=(--target "$target")
  # font-kit (via plotters) links fontconfig through pkg-config, which cannot
  # cross-compile without a target sysroot. Its build script takes this escape
  # hatch and dlopens libfontconfig at runtime instead — which also makes the
  # resulting binary need nothing beyond libc/libm/libgcc_s at load time.
  # Set only for cross builds, so native and Windows builds are unchanged.
  export RUST_FONTCONFIG_DLOPEN=1
else
  arch="$(uname -m)"
  build_dir="$cargo_dir/target/release"
  target_args=()
fi

out_dir="$repo_root/releases/$version"
bin_name="diskoria-$version-linux-$arch"
tar_name="diskoria-$version-portable-linux-$arch.tar.gz"

echo "[build-portable] building $version for $arch${target:+ (cross: $target)}"
# Build from inside the crate: cargo reads .cargo/config.toml relative to the
# *current directory*, not the manifest path, and that config carries the
# aarch64 cross linker. Running it from the repo root silently ignored it and
# tried to link ARM objects with the x86-64 linker.
(cd "$cargo_dir" && cargo build --release --locked "${target_args[@]}")

mkdir -p "$out_dir"
cp "$build_dir/diskoria" "$out_dir/$bin_name"
# Cross builds need the matching strip; plain `strip` cannot read a foreign ELF.
if [ -n "$target" ] && command -v "${target%%-unknown*}-linux-gnu-strip" >/dev/null 2>&1; then
  "${target%%-unknown*}-linux-gnu-strip" "$out_dir/$bin_name" 2>/dev/null || true
elif [ -z "$target" ]; then
  strip "$out_dir/$bin_name" 2>/dev/null || true
fi
chmod 755 "$out_dir/$bin_name"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/diskoria-$version"
cp "$out_dir/$bin_name" "$stage/diskoria-$version/diskoria"
cp "$repo_root/linux/com.diskoria.pkexec.policy" \
   "$repo_root/linux/diskoria.desktop" \
   "$repo_root/linux/diskoria-monitor.service" \
   "$repo_root/linux/diskoria-tray.service" \
   "$repo_root/linux/install-service.sh" \
   "$repo_root/linux/README.md" \
   "$stage/diskoria-$version/"
chmod 755 "$stage/diskoria-$version/install-service.sh"
tar -C "$stage" -czf "$out_dir/$tar_name" "diskoria-$version"

echo "[build-portable] wrote:"
echo "  $out_dir/$bin_name"
echo "  $out_dir/$tar_name"
file "$out_dir/$bin_name" | sed 's/^/  /'
