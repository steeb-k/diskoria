//! Read-only full-disk surface scan (sequential reads, progress to UI).

use std::sync::atomic::AtomicBool;
// The cancel-flag loads live in the Windows worker; gated so the Linux build
// stays warning-free until the port adds its own worker.
#[cfg(windows)]
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Progress fields consumed by the sector-test UI.
#[derive(Clone, Debug)]
pub struct SurfaceTestProgress {
    pub progress_percent: f64,
    pub bytes_scanned: i64,
    pub total_bytes: i64,
    pub good_sectors: i64,
    pub bad_sectors: i64,
    pub slow_sectors: i64,
    pub average_speed_mbps: f64,
    pub current_speed_mbps: f64,
    pub block_index: i32,
    pub block_is_good: bool,
    pub block_read_time_ms: f64,
    pub total_sectors: i64,
}

pub enum SurfaceTestMsg {
    Progress(SurfaceTestProgress),
    /// Normal completion or user cancel (worker finished).
    Completed,
    Error(String),
}

pub const TOTAL_UI_BLOCKS: usize = 1000;
pub const SLOW_THRESHOLD_MS: f64 = 200.0;

/// Spawn background surface test. Sends [`SurfaceTestMsg`] on `tx`; poll from UI thread.
#[cfg(windows)]
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

#[cfg(windows)]
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
    const CHUNK_SIZE: i32 = 1024 * 1024;

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
            physical_drive,
            err
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

    let result = perform_full_sequential_read(
        handle,
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

#[cfg(windows)]
fn perform_full_sequential_read(
    handle: windows_sys::Win32::Foundation::HANDLE,
    total_size: i64,
    bytes_per_sector: i32,
    total_blocks: i32,
    chunk_size: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<SurfaceTestMsg>,
) -> Result<(), String> {
    use std::time::Instant;

    use windows_sys::Win32::Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN};

    let mut aligned_chunk_size = (chunk_size / bytes_per_sector) * bytes_per_sector;
    if aligned_chunk_size == 0 {
        aligned_chunk_size = bytes_per_sector;
    }

    let sector_align = bytes_per_sector.max(512) as usize;
    let align = sector_align.max(512);

    let mut main_buf = AlignedBuf::new(aligned_chunk_size as usize, align).ok_or_else(|| {
        "Out of memory for read buffer.".to_string()
    })?;

    let total_sectors = total_size / bytes_per_sector as i64;

    let mut good_sectors: i64 = 0;
    let mut bad_sectors: i64 = 0;
    let mut slow_sectors: i64 = 0;
    let mut total_bytes_read: i64 = 0;
    let mut current_position: i64 = 0;
    let overall_start = Instant::now();

    let mut last_block_index: i32 = -1;
    let mut current_block_has_bad = false;
    let mut current_block_total_time_ms = 0.0_f64;
    let mut current_block_read_count: i32 = 0;

    unsafe {
        SetFilePointerEx(handle, 0, std::ptr::null_mut(), FILE_BEGIN);
    }

    while current_position < total_size {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Never cast remaining bytes to i32: disks > ~2GB overflow i32 and the low bits can be
        // negative, making min(chunk, remaining) negative → sector alignment yields 0 → no reads.
        let remaining = total_size - current_position;
        let mut bytes_to_read_i64 =
            (aligned_chunk_size as i64).min(remaining);
        bytes_to_read_i64 = (bytes_to_read_i64 / bytes_per_sector as i64)
            * bytes_per_sector as i64;
        if bytes_to_read_i64 <= 0 {
            break;
        }
        let bytes_to_read = bytes_to_read_i64 as i32;

        let t0 = Instant::now();
        let mut bytes_read: u32 = 0;
        let slice = main_buf.as_mut_slice();
        let read_ok = unsafe {
            ReadFile(
                handle,
                slice.as_mut_ptr(),
                bytes_to_read as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        let read_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let read_time_ms = read_time_ms.max(0.01);

        let sectors_in_this_read = (bytes_read as i64) / (bytes_per_sector as i64);

        if read_ok != 0 && bytes_read > 0 {
            if read_time_ms >= SLOW_THRESHOLD_MS {
                slow_sectors += sectors_in_this_read;
            } else {
                good_sectors += sectors_in_this_read;
            }
            total_bytes_read += bytes_read as i64;
            current_position += bytes_read as i64;
        } else {
            let bad_found = handle_read_error(
                handle,
                current_position,
                bytes_to_read,
                bytes_per_sector,
                cancel,
            )?;
            bad_sectors += bad_found;
            good_sectors += (bytes_to_read as i64 / bytes_per_sector as i64) - bad_found;
            current_position += bytes_to_read as i64;
            current_block_has_bad = true;
            unsafe {
                SetFilePointerEx(handle, current_position, std::ptr::null_mut(), FILE_BEGIN);
            }
        }

        current_block_total_time_ms += read_time_ms;
        current_block_read_count += 1;

        let tb = total_blocks.max(1) as f64;
        let mut block_index =
            ((current_position as f64 / total_size as f64) * tb) as i32;
        if block_index >= total_blocks {
            block_index = total_blocks - 1;
        }
        if block_index < 0 {
            block_index = 0;
        }

        if block_index != last_block_index || current_position >= total_size {
            let mut report_block_index = if last_block_index >= 0 {
                last_block_index
            } else {
                block_index
            };
            if last_block_index < 0 {
                report_block_index = 0;
            }

            let avg_block_read_time = if current_block_read_count > 0 {
                current_block_total_time_ms / f64::from(current_block_read_count)
            } else {
                0.0
            };
            let progress_percent = (current_position as f64 * 100.0) / total_size as f64;
            let elapsed_seconds = overall_start.elapsed().as_secs_f64();
            let avg_speed_mbps = if elapsed_seconds > 0.0 {
                (total_bytes_read as f64 / (1024.0 * 1024.0)) / elapsed_seconds
            } else {
                0.0
            };
            let current_speed_mbps = if read_time_ms > 0.0 {
                (bytes_to_read as f64 / (1024.0 * 1024.0)) / (read_time_ms / 1000.0)
            } else {
                0.0
            };

            let progress = SurfaceTestProgress {
                progress_percent,
                bytes_scanned: current_position,
                total_bytes: total_size,
                good_sectors,
                bad_sectors,
                slow_sectors,
                average_speed_mbps: avg_speed_mbps,
                current_speed_mbps,
                block_index: report_block_index,
                block_is_good: !current_block_has_bad,
                block_read_time_ms: avg_block_read_time,
                total_sectors,
            };
            let _ = tx.send(SurfaceTestMsg::Progress(progress));

            last_block_index = block_index;
            current_block_has_bad = false;
            current_block_total_time_ms = 0.0;
            current_block_read_count = 0;
        }
    }

    Ok(())
}

#[cfg(windows)]
fn handle_read_error(
    handle: windows_sys::Win32::Foundation::HANDLE,
    position: i64,
    bytes_to_read: i32,
    bytes_per_sector: i32,
    cancel: &AtomicBool,
) -> Result<i64, String> {
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN};

    let align = (bytes_per_sector.max(512)) as usize;
    let mut one = AlignedBuf::new(bytes_per_sector as usize, align).ok_or_else(|| {
        "Out of memory for sector buffer.".to_string()
    })?;

    let mut bad_sector_count: i64 = 0;
    let end_position = position + bytes_to_read as i64;
    let mut sector_pos = position;
    while sector_pos < end_position {
        if cancel.load(Ordering::Relaxed) {
            return Ok(bad_sector_count);
        }
        let seek_ok = unsafe {
            SetFilePointerEx(
                handle,
                sector_pos,
                std::ptr::null_mut(),
                FILE_BEGIN,
            )
        };
        if seek_ok == 0 {
            bad_sector_count += 1;
            sector_pos += bytes_per_sector as i64;
            continue;
        }
        let mut br: u32 = 0;
        let slice = one.as_mut_slice();
        let ok = unsafe {
            ReadFile(
                handle,
                slice.as_mut_ptr(),
                bytes_per_sector as u32,
                &mut br,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || br == 0 {
            bad_sector_count += 1;
        }
        sector_pos += bytes_per_sector as i64;
    }
    Ok(bad_sector_count)
}

#[cfg(windows)]
struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: std::alloc::Layout,
}

#[cfg(windows)]
impl AlignedBuf {
    fn new(len: usize, align: usize) -> Option<Self> {
        let layout = std::alloc::Layout::from_size_align(len, align).ok()?;
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(Self { ptr, len, layout })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

#[cfg(windows)]
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(self.ptr, self.layout);
        }
    }
}

#[cfg(not(windows))]
pub fn spawn_surface_test(
    _device_path: String,
    _total_blocks: i32,
    _cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<SurfaceTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(SurfaceTestMsg::Error(
            "Sector testing is only available on Windows.".to_string(),
        ));
        let _ = tx.send(SurfaceTestMsg::Completed);
    })
}
