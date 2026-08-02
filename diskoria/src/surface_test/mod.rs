//! Read-only full-disk surface scan (sequential reads, progress to UI).
//!
//! The scan loop, bad-sector reprobe and progress accounting are shared; the
//! platform submodules only open the device, query its geometry, and provide
//! the positioned-read primitive ([`BlockDev`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
// The fallback stub below is the only in-file user of these two.
#[cfg(not(any(windows, target_os = "linux")))]
use std::sync::Arc;
#[cfg(not(any(windows, target_os = "linux")))]
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

pub(crate) const CHUNK_SIZE: i32 = 1024 * 1024;

/// Positioned block-device reads. `Err(())` is a read failure at that offset —
/// the scan loop then reprobes the chunk sector by sector.
pub(crate) trait BlockDev {
    fn read_at(&mut self, offset: i64, buf: &mut [u8]) -> Result<usize, ()>;
}

/// A buffer whose start address satisfies the platform's direct-I/O alignment
/// (`FILE_FLAG_NO_BUFFERING` / `O_DIRECT`).
pub(crate) struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: std::alloc::Layout,
}

impl AlignedBuf {
    pub(crate) fn new(len: usize, align: usize) -> Option<Self> {
        let layout = std::alloc::Layout::from_size_align(len, align).ok()?;
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(Self { ptr, len, layout })
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    #[cfg_attr(not(windows), allow(dead_code))] // destructive verify uses it on Windows today
    pub(crate) fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(self.ptr, self.layout);
        }
    }
}

// Only touched through its slices from the owning worker thread.
unsafe impl Send for AlignedBuf {}

/// The full sequential scan. Identical behavior on every platform; only the
/// device primitive differs.
pub(crate) fn perform_full_sequential_read(
    dev: &mut dyn BlockDev,
    total_size: i64,
    bytes_per_sector: i32,
    total_blocks: i32,
    chunk_size: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<SurfaceTestMsg>,
) -> Result<(), String> {
    use std::time::Instant;

    let mut aligned_chunk_size = (chunk_size / bytes_per_sector) * bytes_per_sector;
    if aligned_chunk_size == 0 {
        aligned_chunk_size = bytes_per_sector;
    }

    let sector_align = bytes_per_sector.max(512) as usize;
    let align = sector_align.max(512);

    let mut main_buf = AlignedBuf::new(aligned_chunk_size as usize, align)
        .ok_or_else(|| "Out of memory for read buffer.".to_string())?;

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

    while current_position < total_size {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Never cast remaining bytes to i32: disks > ~2GB overflow i32 and the low bits can be
        // negative, making min(chunk, remaining) negative → sector alignment yields 0 → no reads.
        let remaining = total_size - current_position;
        let mut bytes_to_read_i64 = (aligned_chunk_size as i64).min(remaining);
        bytes_to_read_i64 =
            (bytes_to_read_i64 / bytes_per_sector as i64) * bytes_per_sector as i64;
        if bytes_to_read_i64 <= 0 {
            break;
        }
        let bytes_to_read = bytes_to_read_i64 as i32;

        let t0 = Instant::now();
        let slice = &mut main_buf.as_mut_slice()[..bytes_to_read as usize];
        let read = dev.read_at(current_position, slice);
        let read_time_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let read_time_ms = read_time_ms.max(0.01);

        match read {
            Ok(bytes_read) if bytes_read > 0 => {
                let sectors_in_this_read = (bytes_read as i64) / (bytes_per_sector as i64);
                if read_time_ms >= SLOW_THRESHOLD_MS {
                    slow_sectors += sectors_in_this_read;
                } else {
                    good_sectors += sectors_in_this_read;
                }
                total_bytes_read += bytes_read as i64;
                current_position += bytes_read as i64;
            }
            _ => {
                let bad_found = reprobe_sectors(
                    dev,
                    current_position,
                    bytes_to_read,
                    bytes_per_sector,
                    cancel,
                )?;
                bad_sectors += bad_found;
                good_sectors += (bytes_to_read as i64 / bytes_per_sector as i64) - bad_found;
                current_position += bytes_to_read as i64;
                current_block_has_bad = true;
            }
        }

        current_block_total_time_ms += read_time_ms;
        current_block_read_count += 1;

        let tb = total_blocks.max(1) as f64;
        let mut block_index = ((current_position as f64 / total_size as f64) * tb) as i32;
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

/// A failed chunk read is re-probed sector by sector to count exactly how many
/// sectors are bad.
pub(crate) fn reprobe_sectors(
    dev: &mut dyn BlockDev,
    position: i64,
    bytes_to_read: i32,
    bytes_per_sector: i32,
    cancel: &AtomicBool,
) -> Result<i64, String> {
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
        match dev.read_at(sector_pos, one.as_mut_slice()) {
            Ok(n) if n > 0 => {}
            _ => bad_sector_count += 1,
        }
        sector_pos += bytes_per_sector as i64;
    }
    Ok(bad_sector_count)
}

// ── Platform workers ─────────────────────────────────────────────────────────

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
pub use windows::spawn_surface_test;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
pub use linux::spawn_surface_test;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn spawn_surface_test(
    _device_path: String,
    _total_blocks: i32,
    _cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<SurfaceTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(SurfaceTestMsg::Error(
            "Sector testing is not implemented on this platform.".to_string(),
        ));
        let _ = tx.send(SurfaceTestMsg::Completed);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    /// In-memory device with one read-failing region.
    struct FakeDev {
        size: i64,
        bad_from: i64,
        bad_to: i64,
    }

    impl BlockDev for FakeDev {
        fn read_at(&mut self, offset: i64, buf: &mut [u8]) -> Result<usize, ()> {
            let end = offset + buf.len() as i64;
            // Any overlap with the bad region fails wholesale, like a real
            // device-level error return; single-sector reprobes then isolate
            // the truly bad sectors.
            if end > self.bad_from && offset < self.bad_to {
                return Err(());
            }
            let n = (self.size - offset).clamp(0, buf.len() as i64);
            Ok(n as usize)
        }
    }

    #[test]
    fn scan_counts_bad_sectors_exactly() {
        let sector = 512;
        let size = 8 * 1024 * 1024_i64;
        // A 16-sector bad region aligned to the middle of a 1 MiB chunk.
        let bad_from = 2 * 1024 * 1024;
        let bad_to = bad_from + 16 * sector as i64;
        let mut dev = FakeDev { size, bad_from, bad_to };
        let cancel = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();

        perform_full_sequential_read(&mut dev, size, sector, 100, CHUNK_SIZE, &cancel, &tx)
            .expect("scan");

        let mut last = None;
        while let Ok(SurfaceTestMsg::Progress(p)) = rx.try_recv() {
            last = Some(p);
        }
        let p = last.expect("progress");
        assert_eq!(p.bad_sectors, 16);
        assert_eq!(p.bytes_scanned, size);
        assert_eq!(
            p.good_sectors + p.bad_sectors + p.slow_sectors,
            size / sector as i64
        );
    }

    #[test]
    fn cancel_stops_the_scan() {
        let mut dev = FakeDev { size: 64 * 1024 * 1024, bad_from: -1, bad_to: -1 };
        let cancel = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel();
        perform_full_sequential_read(
            &mut dev,
            64 * 1024 * 1024,
            512,
            100,
            CHUNK_SIZE,
            &cancel,
            &tx,
        )
        .expect("scan");
        assert!(rx.try_recv().is_err(), "no progress after immediate cancel");
    }
}
