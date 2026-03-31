//! Pro-Monitoring: background health monitoring types and thread.
//!
//! The monitor thread polls all internal drives every 5 minutes, stores
//! snapshots to SQLite, and fires alerts via the channel back to the UI.

use crate::alert_engine::AlertEvent;

// ── Snapshot ──────────────────────────────────────────────────────────────────

/// A single point-in-time health snapshot for one drive.
#[derive(Debug, Clone)]
pub struct HealthSnapshot {
    pub serial: String,
    pub model: String,
    /// Unix timestamp (UTC seconds).
    pub timestamp_unix: i64,
    pub temp_c: Option<i32>,
    /// ATA attribute 0x05 (Reallocated Sectors Count), raw value.
    pub reallocated_sectors: Option<u64>,
    /// ATA attribute 0xC5 (Current Pending Sectors), raw value.
    pub pending_sectors: Option<u64>,
    /// ATA attribute 0xC6 (Uncorrectable Sectors), raw value.
    pub uncorrectable_sectors: Option<u64>,
    /// ATA attribute 0xE7 raw = remaining life %; we store 100-raw as wear %.
    /// For NVMe this is `percentage_used` directly.
    pub wear_pct: Option<u8>,
    /// NVMe only: available spare percentage.
    pub available_spare_pct: Option<u8>,
    /// NVMe only: manufacturer's available spare threshold.
    pub available_spare_threshold: Option<u8>,
    /// NVMe only: critical warning bitmask from log page 0x02.
    pub critical_warning: Option<u8>,
    /// Full serialized `SmartReport` as JSON for archival.
    pub raw_json: String,
}

// ── Messages from monitor thread to UI ───────────────────────────────────────

#[derive(Debug)]
pub enum MonitorMsg {
    /// A fresh batch of snapshots (one per polled drive).
    Snapshots(Vec<HealthSnapshot>),
    /// An alert condition was detected.
    AlertFired(AlertEvent),
}

// ── Lightweight drive summary for the flyout popup ───────────────────────────

/// Subset of health data used by the flyout window — cheap to clone and send.
#[derive(Debug, Clone, Default)]
pub struct DriveSnapshot {
    pub serial: String,
    pub model: String,
    pub temp_c: Option<i32>,
    pub wear_pct: Option<u8>,
    pub available_spare_pct: Option<u8>,
    pub available_spare_threshold: Option<u8>,
    pub critical_warning: Option<u8>,
    pub health_summary: String,
    pub last_updated_unix: i64,
}

impl DriveSnapshot {
    pub fn from_snapshot(snap: &HealthSnapshot) -> Self {
        let health_summary = if snap.critical_warning.unwrap_or(0) != 0 {
            "Critical warning".to_string()
        } else if let Some(w) = snap.wear_pct {
            if w >= 90 {
                format!("Wear level critical ({}%)", w)
            } else if w >= 75 {
                format!("Wear level high ({}%)", w)
            } else {
                "Healthy".to_string()
            }
        } else {
            "Healthy".to_string()
        };

        DriveSnapshot {
            serial: snap.serial.clone(),
            model: snap.model.clone(),
            temp_c: snap.temp_c,
            wear_pct: snap.wear_pct,
            available_spare_pct: snap.available_spare_pct,
            available_spare_threshold: snap.available_spare_threshold,
            critical_warning: snap.critical_warning,
            health_summary,
            last_updated_unix: snap.timestamp_unix,
        }
    }
}

// ── Snapshot extraction from SmartReport ─────────────────────────────────────

use crate::detected_drive::DetectedDrive;
use crate::smart_reader::{AtaSmartData, NvmeHealthData, SmartReport};

/// Build a `HealthSnapshot` from a drive + its current `SmartReport`.
pub fn extract_snapshot(drive: &DetectedDrive, report: &SmartReport) -> HealthSnapshot {
    let timestamp_unix = chrono::Utc::now().timestamp();
    let raw_json = report_to_json(report);

    match report {
        SmartReport::Ata(ata) => extract_ata(drive, ata, timestamp_unix, raw_json),
        SmartReport::Nvme(nvme) => extract_nvme(drive, nvme, timestamp_unix, raw_json),
        SmartReport::Unavailable { .. } => HealthSnapshot {
            serial: drive.serial.clone(),
            model: drive.model.clone(),
            timestamp_unix,
            temp_c: None,
            reallocated_sectors: None,
            pending_sectors: None,
            uncorrectable_sectors: None,
            wear_pct: None,
            available_spare_pct: None,
            available_spare_threshold: None,
            critical_warning: None,
            raw_json,
        },
    }
}

fn extract_ata(
    drive: &DetectedDrive,
    ata: &AtaSmartData,
    timestamp_unix: i64,
    raw_json: String,
) -> HealthSnapshot {
    let find_raw = |id: u8| -> Option<u64> {
        ata.attributes.iter().find(|a| a.id == id).map(|a| a.raw)
    };

    // 0xE7 raw = remaining SSD life %; wear = 100 - remaining
    let wear_pct = find_raw(0xE7).map(|r| {
        let remaining = (r & 0xFF) as u8;
        100u8.saturating_sub(remaining)
    });

    HealthSnapshot {
        serial: drive.serial.clone(),
        model: drive.model.clone(),
        timestamp_unix,
        temp_c: ata.temperature_c,
        reallocated_sectors: find_raw(0x05),
        pending_sectors: find_raw(0xC5),
        uncorrectable_sectors: find_raw(0xC6),
        wear_pct,
        available_spare_pct: None,
        available_spare_threshold: None,
        critical_warning: None,
        raw_json,
    }
}

fn extract_nvme(
    drive: &DetectedDrive,
    nvme: &NvmeHealthData,
    timestamp_unix: i64,
    raw_json: String,
) -> HealthSnapshot {
    HealthSnapshot {
        serial: drive.serial.clone(),
        model: drive.model.clone(),
        timestamp_unix,
        temp_c: Some(nvme.temperature_c as i32),
        reallocated_sectors: None,
        pending_sectors: None,
        uncorrectable_sectors: None,
        wear_pct: Some(nvme.percentage_used),
        available_spare_pct: Some(nvme.available_spare_pct),
        available_spare_threshold: Some(nvme.available_spare_threshold),
        critical_warning: Some(nvme.critical_warning),
        raw_json,
    }
}

fn report_to_json(report: &SmartReport) -> String {
    match report {
        SmartReport::Ata(ata) => {
            let attrs: Vec<serde_json::Value> = ata
                .attributes
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id,
                        "name": a.name,
                        "current": a.current,
                        "worst": a.worst,
                        "threshold": a.threshold,
                        "raw": a.raw,
                        "is_critical": a.is_critical,
                    })
                })
                .collect();
            serde_json::json!({
                "type": "ata",
                "temperature_c": ata.temperature_c,
                "power_on_hours": ata.power_on_hours,
                "power_cycles": ata.power_cycles,
                "attributes": attrs,
            })
            .to_string()
        }
        SmartReport::Nvme(nvme) => serde_json::json!({
            "type": "nvme",
            "temperature_c": nvme.temperature_c,
            "percentage_used": nvme.percentage_used,
            "available_spare_pct": nvme.available_spare_pct,
            "available_spare_threshold": nvme.available_spare_threshold,
            "power_on_hours": nvme.power_on_hours,
            "power_cycles": nvme.power_cycles,
            "data_units_written": nvme.data_units_written,
            "unsafe_shutdowns": nvme.unsafe_shutdowns,
            "media_errors": nvme.media_errors,
            "critical_warning": nvme.critical_warning,
        })
        .to_string(),
        SmartReport::Unavailable { reason } => {
            serde_json::json!({ "type": "unavailable", "reason": reason }).to_string()
        }
    }
}

// ── Background monitor thread ─────────────────────────────────────────────────

#[cfg(windows)]
pub use windows_impl::spawn_monitor_thread;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::alert_engine::AlertCooldownTracker;
    use crate::detected_drive::{BusKind, DetectedDrive};
    use crate::history_db;
    use crate::smart_reader;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;

    /// Spawn the background monitoring thread.
    ///
    /// Returns a cancel handle (`Arc<AtomicBool>`). Set it to `true` to stop the thread.
    pub fn spawn_monitor_thread(
        drives: Vec<DetectedDrive>,
        tx: mpsc::Sender<MonitorMsg>,
        ctx: egui::Context,
        poll_interval: Duration,
        alert_temp_warn: i32,
        alert_temp_critical: i32,
        alert_wear_threshold: u8,
    ) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = Arc::clone(&cancel);

        std::thread::spawn(move || {
            let conn = match history_db::open_or_create() {
                Ok(c) => c,
                Err(e) => {
                    log::error!(target: "diskoria::monitor", "Failed to open history DB: {e}");
                    return;
                }
            };

            // Prune old records on thread start.
            if let Err(e) = history_db::prune_old_records(&conn) {
                log::warn!(target: "diskoria::monitor", "Prune failed: {e}");
            }

            let mut cooldowns = AlertCooldownTracker::new(&conn);
            let internal_drives: Vec<&DetectedDrive> = drives
                .iter()
                .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata))
                .collect();

            loop {
                if cancel2.load(Ordering::SeqCst) {
                    break;
                }

                let mut snapshots: Vec<HealthSnapshot> = Vec::new();

                for drive in &internal_drives {
                    if cancel2.load(Ordering::SeqCst) {
                        break;
                    }

                    let report = smart_reader::query_smart_detail(&drive.device_id, drive.bus);
                    let snap = extract_snapshot(drive, &report);

                    // Load previous snapshot for delta-based alert checks.
                    let prev = history_db::load_last_snapshot(&conn, &drive.serial).ok().flatten();

                    // Check alerts.
                    let alerts = crate::alert_engine::check_alerts(
                        &snap,
                        prev.as_ref(),
                        alert_temp_warn,
                        alert_temp_critical,
                        alert_wear_threshold,
                        &mut cooldowns,
                        &conn,
                    );
                    for alert in alerts {
                        let _ = tx.send(MonitorMsg::AlertFired(alert));
                    }

                    // Persist snapshot.
                    if let Err(e) = history_db::insert_snapshot(&conn, &snap) {
                        log::warn!(target: "diskoria::monitor", "Insert snapshot failed: {e}");
                    }

                    snapshots.push(snap);
                }

                if !snapshots.is_empty() {
                    let _ = tx.send(MonitorMsg::Snapshots(snapshots));
                    ctx.request_repaint();
                }

                // Sleep in small increments so we can respond to cancel quickly.
                let steps = (poll_interval.as_secs() / 5).max(1);
                for _ in 0..steps {
                    if cancel2.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        });

        cancel
    }
}
