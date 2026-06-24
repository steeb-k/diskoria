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
use crate::smart_reader::{AtaSmartData, NvmeHealthData, UfsHealthData, SmartReport};

/// Build a `HealthSnapshot` from a drive + its current `SmartReport`.
pub fn extract_snapshot(drive: &DetectedDrive, report: &SmartReport) -> HealthSnapshot {
    let timestamp_unix = chrono::Utc::now().timestamp();
    let raw_json = report_to_json(report);

    match report {
        SmartReport::Ata(ata) => extract_ata(drive, ata, timestamp_unix, raw_json),
        SmartReport::Nvme(nvme) => extract_nvme(drive, nvme, timestamp_unix, raw_json),
        SmartReport::Ufs(ufs) => extract_ufs(drive, ufs, timestamp_unix, raw_json),
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

fn ufs_lifetime_midpoint(v: u8) -> u8 {
    match v {
        0x01..=0x0A => (v - 1) * 10 + 5,
        0x0B => 100,
        _ => 0,
    }
}

/// Estimated drive health percentage (`100 − wear`) derived from a health
/// report. Used by the sector-test pages to show a meaningful figure for NVMe
/// and UFS drives, which don't expose ATA SMART predict-fail status. Returns
/// `None` when the report carries no wear signal (ATA, USB, unavailable).
pub fn health_pct_from_report(report: &SmartReport) -> Option<u8> {
    let wear = match report {
        SmartReport::Nvme(n) => n.percentage_used,
        SmartReport::Ufs(u) => {
            if u.pre_eol_info >= 0x03 {
                100
            } else {
                let w = ufs_lifetime_midpoint(u.life_time_est_a)
                    .max(ufs_lifetime_midpoint(u.life_time_est_b));
                if w == 0 {
                    return None;
                }
                w
            }
        }
        _ => return None,
    };
    Some(100u8.saturating_sub(wear.min(100)))
}

fn extract_ufs(
    drive: &DetectedDrive,
    ufs: &UfsHealthData,
    timestamp_unix: i64,
    raw_json: String,
) -> HealthSnapshot {
    // Use the worse (higher) of the two lifetime estimates as wear %.
    // PreEOL 0x03 (Urgent) forces wear to 100 to guarantee a WearHigh alert.
    let wear_pct = if ufs.pre_eol_info >= 0x03 {
        Some(100u8)
    } else {
        let a = ufs_lifetime_midpoint(ufs.life_time_est_a);
        let b = ufs_lifetime_midpoint(ufs.life_time_est_b);
        let w = a.max(b);
        if w > 0 { Some(w) } else { None }
    };

    HealthSnapshot {
        serial: drive.serial.clone(),
        model: drive.model.clone(),
        timestamp_unix,
        temp_c: None,
        reallocated_sectors: None,
        pending_sectors: None,
        uncorrectable_sectors: None,
        wear_pct,
        available_spare_pct: None,
        available_spare_threshold: None,
        critical_warning: None,
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
        SmartReport::Ufs(ufs) => serde_json::json!({
            "type": "ufs",
            "pre_eol_info": ufs.pre_eol_info,
            "life_time_est_a": ufs.life_time_est_a,
            "life_time_est_b": ufs.life_time_est_b,
        })
        .to_string(),
        SmartReport::Unavailable { reason } => {
            serde_json::json!({ "type": "unavailable", "reason": reason }).to_string()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detected_drive::{BusKind, DetectedDrive, MediaKind, PartitionTableStyle};
    use crate::smart_reader::{
        AtaAttribute, AtaSmartData, AttrStatus, SmartReport, UfsHealthData,
    };

    fn dummy_drive() -> DetectedDrive {
        DetectedDrive {
            disk_number: 0,
            device_id: "\\\\.\\PhysicalDrive0".into(),
            model: "Model".into(),
            serial: "SER".into(),
            pnp_device_id: String::new(),
            size_bytes: 0,
            media: MediaKind::Ssd,
            bus: BusKind::Sata,
            summary: String::new(),
            partitions: Vec::new(),
            partition_style: PartitionTableStyle::Unknown,
        }
    }

    fn ata_attr(id: u8, raw: u64) -> AtaAttribute {
        AtaAttribute {
            id,
            name: "",
            current: 100,
            worst: 100,
            threshold: 0,
            raw,
            is_critical: false,
            status: AttrStatus::Good,
        }
    }

    #[test]
    fn ufs_midpoint_encoding() {
        assert_eq!(ufs_lifetime_midpoint(0x00), 0);
        assert_eq!(ufs_lifetime_midpoint(0x01), 5);
        assert_eq!(ufs_lifetime_midpoint(0x0A), 95);
        assert_eq!(ufs_lifetime_midpoint(0x0B), 100);
        assert_eq!(ufs_lifetime_midpoint(0xFF), 0);
    }

    #[test]
    fn drive_snapshot_health_summary() {
        let mut snap = HealthSnapshot {
            serial: "S".into(),
            model: "M".into(),
            timestamp_unix: 0,
            temp_c: Some(30),
            reallocated_sectors: None,
            pending_sectors: None,
            uncorrectable_sectors: None,
            wear_pct: Some(50),
            available_spare_pct: None,
            available_spare_threshold: None,
            critical_warning: None,
            raw_json: String::new(),
        };
        assert_eq!(DriveSnapshot::from_snapshot(&snap).health_summary, "Healthy");
        snap.wear_pct = Some(80);
        assert!(DriveSnapshot::from_snapshot(&snap).health_summary.contains("high"));
        snap.wear_pct = Some(95);
        assert!(DriveSnapshot::from_snapshot(&snap).health_summary.contains("critical"));
        snap.critical_warning = Some(0x04);
        assert_eq!(DriveSnapshot::from_snapshot(&snap).health_summary, "Critical warning");
    }

    #[test]
    fn extract_ata_wear_and_sectors() {
        let ata = AtaSmartData {
            attributes: vec![
                ata_attr(0x05, 3),  // reallocated sectors
                ata_attr(0xC5, 2),  // pending sectors
                ata_attr(0xE7, 30), // SSD life left 30% → wear 70%
            ],
            power_on_hours: None,
            power_cycles: None,
            temperature_c: Some(44),
        };
        let snap = extract_snapshot(&dummy_drive(), &SmartReport::Ata(ata));
        assert_eq!(snap.reallocated_sectors, Some(3));
        assert_eq!(snap.pending_sectors, Some(2));
        assert_eq!(snap.wear_pct, Some(70));
        assert_eq!(snap.temp_c, Some(44));
        assert_eq!(snap.serial, "SER");
    }

    #[test]
    fn health_pct_nvme_and_ufs() {
        use crate::smart_reader::NvmeHealthData;
        let nvme = NvmeHealthData {
            temperature_c: 40,
            percentage_used: 7,
            available_spare_pct: 100,
            available_spare_threshold: 10,
            power_on_hours: 0,
            power_cycles: 0,
            data_units_written: 0,
            unsafe_shutdowns: 0,
            media_errors: 0,
            critical_warning: 0,
        };
        assert_eq!(health_pct_from_report(&SmartReport::Nvme(nvme)), Some(93));

        // UFS PreEOL urgent → 0% health.
        let urgent = UfsHealthData { pre_eol_info: 0x03, life_time_est_a: 0x02, life_time_est_b: 0x01 };
        assert_eq!(health_pct_from_report(&SmartReport::Ufs(urgent)), Some(0));

        // ATA has no wear signal here → None.
        assert_eq!(
            health_pct_from_report(&SmartReport::Unavailable { reason: "x".into() }),
            None
        );
    }

    #[test]
    fn extract_ufs_wear() {
        // PreEOL Urgent (0x03) forces wear to 100%.
        let urgent = UfsHealthData { pre_eol_info: 0x03, life_time_est_a: 0x02, life_time_est_b: 0x01 };
        assert_eq!(extract_snapshot(&dummy_drive(), &SmartReport::Ufs(urgent)).wear_pct, Some(100));

        // Otherwise wear is the worse (higher) of the two lifetime midpoints:
        // est_a 0x03 → 25, est_b 0x02 → 15 ⇒ 25.
        let normal = UfsHealthData { pre_eol_info: 0x01, life_time_est_a: 0x03, life_time_est_b: 0x02 };
        assert_eq!(extract_snapshot(&dummy_drive(), &SmartReport::Ufs(normal)).wear_pct, Some(25));
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
                .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
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
