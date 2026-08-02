//! Destructive full-disk write+verify test.
//!
//! Every sector on the physical drive is overwritten with a deterministic
//! pattern (position-derived XOR) and then immediately read back and verified.
//! Before the first write, every mounted volume on the target disk is taken
//! offline — locked+dismounted (with the handles held open) on Windows,
//! unmounted with an `O_EXCL` exclusive open as the second guard on Linux —
//! so the filesystem cannot interfere.
//!
//! The write+verify loop, pattern generator, verify comparator and per-sector
//! error probes are shared; the platform submodules provide volume teardown,
//! device open/geometry, and the positioned read/write primitives.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
// The fallback stub below is the only in-file user of these two.
#[cfg(not(any(windows, target_os = "linux")))]
use std::sync::Arc;
#[cfg(not(any(windows, target_os = "linux")))]
use std::thread::JoinHandle;

use crate::surface_test::{AlignedBuf, BlockDev};

// Re-export the progress struct from surface_test so the UI can use the same type.
pub use crate::surface_test::SurfaceTestProgress as DestructiveTestProgress;

pub const TOTAL_UI_BLOCKS: usize = crate::surface_test::TOTAL_UI_BLOCKS;
pub const SLOW_THRESHOLD_MS: f64 = crate::surface_test::SLOW_THRESHOLD_MS;

pub(crate) const CHUNK_SIZE: i32 = 1024 * 1024;

pub enum DestructiveTestMsg {
    Progress(DestructiveTestProgress),
    /// Normal completion or user cancel (worker finished).
    Completed,
    Error(String),
}

/// Positioned read/write block device for the write+verify loop.
pub(crate) trait RwBlockDev: BlockDev {
    fn write_at(&mut self, offset: i64, buf: &[u8]) -> Result<usize, ()>;
}

/// Fill `buf[..len]` with the position-derived XOR pattern.
/// Each 8-byte word = `position_of_that_word ^ 0xA5A5A5A5_A5A5A5A5`.
pub(crate) fn fill_write_pattern(buf: &mut [u8], chunk_start_position: i64, len: usize) {
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
pub(crate) fn count_mismatched_sectors(
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
fn probe_write_error(
    dev: &mut dyn RwBlockDev,
    position: i64,
    bytes_to_write: i32,
    bytes_per_sector: i32,
    cancel: &AtomicBool,
    pattern_buf: &[u8],
) -> i64 {
    let mut bad_sector_count: i64 = 0;
    let end_position = position + bytes_to_write as i64;
    let mut sector_pos = position;
    let bps = bytes_per_sector as usize;

    while sector_pos < end_position {
        if cancel.load(Ordering::Relaxed) {
            return bad_sector_count;
        }
        let sector_offset = (sector_pos - position) as usize;
        let sector_slice = if sector_offset + bps <= pattern_buf.len() {
            &pattern_buf[sector_offset..sector_offset + bps]
        } else {
            bad_sector_count += 1;
            sector_pos += bytes_per_sector as i64;
            continue;
        };

        match dev.write_at(sector_pos, sector_slice) {
            Ok(n) if n > 0 => {}
            _ => bad_sector_count += 1,
        }
        sector_pos += bytes_per_sector as i64;
    }
    bad_sector_count
}

/// The write → read-back → verify loop. Identical behavior on every platform.
pub(crate) fn perform_write_verify(
    dev: &mut dyn RwBlockDev,
    total_size: i64,
    bytes_per_sector: i32,
    total_blocks: i32,
    chunk_size: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<DestructiveTestMsg>,
) -> Result<(), String> {
    use std::time::Instant;

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
        fill_write_pattern(
            write_buf.as_mut_slice(),
            current_position,
            bytes_to_process as usize,
        );

        let t0 = Instant::now();

        let chunk_bad: bool;
        let actual_bytes: i32;

        // --- Write phase ---
        let wrote = dev.write_at(
            current_position,
            &write_buf.as_slice()[..bytes_to_process as usize],
        );

        match wrote {
            Ok(bytes_written) if bytes_written > 0 => {
                actual_bytes = bytes_written as i32;

                // --- Read-back phase ---
                let read = dev.read_at(
                    current_position,
                    &mut read_buf.as_mut_slice()[..bytes_written],
                );

                match read {
                    Ok(bytes_read) if bytes_read == bytes_written => {
                        // --- Verify phase: compare sector-by-sector ---
                        let mismatched = count_mismatched_sectors(
                            write_buf.as_slice(),
                            read_buf.as_slice(),
                            bytes_read,
                            bytes_per_sector as usize,
                        );
                        let total_sectors_in_chunk =
                            bytes_read as i64 / bytes_per_sector as i64;
                        bad_sectors += mismatched;
                        good_sectors += total_sectors_in_chunk - mismatched;
                        chunk_bad = mismatched > 0;
                    }
                    _ => {
                        // Read-back failure — probe sector by sector.
                        let bad_found = crate::surface_test::reprobe_sectors(
                            dev,
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
                }
            }
            _ => {
                // Write failure — probe writes sector by sector, skip verify.
                actual_bytes = bytes_to_process;
                let bad_found = probe_write_error(
                    dev,
                    current_position,
                    bytes_to_process,
                    bytes_per_sector,
                    cancel,
                    write_buf.as_slice(),
                );
                bad_sectors += bad_found;
                good_sectors += (bytes_to_process as i64 / bytes_per_sector as i64) - bad_found;
                chunk_bad = true;
            }
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

        current_block_total_time_ms += op_time_ms;
        current_block_op_count += 1;

        // --- Progress reporting (same block_index logic as surface_test) ---
        let tb = total_blocks.max(1) as f64;
        let mut block_index = ((current_position as f64 / total_size as f64) * tb) as i32;
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

// ── Platform workers ─────────────────────────────────────────────────────────

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::spawn_destructive_test;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::spawn_destructive_test;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn spawn_destructive_test(
    _device_path: String,
    _volumes: Vec<String>,
    _total_blocks: i32,
    _cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<DestructiveTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(DestructiveTestMsg::Error(
            "Destructive testing is not implemented on this platform.".to_string(),
        ));
        let _ = tx.send(DestructiveTestMsg::Completed);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_is_position_derived_and_deterministic() {
        let mut a = vec![0u8; 64];
        let mut b = vec![0u8; 64];
        fill_write_pattern(&mut a, 4096, 64);
        fill_write_pattern(&mut b, 4096, 64);
        assert_eq!(a, b);
        // First word at position 4096: 4096 ^ MAGIC.
        let w = u64::from_le_bytes(a[0..8].try_into().unwrap());
        assert_eq!(w, 4096u64 ^ 0xA5A5_A5A5_A5A5_A5A5);
        // Different position → different bytes.
        let mut c = vec![0u8; 64];
        fill_write_pattern(&mut c, 8192, 64);
        assert_ne!(a, c);
    }

    #[test]
    fn pattern_tail_shorter_than_a_word() {
        let mut buf = vec![0xFFu8; 12];
        fill_write_pattern(&mut buf, 0, 12);
        let w = 0xA5A5_A5A5_A5A5_A5A5_u64.to_le_bytes();
        assert_eq!(&buf[8..12], &(8u64 ^ 0xA5A5_A5A5_A5A5_A5A5).to_le_bytes()[..4]);
        assert_eq!(&buf[0..8], &w);
    }

    #[test]
    fn verify_counts_mismatched_sectors_only() {
        let written = vec![0xAA; 2048];
        let mut readback = written.clone();
        readback[512] ^= 0x01; // corrupt one byte in sector 1
        readback[1600] ^= 0x80; // and one in sector 3
        assert_eq!(count_mismatched_sectors(&written, &readback, 2048, 512), 2);
        assert_eq!(count_mismatched_sectors(&written, &written, 2048, 512), 0);
    }

    /// In-memory read/write device: verifies the full loop round-trips and
    /// that an unwritable region is counted at sector granularity.
    struct FakeRwDev {
        data: Vec<u8>,
        bad_from: i64,
        bad_to: i64,
    }

    impl crate::surface_test::BlockDev for FakeRwDev {
        fn read_at(&mut self, offset: i64, buf: &mut [u8]) -> Result<usize, ()> {
            let end = (offset as usize + buf.len()).min(self.data.len());
            let n = end.saturating_sub(offset as usize);
            buf[..n].copy_from_slice(&self.data[offset as usize..end]);
            Ok(n)
        }
    }

    impl RwBlockDev for FakeRwDev {
        fn write_at(&mut self, offset: i64, buf: &[u8]) -> Result<usize, ()> {
            let end = offset + buf.len() as i64;
            if end > self.bad_from && offset < self.bad_to {
                return Err(());
            }
            let end = (offset as usize + buf.len()).min(self.data.len());
            let n = end.saturating_sub(offset as usize);
            self.data[offset as usize..end].copy_from_slice(&buf[..n]);
            Ok(n)
        }
    }

    #[test]
    fn write_verify_roundtrip_counts_unwritable_sectors() {
        let sector = 512;
        let size = 4 * 1024 * 1024_i64;
        let bad_from = 1024 * 1024;
        let bad_to = bad_from + 8 * sector as i64; // 8 unwritable sectors
        let mut dev = FakeRwDev {
            data: vec![0u8; size as usize],
            bad_from,
            bad_to,
        };
        let cancel = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();

        perform_write_verify(&mut dev, size, sector, 50, CHUNK_SIZE, &cancel, &tx)
            .expect("write+verify");

        let mut last = None;
        while let Ok(DestructiveTestMsg::Progress(p)) = rx.try_recv() {
            last = Some(p);
        }
        let p = last.expect("progress");
        assert_eq!(p.bad_sectors, 8);
        assert_eq!(p.bytes_scanned, size);
        // Every written region actually carries the pattern.
        let mut expect = vec![0u8; 1024];
        fill_write_pattern(&mut expect, 0, 1024);
        assert_eq!(&dev.data[..1024], &expect[..]);
    }
}
