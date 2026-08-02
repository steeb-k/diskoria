//! File-based read/write benchmarks on a selected volume.

use crate::detected_drive::{BusKind, MediaKind};

/// Progress reported to the UI during a speed test run.
#[derive(Clone, Debug)]
pub struct SpeedTestProgress {
    pub current_operation: String,
    pub progress_percent: f64,
    pub current_speed_mbps: f64,
}

/// Final numbers in MB/s (negative in C# means N/A; we use `Option` or sentinel).
#[derive(Clone, Debug)]
pub struct SpeedTestResult {
    pub sequential_read_mbps: f64,
    pub sequential_write_mbps: f64,
    pub random_4k_read_mbps: f64,
    pub random_4k_write_mbps: f64,
    pub completed: bool,
    pub error_message: Option<String>,
}

pub enum SpeedTestMsg {
    Progress(SpeedTestProgress),
    Done(SpeedTestResult),
}

#[derive(Clone, Copy)]
pub struct SpeedProfile {
    pub test_file_size_mb: usize,
    pub random_4k_iterations: usize,
    pub test_passes: usize,
}

impl SpeedProfile {
    /// Matches C# `ConfigureForDriveType` for NVMe/SSD/HDD.
    fn fast() -> Self {
        Self {
            test_file_size_mb: 1024,
            random_4k_iterations: 4096,
            test_passes: 3,
        }
    }

    /// USB / eMMC / default.
    fn slow() -> Self {
        Self {
            test_file_size_mb: 256,
            random_4k_iterations: 1024,
            test_passes: 2,
        }
    }

    fn hdd() -> Self {
        Self {
            test_file_size_mb: 1024,
            random_4k_iterations: 2048,
            test_passes: 3,
        }
    }
}

/// Map Rust media/bus to UI storage-type buckets.
pub fn speed_profile_for_drive(media: MediaKind, bus: BusKind) -> SpeedProfile {
    match bus {
        BusKind::Nvme => SpeedProfile::fast(),
        BusKind::Usb => SpeedProfile::slow(),
        _ => match media {
            MediaKind::Hdd => SpeedProfile::hdd(),
            MediaKind::Ssd => SpeedProfile::fast(),
            MediaKind::EMmc | MediaKind::Flash | MediaKind::SdCard | MediaKind::Unknown => {
                SpeedProfile::slow()
            }
        },
    }
}

#[cfg(any(windows, target_os = "linux"))]
mod imp {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use rand::Rng;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{SpeedProfile, SpeedTestMsg, SpeedTestProgress, SpeedTestResult};

    const SEQ_BLOCK: usize = 1024 * 1024;
    const RAND_BLOCK: usize = 4096;
    const ALIGN: usize = 4096;

    fn report(
        ctx: &egui::Context,
        tx: &mpsc::Sender<SpeedTestMsg>,
        op: &str,
        pct: f64,
        speed: f64,
    ) -> Result<(), String> {
        let _ = tx.send(SpeedTestMsg::Progress(SpeedTestProgress {
            current_operation: op.to_string(),
            progress_percent: pct,
            current_speed_mbps: speed,
        }));
        ctx.request_repaint();
        Ok(())
    }

    /// `Vec::drain` moves bytes to the front of the allocation and restores an **unaligned**
    /// start address — use a fixed offset into reserved padding instead.
    struct AlignedBuf {
        storage: Vec<u8>,
        off: usize,
        len: usize,
    }

    impl AlignedBuf {
        fn new(len: usize) -> Self {
            let pad = len.saturating_add(ALIGN);
            let storage = vec![0u8; pad];
            let addr = storage.as_ptr() as usize;
            let off = (ALIGN - (addr % ALIGN)) % ALIGN;
            debug_assert!(off + len <= storage.len());
            debug_assert_eq!((addr.wrapping_add(off)) % ALIGN, 0);
            Self { storage, off, len }
        }

        fn as_mut_slice(&mut self) -> &mut [u8] {
            &mut self.storage[self.off..self.off + self.len]
        }

        fn as_slice(&self) -> &[u8] {
            &self.storage[self.off..self.off + self.len]
        }
    }

    /// The unbuffered/write-through open modes, per platform. Shared pass
    /// logic above calls these; block sizes and the AlignedBuf keep every
    /// access 4096-aligned, which O_DIRECT / FILE_FLAG_NO_BUFFERING require.
    #[cfg(windows)]
    mod sys {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use std::path::Path;

        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

        pub fn open_seq_write(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH | FILE_FLAG_SEQUENTIAL_SCAN)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_seq_read(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .share_mode(1 | 2)
                .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_rand_write(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_rand_read(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .share_mode(1 | 2)
                .custom_flags(FILE_FLAG_NO_BUFFERING)
                .open(path)
                .map_err(|e| e.to_string())
        }

        /// Best-effort free bytes on a volume root (e.g. `D:\`).
        pub fn available_space(root: &str) -> Result<i64, std::io::Error> {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

            let wide: Vec<u16> = OsStr::new(root)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut free_bytes: u64 = 0;
            let mut _total: u64 = 0;
            let mut _total_free: u64 = 0;
            let ok = unsafe {
                GetDiskFreeSpaceExW(
                    wide.as_ptr(),
                    &mut free_bytes,
                    &mut _total,
                    &mut _total_free,
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(free_bytes as i64)
        }
    }

    /// Linux flavour: `O_DIRECT` for unbuffered I/O, `O_SYNC` standing in for
    /// write-through; there is no sequential-scan hint worth mapping (the
    /// kernel readahead handles it).
    #[cfg(target_os = "linux")]
    mod sys {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        use std::path::Path;

        pub fn open_seq_write(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_DIRECT | libc::O_SYNC)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_seq_read(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECT)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_rand_write(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_DIRECT | libc::O_SYNC)
                .open(path)
                .map_err(|e| e.to_string())
        }

        pub fn open_rand_read(path: &Path) -> Result<std::fs::File, String> {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECT)
                .open(path)
                .map_err(|e| e.to_string())
        }

        /// Best-effort free bytes at a mount point.
        pub fn available_space(root: &str) -> Result<i64, std::io::Error> {
            let c = std::ffi::CString::new(root)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let frsize = if st.f_frsize > 0 { st.f_frsize } else { st.f_bsize } as i64;
            Ok(st.f_bavail as i64 * frsize)
        }
    }

    fn run_sequential_write_pass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        pass_number: usize,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        rand::thread_rng().fill(buf.as_mut_slice());

        let mut f = sys::open_seq_write(path)?;
        let sw = Instant::now();
        let mut written: u64 = 0;
        let blocks = profile.test_file_size_mb;

        for i in 0..blocks {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            f.write_all(buf.as_slice()).map_err(|e| e.to_string())?;
            written += SEQ_BLOCK as u64;

            if i % 32 == 0 {
                let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
                let cur = written as f64 / elapsed / (1024.0 * 1024.0);
                let pass_p = (i as f64) / (blocks as f64);
                let overall = ((pass_number as f64) + pass_p) / (profile.test_passes as f64) * 25.0;
                let _ = report(
                    ctx,
                    tx,
                    &format!(
                        "Sequential Write (Pass {}/{})",
                        pass_number + 1,
                        profile.test_passes
                    ),
                    overall,
                    cur,
                );
            }
        }
        f.sync_all().map_err(|e| e.to_string())?;
        let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
        Ok(written as f64 / elapsed / (1024.0 * 1024.0))
    }

    fn run_sequential_write_multipass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let mut speeds = Vec::with_capacity(profile.test_passes);
        for pass in 0..profile.test_passes {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let s = run_sequential_write_pass(ctx, path, profile, pass, cancel, tx)?;
            speeds.push(s);
            if pass + 1 < profile.test_passes {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(speeds.iter().sum::<f64>() / speeds.len() as f64)
    }

    fn run_sequential_read_pass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        pass_number: usize,
        file_size: u64,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        let mut f = sys::open_seq_read(path)?;
        let sw = Instant::now();
        let mut read_b: u64 = 0;

        while read_b < file_size {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let n = f.read(buf.as_mut_slice()).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            read_b += n as u64;

            if read_b.is_multiple_of(32 * SEQ_BLOCK as u64) || read_b == file_size {
                let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
                let cur = read_b as f64 / elapsed / (1024.0 * 1024.0);
                let pass_p = read_b as f64 / file_size as f64;
                let overall = 25.0
                    + ((pass_number as f64) + pass_p) / (profile.test_passes as f64) * 25.0;
                let _ = report(
                    ctx,
                    tx,
                    &format!(
                        "Sequential Read (Pass {}/{})",
                        pass_number + 1,
                        profile.test_passes
                    ),
                    overall,
                    cur,
                );
            }
        }
        let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
        Ok(read_b as f64 / elapsed / (1024.0 * 1024.0))
    }

    fn run_sequential_read_multipass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let file_size = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .len();
        let mut speeds = Vec::with_capacity(profile.test_passes);
        for pass in 0..profile.test_passes {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let s = run_sequential_read_pass(ctx, path, profile, pass, file_size, cancel, tx)?;
            speeds.push(s);
            if pass + 1 < profile.test_passes {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(speeds.iter().sum::<f64>() / speeds.len() as f64)
    }

    fn run_random_4k_write_pass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        pass_number: usize,
        file_size: u64,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let mut buf = AlignedBuf::new(RAND_BLOCK);
        rand::thread_rng().fill(buf.as_mut_slice());
        let mut rng = rand::thread_rng();
        let mut f = sys::open_rand_write(path)?;
        let max_blocks = file_size / RAND_BLOCK as u64;
        let sw = Instant::now();
        let mut written: u64 = 0;

        for i in 0..profile.random_4k_iterations {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let mb = max_blocks.saturating_sub(1).max(1);
            let block_idx = rng.gen_range(0..mb);
            let pos = block_idx * RAND_BLOCK as u64;
            f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
            f.write_all(buf.as_slice()).map_err(|e| e.to_string())?;
            written += RAND_BLOCK as u64;

            if i % 128 == 0 {
                let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
                let cur = written as f64 / elapsed / (1024.0 * 1024.0);
                let pass_p = i as f64 / profile.random_4k_iterations as f64;
                let overall = 50.0
                    + ((pass_number as f64) + pass_p) / (profile.test_passes as f64) * 25.0;
                let _ = report(
                    ctx,
                    tx,
                    &format!(
                        "Random 4K Write (Pass {}/{})",
                        pass_number + 1,
                        profile.test_passes
                    ),
                    overall,
                    cur,
                );
            }
        }
        f.sync_all().map_err(|e| e.to_string())?;
        let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
        Ok(written as f64 / elapsed / (1024.0 * 1024.0))
    }

    fn run_random_4k_write_multipass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let file_size = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .len();
        let mut speeds = Vec::with_capacity(profile.test_passes);
        for pass in 0..profile.test_passes {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let s = run_random_4k_write_pass(ctx, path, profile, pass, file_size, cancel, tx)?;
            speeds.push(s);
            if pass + 1 < profile.test_passes {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(speeds.iter().sum::<f64>() / speeds.len() as f64)
    }

    fn run_random_4k_read_pass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        pass_number: usize,
        file_size: u64,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let mut buf = AlignedBuf::new(RAND_BLOCK);
        let mut rng = rand::thread_rng();
        let mut f = sys::open_rand_read(path)?;
        let max_blocks = file_size / RAND_BLOCK as u64;
        let sw = Instant::now();
        let mut read_b: u64 = 0;

        for i in 0..profile.random_4k_iterations {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let mb = max_blocks.saturating_sub(1).max(1);
            let block_idx = rng.gen_range(0..mb);
            let pos = block_idx * RAND_BLOCK as u64;
            f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
            let n = f.read(buf.as_mut_slice()).map_err(|e| e.to_string())?;
            read_b += n as u64;

            if i % 128 == 0 {
                let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
                let cur = read_b as f64 / elapsed / (1024.0 * 1024.0);
                let pass_p = i as f64 / profile.random_4k_iterations as f64;
                let overall = 75.0
                    + ((pass_number as f64) + pass_p) / (profile.test_passes as f64) * 25.0;
                let _ = report(
                    ctx,
                    tx,
                    &format!(
                        "Random 4K Read (Pass {}/{})",
                        pass_number + 1,
                        profile.test_passes
                    ),
                    overall,
                    cur,
                );
            }
        }
        let elapsed = sw.elapsed().as_secs_f64().max(1e-9);
        Ok(read_b as f64 / elapsed / (1024.0 * 1024.0))
    }

    fn run_random_4k_read_multipass(
        ctx: &egui::Context,
        path: &Path,
        profile: &SpeedProfile,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
    ) -> Result<f64, String> {
        let file_size = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .len();
        let mut speeds = Vec::with_capacity(profile.test_passes);
        for pass in 0..profile.test_passes {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }
            let s = run_random_4k_read_pass(ctx, path, profile, pass, file_size, cancel, tx)?;
            speeds.push(s);
            if pass + 1 < profile.test_passes {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(speeds.iter().sum::<f64>() / speeds.len() as f64)
    }

    pub fn run_speed_test(
        test_file_path: String,
        profile: SpeedProfile,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<SpeedTestMsg>,
        ctx: &egui::Context,
    ) -> SpeedTestResult {
        let path = Path::new(&test_file_path);
        let drive_root = path
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty())
            .unwrap_or(if cfg!(windows) { "C:\\" } else { "/" });
        let required = (profile.test_file_size_mb as i64) * 1024 * 1024 * 2;

        if std::fs::metadata(drive_root).is_ok() {
            if let Ok(free) = sys::available_space(drive_root) {
                if free < required {
                    return SpeedTestResult {
                        sequential_read_mbps: -1.0,
                        sequential_write_mbps: -1.0,
                        random_4k_read_mbps: -1.0,
                        random_4k_write_mbps: -1.0,
                        completed: false,
                        error_message: Some(format!(
                            "Not enough free space. Need at least {} MB free.",
                            profile.test_file_size_mb * 2
                        )),
                    };
                }
            }
        }

        let mut seq_r = -1.0_f64;
        let mut seq_w = -1.0_f64;
        let mut r4_r = -1.0_f64;
        let mut r4_w = -1.0_f64;

        let run = (|| -> Result<(), String> {
            let _ = report(ctx, tx, "Sequential Write", 0.0, 0.0);
            seq_w = run_sequential_write_multipass(ctx, path, &profile, cancel, tx)?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }

            let _ = report(ctx, tx, "Sequential Read", 25.0, 0.0);
            seq_r = run_sequential_read_multipass(ctx, path, &profile, cancel, tx)?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }

            let _ = report(ctx, tx, "Random 4K Write", 50.0, 0.0);
            r4_w = run_random_4k_write_multipass(ctx, path, &profile, cancel, tx)?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".to_string());
            }

            let _ = report(ctx, tx, "Random 4K Read", 75.0, 0.0);
            r4_r = run_random_4k_read_multipass(ctx, path, &profile, cancel, tx)?;

            let _ = report(ctx, tx, "Complete", 100.0, 0.0);
            Ok(())
        })();

        let _ = std::fs::remove_file(path);

        match run {
            Ok(()) => SpeedTestResult {
                sequential_read_mbps: seq_r,
                sequential_write_mbps: seq_w,
                random_4k_read_mbps: r4_r,
                random_4k_write_mbps: r4_w,
                completed: true,
                error_message: None,
            },
            Err(e) if e == "Cancelled" => SpeedTestResult {
                sequential_read_mbps: seq_r,
                sequential_write_mbps: seq_w,
                random_4k_read_mbps: r4_r,
                random_4k_write_mbps: r4_w,
                completed: false,
                error_message: None,
            },
            Err(e) => SpeedTestResult {
                sequential_read_mbps: seq_r,
                sequential_write_mbps: seq_w,
                random_4k_read_mbps: r4_r,
                random_4k_write_mbps: r4_w,
                completed: false,
                error_message: Some(e),
            },
        }
    }



    pub fn spawn_speed_test(
        test_file_path: String,
        profile: SpeedProfile,
        cancel: Arc<AtomicBool>,
        tx: mpsc::Sender<SpeedTestMsg>,
        ctx: egui::Context,
    ) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let res = run_speed_test(test_file_path, profile, &cancel, &tx, &ctx);
            let _ = tx.send(SpeedTestMsg::Done(res));
        })
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub use imp::spawn_speed_test;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn spawn_speed_test(
    _test_file_path: String,
    _profile: SpeedProfile,
    _cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: std::sync::mpsc::Sender<SpeedTestMsg>,
    _ctx: egui::Context,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(SpeedTestMsg::Done(SpeedTestResult {
            sequential_read_mbps: -1.0,
            sequential_write_mbps: -1.0,
            random_4k_read_mbps: -1.0,
            random_4k_write_mbps: -1.0,
            completed: false,
            error_message: Some("Speed test is not implemented on this platform.".to_string()),
        }));
    })
}

/// Benchmark temp-file path builders. Both flavours are compiled (and unit
/// tested) on every platform; `DiskoriaApp::speed_test_temp_path` picks the
/// host's at compile time. `mount` accepts the platform's raw mount value:
/// a drive letter in the `C`/`C:`/`C:\` spellings, or a mount-point path.
#[cfg_attr(not(windows), allow(dead_code))] // host dispatch picks one flavour; both stay tested
pub(crate) fn temp_path_windows(mount: &str, id: u128) -> String {
    let clean = mount.trim_end_matches('\\');
    let root = if clean.ends_with(':') {
        format!("{clean}\\")
    } else {
        format!("{}:\\", clean.trim_end_matches(':'))
    };
    format!("{root}Diskoria_SpeedTest_{id}.tmp")
}

#[cfg_attr(windows, allow(dead_code))] // host dispatch picks one flavour; both stay tested
pub(crate) fn temp_path_unix(mount: &str, id: u128) -> String {
    let root = mount.trim_end_matches('/');
    format!("{root}/Diskoria_SpeedTest_{id}.tmp")
}

#[cfg(test)]
mod temp_path_tests {
    use super::{temp_path_unix, temp_path_windows};

    #[test]
    fn windows_flavours_normalize_to_letter_root() {
        assert_eq!(temp_path_windows("C", 7), r"C:\Diskoria_SpeedTest_7.tmp");
        assert_eq!(temp_path_windows("C:", 7), r"C:\Diskoria_SpeedTest_7.tmp");
        assert_eq!(temp_path_windows(r"C:\", 7), r"C:\Diskoria_SpeedTest_7.tmp");
    }

    #[test]
    fn unix_mounts_including_root() {
        assert_eq!(temp_path_unix("/", 7), "/Diskoria_SpeedTest_7.tmp");
        assert_eq!(temp_path_unix("/home", 7), "/home/Diskoria_SpeedTest_7.tmp");
        assert_eq!(temp_path_unix("/mnt/usb/", 7), "/mnt/usb/Diskoria_SpeedTest_7.tmp");
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod hardware_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{mpsc, Arc};

    /// Real direct-I/O benchmark against the home filesystem with a tiny
    /// profile. Not CI: does ~100 MB of writes. tmpfs would reject O_DIRECT,
    /// so the file goes next to the user's data, like a real run.
    #[test]
    #[ignore = "does real disk I/O; run manually with --ignored"]
    fn small_speed_run_on_home_fs() {
        let base = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect("home dir");
        let path = format!("{base}/Diskoria_SpeedTest_selftest.tmp");
        let profile = SpeedProfile {
            test_file_size_mb: 32,
            random_4k_iterations: 256,
            test_passes: 1,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        spawn_speed_test(path.clone(), profile, cancel, tx, ctx)
            .join()
            .expect("worker join");

        let mut done = None;
        while let Ok(m) = rx.try_recv() {
            if let SpeedTestMsg::Done(r) = m {
                done = Some(r);
            }
        }
        let r = done.expect("Done message");
        assert!(r.completed, "error: {:?}", r.error_message);
        assert!(r.sequential_write_mbps > 0.0);
        assert!(r.sequential_read_mbps > 0.0);
        assert!(r.random_4k_write_mbps > 0.0);
        assert!(r.random_4k_read_mbps > 0.0);
        assert!(!std::path::Path::new(&path).exists(), "temp file cleaned up");
        println!(
            "seq w/r = {:.0}/{:.0} MB/s, 4k w/r = {:.1}/{:.1} MB/s",
            r.sequential_write_mbps, r.sequential_read_mbps,
            r.random_4k_write_mbps, r.random_4k_read_mbps
        );
    }
}
