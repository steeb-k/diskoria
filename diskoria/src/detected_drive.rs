//! Physical disk model for UI (WMI-backed on Windows).

use crate::partition_info::PartitionInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Hdd,
    Ssd,
    SdCard,
    Flash,
    /// eMMC (tablets / embedded)
    EMmc,
    Unknown,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Hdd => "HDD",
            MediaKind::Ssd => "SSD",
            MediaKind::SdCard => "SD card",
            MediaKind::Flash => "Flash",
            MediaKind::EMmc => "eMMC",
            MediaKind::Unknown => "Unknown",
        }
    }
}

/// Disk partition table as reported by Windows `MSFT_Disk.PartitionStyle` (Storage WMI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionTableStyle {
    Mbr,
    Gpt,
    /// No partition table signature (blank / not initialized).
    Raw,
    Unknown,
}

impl PartitionTableStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            PartitionTableStyle::Mbr => "MBR",
            PartitionTableStyle::Gpt => "GPT",
            PartitionTableStyle::Raw => "Uninitialized",
            PartitionTableStyle::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusKind {
    Nvme,
    Sata,
    Usb,
    /// Universal Flash Storage (common on mobile; rare on Windows but may appear in WMI).
    Ufs,
}

impl BusKind {
    pub fn label(self) -> &'static str {
        match self {
            BusKind::Nvme => "NVMe",
            BusKind::Sata => "SATA",
            BusKind::Usb => "USB",
            BusKind::Ufs => "UFS",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DetectedDrive {
    pub disk_number: u32,
    /// e.g. `\\.\PhysicalDrive0` — used for raw read access.
    pub device_id: String,
    /// Full model string from WMI (for future sector I/O / tooltips).
    #[allow(dead_code)]
    pub model: String,
    /// Serial number from WMI `Win32_DiskDrive.SerialNumber` (trimmed). Falls back to `"unknown"`.
    pub serial: String,
    /// PNP device ID from WMI, used to correlate with SMART `MSStorageDriver_FailurePredictStatus`.
    pub pnp_device_id: String,
    /// Raw size in bytes from Win32_DiskDrive.
    #[allow(dead_code)]
    pub size_bytes: i64,
    pub media: MediaKind,
    pub bus: BusKind,
    /// One-line label for the combo box
    pub summary: String,
    /// Mounted volumes on this physical disk (for speed test).
    pub partitions: Vec<PartitionInfo>,
    /// From `MSFT_Disk.PartitionStyle` when available.
    pub partition_style: PartitionTableStyle,
}

impl DetectedDrive {
    /// Returns a sanitized `model-serial` string suitable for use as a filename stem.
    /// Non-alphanumeric characters are replaced with dashes; runs of dashes are collapsed.
    pub fn safe_filename_stem(&self) -> String {
        let raw = format!("{}-{}", self.model, self.serial);
        let mut out = String::with_capacity(raw.len());
        let mut last_dash = false;
        for ch in raw.chars() {
            if ch.is_alphanumeric() {
                out.push(ch);
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
        out.trim_matches('-').to_string()
    }

    /// Mounted volume letters as `C:\, D:\` (sorted, unique).
    pub fn drive_letters_display(&self) -> String {
        let mut parts: Vec<String> = self
            .partitions
            .iter()
            .filter_map(|p| {
                let l = p.drive_letter.trim();
                if l.is_empty() {
                    return None;
                }
                let base = l.trim_end_matches('\\').trim_end_matches(':');
                if base.is_empty() {
                    None
                } else {
                    Some(format!("{}:\\", base))
                }
            })
            .collect();
        parts.sort();
        parts.dedup();
        if parts.is_empty() {
            "—".to_string()
        } else {
            parts.join(", ")
        }
    }

    pub fn best_speed_test_partition_index(&self) -> Option<usize> {
        crate::partition_info::best_speed_test_partition_index(&self.partitions)
    }

    pub fn format_size(bytes: i64) -> String {
        if bytes <= 0 {
            return "—".to_string();
        }
        let gb = 1024_i64.pow(3);
        let tb = 1024_i64.pow(4);
        if bytes >= tb {
            format!("{:.2} TB", bytes as f64 / tb as f64)
        } else if bytes >= gb {
            format!("{:.0} GB", bytes as f64 / gb as f64)
        } else {
            let mb = 1024_i64.pow(2);
            format!("{:.0} MB", bytes as f64 / mb as f64)
        }
    }
}
