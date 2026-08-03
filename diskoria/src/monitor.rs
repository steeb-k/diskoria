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

/// Rebuild a [`SmartReport`] from a snapshot's stored `raw_json`.
///
/// The inverse of [`report_to_json`], and the reason the Drive Health page
/// works in an unelevated session: the root monitoring service already read
/// this drive, so the page can render the service's reading instead of telling
/// the user to relaunch as root for data that is sitting in the database.
///
/// Attribute `name` and `status` are derived rather than stored, so a
/// reconstructed report is indistinguishable from a freshly read one.
pub fn report_from_json(raw: &str) -> Option<SmartReport> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let u64_of = |k: &str| v.get(k).and_then(|x| x.as_u64());
    let u8_of = |k: &str| u64_of(k).map(|n| n as u8);
    match v.get("type").and_then(|t| t.as_str())? {
        "ata" => {
            let attributes = v
                .get("attributes")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| {
                            let g = |k: &str| a.get(k).and_then(|x| x.as_u64());
                            Some(crate::smart_reader::ata_attribute_from_parts(
                                g("id")? as u8,
                                g("current")? as u8,
                                g("worst")? as u8,
                                g("threshold")? as u8,
                                g("raw")?,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(SmartReport::Ata(crate::smart_reader::AtaSmartData {
                attributes,
                power_on_hours: u64_of("power_on_hours"),
                power_cycles: u64_of("power_cycles"),
                temperature_c: v.get("temperature_c").and_then(|x| x.as_i64()).map(|n| n as i32),
            }))
        }
        "nvme" => Some(SmartReport::Nvme(crate::smart_reader::NvmeHealthData {
            temperature_c: v.get("temperature_c").and_then(|x| x.as_i64())? as i16,
            percentage_used: u8_of("percentage_used")?,
            available_spare_pct: u8_of("available_spare_pct")?,
            available_spare_threshold: u8_of("available_spare_threshold")?,
            power_on_hours: u64_of("power_on_hours")?,
            power_cycles: u64_of("power_cycles")?,
            data_units_written: u64_of("data_units_written")?,
            unsafe_shutdowns: u64_of("unsafe_shutdowns")?,
            media_errors: u64_of("media_errors")?,
            critical_warning: u8_of("critical_warning")?,
        })),
        "ufs" => Some(SmartReport::Ufs(crate::smart_reader::UfsHealthData {
            pre_eol_info: u8_of("pre_eol_info")?,
            life_time_est_a: u8_of("life_time_est_a")?,
            life_time_est_b: u8_of("life_time_est_b")?,
        })),
        // "unavailable" round-trips to nothing on purpose: there is no reading
        // to show, and the caller should keep its own error message.
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod json_roundtrip_tests {
    use super::{report_from_json, report_to_json};
    use crate::smart_reader::{
        ata_attribute_from_parts, AtaSmartData, NvmeHealthData, SmartReport, UfsHealthData,
    };

    #[test]
    fn nvme_survives_a_round_trip() {
        let original = SmartReport::Nvme(NvmeHealthData {
            temperature_c: 45,
            percentage_used: 3,
            available_spare_pct: 100,
            available_spare_threshold: 10,
            power_on_hours: 320,
            power_cycles: 412,
            data_units_written: 123_456,
            unsafe_shutdowns: 7,
            media_errors: 0,
            critical_warning: 0,
        });
        let back = report_from_json(&report_to_json(&original)).expect("nvme rebuilds");
        let SmartReport::Nvme(n) = back else { panic!("wrong variant") };
        assert_eq!(n.temperature_c, 45);
        assert_eq!(n.percentage_used, 3);
        assert_eq!(n.available_spare_pct, 100);
        assert_eq!(n.power_on_hours, 320);
        assert_eq!(n.data_units_written, 123_456);
        assert_eq!(n.unsafe_shutdowns, 7);
    }

    /// Attribute `name` and `status` are derived, not stored — a rebuilt row
    /// has to come out identical to a freshly read one or the Drive Health
    /// table would render differently depending on where the data came from.
    #[test]
    fn ata_attributes_rebuild_with_derived_name_and_status() {
        let fresh = ata_attribute_from_parts(0x05, 100, 100, 10, 0);
        let original = SmartReport::Ata(AtaSmartData {
            attributes: vec![
                fresh.clone(),
                ata_attribute_from_parts(0xC5, 200, 200, 0, 4),
                ata_attribute_from_parts(0x09, 95, 95, 0, 320),
            ],
            power_on_hours: Some(320),
            power_cycles: Some(412),
            temperature_c: Some(38),
        });
        let back = report_from_json(&report_to_json(&original)).expect("ata rebuilds");
        let SmartReport::Ata(a) = back else { panic!("wrong variant") };
        assert_eq!(a.attributes.len(), 3);
        assert_eq!(a.temperature_c, Some(38));
        assert_eq!(a.power_on_hours, Some(320));

        assert_eq!(a.attributes[0].id, fresh.id);
        assert_eq!(a.attributes[0].name, fresh.name);
        assert_eq!(a.attributes[0].status, fresh.status);
        assert_eq!(a.attributes[0].is_critical, fresh.is_critical);
        assert_eq!(a.attributes[0].raw, fresh.raw);
        // A pending-sector count of 4 must still read as a problem after a
        // round trip through the database.
        assert!(a.attributes[1].is_critical);
        assert_eq!(a.attributes[1].raw, 4);
    }

    #[test]
    fn ufs_survives_a_round_trip() {
        let original = SmartReport::Ufs(UfsHealthData {
            pre_eol_info: 2,
            life_time_est_a: 3,
            life_time_est_b: 4,
        });
        let back = report_from_json(&report_to_json(&original)).expect("ufs rebuilds");
        let SmartReport::Ufs(u) = back else { panic!("wrong variant") };
        assert_eq!((u.pre_eol_info, u.life_time_est_a, u.life_time_est_b), (2, 3, 4));
    }

    /// "Unavailable" carries no reading, so it must not rebuild into one — the
    /// caller keeps its own error message instead.
    #[test]
    fn unavailable_and_junk_do_not_rebuild() {
        let original = SmartReport::Unavailable { reason: "nope".into() };
        assert!(report_from_json(&report_to_json(&original)).is_none());
        assert!(report_from_json("not json").is_none());
        assert!(report_from_json("{}").is_none());
    }
}

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

#[cfg(any(windows, target_os = "linux"))]
pub use imp::spawn_monitor_thread;

#[cfg(any(windows, target_os = "linux"))]
mod imp {
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

    /// The service's latest reading for `serial`, if it is recent enough to
    /// trust.
    ///
    /// Staleness matters: if the service is stopped or masked, its last rows
    /// stay in the database forever, and silently charting a dead drive's final
    /// temperature as if it were current is worse than falling back to polling.
    /// Three poll intervals allows for a missed pass without flapping.
    #[cfg(target_os = "linux")]
    pub(super) fn fresh_service_snapshot(
        db: &rusqlite::Connection,
        serial: &str,
        poll_interval: Duration,
    ) -> Option<HealthSnapshot> {
        let snap = history_db::load_last_snapshot(db, serial).ok().flatten()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        let max_age = (poll_interval.as_secs() as i64).saturating_mul(3).max(60);
        (now.saturating_sub(snap.timestamp_unix) <= max_age).then_some(snap)
    }

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
            // Readings published by the root monitoring service, when it is
            // installed. Reopened lazily so a service started *after* the
            // desktop session still gets picked up.
            #[cfg(target_os = "linux")]
            let mut service_db: Option<rusqlite::Connection> = None;
            let internal_drives: Vec<&DetectedDrive> = drives
                .iter()
                .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
                .collect();

            loop {
                if cancel2.load(Ordering::SeqCst) {
                    break;
                }

                let mut snapshots: Vec<HealthSnapshot> = Vec::new();

                #[cfg(target_os = "linux")]
                if service_db.is_none() {
                    service_db = crate::history_db::open_system_readonly();
                    if service_db.is_some() {
                        log::info!(
                            target: "diskoria::monitor",
                            "using health data from the root monitoring service"
                        );
                    }
                }

                for drive in &internal_drives {
                    if cancel2.load(Ordering::SeqCst) {
                        break;
                    }

                    // Prefer the service's reading. An unelevated session
                    // cannot run the SMART ioctls at all, and when it *can*
                    // there is no point doing them twice.
                    #[cfg(target_os = "linux")]
                    if let Some(snap) = service_db
                        .as_ref()
                        .and_then(|db| fresh_service_snapshot(db, &drive.serial, poll_interval))
                    {
                        let prev = history_db::load_last_snapshot(&conn, &drive.serial).ok().flatten();
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
                        if let Err(e) = history_db::insert_snapshot(&conn, &snap) {
                            log::warn!(target: "diskoria::monitor", "Insert snapshot failed: {e}");
                        }
                        snapshots.push(snap);
                        continue;
                    }

                    let report = smart_reader::query_smart_detail(&drive.device_id, drive.bus);
                    #[cfg_attr(windows, allow(unused_mut))]
                    let mut snap = extract_snapshot(drive, &report);
                    // Unelevated Linux (the `--minimized` autostart mode):
                    // the SMART ioctls need root, but drive temperatures are
                    // still readable via hwmon (drivetemp / nvme), which is
                    // what the tray + temp alerts run on.
                    #[cfg(target_os = "linux")]
                    if matches!(report, smart_reader::SmartReport::Unavailable { .. }) {
                        if let Some(t) = super::hwmon::temp_for_device(&drive.device_id) {
                            snap.temp_c = Some(t);
                        }
                    }

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

/// Unprivileged temperature fallback: the kernel's hwmon nodes. SATA drives
/// get one from the `drivetemp` module, NVMe controllers expose one natively.
#[cfg(target_os = "linux")]
pub(crate) mod hwmon {
    use std::path::Path;

    fn read_temp_millic(dir: &Path) -> Option<i32> {
        let s = std::fs::read_to_string(dir.join("temp1_input")).ok()?;
        s.trim().parse::<i32>().ok().map(|mc| mc / 1000)
    }

    /// hwmonN children of `dir`, whether nested under an `hwmon` subdir
    /// (SCSI/drivetemp: `device/hwmon/hwmonX`) or direct (NVMe:
    /// `nvme0/hwmonX`).
    fn hwmon_children(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == "hwmon" {
                    if let Ok(inner) = std::fs::read_dir(e.path()) {
                        out.extend(inner.flatten().map(|i| i.path()));
                    }
                } else if name.starts_with("hwmon") {
                    out.push(e.path());
                }
            }
        }
        out
    }

    /// °C for `/dev/sdX` / `/dev/nvmeXnY`, via the device's own hwmon node.
    pub fn temp_for_device(device_path: &str) -> Option<i32> {
        let name = Path::new(device_path).file_name()?.to_string_lossy().into_owned();
        let sys = std::fs::canonicalize(format!("/sys/block/{name}")).ok()?;
        // Walk up the device chain looking for hwmon children.
        let mut node = sys.join("device");
        for _ in 0..6 {
            for h in hwmon_children(&node) {
                if let Some(t) = read_temp_millic(&h) {
                    return Some(t);
                }
            }
            if !node.pop() {
                break;
            }
        }
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod service_snapshot_tests {
    use super::imp::fresh_service_snapshot;
    use super::HealthSnapshot;
    use crate::history_db;
    use std::time::Duration;

    fn db_with(serial: &str, age_secs: i64) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        history_db::apply_schema_for_tests(&conn).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let snap = HealthSnapshot {
            serial: serial.to_string(),
            model: "Svc Drive".into(),
            timestamp_unix: now - age_secs,
            temp_c: Some(41),
            reallocated_sectors: Some(0),
            pending_sectors: Some(0),
            uncorrectable_sectors: Some(0),
            wear_pct: Some(3),
            available_spare_pct: None,
            available_spare_threshold: None,
            critical_warning: None,
            raw_json: "{}".into(),
        };
        history_db::insert_snapshot(&conn, &snap).unwrap();
        conn
    }

    #[test]
    fn a_recent_service_reading_is_used() {
        let db = db_with("SVC1", 10);
        let got = fresh_service_snapshot(&db, "SVC1", Duration::from_secs(180));
        assert_eq!(got.and_then(|s| s.temp_c), Some(41));
    }

    /// A stopped or masked service leaves its last rows behind forever.
    /// Charting those as current would quietly show a dead reading; falling
    /// back to polling ourselves is the honest answer.
    #[test]
    fn a_stale_service_reading_is_ignored() {
        let db = db_with("SVC1", 3 * 180 + 60);
        assert!(fresh_service_snapshot(&db, "SVC1", Duration::from_secs(180)).is_none());
    }

    #[test]
    fn a_drive_the_service_never_saw_falls_through() {
        let db = db_with("SVC1", 10);
        assert!(fresh_service_snapshot(&db, "OTHER", Duration::from_secs(180)).is_none());
    }

    /// A very short poll interval must not make every reading look stale;
    /// the freshness window has a floor.
    #[test]
    fn the_freshness_window_has_a_floor() {
        let db = db_with("SVC1", 45);
        assert!(fresh_service_snapshot(&db, "SVC1", Duration::from_secs(1)).is_some());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod hwmon_tests {
    /// Diagnostic against this host's real sysfs; asserts only when a drive
    /// exists. drivetemp may be unloaded for SATA — NVMe works out of the box.
    #[test]
    #[ignore = "reads real sysfs; run manually with --ignored --nocapture"]
    fn print_hwmon_temps() {
        for d in crate::drive_enumeration::enumerate_physical_disks().unwrap_or_default() {
            println!("{} -> {:?} °C", d.device_id, super::hwmon::temp_for_device(&d.device_id));
        }
    }
}
