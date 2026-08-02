//! Unix single-instance guard (the Windows counterpart — a named mutex plus
//! two named events and a shared-memory visibility bit — lives in `lib.rs`).
//!
//! A unix-domain socket in `$XDG_RUNTIME_DIR` doubles as the lock and the
//! activation channel: whoever binds it is the primary; a second launch
//! connects, writes one line, and exits. The primary's listener thread turns
//! every connection into [`crate::UserEvent::SecondLaunch`], and the event
//! handler decides raise-vs-new-window from its own renderer state — unlike
//! Windows there is no cross-process visibility flag to race against (KI-6).
//!
//! When the app later relaunches itself elevated via pkexec, the invoking
//! user's `XDG_RUNTIME_DIR` is passed through, so unelevated second launches
//! still reach the elevated primary's socket.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("diskoria.sock");
        }
    }
    // No runtime dir (rare outside a session): fall back to a per-uid name in
    // /tmp so two users' instances can't collide.
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir().join(format!("diskoria-{uid}.sock"))
}

/// The bound listener of the primary instance, not yet serving.
pub struct Acquired {
    listener: UnixListener,
    path: PathBuf,
}

/// Removes the socket file on clean shutdown; a crashed process leaves a stale
/// file, which the next `acquire` detects (connect fails) and unlinks.
pub struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Claim the single-instance socket or hand off to the running primary.
///
/// **Exits the process** (status 0) when a primary already exists: silently
/// for `--minimized` auto-start launches (so the logon task never steals the
/// tray-only state), otherwise after asking the primary to activate. Returns
/// `None` if the socket can't be bound at all — the app then just runs
/// unguarded, which beats refusing to start.
pub fn acquire(start_minimized: bool) -> Option<Acquired> {
    let path = socket_path();

    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            if start_minimized {
                log::info!(target: "diskoria", "single-instance: primary running, --minimized launch exits silently");
                std::process::exit(0);
            }
            log::info!(target: "diskoria", "single-instance: primary running, requesting activation");
            let _ = stream.write_all(b"ACTIVATE\n");
            let _ = stream.flush();
            std::process::exit(0);
        }
        Err(_) => {
            // No live primary: either the file doesn't exist or it's stale
            // (left by a crash). Unlink and claim it.
            let _ = std::fs::remove_file(&path);
            match UnixListener::bind(&path) {
                Ok(listener) => Some(Acquired { listener, path }),
                Err(e) => {
                    log::warn!(
                        target: "diskoria",
                        "single-instance: cannot bind {}: {e}; running unguarded",
                        path.display()
                    );
                    None
                }
            }
        }
    }
}

impl Acquired {
    /// Give the socket back (listener dropped, file unlinked) without serving.
    /// Used just before the pkexec self-relaunch: the elevated child must be
    /// able to bind the same path.
    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Start serving activation requests, forwarding each connection to the
    /// event loop as `UserEvent::SecondLaunch`.
    pub fn spawn(self, proxy: EventLoopProxy<UserEvent>) -> Guard {
        let Acquired { listener, path } = self;
        std::thread::Builder::new()
            .name("diskoria-single-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    // Drain the (tiny) request; content is currently
                    // informational — any connection means "activate".
                    let mut buf = [0u8; 32];
                    let _ = stream.read(&mut buf);
                    if proxy.send_event(UserEvent::SecondLaunch).is_err() {
                        // Event loop is gone; stop serving.
                        break;
                    }
                }
            })
            .expect("spawn single-instance listener");
        Guard { path }
    }
}
