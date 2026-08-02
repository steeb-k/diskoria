//! Linux surface-test worker: opens `/dev/…` with `O_DIRECT`, queries size and
//! logical sector via ioctls, and feeds the shared scan loop with pread.

use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use super::{perform_full_sequential_read, BlockDev, SurfaceTestMsg, CHUNK_SIZE};

pub(crate) const BLKGETSIZE64: libc::c_ulong = 0x8008_1272; // _IOR(0x12, 114, size_t)
pub(crate) const BLKSSZGET: libc::c_ulong = 0x1268; // _IO(0x12, 104)

/// Positioned reads via `pread(2)` on an `O_DIRECT` fd. The buffer, length and
/// offset are all sector-aligned by the shared loop, which `O_DIRECT` demands.
pub(crate) struct FdDev(pub std::fs::File);

impl BlockDev for FdDev {
    fn read_at(&mut self, offset: i64, buf: &mut [u8]) -> Result<usize, ()> {
        let n = unsafe {
            libc::pread(
                self.0.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                offset,
            )
        };
        if n < 0 {
            Err(())
        } else {
            Ok(n as usize)
        }
    }
}

/// (total bytes, logical sector size) for an open block device.
pub(crate) fn block_geometry(file: &std::fs::File) -> Result<(i64, i32), String> {
    let fd = file.as_raw_fd();
    let mut size: u64 = 0;
    if unsafe { libc::ioctl(fd, BLKGETSIZE64, &mut size) } != 0 {
        return Err(format!(
            "Cannot read device size: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut ssz: libc::c_int = 0;
    if unsafe { libc::ioctl(fd, BLKSSZGET, &mut ssz) } != 0 {
        return Err(format!(
            "Cannot read logical sector size: {}",
            std::io::Error::last_os_error()
        ));
    }
    if size == 0 || ssz <= 0 {
        return Err("Device reports zero size or sector size.".to_string());
    }
    Ok((size as i64, ssz))
}

pub(crate) fn open_reason(device_path: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("Cannot open {device_path}: permission denied. Run Diskoria as root (pkexec).")
    } else {
        format!("Cannot open {device_path}: {e}.")
    }
}

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
        if let Err(e) = run(&device_path, total_blocks, &cancel, &tx) {
            log::warn!(target: "diskoria", "surface worker: run returned Err, sending Error to UI");
            let _ = tx.send(SurfaceTestMsg::Error(e));
        }
        log::debug!(target: "diskoria", "surface worker: sending Completed");
        let _ = tx.send(SurfaceTestMsg::Completed);
    })
}

fn run(
    device_path: &str,
    total_blocks: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<SurfaceTestMsg>,
) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(device_path)
        .map_err(|e| {
            log::warn!(target: "diskoria", "surface worker: open failed path={device_path} err={e}");
            open_reason(device_path, &e)
        })?;

    let (total_size, bytes_per_sector) = block_geometry(&file)?;
    log::info!(
        target: "diskoria",
        "surface worker: geometry disk_size={total_size} bytes_per_sector={bytes_per_sector}"
    );

    let mut dev = FdDev(file);
    perform_full_sequential_read(
        &mut dev,
        total_size,
        bytes_per_sector,
        total_blocks,
        CHUNK_SIZE,
        cancel,
        tx,
    )
}
