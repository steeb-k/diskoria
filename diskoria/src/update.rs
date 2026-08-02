//! GitHub Releases update check and Windows self-update (download → exit → replace exe → relaunch).
//! Release assets are published on the **`diskoria-binaries`** repo (see [`crate::github_config`]).

// Every caller of this module is `#[cfg(windows)]` today, so the whole feature
// is dead code on Linux until the linux-support update phase un-gates it.
#![cfg_attr(not(windows), allow(dead_code))]

use semver::Version;
use serde::Deserialize;

use crate::github_config;

#[derive(Debug, Deserialize)]
struct ReleaseJson {
    tag_name: String,
    assets: Vec<AssetJson>,
}

#[derive(Debug, Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

pub enum UpdateCheckResult {
    UpToDate,
    UpdateAvailable {
        version_display: String,
        download_url: String,
    },
}

fn parse_version_tag(tag: &str) -> Option<Version> {
    let s = tag.trim().trim_start_matches('v').trim_start_matches('V');
    Version::parse(s).ok()
}

/// Prefer installer `.exe` with "setup" in the name, then any non-PDB `.exe`.
fn pick_exe_url(assets: &[AssetJson]) -> Option<String> {
    let setup: Vec<_> = assets
        .iter()
        .filter(|a| {
            let n = a.name.to_ascii_lowercase();
            n.ends_with(".exe") && !n.contains("pdb") && n.contains("setup")
        })
        .map(|a| a.browser_download_url.clone())
        .collect();
    if let Some(u) = setup.into_iter().next() {
        return Some(u);
    }
    assets
        .iter()
        .filter(|a| {
            let n = a.name.to_ascii_lowercase();
            n.ends_with(".exe") && !n.contains("pdb")
        })
        .map(|a| a.browser_download_url.clone())
        .next()
}

pub fn check_for_update_blocking() -> Result<UpdateCheckResult, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("Invalid app version: {e}"))?;

    let api_url = github_config::api_latest_releases_url();
    let resp = ureq::get(&api_url)
        .set(
            "User-Agent",
            &format!("Diskoria/{}", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Network error: {e}"))?;

    if resp.status() == 404 {
        return Ok(UpdateCheckResult::UpToDate);
    }
    if resp.status() != 200 {
        return Err(format!("GitHub returned HTTP {}", resp.status()));
    }

    let release: ReleaseJson = resp
        .into_json()
        .map_err(|e| format!("Unexpected response: {e}"))?;

    let remote = parse_version_tag(&release.tag_name)
        .ok_or_else(|| format!("Unrecognized release tag: {}", release.tag_name))?;

    if remote <= current {
        return Ok(UpdateCheckResult::UpToDate);
    }

    let url = pick_exe_url(&release.assets)
        .ok_or_else(|| "No Windows .exe found in this release.".to_string())?;

    Ok(UpdateCheckResult::UpdateAvailable {
        version_display: release.tag_name.trim_start_matches('v').trim_start_matches('V').to_string(),
        download_url: url,
    })
}

/// Whether a release asset URL points at the Inno installer rather than a bare
/// portable exe. Decided from the asset name in the URL.
pub fn url_is_installer(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .contains("setup")
}

/// Temp filename for a downloaded update.
///
/// The apply step ([`crate::app::DiskoriaApp::poll_update_download`]) decides
/// between "run this installer" and "copy this exe over ourselves" purely from
/// the downloaded file's *name*, so the `setup` marker has to survive the
/// download. It previously did not — every update was saved as
/// `Diskoria_update_<n>.exe`, so an installer asset took the copy-over branch
/// and clobbered `diskoria.exe` with the Inno setup stub (known-issues KI-22).
pub fn update_temp_file_name(url: &str, nonce: u128) -> String {
    if url_is_installer(url) {
        format!("Diskoria_update_setup_{nonce}.exe")
    } else {
        format!("Diskoria_update_{nonce}.exe")
    }
}

pub fn download_to_path(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            &format!("Diskoria/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;

    if resp.status() != 200 {
        return Err(format!("Download returned HTTP {}", resp.status()));
    }

    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp.into_reader(), &mut file).map_err(|e| e.to_string())?;
    Ok(())
}

/// Writes a temp `.bat` that waits for this process to exit, replaces `target_exe`, then launches it.
#[cfg(windows)]
pub fn spawn_apply_update_and_exit(new_exe: &std::path::Path, target_exe: &std::path::Path) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let new_s = new_exe.to_string_lossy().replace('/', "\\");
    let tgt_s = target_exe.to_string_lossy().replace('/', "\\");
    let batch = std::env::temp_dir().join("diskoria_apply_update.bat");

    let script = format!(
        r#"@echo off
setlocal
set "NEW={new}"
set "TARGET={tgt}"
set /a n=0
:wait
set /a n+=1
if %n% GTR 90 goto fail
timeout /t 1 /nobreak >nul
del /f /q "%TARGET%" 2>nul
if exist "%TARGET%" goto wait
copy /y "%NEW%" "%TARGET%"
if errorlevel 1 goto fail
start "" "%TARGET%"
del /f /q "%NEW%" 2>nul
del "%~f0"
exit /b 0
:fail
echo Failed to install update. > "%TEMP%\diskoria_update_error.txt"
echo You can close this window. >> "%TEMP%\diskoria_update_error.txt"
pause
del "%~f0"
exit /b 1
"#,
        new = new_s,
        tgt = tgt_s,
    );

    if std::fs::write(&batch, &script).is_err() {
        log::warn!("diskoria: failed to write apply_update.bat");
        return;
    }

    let batch_display = batch.to_string_lossy().into_owned();
    let r = std::process::Command::new("cmd.exe")
        .args(["/C", &batch_display])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    if r.is_err() {
        log::warn!("diskoria: failed to spawn apply_update.bat");
        return;
    }

    std::thread::sleep(std::time::Duration::from_millis(150));
    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn spawn_apply_update_and_exit(_new_exe: &std::path::Path, _target_exe: &std::path::Path) {}

/// Inno Setup switches for an unattended update install.
///
/// * `/SILENT` — progress window only: no wizard pages and nothing to click.
///   (`/VERYSILENT` would hide even that, which reads as a crash when the app
///   disappears for several seconds mid-update.)
/// * `/MERGETASKS` — carries the *current* selections forward. A silent install
///   otherwise falls back to the `[Tasks]` defaults, and both are checked by
///   default, so every update would re-create a startup task or desktop icon the
///   user had deliberately removed.
/// * `/RELAUNCH` — a custom parameter read by the `[Run]` entry's
///   `RelaunchAfterSilent` check. True when the update is applied *mid-session*
///   (the user is sitting in front of the app and expects it back); false from
///   the exit hook, where the user was closing Diskoria and reopening it would
///   be the last thing they wanted.
///
/// `keep_*` are read from live state rather than a stored preference for the
/// same reason `autostart` and `install_mode` are: the OS is the source of truth.
pub fn silent_install_args(keep_startup: bool, keep_desktop_icon: bool, relaunch: bool) -> Vec<String> {
    let tasks = format!(
        "{},{}",
        if keep_startup { "startup" } else { "!startup" },
        if keep_desktop_icon { "desktopicon" } else { "!desktopicon" },
    );
    vec![
        "/SILENT".to_string(),
        "/SUPPRESSMSGBOXES".to_string(),
        "/NORESTART".to_string(),
        format!("/MERGETASKS={tasks}"),
        format!("/RELAUNCH={}", if relaunch { 1 } else { 0 }),
    ]
}

/// Whether the all-users desktop shortcut the installer's `desktopicon` task
/// creates is currently present.
#[cfg(windows)]
fn desktop_icon_present() -> bool {
    std::env::var_os("PUBLIC")
        .map(|p| {
            std::path::Path::new(&p)
                .join("Desktop")
                .join("Diskoria.lnk")
                .exists()
        })
        .unwrap_or(false)
}

/// Launch a downloaded installer without exiting. Used from the exit hook,
/// where the process is already on its way down.
#[cfg(windows)]
pub fn spawn_installer(installer: &std::path::Path, relaunch: bool) {
    let args = silent_install_args(
        crate::autostart::is_enabled(),
        desktop_icon_present(),
        relaunch,
    );
    log::info!(target: "diskoria", "running installer silently: {}", args.join(" "));
    if let Err(e) = std::process::Command::new(installer).args(&args).spawn() {
        log::warn!(target: "diskoria", "failed to launch staged installer: {e}");
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn spawn_installer(_installer: &std::path::Path, _relaunch: bool) {}

/// Run a downloaded installer (e.g. Inno `*Setup*.exe`) and exit so the file is
/// unlocked. Mid-session path, so the installer relaunches Diskoria afterwards.
#[cfg(windows)]
pub fn spawn_run_installer_and_exit(installer: &std::path::Path) {
    spawn_installer(installer, true);
    std::thread::sleep(std::time::Duration::from_millis(150));
    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn spawn_run_installer_and_exit(_installer: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_tags_parse_with_and_without_v() {
        assert_eq!(parse_version_tag("v1.6.1"), Some(Version::parse("1.6.1").unwrap()));
        assert_eq!(parse_version_tag("1.6.1"), Some(Version::parse("1.6.1").unwrap()));
        assert_eq!(parse_version_tag("nightly"), None);
    }

    #[test]
    fn installer_urls_are_recognized() {
        assert!(url_is_installer(
            "https://github.com/x/y/releases/download/v1.6.1/diskoria-1.6.1-setup.exe"
        ));
        assert!(url_is_installer("https://x/DISKORIA-SETUP.EXE"));
        assert!(!url_is_installer(
            "https://github.com/x/y/releases/download/v1.6.1/diskoria.exe"
        ));
        // "setup" appearing only in the path, not the asset name, must not count.
        assert!(!url_is_installer("https://x/setup/releases/diskoria.exe"));
    }

    #[test]
    fn temp_name_preserves_the_setup_marker() {
        // The regression behind KI-22: the apply step matches on `contains("setup")`,
        // so an installer download must still carry the marker after renaming.
        let installer = update_temp_file_name("https://x/diskoria-1.6.1-setup.exe", 42);
        assert!(installer.to_ascii_lowercase().contains("setup"));
        assert!(installer.ends_with(".exe"));

        let portable = update_temp_file_name("https://x/diskoria.exe", 42);
        assert!(!portable.to_ascii_lowercase().contains("setup"));
        assert!(portable.ends_with(".exe"));

        // Nonce keeps concurrent downloads from colliding.
        assert_ne!(
            update_temp_file_name("https://x/diskoria.exe", 1),
            update_temp_file_name("https://x/diskoria.exe", 2)
        );
    }

    #[test]
    fn silent_args_carry_task_state_and_relaunch_forward() {
        let on = silent_install_args(true, true, true);
        assert!(on.iter().any(|a| a == "/SILENT"));
        assert!(on.iter().any(|a| a == "/SUPPRESSMSGBOXES"));
        assert!(on.iter().any(|a| a == "/MERGETASKS=startup,desktopicon"));
        assert!(on.iter().any(|a| a == "/RELAUNCH=1"));

        // The point of passing these explicitly: a silent install falls back to
        // the [Tasks] defaults (both checked), which would re-create a startup
        // task or desktop icon the user had removed.
        let off = silent_install_args(false, false, false);
        assert!(off.iter().any(|a| a == "/MERGETASKS=!startup,!desktopicon"));
        assert!(off.iter().any(|a| a == "/RELAUNCH=0"));

        // Applying on exit must not bring the app back up.
        assert!(silent_install_args(true, true, false)
            .iter()
            .any(|a| a == "/RELAUNCH=0"));
    }

    #[test]
    fn pick_exe_url_prefers_the_installer() {
        let assets = vec![
            AssetJson {
                name: "diskoria.exe".into(),
                browser_download_url: "https://x/diskoria.exe".into(),
            },
            AssetJson {
                name: "diskoria-1.6.1-setup.exe".into(),
                browser_download_url: "https://x/diskoria-1.6.1-setup.exe".into(),
            },
            AssetJson {
                name: "diskoria.pdb".into(),
                browser_download_url: "https://x/diskoria.pdb".into(),
            },
        ];
        assert_eq!(
            pick_exe_url(&assets).as_deref(),
            Some("https://x/diskoria-1.6.1-setup.exe")
        );
        // The chosen asset must round-trip as an installer through the temp name.
        assert!(url_is_installer(&pick_exe_url(&assets).unwrap()));
    }
}
