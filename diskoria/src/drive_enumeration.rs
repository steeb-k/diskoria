//! WMI enumeration of physical disks (model, size, partitions, BitLocker hints).

#[cfg(windows)]
mod windows {
    use std::collections::HashMap;

    use serde::Deserialize;
    use wmi::WMIConnection;

    use crate::detected_drive::{BusKind, DetectedDrive, MediaKind, PartitionTableStyle};
    use crate::partition_info::{BitLockerStatus, PartitionInfo};

    #[derive(Debug, Deserialize)]
    #[serde(rename = "Win32_DiskDrive")]
    #[allow(non_snake_case)]
    struct Win32DiskDriveRow {
        Index: Option<u32>,
        Model: Option<String>,
        SerialNumber: Option<String>,
        Size: Option<wmi::Variant>,
        MediaType: Option<String>,
        InterfaceType: Option<String>,
        PNPDeviceID: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "MSFT_PhysicalDisk")]
    #[allow(non_snake_case)]
    struct MsftPhysicalDiskRow {
        DeviceId: Option<String>,
        MediaType: Option<u32>,
        BusType: Option<u32>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "Win32_DiskPartition")]
    #[allow(non_snake_case)]
    struct Win32DiskPartitionRow {
        DeviceID: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "Win32_LogicalDisk")]
    #[allow(non_snake_case)]
    struct Win32LogicalDiskRow {
        DeviceID: Option<String>,
        Size: Option<wmi::Variant>,
        FreeSpace: Option<wmi::Variant>,
        FileSystem: Option<String>,
        VolumeName: Option<String>,
        /// WMI drive-type code (removable/fixed/network). Deserialized for
        /// completeness; not surfaced in the UI yet.
        #[allow(dead_code)]
        DriveType: Option<u32>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "Win32_EncryptableVolume")]
    #[allow(non_snake_case)]
    struct Win32EncryptableVolumeRow {
        DriveLetter: Option<String>,
        ProtectionStatus: Option<u32>,
        ConversionStatus: Option<u32>,
    }

    fn variant_to_i64(v: &Option<wmi::Variant>) -> i64 {
        let Some(v) = v else {
            return 0;
        };
        match v {
            wmi::Variant::UI8(n) => *n as i64,
            wmi::Variant::I8(n) => *n,
            wmi::Variant::UI4(n) => *n as i64,
            wmi::Variant::I4(n) => *n as i64,
            wmi::Variant::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn variant_to_u64(v: &Option<wmi::Variant>) -> u64 {
        let Some(v) = v else {
            return 0;
        };
        match v {
            wmi::Variant::UI8(n) => *n,
            wmi::Variant::I8(n) => *n as u64,
            wmi::Variant::UI4(n) => *n as u64,
            wmi::Variant::I4(n) => *n as u64,
            wmi::Variant::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn map_bus(interface_type: &str, model: &str, msft_bus: Option<u32>) -> BusKind {
        // MSFT_PhysicalDisk.BusType is the most reliable source — check it first.
        // Values: 7 = USB, 17 = NVMe, 19 = UFS (Win10 1903+).
        match msft_bus {
            Some(17) => return BusKind::Nvme,
            Some(7) => return BusKind::Usb,
            Some(19) => return BusKind::Ufs,
            _ => {}
        }

        let i = interface_type.to_lowercase();
        let m = model.to_lowercase();
        if i.contains("nvme") || m.contains("nvme") {
            return BusKind::Nvme;
        }
        if i.contains("usb") {
            return BusKind::Usb;
        }
        if i.contains("ufs")
            || m.contains("ufs")
            || m.contains("universal flash storage")
        {
            return BusKind::Ufs;
        }
        BusKind::Sata
    }

    fn map_media(
        disk_index: u32,
        wmi_media: &str,
        interface_type: &str,
        model: &str,
        msft_media: Option<u32>,
    ) -> MediaKind {
        if let Some(mt) = msft_media {
            match mt {
                4 | 5 => return MediaKind::Ssd,
                3 => return MediaKind::Hdd,
                _ => {}
            }
        }

        let ml = model.to_lowercase();
        let wl = wmi_media.to_lowercase();
        let il = interface_type.to_lowercase();

        if il.contains("nvme") || ml.contains("nvme") {
            return MediaKind::Ssd;
        }
        if il.contains("ufs") || ml.contains("ufs") || ml.contains("universal flash storage") {
            return MediaKind::Ssd;
        }
        if ml.contains("emmc") || wl.contains("emmc") {
            return MediaKind::EMmc;
        }
        if ml.contains("sd/mmc")
            || ml.contains("sd card")
            || (ml.contains("multimedia") && ml.contains("sd"))
        {
            return MediaKind::SdCard;
        }
        if wl.contains("removable") || wl.contains("external") {
            return MediaKind::Flash;
        }
        if wl.contains("ssd") || wl.contains("solid") || ml.contains("ssd") {
            return MediaKind::Ssd;
        }
        if wl.contains("hard") || wl.contains("fixed") || wl.contains("hdd") {
            return MediaKind::Hdd;
        }
        if wl.contains("fixed") {
            return MediaKind::Hdd;
        }

        let _ = disk_index;
        MediaKind::Unknown
    }

    /// Returns `(MediaType, BusType)` keyed by disk number from `MSFT_PhysicalDisk`.
    fn msft_media_map() -> Result<HashMap<u32, (u32, u32)>, wmi::WMIError> {
        let wmi = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage")?;
        let rows: Vec<MsftPhysicalDiskRow> =
            wmi.raw_query("SELECT DeviceId, MediaType, BusType FROM MSFT_PhysicalDisk")?;
        let mut map = HashMap::new();
        for row in rows {
            let Some(id_s) = row.DeviceId else { continue };
            let Ok(id) = id_s.trim().parse::<u32>() else {
                continue;
            };
            map.insert(id, (row.MediaType.unwrap_or(0), row.BusType.unwrap_or(0)));
        }
        Ok(map)
    }

    /// `MSFT_Disk.PartitionStyle`: 1 = MBR, 2 = GPT, 3 = RAW (uninitialized).
    fn msft_partition_style_map() -> HashMap<u32, PartitionTableStyle> {
        let Ok(wmi) = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage") else {
            return HashMap::new();
        };
        #[derive(Debug, Deserialize)]
        #[serde(rename = "MSFT_Disk")]
        #[allow(non_snake_case)]
        struct MsftDiskRow {
            Number: Option<u32>,
            PartitionStyle: Option<u32>,
        }
        let rows: Vec<MsftDiskRow> = match wmi.raw_query("SELECT Number, PartitionStyle FROM MSFT_Disk")
        {
            Ok(r) => r,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        for row in rows {
            let Some(n) = row.Number else {
                continue;
            };
            let style = match row.PartitionStyle.unwrap_or(0) {
                1 => PartitionTableStyle::Mbr,
                2 => PartitionTableStyle::Gpt,
                3 => PartitionTableStyle::Raw,
                _ => PartitionTableStyle::Unknown,
            };
            map.insert(n, style);
        }
        map
    }

    fn normalize_drive_letter(s: &str) -> String {
        let s = s.trim();
        if s.is_empty() {
            return String::new();
        }
        let u = s.to_ascii_uppercase();
        if u.ends_with(':') {
            u
        } else {
            format!("{}:", u)
        }
    }

    fn bitlocker_status_map() -> HashMap<String, BitLockerStatus> {
        let Ok(wmi) = WMIConnection::with_namespace_path(
            r"ROOT\cimv2\Security\MicrosoftVolumeEncryption",
        ) else {
            return HashMap::new();
        };
        let rows: Vec<Win32EncryptableVolumeRow> = match wmi.raw_query(
            "SELECT DriveLetter, ProtectionStatus, ConversionStatus FROM Win32_EncryptableVolume",
        ) {
            Ok(r) => r,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        for row in rows {
            let Some(letter) = row.DriveLetter else {
                continue;
            };
            let key = normalize_drive_letter(&letter);
            // Win32_EncryptableVolume.ProtectionStatus:
            //   0 = PROTECTION_OFF, 1 = PROTECTION_ON, 2 = PROTECTION_UNKNOWN.
            // A *locked* volume reports PROTECTION_UNKNOWN (2) because its key is
            // unavailable — NOT PROTECTION_ON (1), which is the normal state of an
            // encrypted volume that has already been unlocked and is accessible.
            let protection = row.ProtectionStatus.unwrap_or(0);
            let conversion = row.ConversionStatus.unwrap_or(0);
            let status = if conversion == 0 {
                BitLockerStatus::NotEncrypted
            } else if protection == 2 {
                BitLockerStatus::Locked
            } else if protection == 1 {
                BitLockerStatus::Unlocked
            } else {
                // Encrypted but protection suspended (key in the clear); still accessible.
                BitLockerStatus::Encrypted
            };
            map.insert(key, status);
        }
        map
    }

    fn is_system_drive(letter: &str) -> bool {
        let Ok(sd) = std::env::var("SystemDrive") else {
            return false;
        };
        normalize_drive_letter(letter) == normalize_drive_letter(sd.trim())
    }

    fn volume_ready(drive_letter: &str) -> bool {
        let root = format!("{}\\", drive_letter.trim_end_matches('\\'));
        std::fs::metadata(&root).is_ok()
    }

    /// Logical disks associated with a partition (same WQL chain as C# DriveDetectionService).
    fn logical_disks_for_partition(
        wmi: &WMIConnection,
        partition_device_id: &str,
    ) -> Result<Vec<Win32LogicalDiskRow>, wmi::WMIError> {
        let escaped = partition_device_id.replace('\'', "''");
        let q = format!(
            "ASSOCIATORS OF {{Win32_DiskPartition.DeviceID='{}'}} WHERE AssocClass = Win32_LogicalDiskToPartition",
            escaped
        );
        wmi.raw_query(&q)
    }

    /// Use `IOCTL_STORAGE_GET_DEVICE_NUMBER` to find which physical disk a volume belongs to.
    /// Returns `Some(disk_number)` on success, `None` if the IOCTL fails (e.g. network drive).
    fn volume_disk_number(drive_letter: &str) -> Option<u32> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::IO::DeviceIoControl;

        const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;

        #[repr(C)]
        struct StorageDeviceNumber {
            device_type: u32,
            device_number: u32,
            partition_number: u32,
        }

        let clean = drive_letter.trim_end_matches('\\').trim_end_matches(':');
        let path: Vec<u16> = format!("\\\\.\\{}:", clean)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            );
            if handle == -1_isize {
                return None;
            }

            let mut sdn = std::mem::zeroed::<StorageDeviceNumber>();
            let mut returned: u32 = 0;
            let ok = DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                std::ptr::null(),
                0,
                &mut sdn as *mut _ as *mut _,
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(handle);
            if ok != 0 {
                Some(sdn.device_number)
            } else {
                None
            }
        }
    }

    /// Fallback partition discovery: scan all drive letters and match by physical disk number.
    /// Used when the WMI association chain (`Win32_DiskPartition` -> `Win32_LogicalDiskToPartition`)
    /// is missing, which commonly happens in Windows PE with mounted VHDx volumes.
    fn fallback_partitions_for_disk(
        disk_index: u32,
        bitlocker: &HashMap<String, BitLockerStatus>,
        already_found: &[String],
    ) -> Vec<PartitionInfo> {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let mut out = Vec::new();
        for letter_byte in b'A'..=b'Z' {
            let letter = format!("{}:", letter_byte as char);
            if already_found.iter().any(|f| f.eq_ignore_ascii_case(&letter)) {
                continue;
            }
            if !volume_ready(&letter) {
                continue;
            }
            let Some(dn) = volume_disk_number(&letter) else {
                continue;
            };
            if dn != disk_index {
                continue;
            }

            let root: Vec<u16> = format!("{}\\", letter)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let (mut free_bytes, mut total_bytes) = (0u64, 0u64);
            unsafe {
                GetDiskFreeSpaceExW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    &mut total_bytes as *mut u64 as *mut _,
                    &mut free_bytes as *mut u64 as *mut _,
                );
            }

            let bl = bitlocker
                .get(&letter)
                .copied()
                .unwrap_or(BitLockerStatus::NotEncrypted);

            log::info!(
                target: "diskoria",
                "fallback_partitions: matched {}:\\ to PhysicalDrive{} (total={}, free={})",
                letter_byte as char, disk_index, total_bytes, free_bytes
            );

            out.push(PartitionInfo {
                drive_letter: letter.clone(),
                volume_name: "Local Disk".to_string(),
                total_size: total_bytes as i64,
                free_space: free_bytes as i64,
                file_system: String::new(),
                is_system_partition: is_system_drive(&letter),
                bitlocker: bl,
            });
        }
        out
    }

    fn partitions_for_disk(
        wmi: &WMIConnection,
        disk_index: u32,
        bitlocker: &HashMap<String, BitLockerStatus>,
    ) -> Result<Vec<PartitionInfo>, wmi::WMIError> {
        let q = format!(
            "SELECT DeviceID FROM Win32_DiskPartition WHERE DiskIndex = {}",
            disk_index
        );
        let part_rows: Vec<Win32DiskPartitionRow> = wmi.raw_query(&q)?;
        let mut out = Vec::new();

        for pr in part_rows {
            let Some(part_id) = pr.DeviceID else {
                continue;
            };
            let logicals = match logical_disks_for_partition(wmi, &part_id) {
                Ok(v) => v,
                Err(_) => continue,
            };

            for log in logicals {
                let Some(device_id) = log.DeviceID else {
                    continue;
                };
                let drive_letter = normalize_drive_letter(&device_id);
                if drive_letter.is_empty() {
                    continue;
                }
                if !volume_ready(&drive_letter) {
                    continue;
                }

                let total = variant_to_u64(&log.Size) as i64;
                let free = variant_to_u64(&log.FreeSpace) as i64;
                let fs = log.FileSystem.unwrap_or_default();
                let vol = log
                    .VolumeName
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Local Disk")
                    .to_string();

                let bl = bitlocker
                    .get(&drive_letter)
                    .copied()
                    .unwrap_or(BitLockerStatus::NotEncrypted);

                out.push(PartitionInfo {
                    drive_letter,
                    volume_name: vol,
                    total_size: total,
                    free_space: free,
                    file_system: fs,
                    is_system_partition: is_system_drive(&device_id),
                    bitlocker: bl,
                });
            }
        }

        if out.is_empty() {
            let already: Vec<String> = out.iter().map(|p| p.drive_letter.clone()).collect();
            let fallback = fallback_partitions_for_disk(disk_index, bitlocker, &already);
            out.extend(fallback);
        }

        out.sort_by(|a, b| a.drive_letter.cmp(&b.drive_letter));
        Ok(out)
    }

    pub fn enumerate_physical_disks() -> Result<Vec<DetectedDrive>, String> {
        let msft = msft_media_map().unwrap_or_default();
        let partition_styles = msft_partition_style_map();
        let bitlocker = bitlocker_status_map();

        let wmi = WMIConnection::new().map_err(|e| e.to_string())?;
        let rows: Vec<Win32DiskDriveRow> = wmi
            .raw_query("SELECT Index, Model, SerialNumber, Size, MediaType, InterfaceType, PNPDeviceID FROM Win32_DiskDrive")
            .map_err(|e| e.to_string())?;

        let mut out: Vec<DetectedDrive> = Vec::with_capacity(rows.len());

        for row in rows {
            let Some(index) = row.Index else {
                continue;
            };
            let model = row
                .Model
                .as_deref()
                .unwrap_or("Unknown disk")
                .trim()
                .to_string();
            if model.is_empty() {
                continue;
            }

            let serial = row
                .SerialNumber
                .as_deref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());

            let pnp_device_id = row
                .PNPDeviceID
                .as_deref()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            let size_bytes = variant_to_i64(&row.Size);
            let wmi_media = row.MediaType.as_deref().unwrap_or("");
            let interface = row.InterfaceType.as_deref().unwrap_or("");

            let (msft_media_type, msft_bus_type) = msft.get(&index).copied().unwrap_or((0, 0));
            let bus = map_bus(interface, &model, Some(msft_bus_type).filter(|&v| v != 0));
            let media = map_media(index, wmi_media, interface, &model, Some(msft_media_type).filter(|&v| v != 0));

            let summary = format!(
                "Drive {} — {} — {}",
                index,
                model,
                DetectedDrive::format_size(size_bytes)
            );

            let device_id = format!(r"\\.\PhysicalDrive{}", index);

            let partitions = partitions_for_disk(&wmi, index, &bitlocker).unwrap_or_default();

            let partition_style = partition_styles
                .get(&index)
                .copied()
                .unwrap_or(PartitionTableStyle::Unknown);

            out.push(DetectedDrive {
                disk_number: index,
                device_id,
                model,
                serial,
                pnp_device_id,
                size_bytes,
                media,
                bus,
                summary,
                partitions,
                partition_style,
            });
        }

        out.sort_by_key(|d| d.disk_number);
        Ok(out)
    }
}

#[cfg(windows)]
pub use windows::enumerate_physical_disks;

#[cfg(not(windows))]
pub fn enumerate_physical_disks() -> Result<Vec<crate::detected_drive::DetectedDrive>, String> {
    Err("Drive enumeration is only available on Windows.".to_string())
}
