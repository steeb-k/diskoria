//! Destructive full-disk write+verify test.
//!
//! Every sector on the physical drive is overwritten with a deterministic pattern
//! (position-derived XOR) and then immediately read back and verified.  All
//! mounted volumes on the target disk are locked and dismounted before the first
//! write so that the filesystem cannot interfere, and the volume handles are kept
//! open for the duration of the test to prevent Windows from auto-remounting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

// Re-export the progress struct from surface_test so the UI can use the same type.
pub use crate::surface_test::SurfaceTestProgress as DestructiveTestProgress;

pub const TOTAL_UI_BLOCKS: usize = crate::surface_test::TOTAL_UI_BLOCKS;
pub const SLOW_THRESHOLD_MS: f64 = crate::surface_test::SLOW_THRESHOLD_MS;

pub enum DestructiveTestMsg {
    Progress(DestructiveTestProgress),
    /// Normal completion or user cancel (worker finished).
    Completed,
    Error(String),
}

/// Spawn background destructive test.  `drive_letters` should contain every
/// volume letter (e.g. `"E:"`) belonging to the target physical disk; they will
/// be locked and dismounted before writing begins.
#[cfg(windows)]
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
        if let Err(e) = run_destructive_drive_test(
            &device_path,
            &drive_letters,
            total_blocks,
            &cancel,
            &tx,
        ) {
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

#[cfg(windows)]
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
            unsafe { CloseHandle(vol_handle); }
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
            unsafe { CloseHandle(*h); }
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
        unsafe { CloseHandle(handle); }
        for h in &volume_handles {
            unsafe { CloseHandle(*h); }
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
        unsafe { CloseHandle(handle); }
        for h in &volume_handles {
            unsafe { CloseHandle(*h); }
        }
        return Err(format!(
            "Disk reports zero size for {} (geometry may be unavailable for this volume).",
            physical_drive
        ));
    }
    if bytes_per_sector <= 0 {
        unsafe { CloseHandle(handle); }
        for h in &volume_handles {
            unsafe { CloseHandle(*h); }
        }
        return Err("Invalid bytes per sector from disk geometry.".to_string());
    }

    // -------------------------------------------------------------------------
    // Step 4: Write+verify loop.
    // -------------------------------------------------------------------------
    let result = perform_write_verify(
        handle,
        total_size,
        bytes_per_sector,
        total_blocks,
        CHUNK_SIZE,
        cancel,
        tx,
    );

    unsafe { CloseHandle(handle); }
    for h in &volume_handles {
        unsafe { CloseHandle(*h); }
    }
    log::debug!(
        target: "diskoria",
        "destructive worker: perform_write_verify finished ok={} err={:?}",
        result.is_ok(),
        result.as_ref().err()
    );
    result
}

#[cfg(windows)]
fn perform_write_verify(
    handle: windows_sys::Win32::Foundation::HANDLE,
    total_size: i64,
    bytes_per_sector: i32,
    total_blocks: i32,
    chunk_size: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<DestructiveTestMsg>,
) -> Result<(), String> {
    use std::time::Instant;

    use windows_sys::Win32::Storage::FileSystem::{
        ReadFile, SetFilePointerEx, WriteFile, FILE_BEGIN,
    };

    let mut aligned_chunk_size = (chunk_size / bytes_per_sector) * bytes_per_sector;
    if aligned_chunk_size == 0 {
        aligned_chunk_size = bytes_per_sector;
    }

    let align = (bytes_per_sector.max(512)) as usize;

    let mut write_buf = AlignedBuf::new(aligned_chunk_size as usize, align)
        .ok_or_else(|| "Out of memory for write buffer.".to_string())?;
    let mut read_buf = AlignedBuf::new(aligned_chunk_size as usize, align)
        .ok_or_else(|| "Out of memory for read buffer.".to_string())?;

    let total_sectors = total_size / bytes_per_sector as i64;

    let mut good_sectors: i64 = 0;
    let mut bad_sectors: i64 = 0;
    let mut slow_sectors: i64 = 0;
    let mut total_bytes_processed: i64 = 0;
    let mut current_position: i64 = 0;
    let overall_start = Instant::now();

    let mut last_block_index: i32 = -1;
    let mut current_block_has_bad = false;
    let mut current_block_total_time_ms = 0.0_f64;
    let mut current_block_op_count: i32 = 0;

    unsafe {
        SetFilePointerEx(handle, 0, std::ptr::null_mut(), FILE_BEGIN);
    }

    while current_position < total_size {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        let remaining = total_size - current_position;
        let mut bytes_to_process_i64 = (aligned_chunk_size as i64).min(remaining);
        bytes_to_process_i64 =
            (bytes_to_process_i64 / bytes_per_sector as i64) * bytes_per_sector as i64;
        if bytes_to_process_i64 <= 0 {
            break;
        }
        let bytes_to_process = bytes_to_process_i64 as i32;

        // --- Fill write buffer with deterministic position-derived pattern ---
        fill_write_pattern(write_buf.as_mut_slice(), current_position, bytes_to_process as usize);

        let t0 = Instant::now();

        // --- Write phase ---
        let mut bytes_written: u32 = 0;
        let write_ok = unsafe {
            WriteFile(
                handle,
                write_buf.as_slice().as_ptr(),
                bytes_to_process as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };

        let chunk_bad: bool;
        let actual_bytes: i32;

        if write_ok != 0 && bytes_written > 0 {
            actual_bytes = bytes_written as i32;

            // --- Seek back ---
            unsafe {
                SetFilePointerEx(handle, current_position, std::ptr::null_mut(), FILE_BEGIN);
            }

            // --- Read-back phase ---
            let mut bytes_read: u32 = 0;
            let read_ok = unsafe {
                ReadFile(
                    handle,
                    read_buf.as_mut_slice().as_mut_ptr(),
                    bytes_written,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };

            if read_ok != 0 && bytes_read == bytes_written {
                // --- Verify phase: compare sector-by-sector ---
                let mismatched = count_mismatched_sectors(
                    write_buf.as_slice(),
                    read_buf.as_slice(),
                    bytes_read as usize,
                    bytes_per_sector as usize,
                );
                let total_sectors_in_chunk = bytes_read as i64 / bytes_per_sector as i64;
                bad_sectors += mismatched;
                let matching = total_sectors_in_chunk - mismatched;
                good_sectors += matching;
                chunk_bad = mismatched > 0;
            } else {
                // Read-back failure — count all sectors as bad.
                let bad_found = handle_read_error(
                    handle,
                    current_position,
                    bytes_to_process,
                    bytes_per_sector,
                    cancel,
                )?;
                bad_sectors += bad_found;
                good_sectors +=
                    (bytes_to_process as i64 / bytes_per_sector as i64) - bad_found;
                chunk_bad = true;
            }
        } else {
            // Write failure — count bad sectors, skip the read/verify.
            actual_bytes = bytes_to_process;
            let bad_found = handle_write_error(
                handle,
                current_position,
                bytes_to_process,
                bytes_per_sector,
                cancel,
                write_buf.as_slice(),
            )?;
            bad_sectors += bad_found;
            good_sectors += (bytes_to_process as i64 / bytes_per_sector as i64) - bad_found;
            chunk_bad = true;
        }

        let op_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let op_time_ms = op_time_ms.max(0.01);

        if chunk_bad {
            current_block_has_bad = true;
        } else {
            let sectors_in_chunk = actual_bytes as i64 / bytes_per_sector as i64;
            if op_time_ms >= SLOW_THRESHOLD_MS {
                slow_sectors += sectors_in_chunk;
            }
        }

        total_bytes_processed += actual_bytes as i64;
        current_position += actual_bytes as i64;

        // Seek forward in case of write error (write error path doesn't advance file pointer).
        unsafe {
            SetFilePointerEx(handle, current_position, std::ptr::null_mut(), FILE_BEGIN);
        }

        current_block_total_time_ms += op_time_ms;
        current_block_op_count += 1;

        // --- Progress reporting (same block_index logic as surface_test) ---
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
            let report_block_index = if last_block_index >= 0 {
                last_block_index
            } else {
                block_index
            };

            let avg_block_time = if current_block_op_count > 0 {
                current_block_total_time_ms / f64::from(current_block_op_count)
            } else {
                0.0
            };
            let progress_percent = (current_position as f64 * 100.0) / total_size as f64;
            let elapsed_seconds = overall_start.elapsed().as_secs_f64();
            let avg_speed_mbps = if elapsed_seconds > 0.0 {
                (total_bytes_processed as f64 / (1024.0 * 1024.0)) / elapsed_seconds
            } else {
                0.0
            };
            let current_speed_mbps = if op_time_ms > 0.0 {
                (bytes_to_process as f64 / (1024.0 * 1024.0)) / (op_time_ms / 1000.0)
            } else {
                0.0
            };

            let progress = DestructiveTestProgress {
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
                block_read_time_ms: avg_block_time,
                total_sectors,
            };
            let _ = tx.send(DestructiveTestMsg::Progress(progress));

            last_block_index = block_index;
            current_block_has_bad = false;
            current_block_total_time_ms = 0.0;
            current_block_op_count = 0;
        }
    }

    Ok(())
}

/// Fill `buf[..len]` with the position-derived XOR pattern.
/// Each 8-byte word = `position_of_that_word ^ 0xA5A5A5A5_A5A5A5A5`.
#[cfg(windows)]
fn fill_write_pattern(buf: &mut [u8], chunk_start_position: i64, len: usize) {
    const MAGIC: u64 = 0xA5A5A5A5_A5A5A5A5;
    let mut offset = 0usize;
    while offset + 8 <= len {
        let word_position = chunk_start_position + offset as i64;
        let word = (word_position as u64) ^ MAGIC;
        buf[offset..offset + 8].copy_from_slice(&word.to_le_bytes());
        offset += 8;
    }
    // Handle tail (< 8 bytes, only happens on last chunk of an odd-sized disk).
    if offset < len {
        let word_position = chunk_start_position + offset as i64;
        let word = (word_position as u64) ^ MAGIC;
        let tail = word.to_le_bytes();
        buf[offset..len].copy_from_slice(&tail[..len - offset]);
    }
}

/// Compare written vs read-back data at sector granularity.
/// Returns number of sectors that do not match.
#[cfg(windows)]
fn count_mismatched_sectors(
    written: &[u8],
    readback: &[u8],
    len: usize,
    bytes_per_sector: usize,
) -> i64 {
    let mut mismatches: i64 = 0;
    let mut offset = 0usize;
    while offset + bytes_per_sector <= len {
        if written[offset..offset + bytes_per_sector] != readback[offset..offset + bytes_per_sector]
        {
            mismatches += 1;
        }
        offset += bytes_per_sector;
    }
    mismatches
}

/// Sector-by-sector write probe: try each sector in the failed chunk and count failures.
#[cfg(windows)]
fn handle_write_error(
    handle: windows_sys::Win32::Foundation::HANDLE,
    position: i64,
    bytes_to_write: i32,
    bytes_per_sector: i32,
    cancel: &AtomicBool,
    pattern_buf: &[u8],
) -> Result<i64, String> {
    use windows_sys::Win32::Storage::FileSystem::{SetFilePointerEx, WriteFile, FILE_BEGIN};

    let mut bad_sector_count: i64 = 0;
    let end_position = position + bytes_to_write as i64;
    let mut sector_pos = position;
    let bps = bytes_per_sector as usize;

    while sector_pos < end_position {
        if cancel.load(Ordering::Relaxed) {
            return Ok(bad_sector_count);
        }
        let sector_offset = (sector_pos - position) as usize;
        let sector_slice = if sector_offset + bps <= pattern_buf.len() {
            &pattern_buf[sector_offset..sector_offset + bps]
        } else {
            bad_sector_count += 1;
            sector_pos += bytes_per_sector as i64;
            continue;
        };

        let seek_ok = unsafe {
            SetFilePointerEx(handle, sector_pos, std::ptr::null_mut(), FILE_BEGIN)
        };
        if seek_ok == 0 {
            bad_sector_count += 1;
            sector_pos += bytes_per_sector as i64;
            continue;
        }

        let mut bw: u32 = 0;
        let ok = unsafe {
            WriteFile(
                handle,
                sector_slice.as_ptr(),
                bytes_per_sector as u32,
                &mut bw,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || bw == 0 {
            bad_sector_count += 1;
        }
        sector_pos += bytes_per_sector as i64;
    }
    Ok(bad_sector_count)
}

/// Sector-by-sector read probe after a read failure.
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
    let mut one = AlignedBuf::new(bytes_per_sector as usize, align)
        .ok_or_else(|| "Out of memory for sector buffer.".to_string())?;

    let mut bad_sector_count: i64 = 0;
    let end_position = position + bytes_to_read as i64;
    let mut sector_pos = position;
    while sector_pos < end_position {
        if cancel.load(Ordering::Relaxed) {
            return Ok(bad_sector_count);
        }
        let seek_ok = unsafe {
            SetFilePointerEx(handle, sector_pos, std::ptr::null_mut(), FILE_BEGIN)
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

// -------------------------------------------------------------------------
// Aligned heap buffer (same as surface_test::AlignedBuf).
// -------------------------------------------------------------------------

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

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
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

// -------------------------------------------------------------------------
// Non-Windows stub
// -------------------------------------------------------------------------

#[cfg(not(windows))]
pub fn spawn_destructive_test(
    _device_path: String,
    _drive_letters: Vec<String>,
    _total_blocks: i32,
    _cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<DestructiveTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(DestructiveTestMsg::Error(
            "Destructive testing is only available on Windows.".to_string(),
        ));
        let _ = tx.send(DestructiveTestMsg::Completed);
    })
}
