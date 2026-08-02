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
