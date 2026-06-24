//! Pro-Monitoring: Windows toast notifications.
//!
//! Primary path: WinRT `ToastNotification` (Win10/11 action center).
//! Fallback: `Shell_NotifyIconW` balloon tip.

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

#[cfg(not(windows))]
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

/// Register `HKCU\Software\Classes\AppUserModelId\Diskoria` so the system
/// can route our toast notifications.  Only writes if the key is absent.
#[cfg(windows)]
fn ensure_aumid_registered() {
    use windows_sys::Win32::System::Registry::{
        RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER,
        KEY_READ, KEY_WRITE, REG_SZ,
        REG_OPENED_EXISTING_KEY,
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

    let exe_path = std::env::current_exe()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let icon_uri: Vec<u16> = OsStr::new(&exe_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

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

        // Only write values if we created the key fresh.
        if disposition != REG_OPENED_EXISTING_KEY {
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

            let icon_bytes = std::slice::from_raw_parts(
                icon_uri.as_ptr() as *const u8,
                icon_uri.len() * 2,
            );
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
