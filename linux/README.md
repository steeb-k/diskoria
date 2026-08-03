# Diskoria on Linux

Diskoria ships as a portable binary on Linux — no installer. This directory
holds the optional desktop-integration files.

## Privileges

Raw disk access (SMART health, surface scans, the destructive test, and the
benchmark's direct I/O) needs root. On startup as a normal user, Diskoria
relaunches itself through `pkexec` (polkit's graphical authentication).

- **Without any setup** this just works: pkexec prompts for admin
  authentication on each launch.
- **Optional:** install `com.diskoria.pkexec.policy` into
  `/usr/share/polkit-1/actions/` and place the binary at the path pinned in
  the policy (default `/usr/local/bin/diskoria`) for a friendlier prompt that
  keeps the authorization for the rest of the session.
- Declining the prompt (or a system without polkit) runs Diskoria
  unelevated: the drive list still works, but health queries and disk tests
  report that they need root.
- `diskoria --no-elevate` skips the prompt entirely (useful for UI work);
  `sudo -E diskoria` is also fine.

## Background monitoring at login (recommended)

Diskoria's autostart entry launches with `--minimized` and **deliberately does
not elevate** — a polkit prompt at every login would be worse than the problem
it solves. Unelevated, though, it can only read temperatures (via hwmon): no
wear level, no reallocated-sector counts, no predict-fail.

`diskoria-monitor.service` closes that gap. It is a small root daemon that does
nothing but read drive health and write it to `/var/lib/diskoria/history.db`,
which your desktop session reads unprivileged. Full health data at login, no
password.

```sh
sudo install -m 0755 diskoria /usr/local/bin/diskoria   # if not already there
sudo ./install-service.sh /usr/local/bin/diskoria
```

That installs **two** units, deliberately as a pair — drive health should never
be collected without something in your session saying so:

| unit | scope | what it does |
|------|-------|--------------|
| `diskoria-monitor.service` | system (root) | reads SMART, writes `/var/lib/diskoria/history.db`, nothing else |
| `diskoria-tray.service` | user | the tray icon — shows collection is happening and can stop it |

Run it from your normal account with `sudo` (not from a root login), so the
script knows whose session gets the tray icon.

Check it and remove it with:

```sh
systemctl status diskoria-monitor.service
journalctl -u diskoria-monitor.service -f
sudo ./install-service.sh --uninstall
```

The desktop app picks the service up automatically — no setting to flip. The
tray, temperature history, alerts **and the Drive Health page** all work in an
unelevated session while the service is running; the page renders the service's
reading instead of asking you to relaunch as root.

If the service is stopped, readings older than three poll intervals are ignored
and the app falls back to polling for itself, so a dead service shows as missing
data rather than a stale number frozen on screen.

**Turning it off.** The desktop app owns the switch — nothing collects in the
background without a way to see and stop it from userspace:

- **Tray menu** — shows `Background service: Running` and a
  *Stop background collection* item.
- **Settings → Background service** — the same toggle with the current state.

Either one runs `systemctl disable --now`, which stops it *and* keeps it from
returning at the next boot. That goes through systemd's own polkit action, so
an unelevated session gets your desktop's normal authentication prompt. The card
and menu entry are hidden when the unit is not installed.

The unit's state is the source of truth — there is no separate persisted
setting to drift out of sync with it, so `systemctl` on the command line and the
app's toggle always agree.

Some rules follow from *nothing collects without a userspace presence that can
stop it*:

- **Quitting Diskoria stops collection** (`systemctl stop`) — but leaves it
  enabled, so a reboot brings it back. Closing a window is not a decision to
  undo the setup you installed. If you decline the authentication prompt, a
  notification tells you it is still running and how to stop it.
- **Settings → Monitoring → "Enable background monitoring" is the master
  switch.** Turning it off stops *and* disables the service; the service card
  and the tray's Start item follow it. Stopping is never gated.
- **Close-to-tray is forced on while monitoring is enabled**, since the tray is
  where you see and stop collection. Your stored preference is untouched and
  returns when monitoring goes off.
- **The tray is installed as a systemd user unit alongside the collector**, so
  the two move together: `install-service.sh` starts both, an icon appears
  immediately and at every login, and it comes back if it crashes. Settings'
  "Launch at startup" is held on while collection is running. Disable the tray
  half on its own with
  `systemctl --user disable --now diskoria-tray.service` — though with nothing
  in your session to stop it, you probably want to turn collection off instead.

**What it deliberately does not do.** It opens no sockets, accepts no commands,
and never writes to a block device. Starting a sector scan, destructive test or
benchmark still goes through `pkexec`, because that is the point where raw
writes actually happen and an authentication prompt is the right thing to have.
Poll interval is `DISKORIA_POLL_SECS` in the unit file (default 180 s).

Not using systemd? Skip this — everything still works exactly as before, just
with hwmon-only temperatures at login until you open the app.

## Architecture (x86-64 and ARM)

Unlike Windows, Linux has no built-in x86-64-on-ARM emulation, so an x86-64
build **will not run** on an ARM64 machine. Each architecture needs its own
build; `scripts/build-portable.sh` names its output from `uname -m`, so run it
on the machine you are targeting (or cross-compile with the matching Rust
target). The self-updater matches the release asset to the running machine and
refuses to install one built for a different architecture.

## Desktop entry

Copy `diskoria.desktop` to `~/.local/share/applications/` (or
`/usr/share/applications/` system-wide) and put the binary on your `PATH`,
or edit `Exec=` to an absolute path. For the icon, export
`assets/applogo.png` from the repo to
`~/.local/share/icons/hicolor/256x256/apps/diskoria.png` or set an absolute
`Icon=` path.

## Data

Settings and the monitoring history database live in
`$XDG_DATA_HOME/diskoria` (defaults to `~/.local/share/diskoria`). Elevated
sessions re-own these files to the invoking user at startup, so mixed
elevated/unelevated use keeps working.

The optional root service writes separately to `/var/lib/diskoria/history.db`
(world-readable, root-writable). Uninstalling the service leaves that file
behind; delete it yourself if you want the collected history gone.
