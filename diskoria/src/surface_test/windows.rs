//! Windows surface-test worker: opens `\\.\PhysicalDriveN` unbuffered, reads
//! the geometry, and feeds the shared scan loop with a seek+ReadFile device.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use super::{perform_full_sequential_read, BlockDev, SurfaceTestMsg, CHUNK_SIZE};

/// Spawn background surface test. Sends [`SurfaceTestMsg`] on `tx`; poll from UI thread.
pub fn spawn_surface_test(
    device_path: String,
    total_blocks: i32,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<SurfaceTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        log::info!(
            target: "diskoria",
            "surface worker: thread start path={} total_blocks={}",
            device_path,
            total_blocks
        );
        if let Err(e) = run_physical_drive_surface_test(&device_path, total_blocks, &cancel, &tx) {
            log::warn!(target: "diskoria", "surface worker: run returned Err, sending Error to UI");
            let _ = tx.send(SurfaceTestMsg::Error(e));
        } else {
            log::debug!(target: "diskoria", "surface worker: run returned Ok");
        }
        log::debug!(target: "diskoria", "surface worker: sending Completed");
        let _ = tx.send(SurfaceTestMsg::Completed);
    })
}

/// Positioned reads over a raw Win32 HANDLE (SetFilePointerEx + ReadFile).
pub(crate) struct HandleDev(pub windows_sys::Win32::Foundation::HANDLE);

impl BlockDev for HandleDev {
    fn read_at(&mut self, offset: i64, buf: &mut [u8]) -> Result<usize, ()> {
        use windows_sys::Win32::Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN};
        let seek_ok =
            unsafe { SetFilePointerEx(self.0, offset, std::ptr::null_mut(), FILE_BEGIN) };
        if seek_ok == 0 {
            return Err(());
        }
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                self.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(())
        } else {
            Ok(bytes_read as usize)
        }
    }
}

fn run_physical_drive_surface_test(
    physical_drive: &str,
    total_blocks: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<SurfaceTestMsg>,
) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_NO_BUFFERING,
        FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

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

    let wide: Vec<u16> = std::ffi::OsStr::new(physical_drive)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let share: FILE_SHARE_MODE = 1 | 2;
    let flags: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN;
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            share,
            std::ptr::null(),
            OPEN_EXISTING as FILE_CREATION_DISPOSITION,
            flags,
            0 as HANDLE,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        log::warn!(
            target: "diskoria",
            "surface worker: CreateFileW failed path={} GetLastError={}",
            physical_drive,
            err
        );
        return Err(format!(
            "Cannot open physical drive {} (error {}). Try running as Administrator.",
            physical_drive, err
        ));
    }
    log::debug!(
        target: "diskoria",
        "surface worker: CreateFileW ok path={}",
        physical_drive
    );

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
        log::warn!(
            target: "diskoria",
            "surface worker: IOCTL_DISK_GET_DRIVE_GEOMETRY_EX failed path={} err={}",
            physical_drive,
            e
        );
        unsafe {
            CloseHandle(handle);
        }
        return Err(format!(
            "Cannot read disk geometry for {} (error {}). Try running as Administrator.",
            physical_drive, e
        ));
    }

    let total_size = geometry_ex.disk_size;
    log::info!(
        target: "diskoria",
        "surface worker: geometry disk_size={} bytes_per_sector={}",
        total_size,
        geometry_ex.geometry.bytes_per_sector
    );
    if total_size <= 0 {
        unsafe {
            CloseHandle(handle);
        }
        log::warn!(
            target: "diskoria",
            "surface worker: disk_size<=0, aborting path={}",
            physical_drive
        );
        return Err(format!(
            "Disk reports zero size for {} (geometry may be unavailable for this volume).",
            physical_drive
        ));
    }
    let bytes_per_sector = geometry_ex.geometry.bytes_per_sector as i32;
    if bytes_per_sector <= 0 {
        unsafe {
            CloseHandle(handle);
        }
        return Err("Invalid bytes per sector from disk geometry.".to_string());
    }

    let mut dev = HandleDev(handle);
    let result = perform_full_sequential_read(
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
    log::debug!(
        target: "diskoria",
        "surface worker: perform_full_sequential_read finished ok={} err={:?}",
        result.is_ok(),
        result.as_ref().err()
    );
    result
}
