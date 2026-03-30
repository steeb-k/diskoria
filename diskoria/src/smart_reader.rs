//! Raw SMART / NVMe health data via Windows DeviceIoControl.
//!
//! Only compiled on Windows.  Non-Windows builds get stub `Unavailable` returns.

// ── Public types (all platforms) ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SmartReport {
    Ata(AtaSmartData),
    Nvme(NvmeHealthData),
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct AtaSmartData {
    pub attributes: Vec<AtaAttribute>,
    pub self_test: Option<SelfTestResult>,
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
    pub raw: u64,
    pub is_critical: bool,
    pub status: AttrStatus,
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

#[derive(Debug, Clone)]
pub struct SelfTestResult {
    pub kind: SelfTestKind,
    pub status: SelfTestStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTestKind {
    Short,
    Long,
    Unknown,
}

impl SelfTestKind {
    pub fn label(self) -> &'static str {
        match self {
            SelfTestKind::Short => "Short",
            SelfTestKind::Long => "Extended",
            SelfTestKind::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SelfTestStatus {
    Passed,
    Failed { reason: String },
    InProgress { pct_remaining: u8 },
    Aborted,
    NeverRun,
    Unknown,
}

// ── Static attribute name table ───────────────────────────────────────────────

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

/// Critical attributes — a non-zero raw value or current ≤ threshold is a failure indicator.
fn is_critical(id: u8) -> bool {
    matches!(
        id,
        0x01 | 0x05 | 0x0A | 0xBB | 0xBC | 0xC4 | 0xC5 | 0xC6 | 0xC7
    )
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

// ── Non-Windows stub ──────────────────────────────────────────────────────────

#[cfg(not(windows))]
pub fn query_smart_detail(_device_path: &str, _bus: crate::detected_drive::BusKind) -> SmartReport {
    SmartReport::Unavailable {
        reason: "SMART queries require Windows.".to_string(),
    }
}

#[cfg(not(windows))]
pub fn trigger_self_test(_device_path: &str, _long_test: bool) -> bool { false }

#[cfg(not(windows))]
pub fn abort_self_test(_device_path: &str) -> bool { false }

// ── Windows implementation ────────────────────────────────────────────────────

#[cfg(windows)]
pub fn query_smart_detail(device_path: &str, bus: crate::detected_drive::BusKind) -> SmartReport {
    use crate::detected_drive::BusKind;
    match bus {
        BusKind::Nvme => query_nvme(device_path),
        BusKind::Sata => query_ata(device_path),
        BusKind::Usb | BusKind::Ufs => SmartReport::Unavailable {
            reason: "SMART is not available over USB or UFS connections.".to_string(),
        },
    }
}

/// Send an ATA SMART self-test or abort command.
/// Returns `true` if the IOCTL was accepted by the driver.
#[cfg(windows)]
fn send_self_test_cmd(device_path: &str, subcommand: u8) -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const SMART_SEND_DRIVE_COMMAND: u32 = 0x0007C084;
    const EXECUTE_OFFLINE_DIAGS: u8 = 0xD4;

    let path_wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::warn!(target: "diskoria", "send_self_test_cmd: CreateFileW failed path={device_path} err={err}");
        return false;
    }

    // SENDCMDINPARAMS: 32 bytes (IDEREGS layout)
    let mut cmd_in = [0u8; 32];
    cmd_in[0..4].copy_from_slice(&512u32.to_le_bytes()); // cBufferSize
    cmd_in[4] = subcommand;      // bFeaturesReg (subcommand)
    cmd_in[5] = 0x00;            // bSectorCountReg
    cmd_in[6] = 0x00;            // bSectorNumberReg
    cmd_in[7] = 0x4F;            // bCylLowReg
    cmd_in[8] = 0xC2;            // bCylHighReg
    cmd_in[9] = 0xA0;            // bDriveHeadReg
    cmd_in[10] = EXECUTE_OFFLINE_DIAGS; // bCommandReg
    cmd_in[11] = 0x00;           // bReserved

    let mut cmd_out = [0u8; 16];
    let mut bytes_returned = 0u32;

    let ok = unsafe {
        let result = DeviceIoControl(
            handle,
            SMART_SEND_DRIVE_COMMAND,
            cmd_in.as_mut_ptr() as *mut _,
            cmd_in.len() as u32,
            cmd_out.as_mut_ptr() as *mut _,
            cmd_out.len() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        ) != 0;
        windows_sys::Win32::Foundation::CloseHandle(handle);
        result
    };

    if ok {
        log::info!(target: "diskoria", "send_self_test_cmd: OK path={device_path} sub={subcommand:#04X}");
    } else {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::warn!(target: "diskoria", "send_self_test_cmd: DeviceIoControl failed path={device_path} sub={subcommand:#04X} err={err}");
    }
    ok
}

/// Start an ATA short (long_test=false) or extended (long_test=true) self-test.
/// Returns true if the command was accepted.
#[cfg(windows)]
pub fn trigger_self_test(device_path: &str, long_test: bool) -> bool {
    let sub = if long_test { 0xC1u8 } else { 0x01u8 };
    send_self_test_cmd(device_path, sub)
}

/// Abort any running ATA self-test.
/// Returns true if the abort command was accepted.
#[cfg(windows)]
pub fn abort_self_test(device_path: &str) -> bool {
    send_self_test_cmd(device_path, 0x7F)
}

// ── ATA SMART via SMART_RCV_DRIVE_DATA ───────────────────────────────────────

#[cfg(windows)]
fn query_ata(device_path: &str) -> SmartReport {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const SMART_RCV_DRIVE_DATA: u32 = 0x0007C088;
    const SMART_READ_DATA: u8 = 0xD0;
    const SMART_READ_THRESHOLDS: u8 = 0xD1;
    const READ_LOG_SECTOR: u8 = 0xD5;
    const SMART_CMD: u8 = 0xB0;

    let path_wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::warn!(target: "diskoria", "query_ata: CreateFileW failed path={device_path} err={err}");
        return SmartReport::Unavailable {
            reason: format!("Could not open drive (error {err})."),
        };
    }

    // SENDCMDINPARAMS = 32 bytes
    // SENDCMDOUTPARAMS header = 16 bytes (4 cBufferSize + 12 DRIVERSTATUS)
    // Data payload = 512 bytes
    const OUT_HDR: usize = 16;
    const DATA_SZ: usize = 512;
    const OUT_SZ: usize = OUT_HDR + DATA_SZ;

    let make_cmd = |feature: u8| -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0..4].copy_from_slice(&(DATA_SZ as u32).to_le_bytes()); // cBufferSize
        // IDEREGS at offset 4:
        b[4] = feature; // bFeaturesReg
        b[5] = 0x01;    // bSectorCountReg (must be 1 for read data)
        b[6] = 0x00;    // bSectorNumberReg
        b[7] = 0x4F;    // bCylLowReg (SMART magic)
        b[8] = 0xC2;    // bCylHighReg (SMART magic)
        b[9] = 0xA0;    // bDriveHeadReg
        b[10] = SMART_CMD; // bCommandReg
        b
    };

    // --- Read attribute data ---
    let cmd_data = make_cmd(SMART_READ_DATA);
    let mut out_data = vec![0u8; OUT_SZ];
    let mut br = 0u32;
    let ok_data = unsafe {
        DeviceIoControl(
            handle,
            SMART_RCV_DRIVE_DATA,
            cmd_data.as_ptr() as *mut _,
            cmd_data.len() as u32,
            out_data.as_mut_ptr() as *mut _,
            out_data.len() as u32,
            &mut br,
            std::ptr::null_mut(),
        )
    } != 0;

    if !ok_data {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::warn!(target: "diskoria", "query_ata: SMART_READ_DATA failed path={device_path} err={err}");
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return SmartReport::Unavailable {
            reason: format!("Drive did not respond to SMART data request (error {err})."),
        };
    }

    // Check DRIVERSTATUS (bytes 4..16 of output, i.e. OUT_HDR offset 4)
    let driver_err = out_data[4];
    if driver_err != 0 {
        log::warn!(target: "diskoria", "query_ata: DRIVERSTATUS.bDriverError={driver_err}");
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return SmartReport::Unavailable {
            reason: "Drive reported a SMART driver error.".to_string(),
        };
    }

    // --- Read threshold data ---
    let cmd_thr = make_cmd(SMART_READ_THRESHOLDS);
    let mut out_thr = vec![0u8; OUT_SZ];
    let ok_thr = unsafe {
        DeviceIoControl(
            handle,
            SMART_RCV_DRIVE_DATA,
            cmd_thr.as_ptr() as *mut _,
            cmd_thr.len() as u32,
            out_thr.as_mut_ptr() as *mut _,
            out_thr.len() as u32,
            &mut br,
            std::ptr::null_mut(),
        )
    } != 0;

    // --- Read self-test log (log address 0x06) ---
    let mut self_test_result: Option<SelfTestResult> = None;
    let mut cmd_log = make_cmd(READ_LOG_SECTOR);
    cmd_log[6] = 0x06; // bSectorNumberReg = log address
    let mut out_log = vec![0u8; OUT_SZ];
    let ok_log = unsafe {
        DeviceIoControl(
            handle,
            SMART_RCV_DRIVE_DATA,
            cmd_log.as_ptr() as *mut _,
            cmd_log.len() as u32,
            out_log.as_mut_ptr() as *mut _,
            out_log.len() as u32,
            &mut br,
            std::ptr::null_mut(),
        )
    } != 0;

    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    // --- Parse attribute data (30 entries × 12 bytes at offset 2 in the payload) ---
    let attr_payload = &out_data[OUT_HDR..OUT_HDR + DATA_SZ];
    let thr_payload = if ok_thr { Some(&out_thr[OUT_HDR..OUT_HDR + DATA_SZ]) } else { None };

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
        let _ = flags; // stored in file but not currently displayed

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

        let crit = is_critical(id);
        let status = compute_status(id, current, worst, threshold, raw);
        let name = attr_name(id);

        // Extract high-level vitals from well-known attribute IDs
        match id {
            0x09 => power_on_hours = Some(raw & 0xFFFF_FFFF),
            0x0C => power_cycles = Some(raw & 0xFFFF_FFFF),
            0xC2 | 0xBE => {
                // Temperature: lower byte of raw
                let t = (raw & 0xFF) as i32;
                if t > 0 && t < 100 {
                    temperature_c = Some(t);
                }
            }
            _ => {}
        }

        attributes.push(AtaAttribute {
            id,
            name,
            current,
            worst,
            threshold,
            raw,
            is_critical: crit,
            status,
        });
    }

    // --- Parse self-test log ---
    if ok_log {
        let log_payload = &out_log[OUT_HDR..OUT_HDR + DATA_SZ];
        // Most recent entry starts at byte 2, each entry is 24 bytes, up to 21 entries
        // Byte 2: descriptor index (1-based) of the most recent entry
        let most_recent_idx = log_payload[2] as usize;
        if most_recent_idx > 0 {
            // Entry n is at offset 2 + (n-1)*24
            let entry_off = 2 + (most_recent_idx.saturating_sub(1)) * 24;
            if entry_off + 24 <= log_payload.len() {
                let e = &log_payload[entry_off..entry_off + 24];
                let status_nibble = (e[1] >> 4) & 0x0F;
                let test_type = e[0] & 0x7F;
                let pct_rem = e[2];

                let kind = match test_type {
                    0x01 => SelfTestKind::Short,
                    0x02 => SelfTestKind::Long,
                    _ => SelfTestKind::Unknown,
                };
                let st = match status_nibble {
                    0x00 => SelfTestStatus::Passed,
                    0x07 => SelfTestStatus::Aborted,
                    0x01..=0x08 => SelfTestStatus::InProgress { pct_remaining: pct_rem },
                    _ => SelfTestStatus::Failed {
                        reason: format!("Error code 0x{:X}", status_nibble),
                    },
                };
                self_test_result = Some(SelfTestResult { kind, status: st });
            }
        } else {
            self_test_result = Some(SelfTestResult {
                kind: SelfTestKind::Unknown,
                status: SelfTestStatus::NeverRun,
            });
        }
    }

    if attributes.is_empty() {
        return SmartReport::Unavailable {
            reason: "Drive returned no SMART attributes.".to_string(),
        };
    }

    log::info!(
        target: "diskoria",
        "query_ata: OK path={device_path} attrs={n} temp={temp:?}°C poh={poh:?}h",
        n    = attributes.len(),
        temp = temperature_c,
        poh  = power_on_hours,
    );
    SmartReport::Ata(AtaSmartData {
        attributes,
        self_test: self_test_result,
        power_on_hours,
        power_cycles,
        temperature_c,
    })
}

// ── NVMe health log (log page 0x02) via IOCTL_STORAGE_QUERY_PROPERTY ─────────

#[cfg(windows)]
fn query_nvme(device_path: &str) -> SmartReport {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    // IOCTL_STORAGE_QUERY_PROPERTY = CTL_CODE(0x2D, 0x500, 0, 0) = 0x002D1400
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D1400;
    // StorageDeviceProtocolSpecificProperty = 50 on Win10 20H1+ SDKs.
    // Value 49 is StorageAdapterProtocolSpecificProperty (adapter-level) and returns
    // ERROR_INVALID_FUNCTION (1) when sent to a \\.\PhysicalDriveN handle.
    const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
    // PropertyStandardQuery = 0
    const PROPERTY_STANDARD_QUERY: u32 = 0;
    // ProtocolTypeNvme = 3
    const PROTOCOL_TYPE_NVME: u32 = 3;
    // NVMeDataTypeLogPage = 2
    const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;

    let path_wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        log::warn!(target: "diskoria", "query_nvme: CreateFileW failed path={device_path} err={err}");
        return SmartReport::Unavailable {
            reason: format!("Could not open NVMe drive (error {err})."),
        };
    }

    // Build combined input/output buffer.
    // Layout:
    //   [0..4]   PropertyId      (u32)
    //   [4..8]   QueryType       (u32)
    //   [8..12]  ProtocolType    (u32) — start of AdditionalParameters / STORAGE_PROTOCOL_SPECIFIC_DATA
    //   [12..16] DataType        (u32)
    //   [16..20] RequestValue    (u32) = 0x02 (SMART/Health log page)
    //   [20..24] RequestSubValue (u32) = 0
    //   [24..28] DataOffset      (u32) = 40 (size of STORAGE_PROTOCOL_SPECIFIC_DATA)
    //   [28..32] DataLength      (u32) = 512
    //   [32..36] FixedReturn     (u32) = 0
    //   [36..40] SubValue2       (u32) = 0
    //   [40..44] SubValue3       (u32) = 0
    //   [44..48] SubValue4       (u32) = 0
    //   [48..560] output data    (512 bytes)
    const PROTOCOL_DATA_OFFSET: u32 = 40;
    const LOG_DATA_SZ: u32 = 512;
    const BUF_SZ: usize = 48 + LOG_DATA_SZ as usize;

    let mut buf = vec![0u8; BUF_SZ];
    buf[0..4].copy_from_slice(&STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY.to_le_bytes());
    buf[4..8].copy_from_slice(&PROPERTY_STANDARD_QUERY.to_le_bytes());
    buf[8..12].copy_from_slice(&PROTOCOL_TYPE_NVME.to_le_bytes());
    buf[12..16].copy_from_slice(&NVME_DATA_TYPE_LOG_PAGE.to_le_bytes());
    buf[16..20].copy_from_slice(&2u32.to_le_bytes()); // log page 0x02
    buf[20..24].copy_from_slice(&0u32.to_le_bytes()); // SubValue
    buf[24..28].copy_from_slice(&PROTOCOL_DATA_OFFSET.to_le_bytes());
    buf[28..32].copy_from_slice(&LOG_DATA_SZ.to_le_bytes());

    let mut bytes_returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            buf.as_mut_ptr() as *mut _,
            buf.len() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    } != 0;

    let err = if ok { 0 } else { unsafe { windows_sys::Win32::Foundation::GetLastError() } };
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    if !ok {
        log::warn!(target: "diskoria", "query_nvme: DeviceIoControl failed path={device_path} err={err}");
        return SmartReport::Unavailable {
            reason: format!("NVMe health query failed (error {err}). Drive may not support this query."),
        };
    }

    // The output descriptor header is 8 bytes (Version + Size), followed by
    // STORAGE_PROTOCOL_SPECIFIC_DATA (40 bytes), then the log data.
    // Log data starts at offset 8 + PROTOCOL_DATA_OFFSET = 8 + 40 = 48.
    if bytes_returned < 48 + LOG_DATA_SZ {
        log::warn!(target: "diskoria", "query_nvme: short response path={device_path} bytes={bytes_returned}");
        return SmartReport::Unavailable {
            reason: "NVMe returned incomplete health data.".to_string(),
        };
    }

    let report = parse_nvme_health_log(&buf[48..48 + LOG_DATA_SZ as usize]);
    if let SmartReport::Nvme(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_nvme: OK path={device_path} temp={}°C wear={}% spare={}% poh={}h",
            d.temperature_c, d.percentage_used, d.available_spare_pct, d.power_on_hours,
        );
    }
    report
}

#[cfg(windows)]
fn parse_nvme_health_log(log: &[u8]) -> SmartReport {
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
