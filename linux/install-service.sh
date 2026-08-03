#!/usr/bin/env bash
# Install (or remove) Diskoria background monitoring.
#
# Two units, always together:
#   diskoria-monitor.service  (system) root collector — reads SMART, writes
#                             /var/lib/diskoria/history.db, nothing else.
#   diskoria-tray.service     (user)   the tray icon — shows that collection is
#                             happening and can stop it.
#
# They are installed as a pair on purpose: drive health should never be
# collected without something in your session saying so and offering an off
# switch. Full health at login with no password; running a sector or
# destructive test still authenticates, which is where a prompt belongs.
#
#   sudo ./install-service.sh [/path/to/diskoria]   install and start both
#   sudo ./install-service.sh --uninstall           stop and remove both
#
# With no path, the diskoria binary sitting next to this script is used.
set -euo pipefail

SYS_UNIT="diskoria-monitor.service"
USER_UNIT="diskoria-tray.service"
SYS_DIR="/etc/systemd/system"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "error: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run this with sudo"
command -v systemctl >/dev/null 2>&1 || die "systemd not found; this script only covers systemd systems"

# The human whose session gets the tray icon. Without one (a real root login)
# there is no session to put an icon in, so the tray half is skipped.
target_user="${SUDO_USER:-}"
if [ -n "$target_user" ] && [ "$target_user" != "root" ]; then
  target_uid="$(id -u "$target_user")"
  target_home="$(getent passwd "$target_user" | cut -d: -f6)"
  user_unit_dir="$target_home/.config/systemd/user"
else
  target_user=""
fi

# Run systemctl --user in the target user's session.
user_systemctl() {
  [ -n "$target_user" ] || return 0
  sudo -u "$target_user" \
    XDG_RUNTIME_DIR="/run/user/$target_uid" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$target_uid/bus" \
    systemctl --user "$@"
}

if [ "${1:-}" = "--uninstall" ]; then
  user_systemctl disable --now "$USER_UNIT" 2>/dev/null || true
  [ -n "$target_user" ] && rm -f "$user_unit_dir/$USER_UNIT"
  user_systemctl daemon-reload 2>/dev/null || true

  systemctl disable --now "$SYS_UNIT" 2>/dev/null || true
  rm -f "$SYS_DIR/$SYS_UNIT"
  systemctl daemon-reload

  echo "removed $SYS_UNIT and $USER_UNIT"
  echo "note: collected history in /var/lib/diskoria was left in place; delete it yourself if you want it gone."
  exit 0
fi

# Resolve the binary to run, and pin it by absolute path in both units — a
# service started at boot has no PATH worth trusting.
bin="${1:-$here/diskoria}"
[ -x "$bin" ] || die "not an executable binary: $bin (pass the path as the first argument)"
bin="$(readlink -f "$bin")"

case "$bin" in
  /usr/local/bin/*|/usr/bin/*|/opt/*) ;;
  *)
    echo "warning: $bin is in a user-writable location."
    echo "         Anything that can overwrite it gains root at next boot."
    echo "         Consider installing it somewhere root-owned first:"
    echo "             sudo install -m 0755 \"$bin\" /usr/local/bin/diskoria"
    echo "         then re-run: sudo $0 /usr/local/bin/diskoria"
    echo
    ;;
esac

# ── System half: the collector ───────────────────────────────────────────────
sed "s|^ExecStart=.*|ExecStart=$bin --service|" "$here/$SYS_UNIT" > "$SYS_DIR/$SYS_UNIT"
chmod 0644 "$SYS_DIR/$SYS_UNIT"
systemctl daemon-reload
systemctl enable --now "$SYS_UNIT"
echo "installed $SYS_UNIT  (ExecStart=$bin --service)"

# ── User half: the tray ──────────────────────────────────────────────────────
if [ -z "$target_user" ]; then
  echo
  echo "warning: no desktop user detected (SUDO_USER unset), so the tray unit was NOT installed."
  echo "         Collection would run with nothing in a session to show or stop it."
  echo "         Re-run this from your normal user account with sudo."
  exit 0
fi

install -d -o "$target_user" -g "$target_user" "$user_unit_dir"
sed "s|^ExecStart=.*|ExecStart=$bin --minimized|" "$here/$USER_UNIT" > "$user_unit_dir/$USER_UNIT"
chown "$target_user":"$target_user" "$user_unit_dir/$USER_UNIT"
chmod 0644 "$user_unit_dir/$USER_UNIT"

# The XDG autostart entry does the same job; leaving both would launch Diskoria
# twice at login. The unit wins — it can be started right now, and it comes
# back if it crashes.
rm -f "$target_home/.config/autostart/diskoria.desktop"

user_systemctl daemon-reload
if user_systemctl enable --now "$USER_UNIT"; then
  echo "installed $USER_UNIT for $target_user  (ExecStart=$bin --minimized)"
else
  echo
  echo "warning: could not start the tray unit for $target_user."
  echo "         If you are not logged in graphically yet, it will start at your next login."
fi

echo
systemctl --no-pager --full status "$SYS_UNIT" || true
echo
echo "Health is collected into /var/lib/diskoria/history.db; the tray icon shows it and can stop it."
