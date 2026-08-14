//! Linux transport for SMART / NVMe / UFS health data.
//!
//! - ATA: `SG_IO` with an ATA PASS-THROUGH (16) CDB — SMART READ DATA
//!   (feature 0xD0) and READ THRESHOLDS (0xD1), same 512-byte payloads the
//!   Windows `SMART_RCV_DRIVE_DATA` ioctl returns.
//! - NVMe: `NVME_IOCTL_ADMIN_CMD` Get Log Page 0x02 on the namespace block
//!   device (the kernel routes admin commands to the controller).
//! - UFS: the kernel's `health_descriptor` sysfs directory.
//!
//! All byte-level parsing is shared with Windows in the parent module.
//! Everything here needs root for the ioctls (the pkexec relaunch provides
//! it); failures degrade to `Unavailable` with a permission-aware reason.

use std::os::fd::AsRawFd;

use super::{RotationRate, SmartReport};

fn open_reason(device_path: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("Could not open {device_path}: permission denied. Run Diskoria as root (pkexec) for SMART access.")
    } else {
        format!("Could not open {device_path}: {e}.")
    }
}

pub fn query_smart_detail(device_path: &str, bus: crate::detected_drive::BusKind) -> SmartReport {
    use crate::detected_drive::BusKind;
    match bus {
        BusKind::Nvme => query_nvme(device_path),
        BusKind::Sata => query_ata(device_path),
        BusKind::Ufs => query_ufs(device_path),
        // Same SG_IO path as SATA: a USB bridge implementing SAT forwards the
        // ATA PASS-THROUGH CDB to the drive unchanged. Bridges that don't (and
        // USB-NVMe enclosures, which speak a vendor protocol) fail the command
        // and land on `Unavailable` with a reason that says so.
        BusKind::Usb => query_ata(device_path),
    }
}

/// IDENTIFY DEVICE word 217 for a drive, or `None` when the device will not
/// answer (NVMe/UFS have no such command; some USB bridges refuse SAT, and
/// without root the device node cannot be opened at all).
///
/// Used by drive enumeration, where `queue/rotational` is not trustworthy for
/// anything behind a USB bridge — the kernel defaults it to 1 unless the
/// bridge exposes the block-limits VPD page.
pub fn nominal_rotation_rate(
    device_path: &str,
    bus: crate::detected_drive::BusKind,
) -> Option<RotationRate> {
    use crate::detected_drive::BusKind;
    match bus {
        // Solid state by construction — there is no rotation rate to ask for.
        BusKind::Nvme | BusKind::Ufs => None,
        BusKind::Sata | BusKind::Usb => {
            let file = std::fs::File::open(device_path)
                .map_err(|e| {
                    log::debug!(target: "diskoria", "nominal_rotation_rate: open {device_path}: {e}");
                })
                .ok()?;
            match sg_ata_pio_in(file.as_raw_fd(), &super::ata16_identify_cdb()) {
                Ok(identify) => super::rotation_rate_from_identify(&identify),
                Err(e) => {
                    log::debug!(target: "diskoria", "nominal_rotation_rate: path={device_path} {e}");
                    None
                }
            }
        }
    }
}

// ── ATA via SG_IO ────────────────────────────────────────────────────────────

const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_NONE: libc::c_int = -1;
const SG_DXFER_FROM_DEV: libc::c_int = -3;

/// <scsi/sg.h> `sg_io_hdr_t` (not exposed by the libc crate).
#[repr(C)]
struct SgIoHdr {
    interface_id: libc::c_int,
    dxfer_direction: libc::c_int,
    cmd_len: libc::c_uchar,
    mx_sb_len: libc::c_uchar,
    iovec_count: libc::c_ushort,
    dxfer_len: libc::c_uint,
    dxferp: *mut libc::c_void,
    cmdp: *mut libc::c_uchar,
    sbp: *mut libc::c_uchar,
    timeout: libc::c_uint,
    flags: libc::c_uint,
    pack_id: libc::c_int,
    usr_ptr: *mut libc::c_void,
    status: libc::c_uchar,
    masked_status: libc::c_uchar,
    msg_status: libc::c_uchar,
    sb_len_wr: libc::c_uchar,
    host_status: libc::c_ushort,
    driver_status: libc::c_ushort,
    resid: libc::c_int,
    duration: libc::c_uint,
    info: libc::c_uint,
}

/// Send one 512-byte PIO Data-In ATA command (the CDBs are built in the parent
/// module, shared with the Windows pass-through).
fn sg_ata_pio_in(fd: libc::c_int, cdb: &[u8; 16]) -> Result<[u8; 512], String> {
    let mut cdb = *cdb;
    let mut data = [0u8; 512];
    let mut sense = [0u8; 32];
    let mut hdr: SgIoHdr = unsafe { std::mem::zeroed() };
    hdr.interface_id = 'S' as libc::c_int;
    hdr.dxfer_direction = SG_DXFER_FROM_DEV;
    hdr.cmd_len = cdb.len() as libc::c_uchar;
    hdr.mx_sb_len = sense.len() as libc::c_uchar;
    hdr.dxfer_len = data.len() as libc::c_uint;
    hdr.dxferp = data.as_mut_ptr() as *mut libc::c_void;
    hdr.cmdp = cdb.as_mut_ptr();
    hdr.sbp = sense.as_mut_ptr();
    hdr.timeout = 5000; // ms

    let rc = unsafe { libc::ioctl(fd, SG_IO, &mut hdr) };
    if rc != 0 {
        return Err(format!("SG_IO failed: {}", std::io::Error::last_os_error()));
    }
    // Any of these non-zero means the command did not complete cleanly.
    if hdr.masked_status != 0 || hdr.host_status != 0 || (hdr.driver_status & !0x08) != 0 {
        return Err(format!(
            "command rejected (status={:#x} host={:#x} driver={:#x})",
            hdr.masked_status, hdr.host_status, hdr.driver_status
        ));
    }
    // Some bridges report success without moving any data. Left unchecked that
    // parses as a drive with an empty SMART table rather than as a failure.
    if hdr.resid >= data.len() as libc::c_int || data.iter().all(|&b| b == 0) {
        return Err("pass-through returned no data".to_string());
    }
    Ok(data)
}

/// Send a non-data ATA command and return the sense buffer it came back with.
///
/// `CK_COND` makes the translator report CHECK CONDITION with sense key
/// RECOVERED ERROR *on success*, carrying the ATA register block — so unlike
/// [`sg_ata_pio_in`], a non-zero status here is the expected outcome.
fn sg_ata_non_data(fd: libc::c_int, cdb: &[u8; 16]) -> Result<[u8; 32], String> {
    let mut cdb = *cdb;
    let mut sense = [0u8; 32];
    let mut hdr: SgIoHdr = unsafe { std::mem::zeroed() };
    hdr.interface_id = 'S' as libc::c_int;
    hdr.dxfer_direction = SG_DXFER_NONE;
    hdr.cmd_len = cdb.len() as libc::c_uchar;
    hdr.mx_sb_len = sense.len() as libc::c_uchar;
    hdr.cmdp = cdb.as_mut_ptr();
    hdr.sbp = sense.as_mut_ptr();
    hdr.timeout = 5000; // ms

    let rc = unsafe { libc::ioctl(fd, SG_IO, &mut hdr) };
    if rc != 0 {
        return Err(format!("SG_IO failed: {}", std::io::Error::last_os_error()));
    }
    if hdr.host_status != 0 {
        return Err(format!("command rejected (host={:#x})", hdr.host_status));
    }
    if hdr.sb_len_wr == 0 {
        return Err("no sense data returned".to_string());
    }
    Ok(sense)
}

/// Whether the drive is spun down, asked with ATA CHECK POWER MODE — the one
/// command that does not wake it (known-issues KI-58).
///
/// `Unknown` on anything that is not an ATA drive, and whenever the register
/// block does not come back (no root, no `CK_COND` support): the caller polls
/// in that case, so a stubborn bridge loses the optimisation, not its
/// monitoring.
pub fn power_mode(device_path: &str, bus: crate::detected_drive::BusKind) -> super::PowerMode {
    use crate::detected_drive::BusKind;
    match bus {
        // No spindle, and no such command.
        BusKind::Nvme | BusKind::Ufs => super::PowerMode::Unknown,
        BusKind::Sata | BusKind::Usb => {
            let Ok(file) = std::fs::File::open(device_path) else {
                return super::PowerMode::Unknown;
            };
            match sg_ata_non_data(file.as_raw_fd(), &super::ata16_check_power_mode_cdb()) {
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

fn query_ata(device_path: &str) -> SmartReport {
    let file = match std::fs::File::open(device_path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!(target: "diskoria", "query_ata: open failed path={device_path} err={e}");
            return SmartReport::Unavailable {
                reason: open_reason(device_path, &e),
            };
        }
    };
    let fd = file.as_raw_fd();

    let attr_payload = match sg_ata_pio_in(fd, &super::ata16_smart_cdb(0xD0)) {
        Ok(d) => d,
        Err(e) => {
            log::warn!(target: "diskoria", "query_ata: SMART READ DATA failed path={device_path}: {e}");
            return SmartReport::Unavailable {
                reason: format!("Drive did not respond to SMART data request ({e})."),
            };
        }
    };
    // Thresholds are best-effort, exactly like the Windows path.
    let thr_payload = sg_ata_pio_in(fd, &super::ata16_smart_cdb(0xD1)).ok();

    let report = super::parse_ata_smart(&attr_payload, thr_payload.as_ref().map(|t| t.as_slice()));
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

// ── NVMe via the admin-command ioctl ─────────────────────────────────────────

/// <linux/nvme_ioctl.h> `struct nvme_admin_cmd` / `nvme_passthru_cmd`.
#[repr(C)]
struct NvmeAdminCmd {
    opcode: u8,
    flags: u8,
    rsvd1: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

// _IOWR('N', 0x41, struct nvme_admin_cmd) with sizeof == 72.
const NVME_IOCTL_ADMIN_CMD: libc::c_ulong = 0xC048_4E41;

fn query_nvme(device_path: &str) -> SmartReport {
    let file = match std::fs::File::open(device_path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!(target: "diskoria", "query_nvme: open failed path={device_path} err={e}");
            return SmartReport::Unavailable {
                reason: open_reason(device_path, &e),
            };
        }
    };

    let mut log_buf = [0u8; 512];
    let numd = (log_buf.len() as u32 / 4) - 1; // 0-based dword count
    let mut cmd: NvmeAdminCmd = unsafe { std::mem::zeroed() };
    cmd.opcode = 0x02; // Get Log Page
    cmd.nsid = 0xFFFF_FFFF; // controller-wide
    cmd.addr = log_buf.as_mut_ptr() as u64;
    cmd.data_len = log_buf.len() as u32;
    cmd.cdw10 = (numd << 16) | 0x02; // NUMDL | LID 0x02 (SMART / Health)

    let rc = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD, &mut cmd) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        log::warn!(target: "diskoria", "query_nvme: admin ioctl failed path={device_path} rc={rc} err={e}");
        return SmartReport::Unavailable {
            reason: format!("NVMe health query failed ({e}). Drive may not support this query."),
        };
    }

    let report = super::parse_nvme_health_log(&log_buf);
    if let SmartReport::Nvme(ref d) = report {
        log::info!(
            target: "diskoria",
            "query_nvme: OK path={device_path} temp={}°C wear={}% spare={}% poh={}h",
            d.temperature_c, d.percentage_used, d.available_spare_pct, d.power_on_hours,
        );
    }
    report
}

// ── UFS via sysfs ────────────────────────────────────────────────────────────

fn read_hex_or_dec(path: &std::path::Path) -> Option<u8> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x") {
        u8::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn query_ufs(device_path: &str) -> SmartReport {
    // Walk up from the block device's sysfs node looking for the UFS host
    // controller's health_descriptor directory.
    let name = std::path::Path::new(device_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut node = match std::fs::canonicalize(format!("/sys/block/{name}")) {
        Ok(p) => p,
        Err(e) => {
            return SmartReport::Unavailable {
                reason: format!("Could not resolve sysfs node for {device_path}: {e}."),
            };
        }
    };
    for _ in 0..8 {
        let hd = node.join("health_descriptor");
        if hd.is_dir() {
            let pre_eol = read_hex_or_dec(&hd.join("eol_info"));
            let lt_a = read_hex_or_dec(&hd.join("life_time_estimation_a"));
            let lt_b = read_hex_or_dec(&hd.join("life_time_estimation_b"));
            if let (Some(p), Some(a), Some(b)) = (pre_eol, lt_a, lt_b) {
                log::info!(
                    target: "diskoria",
                    "query_ufs: OK path={device_path} pre_eol={p:#04x} lt_a={a:#04x} lt_b={b:#04x}",
                );
                return SmartReport::Ufs(super::UfsHealthData {
                    pre_eol_info: p,
                    life_time_est_a: a,
                    life_time_est_b: b,
                });
            }
        }
        if !node.pop() {
            break;
        }
    }
    SmartReport::Unavailable {
        reason: "This kernel does not expose the UFS health descriptor in sysfs.".to_string(),
    }
}
