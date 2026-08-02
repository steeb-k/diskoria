//! pkexec self-elevation for Linux.
//!
//! The Windows build declares `requireAdministrator` in its manifest, so the
//! whole app always runs elevated. The Linux analogue: when started as a
//! normal user, relaunch through `pkexec` (polkit's GUI auth) and let the
//! elevated child run the session while the original process waits and
//! forwards its exit code. Raw block-device access (SMART ioctls, the
//! sector/destructive/speed workers) needs root; sysfs drive enumeration
//! mostly does not.
//!
//! Deliberate non-elevation cases:
//! - already root (`sudo -E cargo run` keeps working for dev),
//! - `--no-elevate` (UI work without an auth prompt),
//! - `DISKORIA_SMOKE` / demo seeding (captures and CI never prompt),
//! - `--minimized` auto-start launches (a polkit prompt at every login is
//!   unacceptable; the tray-only mode runs unelevated and degrades until the
//!   user opens a window — see docs/monitoring.md once the tray phase lands).
//!
//! A declined or failed auth degrades instead of dying: the caller re-claims
//! the single-instance socket and runs unelevated, with per-operation errors
//! where root would have been needed (D6 in the port plan).

#![cfg(target_os = "linux")]

use std::process::Command;

/// Environment variables replayed through the `pkexec env` trampoline.
/// pkexec scrubs the environment; without these the elevated process could
/// not reach the user's display, session bus (file dialogs, notifications),
/// runtime dir (single-instance socket) or data dir (settings, history DB).
const PASSTHROUGH_ENV: &[&str] = &[
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "HOME",
    "RUST_LOG",
    // Compositor IPC sockets — the only way to raise our own window on
    // Wayland (see `compositor_focus`).
    "NIRI_SOCKET",
    "SWAYSOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
];

pub fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Whether this launch should attempt the pkexec relaunch.
pub fn should_elevate(smoke: bool, start_minimized: bool) -> bool {
    !is_elevated()
        && !smoke
        && !crate::demo::config().seeding()
        && !start_minimized
        && !std::env::args().skip(1).any(|a| a == "--no-elevate")
}

/// Build the pkexec argv for the current process. Pure so it can be tested.
fn pkexec_args(exe: &str, app_args: &[String], env_pairs: &[(String, String)]) -> Vec<String> {
    let mut args = Vec::with_capacity(2 + env_pairs.len() + app_args.len());
    args.push("env".to_string());
    for (k, v) in env_pairs {
        args.push(format!("{k}={v}"));
    }
    args.push(exe.to_string());
    args.extend(app_args.iter().cloned());
    args
}

fn passthrough_pairs() -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = PASSTHROUGH_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    // Wayland sessions often leave XAUTHORITY unset, but Xwayland still needs
    // it for the X11 clipboard once we are root (root does not share the
    // user's implicit cookie). Discover the usual locations.
    if !pairs.iter().any(|(k, _)| k == "XAUTHORITY") {
        if let Some(path) = discover_xauthority() {
            pairs.push(("XAUTHORITY".to_string(), path));
        }
    }
    pairs
}

fn discover_xauthority() -> Option<String> {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("xauth") {
                    return Some(e.path().to_string_lossy().into_owned());
                }
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let default = std::path::Path::new(&home).join(".Xauthority");
    default
        .exists()
        .then(|| default.to_string_lossy().into_owned())
}

/// Relaunch this binary elevated and wait for it.
///
/// On success returns the elevated session's exit code — the caller should
/// `std::process::exit` with it. `Err` means the relaunch didn't happen
/// (pkexec missing, auth dismissed or refused) and the caller should continue
/// unelevated.
pub fn relaunch_elevated() -> Result<i32, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?
        .to_string_lossy()
        .into_owned();
    let app_args: Vec<String> = std::env::args().skip(1).collect();
    let args = pkexec_args(&exe, &app_args, &passthrough_pairs());

    log::info!(target: "diskoria", "relaunching elevated via pkexec");
    let status = Command::new("pkexec")
        .args(&args)
        .status()
        .map_err(|e| format!("pkexec not runnable: {e}"))?;

    match status.code() {
        // 126: user dismissed the auth dialog; 127: not authorized / pkexec
        // error. Both mean "no elevated session ran".
        Some(126) => Err("authentication dialog dismissed".to_string()),
        Some(127) => Err("authorization failed".to_string()),
        Some(code) => Ok(code),
        None => Ok(1),
    }
}

/// The uid that launched us, when we are root *because of* pkexec/sudo.
/// `None` when unelevated (we already are that user) or when root logged in
/// directly. This is what makes session access possible: the D-Bus session
/// bus authenticates by peer uid, so an elevated process must act as this
/// user to reach the portal, notifications and the tray.
pub fn session_uid() -> Option<u32> {
    if !is_elevated() {
        return None;
    }
    ["PKEXEC_UID", "SUDO_UID"]
        .iter()
        .find_map(|k| std::env::var(k).ok()?.trim().parse::<u32>().ok())
        .filter(|uid| *uid != 0)
}

/// A `Command` that runs `program` as the invoking user when we are elevated,
/// or plainly when we are not. `setpriv` (util-linux) is preferred; `sudo -u`
/// is the fallback. The session environment is re-exported because `setpriv`
/// keeps our env but the child needs to find the same bus/display.
pub fn command_as_session_user(program: &str) -> std::process::Command {
    let Some(uid) = session_uid() else {
        return std::process::Command::new(program);
    };
    let have = |bin: &str| {
        std::env::var("PATH").is_ok_and(|p| {
            std::env::split_paths(&p).any(|d| d.join(bin).is_file())
        })
    };
    if have("setpriv") {
        let mut c = std::process::Command::new("setpriv");
        c.args([
            format!("--reuid={uid}"),
            format!("--regid={uid}"),
            "--init-groups".to_string(),
            "--".to_string(),
            program.to_string(),
        ]);
        c
    } else if have("sudo") {
        let mut c = std::process::Command::new("sudo");
        c.args(["-n", "-u", &format!("#{uid}"), program]);
        c
    } else {
        std::process::Command::new(program)
    }
}

/// Drop **this thread's** credentials to the invoking user, keeping root as
/// the saved uid so [`restore_thread_privileges`] can take it back.
///
/// Per-thread on purpose: `libc::setresuid` (the glibc wrapper) broadcasts to
/// every thread, which would cost the disk workers their root. The raw syscall
/// changes only the calling thread, and threads cloned from it inherit those
/// credentials — which is how the tray's D-Bus worker ends up running as the
/// user while the rest of the process stays privileged.
///
/// Returns `false` when there is nothing to do (unelevated) or the syscall
/// failed; callers then proceed as-is.
pub fn drop_thread_privileges_to_session_user() -> bool {
    let Some(uid) = session_uid() else {
        return false;
    };
    unsafe {
        // gid first — after dropping uid we could not raise it again.
        if libc::syscall(libc::SYS_setresgid, uid, uid, 0) != 0 {
            return false;
        }
        libc::syscall(libc::SYS_setresuid, uid, uid, 0) == 0
    }
}

/// Undo [`drop_thread_privileges_to_session_user`] on this thread.
pub fn restore_thread_privileges() {
    unsafe {
        let _ = libc::syscall(libc::SYS_setresuid, 0, 0, 0);
        let _ = libc::syscall(libc::SYS_setresgid, 0, 0, 0);
    }
}

/// An elevated session writes root-owned files into the invoking user's XDG
/// data dir (settings.txt, history.db + WAL sidecars). Re-own them to the
/// user pkexec recorded, so later unelevated runs can still read and write
/// them. Runs at every elevated startup — files created mid-session get
/// healed on the next one.
pub fn fix_data_dir_ownership() {
    let Some(uid) = session_uid() else {
        return; // unelevated, or a direct root login — leave ownership alone
    };
    let dir = crate::paths::data_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let _ = std::os::unix::fs::chown(&dir, Some(uid), None);
    for entry in entries.flatten() {
        let _ = std::os::unix::fs::chown(entry.path(), Some(uid), None);
    }
}

#[cfg(test)]
mod tests {
    use super::pkexec_args;

    #[test]
    fn trampoline_order_is_env_then_exe_then_args() {
        let args = pkexec_args(
            "/opt/diskoria/diskoria",
            &["--page".into(), "drive-health".into()],
            &[("DISPLAY".into(), ":0".into()), ("HOME".into(), "/home/u".into())],
        );
        assert_eq!(
            args,
            vec![
                "env",
                "DISPLAY=:0",
                "HOME=/home/u",
                "/opt/diskoria/diskoria",
                "--page",
                "drive-health",
            ]
        );
    }

    #[test]
    fn no_env_still_execs_via_env() {
        let args = pkexec_args("/x", &[], &[]);
        assert_eq!(args, vec!["env", "/x"]);
    }
}
