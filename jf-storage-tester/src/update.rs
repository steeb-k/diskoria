//! GitHub Releases update check and Windows self-update (download → exit → replace exe → relaunch).
//! Release source: [`jjfbno/JF-Storage-Tester`](https://github.com/jjfbno/JF-Storage-Tester/releases).

use semver::Version;
use serde::Deserialize;

const API_LATEST: &str = "https://api.github.com/repos/jjfbno/JF-Storage-Tester/releases/latest";

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

/// Prefer Setup installer `.exe` (matches WPF `UpdateService`), then any non-PDB `.exe`.
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

    let resp = ureq::get(API_LATEST)
        .set(
            "User-Agent",
            &format!("JFStorageTester/{}", env!("CARGO_PKG_VERSION")),
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

pub fn download_to_path(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            &format!("JFStorageTester/{}", env!("CARGO_PKG_VERSION")),
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
    let batch = std::env::temp_dir().join("jf_storage_tester_apply_update.bat");

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
echo Failed to install update. > "%TEMP%\jf_storage_tester_update_error.txt"
echo You can close this window. >> "%TEMP%\jf_storage_tester_update_error.txt"
pause
del "%~f0"
exit /b 1
"#,
        new = new_s,
        tgt = tgt_s,
    );

    if std::fs::write(&batch, &script).is_err() {
        log::warn!("jf_storage_tester: failed to write apply_update.bat");
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
        log::warn!("jf_storage_tester: failed to spawn apply_update.bat");
        return;
    }

    std::thread::sleep(std::time::Duration::from_millis(150));
    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn spawn_apply_update_and_exit(_new_exe: &std::path::Path, _target_exe: &std::path::Path) {}

/// Run a downloaded installer (e.g. Inno `*Setup*.exe`) and exit so the file is unlocked.
#[cfg(windows)]
pub fn spawn_run_installer_and_exit(installer: &std::path::Path) {
    let _ = std::process::Command::new(installer).spawn();
    std::thread::sleep(std::time::Duration::from_millis(150));
    std::process::exit(0);
}

#[cfg(not(windows))]
pub fn spawn_run_installer_and_exit(_installer: &std::path::Path) {}
