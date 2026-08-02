//! Theme tokens — Windows 11-style dark/light palette, accent color, and shared
//! layout constants. Single source of truth for the values the UI draws with.

use egui::{Color32, Stroke};

pub const TITLEBAR_H: f32 = 32.0;
pub const BTN_W: f32 = 46.0;
/// Width of the labeled "New Window" button (icon + text), wider than the plain
/// min/max/close glyph buttons so the caption fits.
pub const NEWWIN_W: f32 = 126.0;
/// Combined width of the window-control strip: the labeled new-window button plus
/// the three min/max/close glyph buttons. Single source of truth shared by the
/// egui title bar and the Win32 resize hit-test so the controls strip and the
/// resize border never disagree.
pub const CONTROLS_W: f32 = NEWWIN_W + 3.0 * BTN_W;
pub const SIDE_PANEL_W: f32 = 240.0;
pub const CONTENT_MARGIN: f32 = 24.0;
pub const MAX_CONTENT_W: f32 = 800.0;

// ── Card geometry ─────────────────────────────────────────────────────────────
// The rounded panels every page stacks vertically. Shared by `card::CardLayout`,
// which is the only thing that should be reading them — see known-issues KI-18.

/// Inner padding between a card's frame and its content, on all four sides.
pub const CARD_PAD: f32 = 16.0;
/// On-screen vertical gap between two stacked cards.
pub const CARD_GAP: f32 = 12.0;
pub const CARD_RADIUS: f32 = 8.0;
pub const CARD_BORDER_W: f32 = 1.5;
/// Height of a card's bold title row, and the gap between it and the first row.
pub const CARD_TITLE_H: f32 = 22.0;
pub const CARD_TITLE_GAP: f32 = 12.0;

const BG_PRI_L: Color32 = Color32::from_rgb(243, 243, 243);
const BG_SEC_L: Color32 = Color32::from_rgb(255, 255, 255);
const SB_BG_L: Color32 = Color32::from_rgb(249, 249, 249);
const TXT_PRI_L: Color32 = Color32::from_rgb(26, 26, 26);
const TXT_SEC_L: Color32 = Color32::from_rgb(92, 92, 92);
const BORDER_L: Color32 = Color32::from_rgb(190, 190, 190);

const BG_PRI_D: Color32 = Color32::from_rgb(32, 32, 32);
const BG_SEC_D: Color32 = Color32::from_rgb(45, 45, 45);
const SB_BG_D: Color32 = Color32::from_rgb(37, 37, 37);
const TXT_PRI_D: Color32 = Color32::WHITE;
const TXT_SEC_D: Color32 = Color32::from_rgb(158, 158, 158);
const BORDER_D: Color32 = Color32::from_rgb(90, 90, 90);

pub const CLOSE_HOVER_BG: Color32 = Color32::from_rgb(196, 43, 28);

/// WCAG relative luminance of an sRGB color (channels linearized first — the
/// naive "weight the raw 0..1 channels" version badly overestimates darkness
/// for saturated mid-tones like orange and teal).
fn relative_luminance(c: Color32) -> f32 {
    fn lin(u8v: u8) -> f32 {
        let c = u8v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio (1.0 … 21.0) between two opaque colors.
fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Minimum white-on-accent contrast we'll accept before switching to black.
/// 3.0 is the WCAG AA bar for large/bold text, which is what actually sits on
/// accent fills here (button captions, segmented-control labels). Keeping the
/// bar at 3.0 rather than 4.5 preserves the familiar white-on-purple/blue/red
/// look while still flipping to black for pale accents (the Windows "white"
/// accent used to render white-on-white) and for low-contrast oranges/teals.
const MIN_ON_ACCENT_CONTRAST: f32 = 3.0;

/// Black or white, whichever stays legible on the given accent fill.
pub fn text_on(accent: Color32) -> Color32 {
    if contrast_ratio(Color32::WHITE, accent) >= MIN_ON_ACCENT_CONTRAST {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

pub struct Theme {
    pub bg_pri: Color32,
    pub bg_sec: Color32,
    pub sb_bg: Color32,
    pub accent: Color32,
    pub txt_pri: Color32,
    pub txt_sec: Color32,
    /// Foreground for anything drawn *on top of* an accent fill — button
    /// captions, selected segmented-control labels, toggle knobs. Never use a
    /// hard-coded `Color32::WHITE` there: the accent can be any color the user
    /// picks (or Windows reports), including white.
    pub txt_on_accent: Color32,
    pub border: Color32,
    pub hover: Color32,
}

impl Theme {
    pub fn new(dark: bool, accent: Color32) -> Self {
        let txt_on_accent = text_on(accent);

        if dark {
            Self {
                bg_pri: BG_PRI_D,
                bg_sec: BG_SEC_D,
                sb_bg: SB_BG_D,
                accent,
                txt_pri: TXT_PRI_D,
                txt_sec: TXT_SEC_D,
                txt_on_accent,
                border: BORDER_D,
                hover: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
            }
        } else {
            Self {
                bg_pri: BG_PRI_L,
                bg_sec: BG_SEC_L,
                sb_bg: SB_BG_L,
                accent,
                txt_pri: TXT_PRI_L,
                txt_sec: TXT_SEC_L,
                txt_on_accent,
                border: BORDER_L,
                hover: Color32::from_rgba_unmultiplied(0, 0, 0, 12),
            }
        }
    }
}

/// The accent color Windows itself is using, or `None` when the OS has none to
/// give — in which case the caller falls back to the palette (known-issues
/// KI-30) and should make that visible rather than silently painting purple.
///
/// Two sources, in order:
///
/// 1. `HKCU\Software\Microsoft\Windows\DWM\AccentColor` — the *exact* accent the
///    user picked in Settings → Personalization.
/// 2. `DwmGetColorizationColor` — the composited **colorization** color, which
///    is the accent blended per `ColorizationColorBalance`/afterglow and so only
///    approximates it (measured: accent `#0078D4` → colorization `#006FC4`).
///    Kept as a fallback for sessions where the value above is missing.
///
/// Both fail where there is no per-user accent at all — notably Windows PE,
/// which has no DWM composition.
#[cfg(windows)]
pub fn os_accent_color() -> Option<Color32> {
    registry_accent_color().or_else(dwm_colorization_color)
}

/// `HKCU\Software\Microsoft\Windows\DWM\AccentColor`, a `REG_DWORD` stored
/// **ABGR** (`0xFFD47800` → `#0078D4`) — note the byte order is the reverse of
/// the ARGB `DwmGetColorizationColor` returns.
#[cfg(windows)]
fn registry_accent_color() -> Option<Color32> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
    };

    let wide = |s: &str| -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    };
    let subkey = wide(r"Software\Microsoft\Windows\DWM");
    let value = wide("AccentColor");

    unsafe {
        let mut abgr: u32 = 0;
        let mut cb: u32 = std::mem::size_of::<u32>() as u32;
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            std::ptr::addr_of_mut!(abgr).cast(),
            &mut cb,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        Some(accent_from_abgr(abgr))
    }
}

#[cfg(windows)]
fn dwm_colorization_color() -> Option<Color32> {
    use windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor;
    unsafe {
        let mut colorization: u32 = 0;
        let mut opaque: i32 = 0;
        let hr = DwmGetColorizationColor(&mut colorization, &mut opaque);
        if hr != 0 {
            return None;
        }
        Some(accent_from_argb(colorization))
    }
}

/// `0xAABBGGRR` (registry `AccentColor`) → opaque RGB.
/// `cfg(test)` too, so the byte-order test still builds on the non-Windows shell.
#[cfg(any(windows, test))]
fn accent_from_abgr(v: u32) -> Color32 {
    Color32::from_rgb((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8)
}

/// `0xAARRGGBB` (`DwmGetColorizationColor`) → opaque RGB.
#[cfg(any(windows, test))]
fn accent_from_argb(v: u32) -> Color32 {
    Color32::from_rgb(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
}

/// Linux: the XDG desktop-settings portal (`org.freedesktop.appearance`),
/// which KDE and GNOME both serve. Queried by shelling out to `dbus-send` —
/// universally present wherever a session bus is — because pulling an async
/// D-Bus runtime onto the UI thread for two scalar reads is not worth the
/// hazard. Results are cached for a few seconds; the accent poll and the
/// per-frame theme check both go through the cache.
#[cfg(target_os = "linux")]
mod linux_portal {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use egui::Color32;

    const CACHE_FOR: Duration = Duration::from_secs(3);

    struct Cached {
        read_at: Option<Instant>,
        prefers_dark: Option<bool>,
        accent: Option<Color32>,
    }

    static CACHE: Mutex<Cached> = Mutex::new(Cached {
        read_at: None,
        prefers_dark: None,
        accent: None,
    });

    fn read_setting(key: &str) -> Option<String> {
        // Elevated runs must ask as the invoking user: the session bus
        // authenticates by peer uid and drops root outright.
        let out = crate::elevation::command_as_session_user("dbus-send")
            .args([
                "--session",
                "--print-reply=literal",
                "--dest=org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings.Read",
                "string:org.freedesktop.appearance",
                &format!("string:{key}"),
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `variant variant uint32 1` → 1 = prefer dark, 2 = prefer light,
    /// 0 = no preference.
    pub(super) fn parse_color_scheme(reply: &str) -> Option<bool> {
        let n: u32 = reply.split_whitespace().last()?.parse().ok()?;
        match n {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// `variant variant struct { double r double g double b }`, sRGB 0..1.
    pub(super) fn parse_accent(reply: &str) -> Option<Color32> {
        let mut chans = reply
            .split_whitespace()
            .filter_map(|t| t.parse::<f64>().ok());
        let (r, g, b) = (chans.next()?, chans.next()?, chans.next()?);
        let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some(Color32::from_rgb(to8(r), to8(g), to8(b)))
    }

    fn refresh_if_stale() {
        let stale = {
            let c = CACHE.lock().expect("portal cache poisoned");
            c.read_at.is_none_or(|t| t.elapsed() > CACHE_FOR)
        };
        if !stale {
            return;
        }
        let prefers_dark = read_setting("color-scheme")
            .as_deref()
            .and_then(parse_color_scheme);
        let accent = read_setting("accent-color")
            .as_deref()
            .and_then(parse_accent);
        let mut c = CACHE.lock().expect("portal cache poisoned");
        c.read_at = Some(Instant::now());
        c.prefers_dark = prefers_dark;
        c.accent = accent;
    }

    pub fn prefers_dark() -> Option<bool> {
        refresh_if_stale();
        CACHE.lock().expect("portal cache poisoned").prefers_dark
    }

    pub fn accent_color() -> Option<Color32> {
        refresh_if_stale();
        CACHE.lock().expect("portal cache poisoned").accent
    }
}

#[cfg(target_os = "linux")]
pub fn os_accent_color() -> Option<Color32> {
    linux_portal::accent_color()
}

/// Linux only: the desktop's light/dark preference from the settings portal.
/// winit cannot report a system theme on X11/Wayland, so `ThemePref::Auto`
/// falls back to this when `ctx.system_theme()` is `None`.
#[cfg(target_os = "linux")]
pub fn os_prefers_dark() -> Option<bool> {
    linux_portal::prefers_dark()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn os_accent_color() -> Option<Color32> {
    None
}

pub fn apply_visuals(ctx: &egui::Context, dark: bool, accent: Color32) {
    let t = Theme::new(dark, accent);
    let mut vis = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    vis.panel_fill = t.bg_pri;
    vis.window_fill = t.bg_sec;
    vis.extreme_bg_color = t.bg_pri;
    vis.faint_bg_color = t.bg_pri;
    vis.window_shadow = egui::Shadow::NONE;

    vis.widgets.noninteractive.bg_fill = t.bg_pri;
    vis.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.txt_pri);
    vis.widgets.noninteractive.bg_stroke = Stroke::new(1.5_f32, t.border);
    vis.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    vis.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, t.accent);
    vis.widgets.inactive.bg_stroke = Stroke::NONE;
    vis.widgets.hovered.bg_fill = t.hover;
    vis.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, t.accent);
    vis.widgets.hovered.bg_stroke = Stroke::NONE;
    vis.widgets.active.bg_fill = if dark {
        Color32::from_rgba_premultiplied(255, 255, 255, 30)
    } else {
        Color32::from_rgba_premultiplied(0, 0, 0, 25)
    };
    vis.widgets.active.fg_stroke = Stroke::new(1.0_f32, t.accent);
    vis.selection.bg_fill =
        Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 120);
    vis.selection.stroke = Stroke::new(1.5_f32, t.accent);
    ctx.set_visuals(vis);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_luminance_spans_black_to_white() {
        assert!(relative_luminance(Color32::BLACK).abs() < 1e-6);
        assert!((relative_luminance(Color32::WHITE) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn contrast_ratio_is_symmetric_and_bounded() {
        let ratio = contrast_ratio(Color32::BLACK, Color32::WHITE);
        assert!((ratio - 21.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - ratio).abs() < 1e-4);
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn pale_accents_get_black_text() {
        // The reported bug: a white/near-white Windows accent rendered the
        // "New Window" caption white-on-white.
        assert_eq!(text_on(Color32::WHITE), Color32::BLACK);
        assert_eq!(text_on(Color32::from_rgb(240, 240, 240)), Color32::BLACK);
        // Cha-Cha (yellow) and Quickstep (light gray) from the palette.
        assert_eq!(text_on(crate::app_settings::ACCENT_PALETTE[4]), Color32::BLACK);
        assert_eq!(text_on(crate::app_settings::ACCENT_PALETTE[7]), Color32::BLACK);
    }

    #[test]
    fn dark_accents_keep_white_text() {
        assert_eq!(text_on(Color32::BLACK), Color32::WHITE);
        // Waltz (purple) and Foxtrot (blue) — the default and most common accents.
        assert_eq!(text_on(crate::app_settings::ACCENT_PALETTE[0]), Color32::WHITE);
        assert_eq!(text_on(crate::app_settings::ACCENT_PALETTE[1]), Color32::WHITE);
    }

    #[test]
    fn accent_dword_byte_orders_are_not_mixed_up() {
        // Both observed on a stock Windows 11 profile whose accent is #0078D4.
        // The registry value is ABGR, DwmGetColorizationColor's is ARGB — reading
        // one with the other's layout silently yields a plausible wrong color.
        assert_eq!(accent_from_abgr(0xFFD4_7800), Color32::from_rgb(0x00, 0x78, 0xD4));
        assert_eq!(accent_from_argb(0xE300_6FC4), Color32::from_rgb(0x00, 0x6F, 0xC4));
        // Alpha is ignored: the accent is always painted opaque.
        assert_eq!(accent_from_abgr(0x0000_00FF), Color32::from_rgb(0xFF, 0x00, 0x00));
        assert_eq!(accent_from_argb(0x0000_00FF), Color32::from_rgb(0x00, 0x00, 0xFF));
    }

    #[test]
    fn theme_exposes_the_same_choice() {
        for &accent in crate::app_settings::ACCENT_PALETTE.iter() {
            for dark in [true, false] {
                assert_eq!(Theme::new(dark, accent).txt_on_accent, text_on(accent));
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod portal_tests {
    use super::linux_portal::{parse_accent, parse_color_scheme};

    #[test]
    fn color_scheme_reply_parses() {
        assert_eq!(parse_color_scheme("   variant       variant          uint32 1\n"), Some(true));
        assert_eq!(parse_color_scheme("variant variant uint32 2"), Some(false));
        // 0 = no preference; garbage = None.
        assert_eq!(parse_color_scheme("variant variant uint32 0"), None);
        assert_eq!(parse_color_scheme("error: no such interface"), None);
    }

    #[test]
    fn accent_reply_parses_srgb_triplet() {
        let reply = "   variant       variant          struct {\n            double 0.568627\n            double 0.254902\n            double 0.67451\n         }\n";
        let c = parse_accent(reply).expect("accent");
        assert_eq!((c.r(), c.g(), c.b()), (145, 65, 172));
        assert_eq!(parse_accent("no numbers here"), None);
    }

    /// Diagnostic against the live session bus.
    #[test]
    #[ignore = "talks to the real session bus; run manually with --ignored --nocapture"]
    fn print_live_portal_values() {
        println!(
            "prefers_dark={:?} accent={:?}",
            super::os_prefers_dark(),
            super::os_accent_color()
        );
    }
}
