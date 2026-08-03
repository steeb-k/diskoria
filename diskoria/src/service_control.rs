//! Linux: visibility and control for the root monitoring service.
//!
//! A background root daemon collecting drive health with no icon, no status
//! anywhere in the UI and no off switch is not acceptable — if something is
//! logging on your machine, the desktop app has to be able to show you that and
//! turn it off. On Windows every background behaviour hangs off the tray icon;
//! this is the Linux equivalent, in the tray menu and in Settings.
//!
//! **Nothing here may run on the event-loop thread.** Every call shells out to
//! `systemctl`, and an `exec` is exactly the kind of blocking work that froze
//! every window in KI-42/KI-43. So status is polled by a worker into a cache
//! that the UI reads for free, and actions are fired onto their own thread.
//!
//! Privileges: reading state needs none. Starting and stopping go through
//! systemd's own polkit action (`org.freedesktop.systemd1.manage-units`), so an
//! unelevated session gets the desktop's normal authentication prompt, and an
//! already-elevated one just proceeds. No new privilege path, and no way for
//! this to become a passwordless "stop auditing me" button.

#![cfg(target_os = "linux")]

use std::sync::Mutex;
use std::time::Duration;

pub const UNIT: &str = "diskoria-monitor.service";

/// How often the worker refreshes the cache.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ServiceStatus {
    /// The unit exists on this system.
    pub installed: bool,
    /// It is running right now.
    pub running: bool,
    /// It will start again at boot.
    pub enabled_at_boot: bool,
}

impl ServiceStatus {
    /// Short line for the tray menu and Settings.
    pub fn summary(&self) -> String {
        if !self.installed {
            return "Not installed".to_string();
        }
        match (self.running, self.enabled_at_boot) {
            (true, true) => "Running".to_string(),
            (true, false) => "Running (not at boot)".to_string(),
            (false, true) => "Stopped (starts at boot)".to_string(),
            (false, false) => "Stopped".to_string(),
        }
    }
}

struct Cache {
    status: Option<ServiceStatus>,
    /// Why the last start/stop failed, for the UI to show instead of silently
    /// doing nothing.
    last_error: Option<String>,
    /// An action is in flight; the UI disables its control meanwhile.
    busy: bool,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    status: None,
    last_error: None,
    busy: false,
});

/// Last known state. `None` until the first refresh completes; never blocks.
pub fn status() -> Option<ServiceStatus> {
    CACHE.lock().ok().and_then(|c| c.status)
}

/// Whether a start/stop is currently in flight.
pub fn busy() -> bool {
    CACHE.lock().map(|c| c.busy).unwrap_or(false)
}

/// Error from the last action, if it failed.
pub fn last_error() -> Option<String> {
    CACHE.lock().ok().and_then(|c| c.last_error.clone())
}

pub fn clear_last_error() {
    if let Ok(mut c) = CACHE.lock() {
        c.last_error = None;
    }
}

/// Parse `systemctl show -p ActiveState -p UnitFileState --value`.
///
/// Systemd prints the properties in the order asked for, one per line, and for
/// a unit it does not know about it still prints `inactive` with an *empty*
/// file state — which is how "not installed" is told apart from "stopped".
fn parse_show(out: &str) -> ServiceStatus {
    let mut lines = out.lines();
    let active = lines.next().unwrap_or("").trim();
    let file_state = lines.next().unwrap_or("").trim();
    ServiceStatus {
        installed: !file_state.is_empty(),
        running: active == "active" || active == "activating",
        // "enabled-runtime" and friends all start at boot; "linked"/"static"
        // do not carry an enablement we can toggle.
        enabled_at_boot: file_state.starts_with("enabled"),
    }
}

fn query() -> Option<ServiceStatus> {
    let out = std::process::Command::new("systemctl")
        .args(["show", "-p", "ActiveState", "-p", "UnitFileState", "--value", UNIT])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(parse_show(&String::from_utf8_lossy(&out.stdout)))
}

/// Refresh the cache now, on the calling thread. Never call from the UI thread.
pub fn refresh_blocking() {
    let fresh = query();
    if let Ok(mut c) = CACHE.lock() {
        if fresh.is_some() {
            c.status = fresh;
        }
    }
}

/// Start the background status poller. Idempotent; call once at startup.
pub fn spawn_status_worker() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("diskoria-svcstat".into())
            .spawn(|| loop {
                refresh_blocking();
                std::thread::sleep(REFRESH_EVERY);
            });
    });
}

/// Turn background collection on or off, off-thread.
///
/// One switch deliberately covers both "running now" and "starts at boot":
/// somebody turning this off wants it to stay off, and leaving a stopped
/// service that silently comes back at the next boot would defeat the point of
/// having the control at all. `--now` on enable/disable is exactly that pairing.
pub fn set_enabled(on: bool) {
    {
        let mut c = match CACHE.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        if c.busy {
            return;
        }
        c.busy = true;
        c.last_error = None;
    }

    let _ = std::thread::Builder::new()
        .name("diskoria-svcctl".into())
        .spawn(move || {
            let verb = if on { "enable" } else { "disable" };
            log::info!(target: "diskoria::service", "{verb} --now {UNIT}");
            let result = std::process::Command::new("systemctl")
                .args([verb, "--now", UNIT])
                .output();

            let err = match result {
                Ok(o) if o.status.success() => None,
                Ok(o) => {
                    let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    Some(if msg.is_empty() {
                        format!("systemctl {verb} failed")
                    } else {
                        msg
                    })
                }
                Err(e) => Some(format!("could not run systemctl: {e}")),
            };
            if let Some(ref e) = err {
                log::warn!(target: "diskoria::service", "{verb} failed: {e}");
            }

            // Reflect the new state immediately rather than waiting out the
            // poll interval, so the toggle does not spring back for 5 seconds.
            refresh_blocking();
            if let Ok(mut c) = CACHE.lock() {
                c.busy = false;
                c.last_error = err;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_and_enabled_unit() {
        let s = parse_show("active\nenabled\n");
        assert_eq!(
            s,
            ServiceStatus { installed: true, running: true, enabled_at_boot: true }
        );
        assert_eq!(s.summary(), "Running");
    }

    /// The distinguishing case: systemd reports a unit it has never heard of as
    /// `inactive` with an empty file state, so an empty second line — not the
    /// first — is what "not installed" looks like.
    fn not_installed() -> ServiceStatus {
        parse_show("inactive\n\n")
    }

    #[test]
    fn a_unit_systemd_does_not_know_reads_as_not_installed() {
        let s = not_installed();
        assert!(!s.installed);
        assert!(!s.running);
        assert_eq!(s.summary(), "Not installed");
    }

    #[test]
    fn installed_but_stopped_is_distinct_from_absent() {
        let s = parse_show("inactive\ndisabled\n");
        assert!(s.installed, "a disabled unit is still installed");
        assert!(!s.running);
        assert!(!s.enabled_at_boot);
        assert_eq!(s.summary(), "Stopped");
        assert_ne!(s, not_installed());
    }

    #[test]
    fn stopped_but_returning_at_boot_says_so() {
        let s = parse_show("inactive\nenabled\n");
        assert_eq!(s.summary(), "Stopped (starts at boot)");
    }

    #[test]
    fn running_without_boot_enablement_says_so() {
        let s = parse_show("active\ndisabled\n");
        assert_eq!(s.summary(), "Running (not at boot)");
    }

    #[test]
    fn activating_counts_as_running() {
        assert!(parse_show("activating\nenabled\n").running);
        assert!(!parse_show("failed\nenabled\n").running);
    }

    #[test]
    fn enabled_runtime_still_starts_at_boot() {
        assert!(parse_show("active\nenabled-runtime\n").enabled_at_boot);
    }

    /// Diagnostic against the real unit on this machine:
    ///   cargo test service_control -- --ignored --nocapture
    #[test]
    #[ignore = "queries the systemd unit actually installed on this machine"]
    fn live_status_of_the_installed_unit() {
        refresh_blocking();
        let s = status().expect("systemctl should have answered");
        println!("{UNIT}: {s:?}  ->  \"{}\"", s.summary());
    }

    #[test]
    fn garbage_does_not_panic() {
        assert_eq!(parse_show(""), ServiceStatus::default());
        assert_eq!(parse_show("\n"), ServiceStatus::default());
    }
}
