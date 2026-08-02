//! Boot-smoke test: launches the real Diskoria binary in `DISKORIA_SMOKE` mode,
//! which renders a few frames through the full `draw()` path and then exits 0.
//! Catches startup/wiring regressions (panics, missing fonts/textures, broken
//! page dispatch) without needing real disks or admin rights.
//!
//! Ignored by default: it opens a real window and therefore needs an
//! interactive desktop session — it will not run headless / in CI.
//! Run it explicitly with:
//!
//! ```text
//! cargo test --test boot_smoke -- --ignored
//! ```

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
#[ignore = "opens a real window; needs an interactive desktop session"]
fn boots_and_exits_cleanly() {
    let exe = env!("CARGO_BIN_EXE_diskoria");
    let tmp = tempfile::tempdir().expect("create temp data dir");

    let mut cmd = Command::new(exe);
    cmd.env("DISKORIA_SMOKE", "3").env("RUST_LOG", "diskoria=info");
    // Keep the run hermetic: don't touch the real history.db / settings.txt.
    // paths::data_dir() reads %PROGRAMDATA% on Windows and $XDG_DATA_HOME
    // (falling back to $HOME) elsewhere.
    if cfg!(windows) {
        cmd.env("PROGRAMDATA", tmp.path());
    } else {
        cmd.env("XDG_DATA_HOME", tmp.path()).env("HOME", tmp.path());
    }
    let mut child = cmd.spawn().expect("spawn diskoria");

    let timeout = Duration::from_secs(30);
    let start = Instant::now();
    loop {
        match child.try_wait().expect("poll diskoria") {
            Some(status) => {
                assert!(status.success(), "diskoria smoke exited with {status:?}");
                return;
            }
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                panic!("diskoria smoke did not exit within {timeout:?}");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
