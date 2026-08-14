//! Physical-disk enumeration.
//!
//! Platform backends live in submodules behind one shared free-function
//! contract (`enumerate_physical_disks`) — the pattern the refactor roadmap
//! prescribes for the Linux port (in place of a runtime `StorageBackend`
//! trait, which would add polymorphism no call site needs):
//! - `windows.rs` — WMI (Win32_DiskDrive, MSFT_PhysicalDisk, BitLocker hints).
//! - `linux.rs` — sysfs + /proc/self/mountinfo + statvfs + LUKS dm state.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::enumerate_physical_disks;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::enumerate_physical_disks;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn enumerate_physical_disks() -> Result<Vec<crate::detected_drive::DetectedDrive>, String> {
    Err("Drive enumeration is not implemented on this platform.".to_string())
}
