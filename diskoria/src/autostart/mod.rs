//! Launch-at-startup.
//!
//! Windows: an elevated Scheduled Task (`windows.rs` — a Run key would UAC-
//! prompt every logon). Linux: an XDG autostart `.desktop` entry
//! (`linux.rs`). On both, the OS artifact's *existence* is the single source
//! of truth for the toggle — no persisted setting to drift out of sync.
//! Autostart launches pass `--minimized` and never trigger the pkexec
//! relaunch: the tray-only session runs unelevated with degraded monitoring
//! until the user opens a window.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{is_enabled, set_enabled};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{is_enabled, set_enabled};
