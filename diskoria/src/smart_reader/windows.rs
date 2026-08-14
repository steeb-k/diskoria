//! Windows transport for SMART / NVMe / UFS health data (DeviceIoControl).
//! Byte-level parsing lives in the parent module; this file only fetches the
//! raw payloads. Moved from the single-file module in the linux-support split.

use super::{RotationRate, SmartReport};

pub fn query_smart_detail(device_path: &str, bus: crate::detected_drive::BusKind) -> SmartReport {
    use crate::detected_drive::BusKind;
    match bus {
        BusKind::Nvme => query_nvme(device_path),
        BusKind::Sata => query_ata(device_path),
        BusKind::Ufs => query_ufs(device_path),
        // The USBSTOR/UASP stack does not implement SMART_RCV_DRIVE_DATA, but
        // most bridges do implement SAT, so the ATA command goes through the
        // SCSI pass-through instead. Bridges that don't degrade to Unavailable.
        BusKind::Usb => query_ata_sat(device_path),
    }
}

/// Whether the drive is spun down, asked with ATA CHECK POWER MODE — the one
/// command that does not wake it (known-issues KI-58).
///
/// `Unknown` on anything that is not an ATA drive, and on any transport that
/// will not return the register block: the caller polls in that case, so a
/// bridge without `CK_COND` support loses the optimisation, not its monitoring.
pub fn power_mode(device_path: &str, bus: crate::detected_drive::BusKind) -> super::PowerMode {
    use crate::detected_drive::BusKind;
    match bus {
        // No spindle, and no such command.
        BusKind::Nvme | BusKind::Ufs => super::PowerMode::Unknown,
        BusKind::Sata | BusKind::Usb => {
            let Ok(handle) = open_for_passthrough(device_path) else {
                return super::PowerMode::Unknown;
            };
            let sense = sat_non_data(handle, &super::ata16_check_power_mode_cdb())
                .or_else(|_| sat_non_data(handle, &super::ata12_check_power_mode_cdb()));
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            match sense {
                Ok(sense) => super::ata_sector_count_from_sense(&sense)
                    .map(super::power_mode_from_sector_count)
                    .unwrap_or(super::PowerMode::Unknown),
                Err(e) => {
                    log::debug!(target: "diskoria", "power_mode: path={device_path} {e}");
                    super::PowerMode::Unknown
                }
            }
        }
    }
}

/// IDENTIFY DEVICE word 217 for a drive, or `None` when the device will not
/// answer (NVMe/UFS have no such command; some USB bridges refuse SAT).
///
/// Used by drive enumeration to tell a spinning disk from a solid-state one
/// when Windows itself reports `MediaType = Unspecified`, which is what it
/// does for everything behind a USB bridge.
pub fn nominal_rotation_rate(
    device_path: &str,
    bus: crate::detected_drive::BusKind,
) -> Option<RotationRate> {
    use crate::detected_drive::BusKind;
    match bus {
        // Solid state by construction — there is no rotation rate to ask for.
        BusKind::Nvme | BusKind::Ufs => None,
        BusKind::Sata | BusKind::Usb => {
            let handle = open_for_passthrough(device_path).ok()?;
            let result = sat_identify(handle);
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            match result {
                Ok(identify) => super::rotation_rate_from_identify(&identify),
                Err(e) => {
                    log::debug!(target: "diskoria", "nominal_rotation_rate: path={device_path} {e}");
                    None
                }
            }
        }
    }
}

// ── ATA SMART via SMART_RCV_DRIVE_DATA ───────────────────────────────────────

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

    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    let attr_payload = &out_data[OUT_HDR..OUT_HDR + DATA_SZ];
    let thr_payload = if ok_thr { Some(&out_thr[OUT_HDR..OUT_HDR + DATA_SZ]) } else { None };

    let report = super::parse_ata_smart(attr_payload, thr_payload);
    if let SmartReport::Ata(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_ata: OK path={device_path} attrs={n} temp={temp:?}°C poh={poh:?}h",
            n    = d.attributes.len(),
            temp = d.temperature_c,
            poh  = d.power_on_hours,
        );
    }
    report
}

// ── ATA over SCSI pass-through (SAT) — USB bridges ───────────────────────────
//
// A USB-SATA bridge that implements SAT (SCSI/ATA Translation) forwards an ATA
// PASS-THROUGH CDB to the drive verbatim, which is how SMART data and IDENTIFY
// DEVICE reach a disk in an external enclosure. Bridges that don't — and
// USB-NVMe enclosures, which need a vendor-specific protocol instead — fail the
// command; every caller here treats that as "no data", never as an error state.

const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004_D014;
const SCSI_IOCTL_DATA_IN: u8 = 1;
const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;
const SAT_SENSE_LEN: u8 = 32;

/// `SCSI_PASS_THROUGH_DIRECT` — hand-declared because `windows-sys` does not
/// expose it. `repr(C)` reproduces the SDK layout; `length` and
/// `sense_info_offset` are derived from `size_of` rather than hard-coded so the
/// header stays correct without a per-target constant.
#[repr(C)]
struct ScsiPassThroughDirect {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    time_out_value: u32,
    data_buffer: *mut std::ffi::c_void,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

/// The pass-through header and its sense buffer in one allocation — which is
/// what `sense_info_offset` addresses.
#[repr(C)]
struct SptdWithSense {
    sptd: ScsiPassThroughDirect,
    sense: [u8; SAT_SENSE_LEN as usize],
}

fn open_for_passthrough(device_path: &str) -> Result<isize, String> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;

    let path_wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // The pass-through ioctl needs a read/write handle (and administrator
    // rights, which the app manifest already requires). Nothing here writes to
    // the medium — every CDB below is a PIO Data-In command.
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
        return Err(format!("could not open drive (error {err})"));
    }
    Ok(handle)
}

/// Send one 512-byte PIO Data-In ATA command through the SCSI pass-through.
/// `cdb` is the 16- or the 12-byte ATA PASS-THROUGH form.
fn sat_pio_in(handle: isize, cdb: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let mut data = vec![0u8; 512];
    let mut req: SptdWithSense = unsafe { std::mem::zeroed() };
    req.sptd.length = std::mem::size_of::<ScsiPassThroughDirect>() as u16;
    req.sptd.cdb_length = cdb.len() as u8;
    req.sptd.sense_info_length = SAT_SENSE_LEN;
    req.sptd.data_in = SCSI_IOCTL_DATA_IN;
    req.sptd.data_transfer_length = data.len() as u32;
    req.sptd.time_out_value = 10; // seconds
    req.sptd.data_buffer = data.as_mut_ptr() as *mut std::ffi::c_void;
    req.sptd.sense_info_offset = std::mem::size_of::<ScsiPassThroughDirect>() as u32;
    req.sptd.cdb[..cdb.len()].copy_from_slice(cdb);

    // The ioctl reads and writes the same buffer, so both pointers are the one
    // request; taking two `&mut` in the call would not borrow-check.
    let req_ptr = &mut req as *mut SptdWithSense as *mut std::ffi::c_void;
    let req_len = std::mem::size_of::<SptdWithSense>() as u32;
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_SCSI_PASS_THROUGH_DIRECT,
            req_ptr,
            req_len,
            req_ptr,
            req_len,
            &mut returned,
            std::ptr::null_mut(),
        )
    } != 0;
    if !ok {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(format!("pass-through rejected (error {err})"));
    }
    if req.sptd.scsi_status != 0 {
        return Err(format!(
            "drive returned SCSI status {:#04X} (sense key {:#X})",
            req.sptd.scsi_status,
            req.sense[2] & 0x0F
        ));
    }
    // Some bridges report success without moving any data. Left unchecked that
    // parses as a drive with an empty SMART table rather than as a failure.
    if (req.sptd.data_transfer_length as usize) < data.len() || data.iter().all(|&b| b == 0) {
        return Err("pass-through returned no data".to_string());
    }
    Ok(data)
}

/// `sat_pio_in` with the 16-byte CDB, retrying with the 12-byte form for the
/// older bridges that only implement ATA PASS-THROUGH (12).
fn sat_pio_in_either(handle: isize, cdb16: &[u8; 16], cdb12: &[u8; 12]) -> Result<Vec<u8>, String> {
    match sat_pio_in(handle, cdb16) {
        Ok(data) => Ok(data),
        Err(e16) => sat_pio_in(handle, cdb12)
            .map_err(|e12| format!("ATA-16 {e16}; ATA-12 {e12}")),
    }
}

/// Send a non-data ATA command and return the sense buffer it came back with.
///
/// `CK_COND` makes the translator report CHECK CONDITION with sense key
/// RECOVERED ERROR *on success*, carrying the ATA register block — so unlike
/// [`sat_pio_in`], a non-zero SCSI status here is the expected outcome, not a
/// failure.
fn sat_non_data(handle: isize, cdb: &[u8]) -> Result<[u8; SAT_SENSE_LEN as usize], String> {
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let mut req: SptdWithSense = unsafe { std::mem::zeroed() };
    req.sptd.length = std::mem::size_of::<ScsiPassThroughDirect>() as u16;
    req.sptd.cdb_length = cdb.len() as u8;
    req.sptd.sense_info_length = SAT_SENSE_LEN;
    req.sptd.data_in = SCSI_IOCTL_DATA_UNSPECIFIED;
    req.sptd.data_transfer_length = 0;
    req.sptd.time_out_value = 10; // seconds
    req.sptd.data_buffer = std::ptr::null_mut();
    req.sptd.sense_info_offset = std::mem::size_of::<ScsiPassThroughDirect>() as u32;
    req.sptd.cdb[..cdb.len()].copy_from_slice(cdb);

    let req_ptr = &mut req as *mut SptdWithSense as *mut std::ffi::c_void;
    let req_len = std::mem::size_of::<SptdWithSense>() as u32;
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_SCSI_PASS_THROUGH_DIRECT,
            req_ptr,
            req_len,
            req_ptr,
            req_len,
            &mut returned,
            std::ptr::null_mut(),
        )
    } != 0;
    if !ok {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(format!("pass-through rejected (error {err})"));
    }
    Ok(req.sense)
}

fn sat_identify(handle: isize) -> Result<Vec<u8>, String> {
    sat_pio_in_either(
        handle,
        &super::ata16_identify_cdb(),
        &super::ata12_identify_cdb(),
    )
}

fn sat_smart(handle: isize, feature: u8) -> Result<Vec<u8>, String> {
    sat_pio_in_either(
        handle,
        &super::ata16_smart_cdb(feature),
        &super::ata12_smart_cdb(feature),
    )
}

fn query_ata_sat(device_path: &str) -> SmartReport {
    let handle = match open_for_passthrough(device_path) {
        Ok(h) => h,
        Err(e) => {
            log::warn!(target: "diskoria", "query_ata_sat: {e} path={device_path}");
            return SmartReport::Unavailable {
                reason: format!("Could not open drive ({e})."),
            };
        }
    };

    let attrs = sat_smart(handle, 0xD0);
    // Thresholds are best-effort, exactly like the internal ATA path.
    let thresholds = if attrs.is_ok() {
        sat_smart(handle, 0xD1).ok()
    } else {
        None
    };
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    let attr_payload = match attrs {
        Ok(d) => d,
        Err(e) => {
            log::warn!(target: "diskoria", "query_ata_sat: SMART READ DATA failed path={device_path}: {e}");
            return SmartReport::Unavailable {
                reason: "This USB enclosure does not forward SMART commands to the drive. \
                         Connecting the drive directly, or to an enclosure that supports \
                         SAT pass-through, will show its health."
                    .to_string(),
            };
        }
    };

    let report = super::parse_ata_smart(&attr_payload, thresholds.as_deref());
    if let SmartReport::Ata(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_ata_sat: OK path={device_path} attrs={n} temp={temp:?}°C poh={poh:?}h",
            n    = d.attributes.len(),
            temp = d.temperature_c,
            poh  = d.power_on_hours,
        );
    }
    report
}

// ── NVMe health log (log page 0x02) via IOCTL_STORAGE_QUERY_PROPERTY ─────────

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

    let report = super::parse_nvme_health_log(&buf[48..48 + LOG_DATA_SZ as usize]);
    if let SmartReport::Nvme(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_nvme: OK path={device_path} temp={}°C wear={}% spare={}% poh={}h",
            d.temperature_c, d.percentage_used, d.available_spare_pct, d.power_on_hours,
        );
    }
    report
}

// ── UFS health descriptor via IOCTL_STORAGE_QUERY_PROPERTY ──────────────────

fn query_ufs(device_path: &str) -> SmartReport {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D1400;
    const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
    const PROPERTY_STANDARD_QUERY: u32 = 0;
    const PROTOCOL_TYPE_UFS: u32 = 4;
    const UFS_DATA_TYPE_QUERY_DESCRIPTOR: u32 = 1;
    const UFS_HEALTH_DESCRIPTOR_IDN: u32 = 0x25;

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
        log::warn!(target: "diskoria", "query_ufs: CreateFileW failed path={device_path} err={err}");
        return SmartReport::Unavailable {
            reason: format!("Could not open UFS drive (error {err})."),
        };
    }

    // Same layout as NVMe query — STORAGE_PROTOCOL_SPECIFIC_DATA header (48 bytes) + data.
    const PROTOCOL_DATA_OFFSET: u32 = 40;
    const DESCRIPTOR_DATA_SZ: u32 = 64;
    const BUF_SZ: usize = 48 + DESCRIPTOR_DATA_SZ as usize;

    let mut buf = vec![0u8; BUF_SZ];
    buf[0..4].copy_from_slice(&STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY.to_le_bytes());
    buf[4..8].copy_from_slice(&PROPERTY_STANDARD_QUERY.to_le_bytes());
    buf[8..12].copy_from_slice(&PROTOCOL_TYPE_UFS.to_le_bytes());
    buf[12..16].copy_from_slice(&UFS_DATA_TYPE_QUERY_DESCRIPTOR.to_le_bytes());
    buf[16..20].copy_from_slice(&UFS_HEALTH_DESCRIPTOR_IDN.to_le_bytes());
    buf[20..24].copy_from_slice(&0u32.to_le_bytes()); // SubValue (index 0)
    buf[24..28].copy_from_slice(&PROTOCOL_DATA_OFFSET.to_le_bytes());
    buf[28..32].copy_from_slice(&DESCRIPTOR_DATA_SZ.to_le_bytes());

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
        log::warn!(target: "diskoria", "query_ufs: DeviceIoControl failed path={device_path} err={err}");
        let reason = if err == 1 {
            // ERROR_INVALID_FUNCTION — the driver accepted the handle but does not implement
            // UFS protocol-specific property queries (ProtocolType=4). This is a known
            // limitation of some Qualcomm storufs.sys builds on ARM devices; the Health
            // Descriptor (IDN 0x25) is simply not exposed through this IOCTL.
            "UFS health data is not available — this controller's driver does not expose the \
             UFS Health Descriptor. This is a known limitation of some Qualcomm UFS drivers \
             on ARM devices.".to_string()
        } else {
            format!("UFS health query failed (error {err}).")
        };
        return SmartReport::Unavailable { reason };
    }

    if bytes_returned < 48 + 5 {
        log::warn!(target: "diskoria", "query_ufs: short response path={device_path} bytes={bytes_returned}");
        return SmartReport::Unavailable {
            reason: "UFS returned incomplete health descriptor.".to_string(),
        };
    }

    // Health descriptor starts at offset 48.
    let report = super::parse_ufs_health_descriptor(&buf[48..]);
    if let SmartReport::Ufs(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_ufs: OK path={device_path} pre_eol={:#04x} lt_a={:#04x} lt_b={:#04x}",
            d.pre_eol_info, d.life_time_est_a, d.life_time_est_b,
        );
    } else if let SmartReport::Unavailable { ref reason } = report {
        log::warn!(target: "diskoria", "query_ufs: {reason} path={device_path}");
    }
    report
}


