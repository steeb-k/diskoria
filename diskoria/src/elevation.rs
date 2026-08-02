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
    PASSTHROUGH_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
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

/// An elevated session writes root-owned files into the invoking user's XDG
/// data dir (settings.txt, history.db + WAL sidecars). Re-own them to the
/// user pkexec recorded, so later unelevated runs can still read and write
/// them. Runs at every elevated startup — files created mid-session get
/// healed on the next one.
pub fn fix_data_dir_ownership() {
    if !is_elevated() {
        return;
    }
    let Some(uid) = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    else {
        return; // launched via sudo/root shell — leave ownership alone
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
