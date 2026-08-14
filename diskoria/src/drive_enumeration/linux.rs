//! sysfs-based enumeration of physical disks on Linux.
//!
//! Deliberately libudev-free: everything comes from `/sys/block`,
//! `/proc/self/mountinfo`, `statvfs`, and (when readable) the first two
//! logical sectors of the device node for the partition-table signature.
//! Runs usefully without root — only the partition-table style and locked-LUKS
//! detection need to read `/dev` nodes and quietly degrade to `Unknown` /
//! `NotEncrypted` when they can't.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::detected_drive::{BusKind, DetectedDrive, MediaKind, PartitionTableStyle};
use crate::partition_info::{EncryptionStatus, PartitionInfo};
use crate::smart_reader::RotationRate;

// ── small sysfs helpers ──────────────────────────────────────────────────────

fn sys_read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn sys_read_u64(path: impl AsRef<Path>) -> Option<u64> {
    sys_read(path)?.parse().ok()
}

/// Whole-disk names we treat as physical drives. Partitions, loop/ram/zram,
/// device-mapper, md-raid and optical drives are not top-level entries.
fn is_physical_disk_name(name: &str) -> bool {
    let is_sd = name.starts_with("sd")
        && name.len() > 2
        && name[2..].chars().all(|c| c.is_ascii_lowercase());
    let is_vd = name.starts_with("vd")
        && name.len() > 2
        && name[2..].chars().all(|c| c.is_ascii_lowercase());
    let is_nvme = name.starts_with("nvme")
        && name.contains('n')
        && !name.contains('p')
        && name[4..].chars().all(|c| c.is_ascii_digit() || c == 'n');
    let is_mmc = name.starts_with("mmcblk")
        && name[6..].chars().all(|c| c.is_ascii_digit());
    is_sd || is_vd || is_nvme || is_mmc
}

/// Bus classification from the resolved `/sys/devices/…` path of the disk.
/// USB wins over everything (an NVMe or SATA drive in a USB enclosure *is* a
/// USB device for our purposes — matching the Windows BusType semantics).
pub(crate) fn bus_from_syspath(syspath: &str) -> BusKind {
    if syspath.contains("/usb") {
        BusKind::Usb
    } else if syspath.contains("/nvme") {
        BusKind::Nvme
    } else if syspath.contains("ufs") {
        BusKind::Ufs
    } else {
        // ata/scsi/mmc all land here; the media chip carries the SD/eMMC
        // distinction, mirroring the Windows default-to-SATA fallback.
        BusKind::Sata
    }
}

/// Media classification from rotational flag, bus, and sysfs hints.
///
/// `rotation` is the drive's own IDENTIFY DEVICE answer and is only consulted
/// for USB, where `queue/rotational` cannot be trusted: the kernel leaves it at
/// 1 unless the bridge exposes the block-limits VPD page, so an SSD in an
/// enclosure looks like a spinning disk and a spinning disk looks like one for
/// the wrong reason (KI-57).
fn classify_media(
    is_mmc: bool,
    mmc_type: Option<&str>,
    removable: bool,
    rotational: Option<bool>,
    bus: BusKind,
    model: &str,
    rotation: impl FnOnce() -> Option<RotationRate>,
) -> MediaKind {
    if is_mmc {
        // /sys/block/mmcblkN/device/type is "SD" or "MMC".
        return match mmc_type {
            Some("SD") => MediaKind::SdCard,
            Some("MMC") => MediaKind::EMmc,
            _ => MediaKind::Flash,
        };
    }
    if bus == BusKind::Nvme {
        return MediaKind::Ssd;
    }
    // The SCSI removable-media bit: thumb drives and card readers set it, an
    // external disk in an enclosure does not. This is the Linux counterpart of
    // Windows' "Removable Media" vs "External hard disk media".
    if removable {
        return MediaKind::Flash;
    }
    if bus == BusKind::Usb {
        match rotation() {
            Some(RotationRate::NonRotating) => return MediaKind::Ssd,
            Some(RotationRate::Rpm(_)) => return MediaKind::Hdd,
            None => {}
        }
    }
    if rotational == Some(true) {
        return MediaKind::Hdd;
    }
    match bus {
        // A bridge that refused IDENTIFY but did report non-rotating, or whose
        // model string admits what it is.
        BusKind::Usb if rotational == Some(false) => MediaKind::Ssd,
        BusKind::Usb => {
            let m = model.to_ascii_lowercase();
            if m.contains("ssd") || m.contains("nvme") {
                MediaKind::Ssd
            } else {
                MediaKind::Flash
            }
        }
        _ => MediaKind::Ssd,
    }
}

fn media_kind(name: &str, sys: &Path, dev_path: &Path, bus: BusKind, model: &str) -> MediaKind {
    let is_mmc = name.starts_with("mmcblk");
    // Only meaningful for mmcblk — on a SCSI disk `device/type` holds the SCSI
    // peripheral type instead, which says nothing about the medium.
    let mmc_type = if is_mmc {
        sys_read(sys.join("device/type"))
    } else {
        None
    };
    classify_media(
        is_mmc,
        mmc_type.as_deref(),
        sys_read_u64(sys.join("removable")) == Some(1),
        sys_read_u64(sys.join("queue/rotational")).map(|r| r == 1),
        bus,
        model,
        || crate::smart_reader::nominal_rotation_rate(&dev_path.to_string_lossy(), bus),
    )
}

// ── mountinfo ────────────────────────────────────────────────────────────────

/// One mounted filesystem: `(major:minor, mount point, fstype, source)`.
/// The source (`/dev/sda1`, `/dev/mapper/root`) matters because some
/// filesystems — notably btrfs — report an anonymous devno in mountinfo, so
/// devno matching alone misses them.
pub(crate) type MountEntry = (String, String, String, String);

/// Parse one `/proc/self/mountinfo` line. Octal escapes (`\040` = space) in
/// the mount-point field are decoded.
pub(crate) fn parse_mountinfo_line(line: &str) -> Option<MountEntry> {
    // 36 35 98:1 /root /mnt rw,noatime shared:1 - ext4 /dev/sda1 rw
    //  0  1  2    3     4    5         6...     ^sep fstype source opts
    let mut fields = line.split(' ');
    let _id = fields.next()?;
    let _parent = fields.next()?;
    let majmin = fields.next()?.to_string();
    let _root = fields.next()?;
    let mount_point = unescape_mount_path(fields.next()?);
    // Skip to the "-" separator, then fstype and source.
    let mut rest = fields;
    for f in rest.by_ref() {
        if f == "-" {
            break;
        }
    }
    let fstype = rest.next()?.to_string();
    let source = unescape_mount_path(rest.next()?);
    Some((majmin, mount_point, fstype, source))
}

/// Decode mountinfo's octal escapes: `\040` space, `\011` tab, `\012` newline,
/// `\134` backslash.
pub(crate) fn unescape_mount_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() + 1 && i + 3 <= bytes.len() {
            if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 4])
                .ok()
                .and_then(|o| u8::from_str_radix(o, 8).ok())
            {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Mounted-filesystem lookup, keyed two ways: by `major:minor` and by the
/// canonicalized source device's basename (`sda1`, `dm-0`). btrfs mounts are
/// only findable by source — their mountinfo devno is anonymous.
struct MountTable {
    by_devno: HashMap<String, (String, String)>,
    by_source_dev: HashMap<String, (String, String)>,
}

impl MountTable {
    fn load() -> Self {
        let mut by_devno = HashMap::new();
        let mut by_source_dev = HashMap::new();
        if let Ok(s) = std::fs::read_to_string("/proc/self/mountinfo") {
            for line in s.lines() {
                if let Some((majmin, mp, fs, source)) = parse_mountinfo_line(line) {
                    // First mount of a device wins (bind mounts repeat it).
                    by_devno
                        .entry(majmin)
                        .or_insert((mp.clone(), fs.clone()));
                    if source.starts_with("/dev/") {
                        let dev = std::fs::canonicalize(&source)
                            .unwrap_or_else(|_| PathBuf::from(&source));
                        if let Some(base) = dev.file_name() {
                            by_source_dev
                                .entry(base.to_string_lossy().into_owned())
                                .or_insert((mp, fs));
                        }
                    }
                }
            }
        }
        Self { by_devno, by_source_dev }
    }

    /// Look up by devno first, then by device basename.
    fn get(&self, devno: &str, dev_name: &str) -> Option<&(String, String)> {
        self.by_devno
            .get(devno)
            .or_else(|| self.by_source_dev.get(dev_name))
    }
}

// ── LUKS / device-mapper ─────────────────────────────────────────────────────

/// Whether a device-mapper UUID names an (open) LUKS/crypt mapping.
pub(crate) fn dm_uuid_is_crypt(uuid: &str) -> bool {
    uuid.starts_with("CRYPT-LUKS") || uuid.starts_with("CRYPT-PLAIN") || uuid.starts_with("CRYPT-")
}

/// If `part_sys` (e.g. `/sys/block/sda/sda3`) has an open dm-crypt holder,
/// return that holder's `(major:minor, device name)` — e.g. `("254:0",
/// "dm-0")` — so mount/fs data can be read from the mapping instead of the
/// raw partition.
fn crypt_holder(part_sys: &Path) -> Option<(String, String)> {
    let holders = std::fs::read_dir(part_sys.join("holders")).ok()?;
    for h in holders.flatten() {
        let hpath = h.path();
        if let Some(uuid) = sys_read(hpath.join("dm/uuid")) {
            if dm_uuid_is_crypt(&uuid) {
                let devno = sys_read(hpath.join("dev"))?;
                return Some((devno, h.file_name().to_string_lossy().into_owned()));
            }
        }
    }
    None
}

/// LUKS superblock magic ("LUKS\xba\xbe") at offset 0 of the partition.
/// Needs read access to the device node; unreadable → `None`.
fn reads_as_luks(dev_path: &Path) -> Option<bool> {
    let mut f = std::fs::File::open(dev_path).ok()?;
    let mut magic = [0u8; 6];
    f.read_exact(&mut magic).ok()?;
    Some(&magic == b"LUKS\xba\xbe")
}

// ── partition table style ────────────────────────────────────────────────────

/// Classify LBA0/LBA1 contents: GPT ("EFI PART" at LBA1), MBR (0x55AA at 510),
/// neither → Raw. Pure so the signature logic is unit-testable.
pub(crate) fn classify_partition_table(lba0: &[u8], lba1: &[u8]) -> PartitionTableStyle {
    if lba1.len() >= 8 && &lba1[0..8] == b"EFI PART" {
        return PartitionTableStyle::Gpt;
    }
    if lba0.len() >= 512 && lba0[510] == 0x55 && lba0[511] == 0xAA {
        return PartitionTableStyle::Mbr;
    }
    PartitionTableStyle::Raw
}

fn partition_table_style(dev_path: &Path, logical_block: u64) -> PartitionTableStyle {
    let Ok(mut f) = std::fs::File::open(dev_path) else {
        return PartitionTableStyle::Unknown;
    };
    let n = (logical_block.max(512) as usize) * 2;
    let mut buf = vec![0u8; n];
    if f.read_exact(&mut buf).is_err() {
        return PartitionTableStyle::Unknown;
    }
    let lb = logical_block.max(512) as usize;
    classify_partition_table(&buf[..lb], &buf[lb..])
}

// ── statvfs ──────────────────────────────────────────────────────────────────

fn statvfs_sizes(mount_point: &str) -> Option<(i64, i64)> {
    let c = std::ffi::CString::new(mount_point).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = if st.f_frsize > 0 { st.f_frsize } else { st.f_bsize } as i64;
    Some((st.f_blocks as i64 * frsize, st.f_bavail as i64 * frsize))
}

// ── filesystem labels ────────────────────────────────────────────────────────

/// Device name (`sda1`) → filesystem label, from /dev/disk/by-label symlinks.
fn labels_by_device() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/dev/disk/by-label") {
        for e in dir.flatten() {
            let label = unescape_mount_path(&e.file_name().to_string_lossy());
            if let Ok(target) = std::fs::read_link(e.path()) {
                if let Some(dev) = target.file_name() {
                    map.insert(dev.to_string_lossy().into_owned(), label);
                }
            }
        }
    }
    map
}

// ── enumeration ──────────────────────────────────────────────────────────────

fn model_for(sys: &Path, name: &str) -> String {
    sys_read(sys.join("device/model"))
        .or_else(|| sys_read(sys.join("device/name"))) // mmc
        .unwrap_or_else(|| name.to_string())
}

fn serial_for(sys: &Path) -> String {
    sys_read(sys.join("device/serial"))
        .or_else(|| sys_read(sys.join("device/wwid")))
        .unwrap_or_else(|| "unknown".to_string())
}

fn partitions_for(
    disk_name: &str,
    sys: &Path,
    mounts: &MountTable,
    labels: &HashMap<String, String>,
) -> Vec<PartitionInfo> {
    let mut parts: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(sys) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(disk_name) && e.path().join("partition").exists() {
                parts.push((name, e.path()));
            }
        }
    }
    // sysfs order is arbitrary; sort by partition number.
    parts.sort_by_key(|(_, p)| sys_read_u64(p.join("partition")).unwrap_or(0));

    parts
        .into_iter()
        .map(|(pname, psys)| {
            let part_bytes = sys_read_u64(psys.join("size")).unwrap_or(0) as i64 * 512;
            let devno = sys_read(psys.join("dev")).unwrap_or_default();
            let dev_path = PathBuf::from("/dev").join(&pname);

            // Where does the *filesystem* live? Directly on the partition, or
            // on an open dm-crypt mapping stacked on it.
            let holder = crypt_holder(&psys);
            let (fs_devno, fs_devname) = holder
                .clone()
                .unwrap_or_else(|| (devno.clone(), pname.clone()));
            let mounted = mounts.get(&fs_devno, &fs_devname);

            let encryption = if holder.is_some() {
                EncryptionStatus::Unlocked
            } else {
                match reads_as_luks(&dev_path) {
                    Some(true) => EncryptionStatus::Locked,
                    _ => EncryptionStatus::NotEncrypted,
                }
            };

            let (mount_point, file_system) = mounted
                .map(|(mp, fs)| (mp.clone(), fs.clone()))
                .unwrap_or_default();
            let (total_size, free_space) = if mount_point.is_empty() {
                (part_bytes, 0)
            } else {
                statvfs_sizes(&mount_point).unwrap_or((part_bytes, 0))
            };
            let is_system_partition =
                mount_point == "/" || mount_point.starts_with("/boot") || mount_point == "/usr";

            PartitionInfo {
                mount_point,
                volume_name: labels.get(&pname).cloned().unwrap_or_default(),
                total_size,
                free_space,
                file_system,
                is_system_partition,
                encryption,
            }
        })
        .collect()
}

pub fn enumerate_physical_disks() -> Result<Vec<DetectedDrive>, String> {
    let entries = std::fs::read_dir("/sys/block")
        .map_err(|e| format!("reading /sys/block: {e}"))?;

    let mounts = MountTable::load();
    let labels = labels_by_device();

    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_physical_disk_name(n))
        .collect();
    names.sort();

    let mut out = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let sys = PathBuf::from("/sys/block").join(name);
        let syspath = std::fs::canonicalize(&sys)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let bus = bus_from_syspath(&syspath);
        let model = model_for(&sys, name);
        let serial = serial_for(&sys);
        let size_bytes = sys_read_u64(sys.join("size")).unwrap_or(0) as i64 * 512;
        let dev_path = PathBuf::from("/dev").join(name);
        let media = media_kind(name, &sys, &dev_path, bus, &model);
        let logical_block = sys_read_u64(sys.join("queue/logical_block_size")).unwrap_or(512);
        let partition_style = partition_table_style(&dev_path, logical_block);
        let partitions = partitions_for(name, &sys, &mounts, &labels);

        let disk_number = index as u32;
        out.push(DetectedDrive {
            disk_number,
            device_id: dev_path.to_string_lossy().into_owned(),
            summary: format!(
                "Drive {} — {} — {}",
                disk_number,
                model.trim(),
                DetectedDrive::format_size(size_bytes)
            ),
            model,
            serial,
            // The resolved sysfs device path plays the PNPDeviceID role: a
            // stable hardware identity string (informational on Linux).
            pnp_device_id: syspath,
            size_bytes,
            media,
            bus,
            partitions,
            partition_style,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No IDENTIFY answer — the bridge refused, or there is no root.
    fn blind() -> Option<RotationRate> {
        None
    }

    /// For the cases the ladder must settle without touching the drive.
    fn never_probed() -> Option<RotationRate> {
        panic!("this case must not cost a device command");
    }

    /// The bug this ladder was rebuilt for: a 4 TB USB hard disk. The kernel
    /// leaves `rotational` at 1 for USB whatever the medium, so the label was
    /// right for the wrong reason; the drive's own answer is what settles it.
    #[test]
    fn usb_hard_disk_is_a_disk() {
        assert_eq!(
            classify_media(false, None, false, Some(true), BusKind::Usb, "Expansion", || {
                Some(RotationRate::Rpm(5526))
            }),
            MediaKind::Hdd
        );
    }

    /// The mirror case, and the one `queue/rotational` gets wrong: an SSD in an
    /// enclosure that the kernel also flags as rotating.
    #[test]
    fn usb_ssd_enclosure_is_not_a_hard_disk() {
        assert_eq!(
            classify_media(false, None, false, Some(true), BusKind::Usb, "ASMT 2115", || {
                Some(RotationRate::NonRotating)
            }),
            MediaKind::Ssd
        );
    }

    /// Flash is for a genuinely removable medium — the SCSI removable-media
    /// bit — not for everything that happens to be plugged into USB.
    #[test]
    fn removable_bit_means_flash() {
        assert_eq!(
            classify_media(false, None, true, Some(true), BusKind::Usb, "Corvid Pocket", never_probed),
            MediaKind::Flash
        );
    }

    #[test]
    fn internal_disks_trust_the_rotational_flag_without_a_probe() {
        // Only USB needs the drive's own answer.
        assert_eq!(
            classify_media(false, None, false, Some(true), BusKind::Sata, "ST2000DM008", never_probed),
            MediaKind::Hdd
        );
        assert_eq!(
            classify_media(false, None, false, Some(false), BusKind::Sata, "CT1000MX500", never_probed),
            MediaKind::Ssd
        );
        assert_eq!(
            classify_media(false, None, false, Some(true), BusKind::Nvme, "SN810", never_probed),
            MediaKind::Ssd
        );
    }

    #[test]
    fn usb_without_an_answer_keeps_the_old_hints() {
        // The bridge did report non-rotating even though it refused IDENTIFY.
        assert_eq!(
            classify_media(false, None, false, Some(false), BusKind::Usb, "Generic", blind),
            MediaKind::Ssd
        );
        // Nothing to go on but the model string.
        assert_eq!(
            classify_media(false, None, false, None, BusKind::Usb, "Corvid Portable SSD", blind),
            MediaKind::Ssd
        );
        assert_eq!(
            classify_media(false, None, false, None, BusKind::Usb, "Generic", blind),
            MediaKind::Flash
        );
    }

    #[test]
    fn mmc_type_picks_card_or_embedded() {
        assert_eq!(
            classify_media(true, Some("SD"), true, Some(false), BusKind::Sata, "SD32G", blind),
            MediaKind::SdCard
        );
        assert_eq!(
            classify_media(true, Some("MMC"), false, Some(false), BusKind::Sata, "BJTD4R", blind),
            MediaKind::EMmc
        );
        assert_eq!(
            classify_media(true, None, false, Some(false), BusKind::Sata, "?", blind),
            MediaKind::Flash
        );
    }

    #[test]
    fn physical_disk_name_filter() {
        for good in ["sda", "sdb", "sdaa", "nvme0n1", "nvme10n2", "mmcblk0", "vda"] {
            assert!(is_physical_disk_name(good), "{good} should be a disk");
        }
        for bad in [
            "sda1", "nvme0n1p2", "mmcblk0p1", "loop0", "zram0", "dm-0", "md127", "sr0", "ram3",
        ] {
            assert!(!is_physical_disk_name(bad), "{bad} should not be a disk");
        }
    }

    #[test]
    fn mountinfo_parses_and_unescapes() {
        let line = r"36 35 98:1 / /mnt/usb\040stick rw,noatime shared:1 - ext4 /dev/sda1 rw";
        let (devno, mp, fs, source) = parse_mountinfo_line(line).expect("parse");
        assert_eq!(devno, "98:1");
        assert_eq!(mp, "/mnt/usb stick");
        assert_eq!(fs, "ext4");
        assert_eq!(source, "/dev/sda1");
    }

    #[test]
    fn mountinfo_optional_fields_are_skipped() {
        // Two optional shared/master fields before the separator.
        let line = "41 21 253:0 / / rw shared:1 master:2 - btrfs /dev/mapper/root rw";
        let (devno, mp, fs, source) = parse_mountinfo_line(line).expect("parse");
        assert_eq!(devno, "253:0");
        assert_eq!(mp, "/");
        assert_eq!(fs, "btrfs");
        assert_eq!(source, "/dev/mapper/root");
    }

    #[test]
    fn bus_classification() {
        assert_eq!(
            bus_from_syspath("/sys/devices/pci0000:00/0000:00:14.0/usb2/2-1/host6/target6:0:0/6:0:0:0/block/sda"),
            BusKind::Usb
        );
        assert_eq!(
            bus_from_syspath("/sys/devices/pci0000:00/0000:00:03.1/0000:26:00.0/nvme/nvme0/nvme0n1"),
            BusKind::Nvme
        );
        assert_eq!(
            bus_from_syspath("/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"),
            BusKind::Sata
        );
        assert_eq!(
            bus_from_syspath("/sys/devices/platform/soc/1d84000.ufshc/host0/target0:0:0/0:0:0:49/block/sdb"),
            BusKind::Ufs
        );
    }

    #[test]
    fn dm_crypt_uuid_detection() {
        assert!(dm_uuid_is_crypt("CRYPT-LUKS2-abc-root"));
        assert!(!dm_uuid_is_crypt("LVM-abcdef"));
    }

    #[test]
    fn partition_table_signatures() {
        let mut mbr = vec![0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        let mut gpt_lba1 = vec![0u8; 512];
        gpt_lba1[..8].copy_from_slice(b"EFI PART");
        // GPT drives keep a protective MBR in LBA0 — GPT must win.
        assert_eq!(classify_partition_table(&mbr, &gpt_lba1), PartitionTableStyle::Gpt);
        assert_eq!(classify_partition_table(&mbr, &[0u8; 512]), PartitionTableStyle::Mbr);
        assert_eq!(
            classify_partition_table(&[0u8; 512], &[0u8; 512]),
            PartitionTableStyle::Raw
        );
    }
}

#[cfg(test)]
mod hardware_tests {
    /// Diagnostic, not CI: prints what this host's enumeration sees so it can
    /// be compared against `lsblk -o NAME,MODEL,SERIAL,SIZE,ROTA,TRAN`.
    #[test]
    #[ignore = "inspects real hardware; run manually with --ignored --nocapture"]
    fn print_real_enumeration() {
        let drives = super::enumerate_physical_disks().expect("enumerate");
        for d in &drives {
            println!(
                "#{} {} model={:?} serial={:?} size={} media={:?} bus={:?} style={:?}",
                d.disk_number,
                d.device_id,
                d.model,
                d.serial,
                crate::detected_drive::DetectedDrive::format_size(d.size_bytes),
                d.media,
                d.bus,
                d.partition_style,
            );
            for p in &d.partitions {
                println!(
                    "   mount={:?} fs={:?} label={:?} total={} free={} sys={} enc={:?}",
                    p.mount_point,
                    p.file_system,
                    p.volume_name,
                    p.total_size,
                    p.free_space,
                    p.is_system_partition,
                    p.encryption,
                );
            }
        }
        assert!(!drives.is_empty());
    }
}
