//! GitHub Releases update check and self-update.
//! Release assets are published on the **`diskoria-binaries`** repo (see [`crate::github_config`]).
//!
//! Windows: download the Inno installer (or a bare exe) → stage → run the
//! installer silently / batch-replace the exe. Linux: download the bare
//! portable binary next to the running one (same filesystem) → stage →
//! atomic `rename` over `current_exe`, either immediately with a re-exec
//! ("Update now") or from the exit hook ("Update on close").

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

/// Linux portable asset: a bare binary named for the platform (see
/// `scripts/build-portable.sh` — `diskoria-<ver>-linux-x86_64`), never an
/// archive or checksum sidecar.
#[cfg_attr(windows, allow(dead_code))]
fn pick_linux_url(assets: &[AssetJson]) -> Option<String> {
    pick_linux_url_for_arch(assets, std::env::consts::ARCH)
}

/// Split out from [`pick_linux_url`] so the architecture rules can be tested
/// for machines other than the one running the tests.
#[cfg_attr(windows, allow(dead_code))]
fn pick_linux_url_for_arch(assets: &[AssetJson], arch: &str) -> Option<String> {
    let is_other = |n: &str| {
        [".tar.gz", ".tgz", ".tar.xz", ".zip", ".sha256", ".sig", ".asc", ".exe", ".pdb"]
            .iter()
            .any(|ext| n.ends_with(ext))
    };
    let linux: Vec<&AssetJson> = assets
        .iter()
        .filter(|a| {
            let n = a.name.to_ascii_lowercase();
            n.contains("linux") && !is_other(&n)
        })
        .collect();
    linux
        .iter()
        .find(|a| arch_matches(&a.name.to_ascii_lowercase(), arch))
        // Only an asset that names *no* architecture may stand in for ours.
        // Falling back to "whatever is first" would hand an aarch64 machine the
        // x86_64 binary and overwrite a working install with one the kernel
        // cannot exec — Linux has no equivalent of Windows' x64-on-ARM
        // emulation, so a missing build for this machine has to mean "no
        // update", not "close enough" (KI-46).
        .or_else(|| linux.iter().find(|a| !names_an_arch(&a.name.to_ascii_lowercase())))
        .map(|a| a.browser_download_url.clone())
}

/// Architecture tokens that can appear in a release asset name, grouped by the
/// `std::env::consts::ARCH` value they correspond to.
#[cfg_attr(windows, allow(dead_code))]
const ARCH_ALIASES: &[(&str, &[&str])] = &[
    ("x86_64", &["x86_64", "x86-64", "amd64"]),
    ("x86", &["i686", "i386"]),
    ("aarch64", &["aarch64", "arm64"]),
    ("arm", &["armv7", "armhf"]),
    ("riscv64", &["riscv64"]),
    ("powerpc64", &["ppc64le", "powerpc64"]),
];

/// Does this asset name carry a token for `arch`?
#[cfg_attr(windows, allow(dead_code))]
fn arch_matches(name: &str, arch: &str) -> bool {
    match ARCH_ALIASES.iter().find(|(canonical, _)| *canonical == arch) {
        Some((_, toks)) => toks.iter().any(|tok| name.contains(tok)),
        // An architecture with no aliases listed still matches its own name.
        None => name.contains(arch),
    }
}

/// Does this asset name name *any* architecture we recognise? Used to tell
/// "built for a different machine" apart from "architecture-neutral".
#[cfg_attr(windows, allow(dead_code))]
fn names_an_arch(name: &str) -> bool {
    ARCH_ALIASES
        .iter()
        .any(|(_, toks)| toks.iter().any(|tok| name.contains(tok)))
}

/// Prefer installer `.exe` with "setup" in the name, then any non-PDB `.exe`.
#[cfg_attr(not(windows), allow(dead_code))]
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

    #[cfg(windows)]
    let url = pick_exe_url(&release.assets)
        .ok_or_else(|| "No Windows .exe found in this release.".to_string())?;
    #[cfg(not(windows))]
    let url = pick_linux_url(&release.assets)
        .ok_or_else(|| "No Linux binary found in this release.".to_string())?;

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
    #[cfg(windows)]
    {
        temp_file_name_windows(url, nonce)
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        temp_file_name_unix(nonce)
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn temp_file_name_windows(url: &str, nonce: u128) -> String {
    if url_is_installer(url) {
        format!("Diskoria_update_setup_{nonce}.exe")
    } else {
        format!("Diskoria_update_{nonce}.exe")
    }
}

/// Dotfile next to the running binary — the same filesystem, so the apply
/// step is one atomic `rename`.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn temp_file_name_unix(nonce: u128) -> String {
    format!(".diskoria-update-{nonce}")
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
#[allow(dead_code)] // Linux applies via replace_exe/apply_update_now_and_restart instead
pub fn spawn_apply_update_and_exit(_new_exe: &std::path::Path, _target_exe: &std::path::Path) {}

/// Replace `target_exe` with the staged `new_exe` by atomic rename,
/// mirroring the target's ownership and mode first (the process may be root
/// via pkexec while the binary lives in a user-owned directory). Linux allows
/// replacing a running executable's path — the old inode stays mapped.
#[cfg(target_os = "linux")]
pub fn replace_exe(new_exe: &std::path::Path, target_exe: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::metadata(target_exe)
        .map_err(|e| format!("Cannot stat {}: {e}", target_exe.display()))?;
    std::fs::set_permissions(new_exe, std::fs::Permissions::from_mode(meta.mode()))
        .map_err(|e| format!("Cannot set update permissions: {e}"))?;
    let _ = std::os::unix::fs::chown(new_exe, Some(meta.uid()), Some(meta.gid()));
    std::fs::rename(new_exe, target_exe)
        .map_err(|e| format!("Cannot replace {}: {e}", target_exe.display()))
}

/// "Update now": swap the binary and re-exec it in place (keeps the current
/// privileges — no second pkexec prompt). Only returns on failure.
#[cfg(target_os = "linux")]
pub fn apply_update_now_and_restart(
    new_exe: &std::path::Path,
    target_exe: &std::path::Path,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    replace_exe(new_exe, target_exe)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(target_exe).args(args).exec();
    Err(format!("Re-exec after update failed: {err}"))
}

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
#[cfg_attr(not(windows), allow(dead_code))] // Inno-installer helper; tests cover it everywhere
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
#[allow(dead_code)] // Linux applies via apply_update_now_and_restart instead
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
        let installer = temp_file_name_windows("https://x/diskoria-1.6.1-setup.exe", 42);
        assert!(installer.to_ascii_lowercase().contains("setup"));
        assert!(installer.ends_with(".exe"));

        let portable = temp_file_name_windows("https://x/diskoria.exe", 42);
        assert!(!portable.to_ascii_lowercase().contains("setup"));
        assert!(portable.ends_with(".exe"));

        // Nonce keeps concurrent downloads from colliding.
        assert_ne!(
            temp_file_name_windows("https://x/diskoria.exe", 1),
            temp_file_name_windows("https://x/diskoria.exe", 2)
        );
        assert_ne!(temp_file_name_unix(1), temp_file_name_unix(2));
        assert!(temp_file_name_unix(1).starts_with('.'));
    }

    /// The one that matters for a two-architecture release: an ARM machine
    /// must not be handed the x86_64 build. Linux has no x64-on-ARM emulation,
    /// so applying it would replace a working binary with one that cannot exec.
    #[test]
    fn an_arm_machine_refuses_an_x86_only_release() {
        let assets = vec![AssetJson {
            name: "diskoria-1.7.0-linux-x86_64".into(),
            browser_download_url: "https://x/x86".into(),
        }];
        assert_eq!(pick_linux_url_for_arch(&assets, "aarch64"), None);
        assert_eq!(pick_linux_url_for_arch(&assets, "x86_64").as_deref(), Some("https://x/x86"));
    }

    #[test]
    fn each_machine_takes_its_own_build_from_a_multi_arch_release() {
        let assets = vec![
            AssetJson {
                name: "diskoria-1.7.0-linux-x86_64".into(),
                browser_download_url: "https://x/x86".into(),
            },
            AssetJson {
                name: "diskoria-1.7.0-linux-aarch64".into(),
                browser_download_url: "https://x/arm".into(),
            },
        ];
        assert_eq!(pick_linux_url_for_arch(&assets, "x86_64").as_deref(), Some("https://x/x86"));
        assert_eq!(pick_linux_url_for_arch(&assets, "aarch64").as_deref(), Some("https://x/arm"));
    }

    #[test]
    fn common_arch_spellings_are_recognised() {
        assert!(arch_matches("diskoria-linux-amd64", "x86_64"));
        assert!(arch_matches("diskoria-linux-arm64", "aarch64"));
        assert!(!arch_matches("diskoria-linux-arm64", "x86_64"));
        assert!(!arch_matches("diskoria-linux-amd64", "aarch64"));
        assert!(names_an_arch("diskoria-linux-ppc64le"));
        assert!(!names_an_arch("diskoria-linux"));
    }

    /// A release that names no architecture at all is still usable — that is
    /// what old single-build releases look like.
    #[test]
    fn an_arch_neutral_asset_is_accepted_anywhere() {
        let assets = vec![AssetJson {
            name: "diskoria-1.7.0-linux".into(),
            browser_download_url: "https://x/any".into(),
        }];
        assert_eq!(pick_linux_url_for_arch(&assets, "aarch64").as_deref(), Some("https://x/any"));
        assert_eq!(pick_linux_url_for_arch(&assets, "riscv64").as_deref(), Some("https://x/any"));
    }

    #[test]
    fn pick_linux_url_prefers_bare_arch_binary() {
        let assets = vec![
            AssetJson {
                name: "diskoria-1.7.0-setup.exe".into(),
                browser_download_url: "https://x/setup.exe".into(),
            },
            AssetJson {
                name: "diskoria-1.7.0-portable-linux-x86_64.tar.gz".into(),
                browser_download_url: "https://x/tar".into(),
            },
            AssetJson {
                name: "diskoria-1.7.0-linux-x86_64".into(),
                browser_download_url: "https://x/bin".into(),
            },
            AssetJson {
                name: "diskoria-1.7.0-linux-x86_64.sha256".into(),
                browser_download_url: "https://x/sha".into(),
            },
        ];
        assert_eq!(pick_linux_url(&assets).as_deref(), Some("https://x/bin"));
        // No bare binary → nothing (archives are for humans, not the updater).
        let only_tar = vec![AssetJson {
            name: "diskoria-portable-linux-x86_64.tar.gz".into(),
            browser_download_url: "https://x/tar".into(),
        }];
        assert_eq!(pick_linux_url(&only_tar), None);
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

#[cfg(all(test, target_os = "linux"))]
mod linux_apply_tests {
    use super::replace_exe;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replace_exe_swaps_content_and_preserves_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("diskoria");
        let staged = dir.path().join(".diskoria-update-1");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&staged, b"new").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644)).unwrap();

        replace_exe(&staged, &target).expect("replace");

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755,
            "target keeps its executable mode"
        );
        assert!(!staged.exists(), "staged file consumed by the rename");
    }
}
