#!/usr/bin/env bash
# Diskoria — Linux installer.
#
#   curl -fsSL https://raw.githubusercontent.com/steeb-k/diskoria/main/scripts/install-linux.sh | bash
#
# Downloads the latest release from the binaries repo, installs the executable,
# and registers the desktop entry and polkit policy so the app appears in the
# launcher and can ask for privileges properly. Re-running upgrades in place.
#
# Options (after `| bash -s --`):
#   --version <x.y.z>   install a specific release instead of the latest
#   --prefix <dir>      install root (default /usr/local; binary in <dir>/bin)
#   --with-service      also install the root health collector (systemd)
#   --help
#
# Deliberately does *not* install the background service by default: that is a
# system-wide unit running as root, which is a choice the user should make
# rather than a side effect of installing an app.
set -euo pipefail

REPO="${DISKORIA_RELEASES_REPO:-steeb-k/diskoria-binaries}"
# Override point for a mirror, a fork's own releases, or a local server when
# testing this script end to end without publishing anything.
API_BASE="${DISKORIA_API_BASE:-https://api.github.com}"
PREFIX="${PREFIX:-/usr/local}"
VERSION="latest"
WITH_SERVICE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?--version needs x.y.z}"; shift 2 ;;
    --version=*) VERSION="${1#--version=}"; shift ;;
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --prefix=*) PREFIX="${1#--prefix=}"; shift ;;
    --with-service) WITH_SERVICE=1; shift ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
done

die() { echo "error: $*" >&2; exit 1; }
info() { echo "==> $*"; }

# ── Prerequisites ────────────────────────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1"; }
  fetch_to() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO- "$1"; }
  fetch_to() { wget -q -O "$2" "$1"; }
else
  die "needs curl or wget"
fi
command -v tar >/dev/null 2>&1 || die "needs tar"

# Elevation only for the steps that write outside \$HOME, and only if needed.
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    die "needs root or sudo to install into $PREFIX (or pass --prefix \"\$HOME/.local\")"
  fi
fi

# ── Architecture ─────────────────────────────────────────────────────────────
# Linux has no x86-64-on-ARM emulation, so the wrong binary does not run at all.
# Never substitute a different architecture — the updater takes the same line
# (known-issues KI-46).
case "$(uname -m)" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) die "unsupported architecture: $(uname -m) (Diskoria ships x86_64 and aarch64)" ;;
esac
info "architecture: $ARCH"

# ── Locate the release asset ─────────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
  API="$API_BASE/repos/$REPO/releases/latest"
else
  API="$API_BASE/repos/$REPO/releases/tags/$VERSION"
fi

info "querying $REPO ($VERSION)"
JSON="$(fetch "$API")" || die "could not reach the GitHub API"

TAG="$(printf '%s' "$JSON" | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/')"
[ -n "$TAG" ] || die "no release found for '$VERSION'"

# Prefer the portable tarball: it carries the desktop entry, polkit policy and
# service units. Fall back to the bare binary, which is all the updater needs.
URLS="$(printf '%s' "$JSON" \
  | grep -o '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*"' \
  | sed 's/.*:[[:space:]]*"\([^"]*\)"$/\1/')"

TARBALL="$(printf '%s\n' "$URLS" | grep -E "portable-linux-${ARCH}\.tar\.gz$" | head -1 || true)"
BARE="$(printf '%s\n' "$URLS" | grep -E "linux-${ARCH}$" | head -1 || true)"

if [ -z "$TARBALL" ] && [ -z "$BARE" ]; then
  die "release $TAG has no Linux $ARCH asset.
     Diskoria publishes 'diskoria-<ver>-portable-linux-$ARCH.tar.gz' and
     'diskoria-<ver>-linux-$ARCH'. If this release predates Linux support, or
     ships only the other architecture, there is nothing here this machine can
     run -- installing the wrong one would produce a binary the kernel refuses
     to exec. See https://github.com/$REPO/releases"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ── Download ─────────────────────────────────────────────────────────────────
if [ -n "$TARBALL" ]; then
  info "downloading $(basename "$TARBALL")"
  fetch_to "$TARBALL" "$TMP/diskoria.tar.gz" || die "download failed"
  tar xzf "$TMP/diskoria.tar.gz" -C "$TMP" || die "could not unpack the tarball"
  SRC="$(find "$TMP" -maxdepth 2 -type f -name diskoria -perm -u+x | head -1)"
  [ -n "$SRC" ] || die "no 'diskoria' executable inside the tarball"
  PAYLOAD="$(dirname "$SRC")"
else
  info "downloading $(basename "$BARE")"
  fetch_to "$BARE" "$TMP/diskoria" || die "download failed"
  chmod +x "$TMP/diskoria"
  SRC="$TMP/diskoria"
  PAYLOAD=""
fi

# ── Install ──────────────────────────────────────────────────────────────────
BIN_DIR="$PREFIX/bin"
info "installing to $BIN_DIR/diskoria"
$SUDO install -d "$BIN_DIR"
# `install` replaces by rename, so upgrading while Diskoria is running is safe:
# the old inode stays alive for the running process.
$SUDO install -m 0755 "$SRC" "$BIN_DIR/diskoria"

if [ -n "$PAYLOAD" ]; then
  if [ -f "$PAYLOAD/diskoria.desktop" ]; then
    info "installing desktop entry"
    $SUDO install -d "$PREFIX/share/applications"
    $SUDO install -m 0644 "$PAYLOAD/diskoria.desktop" "$PREFIX/share/applications/diskoria.desktop"
  fi
  # polkit only reads its system directory; --prefix does not apply.
  if [ -f "$PAYLOAD/com.diskoria.pkexec.policy" ]; then
    info "installing polkit policy"
    $SUDO install -d /usr/share/polkit-1/actions
    $SUDO install -m 0644 "$PAYLOAD/com.diskoria.pkexec.policy" \
      /usr/share/polkit-1/actions/com.diskoria.pkexec.policy
  fi
  if [ "$WITH_SERVICE" -eq 1 ]; then
    [ -f "$PAYLOAD/install-service.sh" ] || die "--with-service: the tarball has no install-service.sh"
    info "installing the background collector"
    $SUDO bash "$PAYLOAD/install-service.sh" "$BIN_DIR/diskoria"
  fi
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  $SUDO update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi

echo
info "Diskoria $TAG installed: $BIN_DIR/diskoria"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "    note: $BIN_DIR is not on your PATH" ;;
esac
if [ "$WITH_SERVICE" -eq 0 ] && [ -n "$PAYLOAD" ]; then
  echo "    background health monitoring is not installed; re-run with --with-service to add it"
fi
echo "    disk tests need privileges; Diskoria asks via polkit when it needs them"
