//! Mounted volume / partition info for speed tests (WMI-backed on Windows).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitLockerStatus {
    NotEncrypted,
    Locked,
    Unlocked,
    Encrypted,
}

#[derive(Clone, Debug)]
pub struct PartitionInfo {
    /// e.g. `C:`
    pub drive_letter: String,
    pub volume_name: String,
    pub total_size: i64,
    pub free_space: i64,
    pub file_system: String,
    pub is_system_partition: bool,
    pub bitlocker: BitLockerStatus,
}

impl PartitionInfo {
    pub fn is_bitlocker_locked(&self) -> bool {
        self.bitlocker == BitLockerStatus::Locked
    }

    pub fn free_space_display(&self) -> String {
        format_bytes(self.free_space)
    }
}

fn format_bytes(bytes: i64) -> String {
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

/// Largest non-system, non-locked partition by free space; else any unlocked; else first unlocked.
pub fn best_speed_test_partition_index(partitions: &[PartitionInfo]) -> Option<usize> {
    let best_non_system = partitions
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_system_partition && !p.is_bitlocker_locked())
        .max_by_key(|(_, p)| p.free_space)
        .map(|(i, _)| i);
    if best_non_system.is_some() {
        return best_non_system;
    }
    partitions
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_bitlocker_locked())
        .max_by_key(|(_, p)| p.free_space)
        .map(|(i, _)| i)
}
