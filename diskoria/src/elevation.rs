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

/// `--no-elevate` on the command line: run unelevated on purpose.
fn no_elevate_flag() -> bool {
    std::env::args().skip(1).any(|a| a == "--no-elevate")
}

/// Whether this launch should attempt the pkexec relaunch.
pub fn should_elevate(smoke: bool, start_minimized: bool) -> bool {
    !is_elevated()
        && !smoke
        && !crate::demo::config().seeding()
        && !start_minimized
        && !no_elevate_flag()
}

/// Whether a process in this state may put a **window** on screen without
/// being root.
///
/// `should_elevate` deliberately skips the relaunch for `--minimized`, so the
/// tray/autostart launch does not prompt at login. The cost was that the same
/// unelevated process then answered every later "show me Diskoria" — the
/// single-instance guard hands off to whatever is already running — and opened
/// a window whose SMART reads all fail with EACCES. That is what shipped in
/// 1.7.0: `query_nvme: open failed ... Permission denied` behind a window that
/// looked perfectly normal (KI-36, KI-55).
///
/// So the rule is narrower than "may this process exist": a *window* requires
/// root, unless this is deliberately an unelevated run — tests, demos, smoke.
/// Kept as a separate predicate from [`should_elevate`] because the answers
/// differ for exactly one case, the `--minimized` tray process, which is
/// allowed to run but not to open a window.
pub fn window_allowed_unelevated() -> bool {
    window_allowed_unelevated_with(
        is_elevated(),
        std::env::var_os("DISKORIA_SMOKE").is_some(),
        crate::demo::config().seeding(),
        no_elevate_flag(),
    )
}

/// Internal flag marking the elevated process spawned by [`spawn_elevated_window`].
///
/// It carries two exemptions: skip the single-instance guard, and skip the
/// tray. Without the first it would connect to the socket the tray process
/// still holds and be handed straight back to it — the same unelevated window,
/// via an infinite round trip. Without the second there would be two tray
/// icons for one app.
pub const ELEVATED_WINDOW_FLAG: &str = "--elevated-window";

pub fn is_elevated_window_child() -> bool {
    std::env::args().skip(1).any(|a| a == ELEVATED_WINDOW_FLAG)
}

/// Run an elevated instance that owns a window, and wait for it.
///
/// **Blocking — worker thread only.** `watchdog.rs` exists because event-loop
/// stalls were diagnosed wrong twice; waiting on a polkit prompt from the loop
/// would be the worst case of it, since the wait is as long as the user takes
/// to type a password.
///
/// The caller keeps running as tray-only throughout, so a dismissed prompt
/// costs nothing: no window opens, the tray stays, monitoring continues.
/// Spawning and exiting instead would leave a declined prompt with no tray and
/// no collector at all — worse than the bug this fixes (KI-55).
pub fn spawn_elevated_window() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?
        .to_string_lossy()
        .into_owned();
    let app_args = vec![ELEVATED_WINDOW_FLAG.to_string()];
    let args = pkexec_args(&exe, &app_args, &passthrough_pairs());

    log::info!(target: "diskoria", "opening an elevated window via pkexec");
    let status = Command::new("pkexec")
        .args(&args)
        .status()
        .map_err(|e| format!("pkexec not runnable: {e}"))?;

    match status.code() {
        Some(126) => Err("authentication dialog dismissed".to_string()),
        Some(127) => Err("authorization failed".to_string()),
        // Any other code means the elevated window ran and has now closed.
        _ => Ok(()),
    }
}

/// The rule itself, taking its inputs as arguments so it can be tested without
/// a uid, a command line or an environment.
fn window_allowed_unelevated_with(
    elevated: bool,
    smoke: bool,
    demo: bool,
    no_elevate: bool,
) -> bool {
    elevated || smoke || demo || no_elevate
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

    use super::window_allowed_unelevated_with as may_open;

    /// The 1.7.0 bug, as a test. `install-service.sh` enables
    /// `diskoria-tray.service`, which runs `--minimized` and unelevated on
    /// purpose; the single-instance guard then handed every later launch to
    /// that process, which opened a window that could not read a single
    /// device. Nothing was wrong with the elevation code — it was simply never
    /// consulted again after login.
    #[test]
    fn a_plain_unelevated_process_may_not_open_a_window() {
        assert!(
            !may_open(false, false, false, false),
            "an unelevated session with no opt-out must not put a window on screen"
        );
    }

    /// The counterpart the user asked for: an already-root process must never
    /// be sent back through pkexec, or Ctrl+N and the tray's New Window would
    /// prompt for a password each time. `should_elevate` short-circuits on
    /// `is_elevated`, and new windows are built in-process, but pin it here so
    /// neither can regress quietly.
    #[test]
    fn an_elevated_process_never_needs_to_ask_again() {
        assert!(may_open(true, false, false, false));
        // Still true with every other input off — being root is sufficient on
        // its own, whatever else is set.
        assert!(may_open(true, true, true, true));
    }

    /// Unelevated runs stay available on purpose: `--no-elevate` for UI work
    /// without an auth prompt, demo seeding for captures, smoke for CI.
    #[test]
    fn deliberate_unelevated_runs_keep_working() {
        assert!(may_open(false, true, false, false), "smoke");
        assert!(may_open(false, false, true, false), "demo");
        assert!(may_open(false, false, false, true), "--no-elevate");
    }
}
