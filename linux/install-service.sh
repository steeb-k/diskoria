#!/usr/bin/env bash
# Install (or remove) the Diskoria root monitoring service.
#
# What this buys you: full SMART/NVMe health at login with no password prompt.
# The desktop app stays unelevated and reads what the service collects.
# Running a sector or destructive test still asks for authentication, which is
# deliberate — that is the point where raw writes happen.
#
#   sudo ./install-service.sh [/path/to/diskoria]   install and start
#   sudo ./install-service.sh --uninstall           stop and remove
#
# With no path, the diskoria binary sitting next to this script is used.
set -euo pipefail

UNIT_NAME="diskoria-monitor.service"
UNIT_DIR="/etc/systemd/system"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "error: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run this with sudo"
command -v systemctl >/dev/null 2>&1 || die "systemd not found; this script only covers systemd systems"

if [ "${1:-}" = "--uninstall" ]; then
  systemctl disable --now "$UNIT_NAME" 2>/dev/null || true
  rm -f "$UNIT_DIR/$UNIT_NAME"
  systemctl daemon-reload
  echo "removed $UNIT_NAME"
  echo "note: collected history in /var/lib/diskoria was left in place; delete it yourself if you want it gone."
  exit 0
fi

# Resolve the binary to run, and pin it by absolute path in the unit — a
# service started at boot has no PATH worth trusting.
bin="${1:-$here/diskoria}"
[ -x "$bin" ] || die "not an executable binary: $bin (pass the path as the first argument)"
bin="$(readlink -f "$bin")"

case "$bin" in
  "$here"/*|/home/*|/tmp/*)
    echo "warning: $bin is in a user-writable location."
    echo "         Anything that can overwrite it gains root at next boot."
    echo "         Consider installing to /usr/local/bin first:"
    echo "             sudo install -m 0755 \"$bin\" /usr/local/bin/diskoria"
    echo "         then re-run: sudo $0 /usr/local/bin/diskoria"
    ;;
esac

sed "s|^ExecStart=.*|ExecStart=$bin --service|" "$here/$UNIT_NAME" > "$UNIT_DIR/$UNIT_NAME"
chmod 0644 "$UNIT_DIR/$UNIT_NAME"

systemctl daemon-reload
systemctl enable --now "$UNIT_NAME"

echo "installed $UNIT_NAME  (ExecStart=$bin --service)"
echo
systemctl --no-pager --full status "$UNIT_NAME" || true
echo
echo "health is collected into /var/lib/diskoria/history.db; the desktop app picks it up automatically."
