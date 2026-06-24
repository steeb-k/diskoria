//! Centralized on-disk locations for Diskoria's persistent data.
//!
//! Single seam for per-OS paths so a future Linux port only changes this file
//! (see `docs/refactor-roadmap.md`). On Windows everything lives under
//! `%PROGRAMDATA%\Diskoria`; other platforms get a best-effort XDG-style
//! fallback (the non-Windows build is currently a compile-only shell).

use std::path::PathBuf;

/// Base directory for Diskoria's persistent data files.
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        PathBuf::from(base).join("Diskoria")
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var("XDG_DATA_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("diskoria")
    }
}

/// Theme / accent / monitoring preferences file.
pub fn settings_file() -> PathBuf {
    data_dir().join("settings.txt")
}

/// Pro-Monitoring health-history SQLite database.
pub fn history_db_file() -> PathBuf {
    data_dir().join("history.db")
}
