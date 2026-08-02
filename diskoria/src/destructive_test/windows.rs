//! Windows destructive-test worker: locks + dismounts every mounted volume on
//! the target disk (handles held open so Windows cannot auto-remount), opens
//! the physical drive unbuffered, and feeds the shared write+verify loop.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::surface_test::windows::HandleDev;

use super::{perform_write_verify, DestructiveTestMsg, RwBlockDev, CHUNK_SIZE};

impl RwBlockDev for HandleDev {
    fn write_at(&mut self, offset: i64, buf: &[u8]) -> Result<usize, ()> {
        use windows_sys::Win32::Storage::FileSystem::{SetFilePointerEx, WriteFile, FILE_BEGIN};
        let seek_ok =
            unsafe { SetFilePointerEx(self.0, offset, std::ptr::null_mut(), FILE_BEGIN) };
        if seek_ok == 0 {
            return Err(());
        }
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                self.0,
                buf.as_ptr(),
                buf.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(())
        } else {
            Ok(bytes_written as usize)
        }
    }
}

/// Spawn background destructive test.  `drive_letters` should contain every
/// volume letter (e.g. `"E:"`) belonging to the target physical disk; they will
/// be locked and dismounted before writing begins.
pub fn spawn_destructive_test(
    device_path: String,
    drive_letters: Vec<String>,
    total_blocks: i32,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<DestructiveTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        log::info!(
            target: "diskoria",
            "destructive worker: thread start path={} volumes={:?} total_blocks={}",
            device_path,
            drive_letters,
            total_blocks
        );
        if let Err(e) =
            run_destructive_drive_test(&device_path, &drive_letters, total_blocks, &cancel, &tx)
        {
            log::warn!(
                target: "diskoria",
                "destructive worker: run returned Err: {}",
                e
            );
            let _ = tx.send(DestructiveTestMsg::Error(e));
        } else {
            log::debug!(target: "diskoria", "destructive worker: run returned Ok");
        }
        log::debug!(target: "diskoria", "destructive worker: sending Completed");
        let _ = tx.send(DestructiveTestMsg::Completed);
    })
}

fn run_destructive_drive_test(
    physical_drive: &str,
    drive_letters: &[String],
    total_blocks: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<DestructiveTestMsg>,
) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_NO_BUFFERING,
        FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
    const FSCTL_LOCK_VOLUME: u32 = 0x00090018;
    const FSCTL_DISMOUNT_VOLUME: u32 = 0x00090020;

    #[repr(C)]
    struct DiskGeometry {
        cylinders: i64,
        media_type: u32,
        tracks_per_cylinder: u32,
        sectors_per_track: u32,
        bytes_per_sector: u32,
    }

    #[repr(C)]
    struct DiskGeometryEx {
        geometry: DiskGeometry,
        disk_size: i64,
    }

    // -------------------------------------------------------------------------
    // Step 1: Lock and dismount every mounted volume on this physical disk.
    // We keep the volume handles open throughout the test so Windows cannot
    // auto-remount them while we are writing raw sectors.
    // -------------------------------------------------------------------------
    let mut volume_handles: Vec<HANDLE> = Vec::new();

    for letter in drive_letters {
        let raw = letter.trim().trim_end_matches('\\').trim_end_matches(':');
        if raw.is_empty() {
            continue;
        }
        let vol_path = format!(r"\\.\{}:", raw);
        log::info!(
            target: "diskoria",
            "destructive worker: opening volume {} for dismount",
            vol_path
        );

        let vol_wide: Vec<u16> = std::ffi::OsStr::new(&vol_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let share: FILE_SHARE_MODE = 1 | 2; // FILE_SHARE_READ | FILE_SHARE_WRITE
        let vol_handle = unsafe {
            CreateFileW(
                vol_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                share,
                std::ptr::null(),
                OPEN_EXISTING as FILE_CREATION_DISPOSITION,
                0,
                0 as HANDLE,
            )
        };

        if vol_handle == INVALID_HANDLE_VALUE {
            let e = unsafe { GetLastError() };
            log::warn!(
                target: "diskoria",
                "destructive worker: cannot open volume {} (err {}), skipping dismount for it",
                vol_path,
                e
            );
            // Non-fatal: volume may not be mounted (unformatted partition, etc.)
            continue;
        }

        // FSCTL_LOCK_VOLUME: retry for up to ~3 s so any background I/O can finish.
        let mut lock_ok = false;
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < lock_deadline {
            let mut bytes_returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    vol_handle,
                    FSCTL_LOCK_VOLUME,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut::<c_void>(),
                    0,
                    &mut bytes_returned,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                lock_ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !lock_ok {
            log::warn!(
                target: "diskoria",
                "destructive worker: FSCTL_LOCK_VOLUME failed for {} (err {}); forcing dismount anyway",
                vol_path,
                unsafe { GetLastError() }
            );
        }

        // FSCTL_DISMOUNT_VOLUME: invalidates the FS even without a lock.
        let mut bytes_returned: u32 = 0;
        let dismount_ok = unsafe {
            DeviceIoControl(
                vol_handle,
                FSCTL_DISMOUNT_VOLUME,
                std::ptr::null(),
                0,
                std::ptr::null_mut::<c_void>(),
                0,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };
        if dismount_ok == 0 {
            let e = unsafe { GetLastError() };
            log::warn!(
                target: "diskoria",
                "destructive worker: FSCTL_DISMOUNT_VOLUME failed for {} (err {})",
                vol_path,
                e
            );
            unsafe {
                CloseHandle(vol_handle);
            }
        } else {
            log::info!(
                target: "diskoria",
                "destructive worker: volume {} dismounted successfully",
                vol_path
            );
            // Keep handle open to prevent auto-remount.
            volume_handles.push(vol_handle);
        }
    }

    // -------------------------------------------------------------------------
    // Step 2: Open the physical disk for read+write.
    // -------------------------------------------------------------------------
    let wide: Vec<u16> = std::ffi::OsStr::new(physical_drive)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let share: FILE_SHARE_MODE = 1 | 2;
    let flags: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN;
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            share,
            std::ptr::null(),
            OPEN_EXISTING as FILE_CREATION_DISPOSITION,
            flags,
            0 as HANDLE,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        // Release volume handles before returning.
        for h in &volume_handles {
            unsafe {
                CloseHandle(*h);
            }
        }
        log::warn!(
            target: "diskoria",
            "destructive worker: CreateFileW failed path={} err={}",
            physical_drive,
            err
        );
        return Err(format!(
            "Cannot open physical drive {} (error {}). Try running as Administrator.",
            physical_drive, err
        ));
    }
    log::info!(
        target: "diskoria",
        "destructive worker: CreateFileW ok path={}",
        physical_drive
    );

    // -------------------------------------------------------------------------
    // Step 3: Query disk geometry.
    // -------------------------------------------------------------------------
    let mut geometry_ex = DiskGeometryEx {
        geometry: DiskGeometry {
            cylinders: 0,
            media_type: 0,
            tracks_per_cylinder: 0,
            sectors_per_track: 0,
            bytes_per_sector: 0,
        },
        disk_size: 0,
    };
    let mut bytes_returned: u32 = 0;
    let geom_ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            std::ptr::null(),
            0,
            &mut geometry_ex as *mut _ as *mut c_void,
            std::mem::size_of::<DiskGeometryEx>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    if geom_ok == 0 {
        let e = unsafe { GetLastError() };
        unsafe {
            CloseHandle(handle);
        }
        for h in &volume_handles {
            unsafe {
                CloseHandle(*h);
            }
        }
        log::warn!(
            target: "diskoria",
            "destructive worker: IOCTL_DISK_GET_DRIVE_GEOMETRY_EX failed path={} err={}",
            physical_drive,
            e
        );
        return Err(format!(
            "Cannot read disk geometry for {} (error {}). Try running as Administrator.",
            physical_drive, e
        ));
    }

    let total_size = geometry_ex.disk_size;
    let bytes_per_sector = geometry_ex.geometry.bytes_per_sector as i32;
    log::info!(
        target: "diskoria",
        "destructive worker: geometry disk_size={} bytes_per_sector={}",
        total_size,
        bytes_per_sector
    );

    if total_size <= 0 {
        unsafe {
            CloseHandle(handle);
        }
        for h in &volume_handles {
            unsafe {
                CloseHandle(*h);
            }
        }
        return Err(format!(
            "Disk reports zero size for {} (geometry may be unavailable for this volume).",
            physical_drive
        ));
    }
    if bytes_per_sector <= 0 {
        unsafe {
            CloseHandle(handle);
        }
        for h in &volume_handles {
            unsafe {
                CloseHandle(*h);
            }
        }
        return Err("Invalid bytes per sector from disk geometry.".to_string());
    }

    // -------------------------------------------------------------------------
    // Step 4: Write+verify loop (shared).
    // -------------------------------------------------------------------------
    let mut dev = HandleDev(handle);
    let result = perform_write_verify(
        &mut dev,
        total_size,
        bytes_per_sector,
        total_blocks,
        CHUNK_SIZE,
        cancel,
        tx,
    );

    unsafe {
        CloseHandle(handle);
    }
    for h in &volume_handles {
        unsafe {
            CloseHandle(*h);
        }
    }
    log::debug!(
        target: "diskoria",
        "destructive worker: perform_write_verify finished ok={} err={:?}",
        result.is_ok(),
        result.as_ref().err()
    );
    result
}
