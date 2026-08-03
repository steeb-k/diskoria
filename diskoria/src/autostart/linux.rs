//! Launch-at-startup via an XDG autostart `.desktop` entry.
//!
//! The entry lives in the *invoking user's* `~/.config/autostart` — resolved
//! through `PKEXEC_UID`/`SUDO_UID` when the toggle is flipped from an
//! elevated session, so the file lands in (and is owned by) the real user's
//! home, not root's. The `Exec` line passes `--minimized`; such launches skip
//! the pkexec relaunch (no polkit prompt at login) and run tray-only,
//! unelevated, with degraded monitoring until the user opens a window.

use std::path::PathBuf;

const ENTRY_NAME: &str = "diskoria.desktop";

/// The uid/gid to own the autostart file: the pkexec/sudo invoking user when
/// running elevated, else the current euid. Pure-ish resolution so it can be
/// unit-tested via the env-value parser.
pub(crate) fn invoking_uid() -> Option<u32> {
    for var in ["PKEXEC_UID", "SUDO_UID"] {
        if let Some(uid) = std::env::var(var).ok().and_then(|v| parse_uid(&v)) {
            return Some(uid);
        }
    }
    None
}

pub(crate) fn parse_uid(v: &str) -> Option<u32> {
    v.trim().parse().ok()
}

/// Home directory of the invoking user (through pkexec/sudo), else `$HOME`.
fn invoking_home() -> Option<PathBuf> {
    if let Some(uid) = invoking_uid() {
        // Look the home up from passwd; $HOME is passed through the pkexec
        // trampoline, but resolve defensively in case it wasn't.
        let home = unsafe {
            let pw = libc::getpwuid(uid);
            if pw.is_null() {
                None
            } else {
                let dir = std::ffi::CStr::from_ptr((*pw).pw_dir);
                Some(PathBuf::from(dir.to_string_lossy().into_owned()))
            }
        };
        if home.is_some() {
            return home;
        }
    }
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// The systemd *user* unit installed by `linux/install-service.sh`, when it is
/// enabled for login.
///
/// It does the same job as the XDG entry, so when it is present that entry must
/// not also be written — both would launch Diskoria twice at login, and the
/// second launch raises a window rather than doing nothing. The unit wins: it
/// can be started immediately (an XDG entry only takes effect at the *next*
/// login, which is no use when collection is starting now) and it comes back
/// if the tray crashes.
///
/// Detected by the enablement symlink rather than by running `systemctl`, so
/// this stays a cheap filesystem check with no subprocess.
fn tray_unit_enabled() -> bool {
    invoking_home().is_some_and(|home| {
        home.join(".config/systemd/user/default.target.wants/diskoria-tray.service")
            .symlink_metadata()
            .is_ok()
    })
}

fn entry_path() -> Option<PathBuf> {
    Some(invoking_home()?.join(".config/autostart").join(ENTRY_NAME))
}

/// The `.desktop` entry content. Pure so it can be unit-tested.
pub(crate) fn desktop_entry(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Diskoria\n\
         Comment=Background drive monitoring\n\
         Exec=\"{exe}\" --minimized\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Create (enable) or remove (disable) the autostart entry.
pub fn set_enabled(enabled: bool) -> std::io::Result<()> {
    let path = entry_path()
        .ok_or_else(|| std::io::Error::other("cannot resolve the user's home directory"))?;
    if tray_unit_enabled() {
        // Already guaranteed by the systemd user unit. Writing the XDG entry
        // on top would double-launch; removing the unit is not ours to do from
        // here, so say where the switch actually lives.
        if !enabled {
            log::info!(
                target: "diskoria",
                "launch-at-startup is managed by diskoria-tray.service; \
                 disable it with: systemctl --user disable --now diskoria-tray.service"
            );
        }
        return Ok(());
    }
    if enabled {
        let exe = std::env::current_exe()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            // Elevated session: the dir may have just been created root-owned.
            if let Some(uid) = invoking_uid() {
                let _ = std::os::unix::fs::chown(dir, Some(uid), None);
            }
        }
        std::fs::write(&path, desktop_entry(&exe.to_string_lossy()))?;
        if let Some(uid) = invoking_uid() {
            let _ = std::os::unix::fs::chown(&path, Some(uid), None);
        }
        Ok(())
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Whether Diskoria starts at login — by either mechanism.
///
/// The systemd user unit counts: it is what `install-service.sh` sets up, and
/// treating only the XDG entry as "enabled" made the app write a duplicate.
pub fn is_enabled() -> bool {
    tray_unit_enabled() || entry_path().is_some_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_exe_and_minimizes() {
        let e = desktop_entry("/opt/my apps/diskoria");
        assert!(e.contains("Exec=\"/opt/my apps/diskoria\" --minimized"));
        assert!(e.starts_with("[Desktop Entry]"));
        assert!(e.contains("Terminal=false"));
    }

    #[test]
    fn uid_parsing() {
        assert_eq!(parse_uid("1000"), Some(1000));
        assert_eq!(parse_uid(" 1000\n"), Some(1000));
        assert_eq!(parse_uid("nope"), None);
    }
}
