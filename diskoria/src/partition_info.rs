//! Mounted volume / partition info for speed tests (WMI-backed on Windows,
//! mountinfo/statvfs-backed on Linux).

/// Full-volume-encryption state: BitLocker on Windows, LUKS/dm-crypt on
/// Linux. `Locked` means the key is unavailable and the contents are
/// unreadable (a closed LUKS container); `Encrypted` is Windows-only
/// (BitLocker present but protection suspended) — LUKS has no analogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptionStatus {
    NotEncrypted,
    Locked,
    Unlocked,
    Encrypted,
}

#[derive(Clone, Debug)]
pub struct PartitionInfo {
    /// Where the volume is reachable: a drive letter (`C:`) on Windows, a
    /// mount point (`/`, `/home`) on Linux. Empty when not mounted.
    pub mount_point: String,
    pub volume_name: String,
    pub total_size: i64,
    pub free_space: i64,
    pub file_system: String,
    pub is_system_partition: bool,
    pub encryption: EncryptionStatus,
}

/// Canonical display form of a mount: `C:\` on Windows (accepting the `C`,
/// `C:` and `C:\` spellings), the mount-point path itself elsewhere.
/// `None` when the volume isn't mounted.
pub fn display_mount(raw: &str) -> Option<String> {
    let l = raw.trim();
    if l.is_empty() {
        return None;
    }
    #[cfg(windows)]
    {
        let base = l.trim_end_matches('\\').trim_end_matches(':');
        if base.is_empty() {
            None
        } else {
            Some(format!("{base}:\\"))
        }
    }
    #[cfg(not(windows))]
    {
        Some(l.to_string())
    }
}

impl PartitionInfo {
    pub fn is_encryption_locked(&self) -> bool {
        self.encryption == EncryptionStatus::Locked
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

/// Indices of the partitions the file-based benchmark can actually run on:
/// mounted (there is a path to write the test file to) and not locked.
///
/// An unmounted partition is a real partition worth *listing* on the drive
/// card, but it is not a benchmark target — it has no filesystem path, and
/// treating its empty mount point as one made the benchmark write to whatever
/// path that produced (KI-38).
pub fn benchmarkable_partitions(partitions: &[PartitionInfo]) -> Vec<usize> {
    partitions
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.mount_point.trim().is_empty() && !p.is_encryption_locked())
        .map(|(i, _)| i)
        .collect()
}

/// Largest non-system benchmarkable partition by free space; else any
/// benchmarkable one.
pub fn best_speed_test_partition_index(partitions: &[PartitionInfo]) -> Option<usize> {
    let eligible = benchmarkable_partitions(partitions);
    let best_non_system = eligible
        .iter()
        .copied()
        .filter(|i| !partitions[*i].is_system_partition)
        .max_by_key(|i| partitions[*i].free_space);
    if best_non_system.is_some() {
        return best_non_system;
    }
    eligible
        .into_iter()
        .max_by_key(|i| partitions[*i].free_space)
}
