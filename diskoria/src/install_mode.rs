//! Installed-build vs. portable-exe detection.
//!
//! Diskoria ships two ways: an Inno Setup installer and a bare portable
//! `diskoria.exe`. A few behaviours should differ between them — most notably
//! the default for "Close to system tray" (on when installed, off when
//! portable) — and the About page shows which build is running.
//!
//! Detection mirrors the [`crate::autostart`] philosophy: *OS state is the
//! source of truth*, not a persisted setting that could drift. The installer
//! writes
//!
//! ```text
//! HKLM\Software\Diskoria\InstallDir = <install directory>
//! ```
//!
//! (the *value* is removed again on uninstall — the enclosing key is shared with
//! other Diskoria settings such as `AutoCheckUpdates`, so it is never deleted
//! wholesale; see the `[Registry]` comment in the .iss). A build is
//! [`InstallMode::Installed`] only when that value exists **and** names the
//! directory the running exe actually lives in. The directory check matters:
//! copying `diskoria.exe` out of `C:\Program Files\Diskoria` onto a USB stick
//! must report Portable even though the machine still has Diskoria installed.
//!
//! Everything non-Windows is Portable (the non-Windows build is a compile-only
//! shell).

use std::sync::OnceLock;

/// Which of the two distribution shapes this process is running as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallMode {
    /// Running from the directory the Inno Setup installer wrote.
    Installed,
    /// A standalone exe run from anywhere else.
    Portable,
}

impl InstallMode {
    /// Short user-facing label for the About page.
    pub fn label(self) -> &'static str {
        match self {
            InstallMode::Installed => "Installed",
            InstallMode::Portable => "Portable",
        }
    }

    pub fn is_installed(self) -> bool {
        matches!(self, InstallMode::Installed)
    }
}

/// Registry key the installer writes. Must match the `[Registry]` section of
/// `installer/diskoria.iss`.
#[cfg(windows)]
const REG_SUBKEY: &str = r"Software\Diskoria";
#[cfg(windows)]
const REG_VALUE: &str = "InstallDir";

static MODE: OnceLock<InstallMode> = OnceLock::new();

/// The mode of the running process. The (cheap) registry probe happens once.
pub fn current() -> InstallMode {
    *MODE.get_or_init(detect)
}

/// Default for the "Close to system tray" setting when the user has never
/// chosen one: installed builds live in the tray, portable exes quit on close.
pub fn default_close_to_tray() -> bool {
    current().is_installed()
}

#[cfg(windows)]
fn detect() -> InstallMode {
    let Some(install_dir) = read_install_dir() else {
        return InstallMode::Portable;
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()));
    match exe_dir {
        Some(dir) if same_dir(&install_dir, &dir) => InstallMode::Installed,
        _ => InstallMode::Portable,
    }
}

#[cfg(not(windows))]
fn detect() -> InstallMode {
    InstallMode::Portable
}

/// Read `HKLM\Software\Diskoria\InstallDir`, or `None` when absent/unreadable
/// (i.e. Diskoria was never installed on this machine).
#[cfg(windows)]
fn read_install_dir() -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    let wide = |s: &str| -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    };
    let subkey = wide(REG_SUBKEY);
    let value = wide(REG_VALUE);

    unsafe {
        // First call sizes the buffer (in *bytes*), second fills it.
        let mut cb: u32 = 0;
        let rc = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut cb,
        );
        if rc != ERROR_SUCCESS || cb == 0 {
            return None;
        }
        // +1 wchar of slack: RegGetValueW guarantees NUL termination but can
        // report an odd byte count for a malformed value.
        let mut buf = vec![0u16; (cb as usize).div_ceil(2) + 1];
        let mut cb2: u32 = (buf.len() * 2) as u32;
        let rc = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut cb2,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// Whether two directory strings name the same directory, ignoring case,
/// separator flavour and a trailing separator. Pure so it can be unit-tested
/// without touching the registry. An empty/blank side never matches.
fn same_dir(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.trim()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dir_ignores_case_slashes_and_trailing_separator() {
        assert!(same_dir(
            r"C:\Program Files\Diskoria",
            r"c:\program files\diskoria"
        ));
        assert!(same_dir(r"C:\Program Files\Diskoria\", r"C:\Program Files\Diskoria"));
        assert!(same_dir("C:/Program Files/Diskoria", r"C:\Program Files\Diskoria"));
        assert!(same_dir(r"  C:\Diskoria  ", r"C:\Diskoria"));
    }

    #[test]
    fn same_dir_rejects_different_or_empty_dirs() {
        // A copy of the exe living next to the install dir is still portable.
        assert!(!same_dir(r"C:\Program Files\Diskoria", r"C:\Program Files\Diskoria2"));
        assert!(!same_dir(r"C:\Program Files\Diskoria", r"D:\Tools"));
        // A blank registry value must never be read as "installed here".
        assert!(!same_dir("", ""));
        assert!(!same_dir("   ", r"C:\Diskoria"));
    }

    #[test]
    fn labels_are_distinct() {
        assert_eq!(InstallMode::Installed.label(), "Installed");
        assert_eq!(InstallMode::Portable.label(), "Portable");
        assert!(InstallMode::Installed.is_installed());
        assert!(!InstallMode::Portable.is_installed());
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_is_always_portable() {
        assert_eq!(current(), InstallMode::Portable);
        assert!(!default_close_to_tray());
    }
}
