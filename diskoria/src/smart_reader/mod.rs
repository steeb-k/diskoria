//! Raw SMART / NVMe / UFS health data.
//!
//! The shared types, attribute tables and byte-level parsers live here;
//! the transport that fetches the raw bytes is per-platform:
//! `windows.rs` (DeviceIoControl) and `linux.rs` (SG_IO ATA pass-through,
//! the NVMe admin ioctl, and the UFS sysfs health descriptor).

// ── Public types (all platforms) ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SmartReport {
    Ata(AtaSmartData),
    Nvme(NvmeHealthData),
    Ufs(UfsHealthData),
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct AtaSmartData {
    pub attributes: Vec<AtaAttribute>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub temperature_c: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrStatus {
    Good,
    Warning,
    Failed,
    Info,
}

#[derive(Debug, Clone)]
pub struct AtaAttribute {
    pub id: u8,
    pub name: &'static str,
    pub current: u8,
    pub worst: u8,
    pub threshold: u8,
    /// The vendor's full 48-bit raw, exactly as the drive reported it. For
    /// anything user-facing use [`AtaAttribute::display_raw`] instead — some
    /// attributes pack several fields in here (KI-27).
    pub raw: u64,
    pub is_critical: bool,
    pub status: AttrStatus,
}

impl AtaAttribute {
    /// `raw` with vendor packing removed — what the UI and the exported report
    /// should show. See [`display_raw`].
    pub fn display_raw(&self) -> u64 {
        display_raw(self.id, self.raw)
    }
}

#[derive(Debug, Clone)]
pub struct UfsHealthData {
    /// bPreEOLInfo: 0x01=Normal, 0x02=Warning (80%+ reserved blocks consumed), 0x03=Urgent
    pub pre_eol_info: u8,
    /// bDeviceLifeTimeEstA (SLC/Type A): 0x01=0-10%, 0x02=10-20%, ..., 0x0A=90-100%, 0x0B=exceeded
    pub life_time_est_a: u8,
    /// bDeviceLifeTimeEstB (MLC/TLC/Type B): same encoding as A
    pub life_time_est_b: u8,
}

#[derive(Debug, Clone)]
pub struct NvmeHealthData {
    pub temperature_c: i16,
    pub percentage_used: u8,
    pub available_spare_pct: u8,
    pub available_spare_threshold: u8,
    pub power_on_hours: u64,
    pub power_cycles: u64,
    pub data_units_written: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    pub critical_warning: u8,
}

// ── Static attribute name table ───────────────────────────────────────────────

/// Rebuild a full [`AtaAttribute`] from the fields that are persisted in a
/// snapshot's `raw_json` — the name and status are *derived*, not stored, so a
/// report reconstructed from the database is identical to a freshly read one.
pub(crate) fn ata_attribute_from_parts(
    id: u8,
    current: u8,
    worst: u8,
    threshold: u8,
    raw: u64,
) -> AtaAttribute {
    AtaAttribute {
        id,
        name: attr_name(id),
        current,
        worst,
        threshold,
        raw,
        is_critical: is_critical(id),
        status: compute_status(id, current, worst, threshold, raw),
    }
}

fn attr_name(id: u8) -> &'static str {
    match id {
        0x01 => "Read Error Rate",
        0x02 => "Throughput Performance",
        0x03 => "Spin-Up Time",
        0x04 => "Start/Stop Count",
        0x05 => "Reallocated Sectors",
        0x06 => "Read Channel Margin",
        0x07 => "Seek Error Rate",
        0x08 => "Seek Time Performance",
        0x09 => "Power-On Hours",
        0x0A => "Spin Retry Count",
        0x0B => "Recalibration Retries",
        0x0C => "Power Cycle Count",
        0x0D => "Soft Read Error Rate",
        0xAA => "Available Reserved Space",
        0xAB => "Program Fail Count",
        0xAC => "Erase Fail Count",
        0xAD => "Wear Leveling Count",
        0xAE => "Unexpected Power Loss",
        0xAF => "Program Fail Count (chip)",
        0xB0 => "Erase Fail Count (chip)",
        0xB1 => "Wear Range Delta",
        0xB3 => "Used Reserved Block Count",
        0xB4 => "Unused Reserved Block Count",
        0xB5 => "Program Fail Count Total",
        0xB6 => "Erase Fail Count Total",
        0xB7 => "Runtime Bad Block Total",
        0xBB => "Uncorrectable Error Count",
        0xBC => "Command Timeout Count",
        0xBD => "High Fly Writes",
        0xBE => "Airflow Temperature",
        0xBF => "G-Sense Error Rate",
        0xC0 => "Power-off Retract Count",
        0xC1 => "Load Cycle Count",
        0xC2 => "Temperature",
        0xC3 => "Hardware ECC Recovered",
        0xC4 => "Reallocation Event Count",
        0xC5 => "Current Pending Sectors",
        0xC6 => "Uncorrectable Sectors",
        0xC7 => "UltraDMA CRC Errors",
        0xC8 => "Write Error Rate",
        0xC9 => "Soft Read Error Rate",
        0xCA => "Data Address Mark Errors",
        0xCB => "Run Out Cancel",
        0xCC => "Soft ECC Correction",
        0xCD => "Thermal Asperity Rate",
        0xCE => "Flying Height",
        0xCF => "Spin High Current",
        0xD0 => "Spin Buzz",
        0xD1 => "Offline Seek Performance",
        0xD3 => "Vibration During Write",
        0xD4 => "Shock During Write",
        0xDC => "Disk Shift",
        0xDD => "G-Sense Error Rate (alt)",
        0xDE => "Loaded Hours",
        0xDF => "Load/Unload Retry Count",
        0xE0 => "Load Friction",
        0xE1 => "Load/Unload Cycle Count",
        0xE2 => "Load-in Time",
        0xE3 => "Torque Amplification Count",
        0xE4 => "Power-Off Retract Cycle",
        0xE6 => "GMR Head Amplitude",
        0xE7 => "SSD Life Left",
        0xE8 => "Available Reserved Space",
        0xE9 => "NAND Writes (1 GiB)",
        0xEA => "NAND Bytes Written",
        0xEB => "NAND Pages Written",
        0xF0 => "Head Flying Hours",
        0xF1 => "Total LBAs Written",
        0xF2 => "Total LBAs Read",
        0xF3 => "Total NAND Writes",
        0xF4 => "Total NAND Reads",
        0xF9 => "NAND Writes (cycles)",
        0xFA => "Read Error Retry Rate",
        0xFB => "Minimum Spares Remaining",
        0xFC => "Newly Added Bad Flash Block",
        0xFE => "Free Fall Protection",
        _ => "Unknown",
    }
}

/// The meaningful part of a packed 48-bit raw value, for display.
///
/// Several attributes pack more than one field into the six raw bytes. Power-On
/// Hours is the visible offender: a Seagate ST2000DM008 sitting at 10,231 hours
/// reports `0xCEF0_0000_27F7` = 227,530,187,483,127, because the vendor leaves
/// its own data in the high word while the hours live in the low 32 bits.
///
/// `query_ata` routes its vitals extraction through here too, so the Vitals card
/// and the attribute grid cannot report different numbers for the same fact —
/// which is exactly what they used to do (known-issues KI-27).
///
/// The untouched 48-bit value stays on [`AtaAttribute::raw`]: the history-DB
/// snapshot archives it, and a diagnostic record must not lose the vendor's
/// bytes just because the UI cannot render them usefully.
pub(crate) fn display_raw(id: u8, raw: u64) -> u64 {
    match id {
        // Hour counters — hours in the low 32 bits, vendor data above.
        // 0x09 Power-On Hours, 0xF0 Head Flying Hours.
        0x09 | 0xF0 => raw & 0xFFFF_FFFF,
        // 0x0C Power Cycle Count — same shape.
        0x0C => raw & 0xFFFF_FFFF,
        // Temperature attributes carry the current reading in the low byte and
        // frequently min/max above it. 0xC2 Temperature, 0xBE Airflow Temp.
        0xC2 | 0xBE => raw & 0xFF,
        _ => raw,
    }
}

/// Critical attributes — a non-zero raw value or current ≤ threshold is a failure indicator.
///
/// Deliberately excludes **0x01 Read Error Rate**: its raw is a vendor-packed
/// rate, not a count, and is large and non-zero on perfectly healthy Seagate and
/// WD drives (the ST2000DM008 above reports 120,202,145 with a healthy
/// normalised 81 against a threshold of 6). Treating that as "non-zero raw ⇒
/// warning" flagged every such drive amber forever. The normalised
/// current/worst-vs-threshold checks below still catch a genuinely failing
/// read-error rate, which is how smartctl and CrystalDiskInfo judge this
/// attribute too. Same reasoning is why 0x07 Seek Error Rate was never here.
fn is_critical(id: u8) -> bool {
    matches!(id, 0x05 | 0x0A | 0xBB | 0xBC | 0xC4 | 0xC5 | 0xC6 | 0xC7)
}

/// Info-only attributes — normalised values are meaningful but threshold is 0.
fn is_info_only(id: u8) -> bool {
    matches!(
        id,
        0x03 | 0x04 | 0x09 | 0x0C | 0xAD | 0xAE | 0xBE | 0xC0 | 0xC1 | 0xC2
            | 0xC3 | 0xE7 | 0xE8 | 0xE9 | 0xF0 | 0xF1 | 0xF2
    )
}

fn compute_status(id: u8, current: u8, worst: u8, threshold: u8, raw: u64) -> AttrStatus {
    if is_info_only(id) {
        return AttrStatus::Info;
    }
    // Failure: current or worst at/below threshold
    if threshold > 0 && (current <= threshold || worst <= threshold) {
        return AttrStatus::Failed;
    }
    // Warning: within 10% of threshold, or non-zero raw for critical IDs
    if threshold > 0 && current <= threshold + (threshold / 10).max(2) {
        return AttrStatus::Warning;
    }
    if is_critical(id) && raw > 0 {
        return AttrStatus::Warning;
    }
    AttrStatus::Good
}

/// Build an [`AtaAttribute`] from the four raw SMART fields, deriving the name,
/// criticality and status the same way the ATA reader does.  Shared so demo
/// data (`crate::demo`) cannot drift from what a real drive would produce.
pub(crate) fn ata_attribute(
    id: u8,
    current: u8,
    worst: u8,
    threshold: u8,
    raw: u64,
) -> AtaAttribute {
    AtaAttribute {
        id,
        name: attr_name(id),
        current,
        worst,
        threshold,
        raw,
        is_critical: is_critical(id),
        status: compute_status(id, current, worst, threshold, raw),
    }
}


// ── Shared byte-level parsers (fed by every platform transport) ──────────────

/// Parse the 512-byte ATA SMART READ DATA payload (+ optional READ THRESHOLDS
/// payload): 30 entries × 12 bytes starting at offset 2 —
/// `[id][flags:u16][current][worst][raw: 6 bytes LE][reserved]`. Vitals are
/// routed through `display_raw` so the Vitals card and the attribute grid can
/// never disagree about the same number (KI-27).
pub(crate) fn parse_ata_smart(attr_payload: &[u8], thr_payload: Option<&[u8]>) -> SmartReport {
    let mut attributes: Vec<AtaAttribute> = Vec::new();
    let mut power_on_hours: Option<u64> = None;
    let mut power_cycles: Option<u64> = None;
    let mut temperature_c: Option<i32> = None;

    for i in 0..30 {
        let off = 2 + i * 12;
        if off + 12 > attr_payload.len() {
            break;
        }
        let id = attr_payload[off];
        if id == 0 {
            continue;
        }

        let flags = u16::from_le_bytes([attr_payload[off + 1], attr_payload[off + 2]]);
        let current = attr_payload[off + 3];
        let worst = attr_payload[off + 4];
        // Raw value: 6 bytes at off+5, little-endian
        let raw_bytes = &attr_payload[off + 5..off + 11];
        let raw = u64::from_le_bytes([
            raw_bytes[0], raw_bytes[1], raw_bytes[2],
            raw_bytes[3], raw_bytes[4], raw_bytes[5], 0, 0,
        ]);
        let _ = flags; // stored in the payload but not currently displayed

        let threshold = thr_payload
            .and_then(|t| {
                // Threshold block: first 2 bytes header, then 30×12 entries
                for j in 0..30 {
                    let to = 2 + j * 12;
                    if to + 2 <= t.len() && t[to] == id {
                        return Some(t[to + 1]);
                    }
                }
                None
            })
            .unwrap_or(0);

        match id {
            0x09 => power_on_hours = Some(display_raw(id, raw)),
            0x0C => power_cycles = Some(display_raw(id, raw)),
            0xC2 | 0xBE => {
                let t = display_raw(id, raw) as i32;
                if t > 0 && t < 100 {
                    temperature_c = Some(t);
                }
            }
            _ => {}
        }

        attributes.push(ata_attribute(id, current, worst, threshold, raw));
    }

    if attributes.is_empty() {
        return SmartReport::Unavailable {
            reason: "Drive returned no SMART attributes.".to_string(),
        };
    }

    SmartReport::Ata(AtaSmartData {
        attributes,
        power_on_hours,
        power_cycles,
        temperature_c,
    })
}

/// Parse an NVMe SMART / Health Information log page (log page 0x02).
pub(crate) fn parse_nvme_health_log(log: &[u8]) -> SmartReport {
    if log.len() < 512 {
        return SmartReport::Unavailable {
            reason: "NVMe health log too short.".to_string(),
        };
    }

    let critical_warning = log[0];

    // Composite temperature: bytes 1-2 (Kelvin, LE), convert to Celsius
    let temp_k = u16::from_le_bytes([log[1], log[2]]) as i32;
    let temperature_c = if temp_k > 273 { (temp_k - 273) as i16 } else { 0 };

    let available_spare_pct = log[3];
    let available_spare_threshold = log[4];
    let percentage_used = log[5];

    // 128-bit LE fields — take lower 64 bits
    let le128 = |off: usize| -> u64 {
        u64::from_le_bytes(log[off..off + 8].try_into().unwrap_or([0; 8]))
    };

    let _data_units_read = le128(32);
    let data_units_written = le128(48);
    let _host_reads = le128(64);
    let _host_writes = le128(80);
    let power_cycles = le128(112);
    let power_on_hours = le128(128);
    let unsafe_shutdowns = le128(144);
    let media_errors = le128(160);

    SmartReport::Nvme(NvmeHealthData {
        temperature_c,
        percentage_used,
        available_spare_pct,
        available_spare_threshold,
        power_on_hours,
        power_cycles,
        data_units_written,
        unsafe_shutdowns,
        media_errors,
        critical_warning,
    })
}

/// Parse a UFS Device Health descriptor (IDN 0x25):
/// `[0]=bLength [1]=bDescriptorIDN [2]=bPreEOLInfo [3]=bDeviceLifeTimeEstA
/// [4]=bDeviceLifeTimeEstB`.
#[cfg_attr(not(windows), allow(dead_code))] // Linux reads the descriptor fields from sysfs instead
pub(crate) fn parse_ufs_health_descriptor(desc: &[u8]) -> SmartReport {
    if desc.len() < 5 {
        return SmartReport::Unavailable {
            reason: "UFS returned incomplete health descriptor.".to_string(),
        };
    }
    let idn = desc[1];
    if idn != 0x25 {
        return SmartReport::Unavailable {
            reason: format!("UFS returned unexpected descriptor type (0x{idn:02X}), expected 0x25."),
        };
    }
    SmartReport::Ufs(UfsHealthData {
        pre_eol_info: desc[2],
        life_time_est_a: desc[3],
        life_time_est_b: desc[4],
    })
}

// ── Platform transports ──────────────────────────────────────────────────────

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::query_smart_detail;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::query_smart_detail;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn query_smart_detail(_device_path: &str, _bus: crate::detected_drive::BusKind) -> SmartReport {
    SmartReport::Unavailable {
        reason: "SMART queries are not implemented on this platform.".to_string(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_names_and_flags() {
        assert_eq!(attr_name(0x05), "Reallocated Sectors");
        assert_eq!(attr_name(0xC2), "Temperature");
        assert_eq!(attr_name(0x00), "Unknown");
        assert!(is_critical(0x05));
        assert!(!is_critical(0x09));
        assert!(is_info_only(0x09));
        assert!(!is_info_only(0x05));
    }

    #[test]
    fn compute_status_classification() {
        // Info-only IDs are always Info, regardless of values.
        assert_eq!(compute_status(0x09, 100, 100, 0, 1234), AttrStatus::Info);
        // Failed: current at/below threshold.
        assert_eq!(compute_status(0x01, 10, 50, 20, 0), AttrStatus::Failed);
        // Warning: within the 10% band above threshold.
        assert_eq!(compute_status(0x01, 21, 90, 20, 0), AttrStatus::Warning);
        // Good: healthy normalised value, zero raw.
        assert_eq!(compute_status(0x01, 100, 100, 20, 0), AttrStatus::Good);
        // Critical ID with non-zero raw and no threshold → Warning.
        assert_eq!(compute_status(0x05, 100, 100, 0, 1), AttrStatus::Warning);
    }

    /// Observed on a real Seagate ST2000DM008-2FR102 at 10,231 power-on hours:
    /// the drive reports `0xCEF0_0000_27F7`, hours in the low 32 bits and vendor
    /// data in the high word. Printing that verbatim gave 227,530,187,483,127
    /// in the attribute grid while the Vitals card said 10,231 (KI-27).
    #[test]
    fn display_raw_unpacks_vendor_hour_counters() {
        let packed = 0xCEF0_0000_27F7_u64;
        assert_eq!(packed, 227_530_187_483_127);
        assert_eq!(display_raw(0x09, packed), 10_231);
        // Head Flying Hours and Power Cycle Count pack the same way.
        assert_eq!(display_raw(0xF0, packed), 10_231);
        assert_eq!(display_raw(0x0C, packed), 10_231);
    }

    #[test]
    fn display_raw_takes_the_low_byte_of_temperature_attributes() {
        // Current in the low byte, min/max above it.
        assert_eq!(display_raw(0xC2, 0x0015_0032_0026), 38);
        assert_eq!(display_raw(0xBE, 38), 38);
    }

    #[test]
    fn display_raw_leaves_genuine_counts_untouched() {
        // Sector counts are plain scalars — masking them would corrupt a real
        // failure signal.
        assert_eq!(display_raw(0x05, 24), 24);
        assert_eq!(display_raw(0xC5, 8), 8);
        assert_eq!(display_raw(0xC6, 2), 2);
        assert_eq!(display_raw(0xC7, 0), 0);
        // Seagate's packed error *rates* are shown whole, as smartctl and
        // CrystalDiskInfo do — there is no portable way to decode them.
        assert_eq!(display_raw(0x01, 120_202_145), 120_202_145);
        assert_eq!(display_raw(0x07, 102_114_014), 102_114_014);
    }

    /// The same drive reports Read Error Rate raw 120,202,145 with a healthy
    /// normalised 81 against threshold 6. Treating a packed rate as a count
    /// flagged every Seagate/WD drive amber forever (KI-27).
    #[test]
    fn read_error_rate_does_not_warn_on_a_packed_raw() {
        assert!(!is_critical(0x01));
        assert_eq!(
            compute_status(0x01, 81, 64, 6, 120_202_145),
            AttrStatus::Good
        );
        // …but a genuinely failing read error rate still trips the normalised
        // threshold checks.
        assert_eq!(compute_status(0x01, 5, 5, 6, 0), AttrStatus::Failed);
        // And real sector counts still warn.
        assert_eq!(compute_status(0xC5, 100, 100, 0, 8), AttrStatus::Warning);
    }

    #[test]
    fn parse_nvme_log_extracts_fields() {
        let mut log = vec![0u8; 512];
        log[0] = 0x01; // critical warning
        log[1..3].copy_from_slice(&313u16.to_le_bytes()); // 313 K = 40 °C
        log[3] = 95; // available spare %
        log[4] = 10; // available spare threshold %
        log[5] = 7; // percentage used
        log[112..120].copy_from_slice(&42u64.to_le_bytes()); // power cycles
        log[128..136].copy_from_slice(&1234u64.to_le_bytes()); // power-on hours

        match parse_nvme_health_log(&log) {
            SmartReport::Nvme(d) => {
                assert_eq!(d.critical_warning, 0x01);
                assert_eq!(d.temperature_c, 40);
                assert_eq!(d.available_spare_pct, 95);
                assert_eq!(d.available_spare_threshold, 10);
                assert_eq!(d.percentage_used, 7);
                assert_eq!(d.power_cycles, 42);
                assert_eq!(d.power_on_hours, 1234);
            }
            other => panic!("expected Nvme, got {other:?}"),
        }
    }

    #[test]
    fn parse_nvme_log_rejects_short_buffer() {
        assert!(matches!(
            parse_nvme_health_log(&[0u8; 100]),
            SmartReport::Unavailable { .. }
        ));
    }

    /// End-to-end fixture through the shared ATA payload parser: one packed
    /// Power-On Hours entry and one pending-sector entry, with a matching
    /// threshold block.
    #[test]
    fn parse_ata_smart_payload_roundtrip() {
        let mut attrs = vec![0u8; 512];
        // entry 0: id 0x09, current 98, worst 97, raw = packed hours
        attrs[2] = 0x09;
        attrs[5] = 98;
        attrs[6] = 97;
        attrs[7..13].copy_from_slice(&0xCEF0_0000_27F7_u64.to_le_bytes()[..6]);
        // entry 1: id 0xC5, current 100, worst 100, raw = 8 pending sectors
        attrs[14] = 0xC5;
        attrs[17] = 100;
        attrs[18] = 100;
        attrs[19..25].copy_from_slice(&8u64.to_le_bytes()[..6]);
        let mut thr = vec![0u8; 512];
        thr[2] = 0x09; // id
        thr[3] = 0;    // threshold
        thr[14] = 0xC5;
        thr[15] = 0;

        match parse_ata_smart(&attrs, Some(&thr)) {
            SmartReport::Ata(d) => {
                assert_eq!(d.power_on_hours, Some(10_231));
                assert_eq!(d.attributes.len(), 2);
                let pending = &d.attributes[1];
                assert_eq!(pending.id, 0xC5);
                assert_eq!(pending.status, AttrStatus::Warning);
            }
            other => panic!("expected Ata, got {other:?}"),
        }
    }

    #[test]
    fn parse_ufs_descriptor_checks_idn() {
        let good = [0x09, 0x25, 0x01, 0x02, 0x03];
        match parse_ufs_health_descriptor(&good) {
            SmartReport::Ufs(d) => {
                assert_eq!(d.pre_eol_info, 0x01);
                assert_eq!(d.life_time_est_a, 0x02);
                assert_eq!(d.life_time_est_b, 0x03);
            }
            other => panic!("expected Ufs, got {other:?}"),
        }
        let wrong_idn = [0x09, 0x24, 0x01, 0x02, 0x03];
        assert!(matches!(
            parse_ufs_health_descriptor(&wrong_idn),
            SmartReport::Unavailable { .. }
        ));
    }

    #[test]
    fn parse_ata_smart_empty_payload_is_unavailable() {
        assert!(matches!(
            parse_ata_smart(&[0u8; 512], None),
            SmartReport::Unavailable { .. }
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod hardware_tests {
    use super::SmartReport;

    /// Diagnostic, not CI: queries every enumerated drive's real SMART data.
    /// Needs root for the ioctls; compare against `smartctl -A <dev>`.
    #[test]
    #[ignore = "needs root; run via scripts/test-elevated.sh"]
    fn print_real_smart_reports() {
        let drives = crate::drive_enumeration::enumerate_physical_disks().expect("enumerate");
        assert!(!drives.is_empty(), "no drives found");
        for d in &drives {
            println!("--- {} ({:?}) {}", d.device_id, d.bus, d.model.trim());
            match super::query_smart_detail(&d.device_id, d.bus) {
                SmartReport::Nvme(n) => {
                    println!(
                        "NVMe: temp={}°C wear={}% spare={}% (thr {}%) poh={}h cycles={} \
                         written={} unsafe_shutdowns={} media_errors={} crit=0x{:02X}",
                        n.temperature_c, n.percentage_used, n.available_spare_pct,
                        n.available_spare_threshold, n.power_on_hours, n.power_cycles,
                        n.data_units_written, n.unsafe_shutdowns, n.media_errors,
                        n.critical_warning,
                    );
                }
                SmartReport::Ata(a) => {
                    println!(
                        "ATA: temp={:?}°C poh={:?}h cycles={:?}, {} attributes:",
                        a.temperature_c, a.power_on_hours, a.power_cycles,
                        a.attributes.len()
                    );
                    for at in &a.attributes {
                        println!(
                            "  {:#04x} {:<28} cur={:>3} worst={:>3} thr={:>3} raw={} ({:?})",
                            at.id, at.name, at.current, at.worst, at.threshold,
                            at.display_raw(), at.status,
                        );
                    }
                }
                SmartReport::Ufs(u) => println!(
                    "UFS: pre_eol={:#04x} lt_a={:#04x} lt_b={:#04x}",
                    u.pre_eol_info, u.life_time_est_a, u.life_time_est_b
                ),
                SmartReport::Unavailable { reason } => println!("Unavailable: {reason}"),
            }
        }
    }
}
