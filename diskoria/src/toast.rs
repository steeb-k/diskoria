//! Pro-Monitoring: desktop notifications.
//!
//! Windows: WinRT `ToastNotification` (Win10/11 action center), falling back
//! to a `Shell_NotifyIconW` balloon tip.
//! Linux: `org.freedesktop.Notifications` over the session bus (notify-rust)
//! when running as the user. Elevated runs cannot connect in-process — the
//! session bus authenticates by peer uid and refuses root even though the
//! socket is reachable — so they post through `dbus-send`/`notify-send`
//! running as the invoking user (see `elevation::command_as_session_user`).

/// Send a Windows toast notification.
///
/// Must be called from a background thread — WinRT requires a COM MTA
/// context and the main winit thread is STA.
#[cfg(windows)]
pub fn send_toast(title: &str, body: &str) {
    if try_winrt_toast(title, body).is_err() {
        // Fall back to a simple balloon tip via the app-level tray icon.
        log::warn!(target: "diskoria::toast", "WinRT toast failed, using balloon fallback");
        send_balloon_tip(title, body);
    }
}

/// Absolute path to the app icon on disk, extracted from the binary on first
/// use.
///
/// Freedesktop notifications take either a themed icon *name* or a path. A
/// portable binary installs nothing into the icon theme, so a name like
/// "diskoria" resolves to nothing and daemons fall back to a generic glyph —
/// hence writing the bundled PNG out once and passing its path. Falls back to
/// a themed disk icon if the file cannot be written.
#[cfg(target_os = "linux")]
fn app_icon_arg() -> String {
    use std::sync::OnceLock;

    const FALLBACK: &str = "drive-harddisk";
    static RESOLVED: OnceLock<String> = OnceLock::new();

    RESOLVED
        .get_or_init(|| match write_icon_png() {
            Some(path) => path,
            None => FALLBACK.to_string(),
        })
        .clone()
}

/// Decode the bundled app icon and write it out as PNG, returning its path.
/// Written once per run (rather than only when missing) so a stale file from
/// an older build is replaced.
///
/// Both platforms need the icon as a *file* rather than as something embedded
/// in the binary: freedesktop daemons take an icon name or path and cannot read
/// a themed name a portable binary never installed, and Windows' toast
/// `IconUri` wants an image file — it will not extract an icon resource out of
/// an exe (known-issues KI-49).
#[cfg(any(windows, target_os = "linux"))]
fn write_icon_png() -> Option<String> {
    // The window/app icon — not `applogo.png`, which is the in-app sidebar
    // logo. Neither consumer decodes ICO, so it is re-encoded as PNG.
    static APP_ICO: &[u8] = include_bytes!("../../assets/appicon2.ico");

    let img = image::load_from_memory_with_format(APP_ICO, image::ImageFormat::Ico)
        .map_err(|e| log::warn!(target: "diskoria::toast", "app icon decode failed: {e}"))
        .ok()?;
    let path = crate::paths::data_dir().join("appicon.png");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    img.save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| log::warn!(target: "diskoria::toast", "app icon write failed: {e}"))
        .ok()?;
    // An elevated session writes it as root; the notification daemon reads it
    // as the user, so hand ownership over like the rest of the data dir.
    #[cfg(target_os = "linux")]
    if let Some(uid) = crate::elevation::session_uid() {
        let _ = std::os::unix::fs::chown(&path, Some(uid), None);
    }
    Some(path.to_string_lossy().into_owned())
}

/// Send a desktop notification via the freedesktop session bus. Failure is
/// logged, not fatal — a missing daemon just means no popup.
#[cfg(target_os = "linux")]
pub fn send_toast(title: &str, body: &str) {
    // Elevated: the in-process D-Bus connection would be refused (the session
    // bus authenticates by peer uid), so post the notification through a
    // helper running as the invoking user.
    if crate::elevation::session_uid().is_some() {
        if send_toast_as_session_user(title, body) {
            return;
        }
        log::warn!(target: "diskoria::toast", "elevated notification failed; falling back");
    }
    let result = notify_rust::Notification::new()
        .appname("Diskoria")
        .summary(title)
        .body(body)
        .icon(&app_icon_arg())
        .show();
    if let Err(e) = result {
        log::warn!(target: "diskoria::toast", "notification failed: {e}");
    }
}

/// `org.freedesktop.Notifications.Notify` via `dbus-send` as the invoking
/// user. Returns whether the call succeeded.
#[cfg(target_os = "linux")]
fn send_toast_as_session_user(title: &str, body: &str) -> bool {
    let icon = app_icon_arg();
    let ok = |mut c: std::process::Command| -> bool {
        c.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    let mut c = crate::elevation::command_as_session_user("dbus-send");
    c.args([
        "--session",
        "--type=method_call",
        "--dest=org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications.Notify",
        "string:Diskoria",
        "uint32:0",
        &format!("string:{icon}"),
        &format!("string:{title}"),
        &format!("string:{body}"),
        "array:string:",
        "dict:string:variant:",
        "int32:8000",
    ]);
    if ok(c) {
        return true;
    }
    // Some desktops ship notify-send but no dbus-send.
    let mut c = crate::elevation::command_as_session_user("notify-send");
    c.args(["-a", "Diskoria", "-i", &icon, title, body]);
    ok(c)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn send_toast(_title: &str, _body: &str) {}

// ── WinRT path ────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn try_winrt_toast(title: &str, body: &str) -> windows::core::Result<()> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    // Ensure AUMID is registered so notifications can be routed.
    ensure_aumid_registered();

    let xml = XmlDocument::new()?;
    let template = format!(
        "<toast>\
           <visual>\
             <binding template=\"ToastGeneric\">\
               <text>{}</text>\
               <text>{}</text>\
             </binding>\
           </visual>\
         </toast>",
        escape_xml(title),
        escape_xml(body),
    );
    xml.LoadXml(&windows::core::HSTRING::from(template.as_str()))?;

    let notif = ToastNotification::CreateToastNotification(&xml)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(
        &windows::core::HSTRING::from("Diskoria"),
    )?;
    notifier.Show(&notif)?;
    Ok(())
}

#[cfg(windows)]
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── AUMID registration ────────────────────────────────────────────────────────

/// Register `HKCU\Software\Classes\AppUserModelId\Diskoria` so the system can
/// route our toast notifications, and so they carry the app icon.
///
/// The values are rewritten on every call, not just when the key is created
/// (KI-49). Two reasons: a registration written by an older build points
/// `IconUri` at the exe, which renders as an empty icon slot forever because
/// nothing ever revisits it; and the portable exe moves, so a path recorded
/// once goes stale.
#[cfg(windows)]
fn ensure_aumid_registered() {
    use windows_sys::Win32::System::Registry::{
        RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER,
        KEY_READ, KEY_WRITE, REG_SZ,
    };
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let key_path: Vec<u16> = OsStr::new(
        "Software\\Classes\\AppUserModelId\\Diskoria",
    )
    .encode_wide()
    .chain(std::iter::once(0))
    .collect();

    let display_name: Vec<u16> = OsStr::new("Diskoria")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // `IconUri` must name an image file. Windows does not pull an icon resource
    // out of an exe here, so pointing it at `current_exe()` left the toast's
    // icon slot blank — the bug this fixes. If the PNG cannot be written, leave
    // the value alone rather than replacing it with something equally broken.
    let icon_path = write_icon_png();
    let icon_uri: Option<Vec<u16>> = icon_path.as_deref().map(|p| {
        OsStr::new(p)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    });

    unsafe {
        let mut hkey = 0isize;
        let mut disposition = 0u32;
        let result = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_READ | KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            &mut disposition,
        );
        if result != 0 {
            return;
        }

        let dn_bytes = std::slice::from_raw_parts(
            display_name.as_ptr() as *const u8,
            display_name.len() * 2,
        );
        let dn_name: Vec<u16> = OsStr::new("DisplayName")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        RegSetValueExW(
            hkey,
            dn_name.as_ptr(),
            0,
            REG_SZ,
            dn_bytes.as_ptr(),
            dn_bytes.len() as u32,
        );

        if let Some(ref icon) = icon_uri {
            let icon_bytes =
                std::slice::from_raw_parts(icon.as_ptr() as *const u8, icon.len() * 2);
            let icon_name: Vec<u16> = OsStr::new("IconUri")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            RegSetValueExW(
                hkey,
                icon_name.as_ptr(),
                0,
                REG_SZ,
                icon_bytes.as_ptr(),
                icon_bytes.len() as u32,
            );
        }

        let _ = windows_sys::Win32::System::Registry::RegCloseKey(hkey);
    }
}

// ── Balloon tip fallback ──────────────────────────────────────────────────────

/// Send a balloon tip via `Shell_NotifyIconW`. This uses a temporary invisible
/// notification icon and is the legacy fallback when WinRT fails.
#[cfg(windows)]
fn send_balloon_tip(title: &str, body: &str) {
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIIF_INFO,
        NOTIFYICONDATAW, NIM_ADD, NIM_DELETE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WS_OVERLAPPED,
    };
    use windows_sys::Win32::Foundation::HWND;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    unsafe {
        // Create a message-only window as the HWND owner for the tray icon.
        let hwnd: HWND = CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            windows_sys::w!(""),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            HWND_MESSAGE,
            0,
            0,
            std::ptr::null(),
        );
        if hwnd == 0 {
            return;
        }

        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 0xD15C_0001; // arbitrary unique ID for our temp balloon icon
        nid.uFlags = NIF_ICON | NIF_INFO | NIF_MESSAGE;
        nid.uCallbackMessage = 0;
        nid.Anonymous.uTimeout = 5000;

        // Copy title (max 64 wchars including null).
        let title_w: Vec<u16> = OsStr::new(title)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let title_len = title_w.len().min(nid.szInfoTitle.len() - 1);
        nid.szInfoTitle[..title_len].copy_from_slice(&title_w[..title_len]);

        // Copy body (max 256 wchars including null).
        let body_w: Vec<u16> = OsStr::new(body)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let body_len = body_w.len().min(nid.szInfo.len() - 1);
        nid.szInfo[..body_len].copy_from_slice(&body_w[..body_len]);

        nid.dwInfoFlags = NIIF_INFO;

        Shell_NotifyIconW(NIM_ADD, &nid);
        std::thread::sleep(std::time::Duration::from_millis(100));
        Shell_NotifyIconW(NIM_DELETE, &nid);
        DestroyWindow(hwnd);
    }
}
