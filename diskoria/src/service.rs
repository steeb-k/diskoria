//! Headless root monitoring service (`--service`), run by
//! `diskoria-monitor.service` (see `linux/`).
//!
//! **Why this exists.** Reading SMART/NVMe health needs root, but a desktop
//! session should not. Before this, a login-time start (`--minimized`) ran
//! unelevated and had to make do with hwmon temperatures — no wear, no
//! reallocated-sector counts, no predict-fail — because the alternative was a
//! polkit prompt at every login, which is worse.
//!
//! So the privileged half runs here: a small root daemon that does nothing but
//! poll drive health and write it to `/var/lib/diskoria/history.db`, which the
//! session reads unprivileged. Deliberately **collection only** — it opens no
//! sockets, accepts no commands, and never writes to a block device. Running a
//! sector or destructive test still goes through pkexec, because that is the
//! point where raw writes actually happen and an authentication prompt is the
//! right thing to have.
//!
//! Alerts and notifications stay in the desktop session, where the session bus
//! is reachable and there is someone to notify.

#![cfg(target_os = "linux")]

use std::time::Duration;

use crate::detected_drive::BusKind;

/// Default seconds between polls; override with `DISKORIA_POLL_SECS` in the
/// unit file. Matches the GUI's default poll interval (3 minutes).
const DEFAULT_POLL_SECS: u64 = 180;

/// Is `--service` present on the command line?
pub fn requested() -> bool {
    std::env::args().skip(1).any(|a| a == "--service")
}

fn poll_interval() -> Duration {
    let secs = std::env::var("DISKORIA_POLL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_SECS)
        // A runaway-config guard: never hammer the drives, never sleep for days.
        .clamp(15, 24 * 60 * 60);
    Duration::from_secs(secs)
}

/// Run the collection loop. Returns a process exit code; never returns during
/// normal operation (systemd stops us with SIGTERM).
pub fn run() -> i32 {
    if !crate::elevation::is_elevated() {
        log::error!(
            target: "diskoria::service",
            "--service needs root (it reads SMART via ioctls); run it through diskoria-monitor.service"
        );
        return 1;
    }

    let interval = poll_interval();
    log::info!(
        target: "diskoria::service",
        "Diskoria monitoring service {} starting (poll every {}s, db {})",
        env!("CARGO_PKG_VERSION"),
        interval.as_secs(),
        crate::paths::system_history_db_file().display()
    );

    let conn = match crate::history_db::open_system_rw() {
        Ok(c) => c,
        Err(e) => {
            log::error!(target: "diskoria::service", "cannot open {}: {e}", crate::paths::system_history_db_file().display());
            return 1;
        }
    };
    if let Err(e) = crate::history_db::prune_old_records(&conn) {
        log::warn!(target: "diskoria::service", "prune failed: {e}");
    }

    loop {
        // Re-enumerate every pass so drives plugged in after boot are picked
        // up; the service has no device-event loop of its own.
        let drives = match crate::drive_enumeration::enumerate_physical_disks() {
            Ok(d) => d,
            Err(e) => {
                log::warn!(target: "diskoria::service", "drive enumeration failed: {e}");
                Vec::new()
            }
        };

        let mut written = 0usize;
        for drive in drives
            .iter()
            .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
        {
            // Same rule as the session monitor: a sleeping disk is left alone
            // rather than spun up for a temperature reading (KI-58). The
            // service polls unattended, so this matters more here, not less.
            if drive.media == crate::detected_drive::MediaKind::Hdd
                && crate::smart_reader::power_mode(&drive.device_id, drive.bus)
                    == crate::smart_reader::PowerMode::Standby
            {
                log::debug!(
                    target: "diskoria::service",
                    "{} is in standby; skipping this poll", drive.serial
                );
                continue;
            }

            let report = crate::smart_reader::query_smart_detail(&drive.device_id, drive.bus);
            let mut snap = crate::monitor::extract_snapshot(drive, &report);
            // Same fallback the GUI uses: hwmon still has a temperature even
            // when the health log is unavailable (odd USB bridges, locked UFS).
            if matches!(report, crate::smart_reader::SmartReport::Unavailable { .. }) {
                if let Some(t) = crate::monitor::hwmon::temp_for_device(&drive.device_id) {
                    snap.temp_c = Some(t);
                }
            }
            match crate::history_db::insert_snapshot(&conn, &snap) {
                Ok(()) => written += 1,
                Err(e) => log::warn!(
                    target: "diskoria::service",
                    "insert failed for {}: {e}", drive.serial
                ),
            }
        }
        // Info, not debug: this is the line an operator checks to confirm the
        // service is doing its job, and the shipped unit runs at info.
        log::info!(target: "diskoria::service", "wrote {written} snapshot(s)");

        // Housekeeping once a pass; cheap and keeps the file from growing
        // without bound on a machine that is never rebooted.
        if let Err(e) = crate::history_db::prune_old_records(&conn) {
            log::warn!(target: "diskoria::service", "prune failed: {e}");
        }

        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_interval_is_clamped_to_something_sane() {
        // Guard rails, not preferences: a 0 or absurd value in a hand-edited
        // unit file should not busy-loop the drives or park for a week.
        std::env::set_var("DISKORIA_POLL_SECS", "0");
        assert_eq!(poll_interval(), Duration::from_secs(15));
        std::env::set_var("DISKORIA_POLL_SECS", "999999999");
        assert_eq!(poll_interval(), Duration::from_secs(24 * 60 * 60));
        std::env::set_var("DISKORIA_POLL_SECS", "60");
        assert_eq!(poll_interval(), Duration::from_secs(60));
        std::env::remove_var("DISKORIA_POLL_SECS");
        assert_eq!(poll_interval(), Duration::from_secs(DEFAULT_POLL_SECS));
    }
}
