//! Linux destructive-test worker.
//!
//! Pre-flight mirrors the Windows lock+dismount semantics (D4 in the port
//! plan): every mounted filesystem on the target disk is unmounted with
//! `umount2(path, 0)` — deliberately *not* lazily, because `MNT_DETACH` hides
//! busy state and leaves I/O in flight — and a busy mount aborts the test
//! with a clear error, which inherently protects the OS disk. The whole-disk
//! node is then opened `O_RDWR|O_EXCL|O_DIRECT`; `O_EXCL` on a block device
//! fails with `EBUSY` if anything (a remount, another process) still holds
//! it, closing the desktop-automount race window as a second guard.

use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::surface_test::linux::{block_geometry, open_reason, FdDev};

use super::{perform_write_verify, DestructiveTestMsg, RwBlockDev, CHUNK_SIZE};

impl RwBlockDev for FdDev {
    fn write_at(&mut self, offset: i64, buf: &[u8]) -> Result<usize, ()> {
        let n = unsafe {
            libc::pwrite(
                self.0.as_raw_fd(),
                buf.as_ptr() as *const libc::c_void,
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

/// Spawn background destructive test. `volumes` holds the mount points of the
/// target disk's filesystems (empty strings for unmounted partitions); they
/// are unmounted before writing begins.
pub fn spawn_destructive_test(
    device_path: String,
    volumes: Vec<String>,
    total_blocks: i32,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<DestructiveTestMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        log::info!(
            target: "diskoria",
            "destructive worker: thread start path={} volumes={:?} total_blocks={}",
            device_path,
            volumes,
            total_blocks
        );
        if let Err(e) = run(&device_path, &volumes, total_blocks, &cancel, &tx) {
            log::warn!(target: "diskoria", "destructive worker: run returned Err: {e}");
            let _ = tx.send(DestructiveTestMsg::Error(e));
        } else {
            log::debug!(target: "diskoria", "destructive worker: run returned Ok");
        }
        log::debug!(target: "diskoria", "destructive worker: sending Completed");
        let _ = tx.send(DestructiveTestMsg::Completed);
    })
}

fn unmount(mount_point: &str) -> Result<(), String> {
    let c = std::ffi::CString::new(mount_point)
        .map_err(|_| format!("Invalid mount point {mount_point:?}."))?;
    // No MNT_DETACH: a lazy unmount would report success while I/O is still
    // in flight against sectors we are about to overwrite.
    let rc = unsafe { libc::umount2(c.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EBUSY) => Err(format!(
            "{mount_point} is busy — files on it are still in use. Close everything using this \
             disk (or unmount it manually) and try again."
        )),
        Some(libc::EINVAL) | Some(libc::ENOENT) => {
            // Not mounted (anymore) — fine.
            Ok(())
        }
        Some(libc::EPERM) => Err(format!(
            "Not permitted to unmount {mount_point}. Run Diskoria as root (pkexec)."
        )),
        _ => Err(format!("Could not unmount {mount_point}: {e}.")),
    }
}

fn run(
    device_path: &str,
    volumes: &[String],
    total_blocks: i32,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<DestructiveTestMsg>,
) -> Result<(), String> {
    // Step 1: unmount every mounted filesystem on the disk; abort on busy.
    for mp in volumes {
        let mp = mp.trim();
        if mp.is_empty() {
            continue;
        }
        log::info!(target: "diskoria", "destructive worker: unmounting {mp}");
        unmount(mp)?;
    }

    // Step 2: exclusive direct-I/O open — the second guard. Open immediately
    // after unmounting so a desktop automounter has no window to grab it.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_DIRECT | libc::O_EXCL)
        .open(device_path)
        .map_err(|e| {
            log::warn!(target: "diskoria", "destructive worker: open failed path={device_path} err={e}");
            if e.raw_os_error() == Some(libc::EBUSY) {
                format!(
                    "{device_path} is still in use (a filesystem re-mounted or another process \
                     holds it). Unmount everything on this disk and try again."
                )
            } else {
                open_reason(device_path, &e)
            }
        })?;

    // Step 3: geometry.
    let (total_size, bytes_per_sector) = block_geometry(&file)?;
    log::info!(
        target: "diskoria",
        "destructive worker: geometry disk_size={total_size} bytes_per_sector={bytes_per_sector}"
    );

    // Step 4: shared write+verify loop. The exclusive fd stays open for the
    // whole test, mirroring the held volume handles on Windows.
    let mut dev = FdDev(file);
    perform_write_verify(
        &mut dev,
        total_size,
        bytes_per_sector,
        total_blocks,
        CHUNK_SIZE,
        cancel,
        tx,
    )
}
