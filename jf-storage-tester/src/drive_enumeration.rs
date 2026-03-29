//! WMI enumeration of physical disks (parity with `_original/Services/DriveDetectionService.cs` core fields).

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
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename = "MSFT_PhysicalDisk")]
    #[allow(non_snake_case)]
    struct MsftPhysicalDiskRow {
        DeviceId: Option<String>,
        MediaType: Option<u32>,
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
            wmi::Variant::I8(n) => *n as i64,
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

    fn map_bus(interface_type: &str, model: &str) -> BusKind {
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

    fn msft_media_map() -> Result<HashMap<u32, u32>, wmi::WMIError> {
        let wmi = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage")?;
        let rows: Vec<MsftPhysicalDiskRow> =
            wmi.raw_query("SELECT DeviceId, MediaType FROM MSFT_PhysicalDisk")?;
        let mut map = HashMap::new();
        for row in rows {
            let Some(id_s) = row.DeviceId else { continue };
            let Ok(id) = id_s.trim().parse::<u32>() else {
                continue;
            };
            if let Some(mt) = row.MediaType {
                map.insert(id, mt);
            }
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
            let protection = row.ProtectionStatus.unwrap_or(0);
            let conversion = row.ConversionStatus.unwrap_or(0);
            let status = if conversion == 0 {
                BitLockerStatus::NotEncrypted
            } else if protection == 1 {
                BitLockerStatus::Locked
            } else if conversion == 1 && protection == 0 {
                BitLockerStatus::Unlocked
            } else {
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

        out.sort_by(|a, b| a.drive_letter.cmp(&b.drive_letter));
        Ok(out)
    }

    pub fn enumerate_physical_disks() -> Result<Vec<DetectedDrive>, String> {
        let msft = msft_media_map().unwrap_or_default();
        let partition_styles = msft_partition_style_map();
        let bitlocker = bitlocker_status_map();

        let wmi = WMIConnection::new().map_err(|e| e.to_string())?;
        let rows: Vec<Win32DiskDriveRow> = wmi
            .raw_query("SELECT Index, Model, SerialNumber, Size, MediaType, InterfaceType FROM Win32_DiskDrive")
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

            let size_bytes = variant_to_i64(&row.Size);
            let wmi_media = row.MediaType.as_deref().unwrap_or("");
            let interface = row.InterfaceType.as_deref().unwrap_or("");

            let bus = map_bus(interface, &model);
            let media = map_media(index, wmi_media, interface, &model, msft.get(&index).copied());

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
