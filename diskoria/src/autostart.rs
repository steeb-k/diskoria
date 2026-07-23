//! Launch-at-startup via a Windows Scheduled Task.
//!
//! Diskoria's manifest is `requireAdministrator`, so a
//! `...\CurrentVersion\Run` entry would trigger a UAC consent prompt at every
//! logon. A Scheduled Task created with "run with highest privileges"
//! (`/RL HIGHEST`) instead starts the app elevated at logon with no prompt.
//!
//! The task's *existence* is the single source of truth for the "Launch at
//! startup" state — there is no persisted setting to drift out of sync with it.
//! The installer (`installer/diskoria.iss`) creates the same-named task by
//! default, so installed builds start ON; the portable exe never creates it, so
//! it starts OFF unless the user flips the in-app toggle.
//!
//! This module is Windows-only (mirrors `tray`/`flyout`); it is never referenced
//! from the non-Windows shell.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Scheduled-task name. Must match the `/TN` used by the installer.
pub const TASK_NAME: &str = "Diskoria Startup";

/// Spawn `schtasks.exe` without flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The `schtasks /Create` argument vector for the given exe path. Split out
/// (and pure) so it can be unit-tested without touching the OS.
fn create_args(exe: &Path) -> Vec<String> {
    // `/TR` is a single string; the exe path must be quoted inside it so a path
    // with spaces (e.g. `C:\Program Files\Diskoria\diskoria.exe`) is treated as
    // one token by the task engine, followed by our `--minimized` flag.
    let tr = format!("\"{}\" --minimized", exe.display());
    vec![
        "/Create".into(),
        "/TN".into(),
        TASK_NAME.into(),
        "/TR".into(),
        tr,
        "/SC".into(),
        "ONLOGON".into(),
        "/RL".into(),
        "HIGHEST".into(),
        "/F".into(),
    ]
}

fn run_schtasks(args: &[String]) -> std::io::Result<bool> {
    Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
}

/// Create (enable) or delete (disable) the logon task. Returns an error if
/// `schtasks` cannot be spawned or reports failure.
pub fn set_enabled(enabled: bool) -> std::io::Result<()> {
    let ok = if enabled {
        let exe = std::env::current_exe()?;
        run_schtasks(&create_args(&exe))?
    } else {
        run_schtasks(&[
            "/Delete".into(),
            "/TN".into(),
            TASK_NAME.into(),
            "/F".into(),
        ])?
    };
    if ok {
        Ok(())
    } else {
        Err(std::io::Error::other("schtasks reported a non-zero exit"))
    }
}

/// Whether the logon task currently exists (the source of truth for the toggle).
pub fn is_enabled() -> bool {
    Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn create_args_quotes_exe_and_sets_flags() {
        let args = create_args(&PathBuf::from(r"C:\Program Files\Diskoria\diskoria.exe"));
        // Task name + our startup flag must be present.
        assert!(args.iter().any(|a| a == TASK_NAME));
        assert!(args.iter().any(|a| a == "/Create"));
        // Elevated, triggered at logon, force-overwrite.
        assert!(args.iter().any(|a| a == "HIGHEST"));
        assert!(args.iter().any(|a| a == "ONLOGON"));
        assert!(args.iter().any(|a| a == "/F"));
        // The `/TR` value must quote the (spaced) exe path and append --minimized.
        let tr = args
            .iter()
            .find(|a| a.contains("diskoria.exe"))
            .expect("TR arg present");
        assert_eq!(tr, r#""C:\Program Files\Diskoria\diskoria.exe" --minimized"#);
    }
}
