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

// ── Responsive nav ────────────────────────────────────────────────────────────
// The sidebar has three forms; which one a window draws depends only on that
// window's own width, so two windows at different sizes disagree happily. See
// `docs/gui-architecture.md` § Responsive nav.

/// Centre of the icon column, and left edge of the label, within a nav row.
/// Shared by all three nav forms so a row's icon lands in the same place no
/// matter which one is drawn.
pub const NAV_ICON_X: f32 = 24.0;
pub const NAV_LABEL_X: f32 = 52.0;

/// Width of the collapsed icon rail (no labels; hover opens [`RAIL_FLYOUT_W`]).
///
/// Exactly twice [`NAV_ICON_X`], which is what makes the rail's icons sit at
/// once *centred in the rail* and *at the same x as the expanded overlay's*.
/// A wider rail — it was 72px, sized to fit the wordmark logo — centres the
/// icons further right, so they visibly jump left the moment the panel opens.
pub const RAIL_W: f32 = 2.0 * NAV_ICON_X;
/// Width of the panel the rail expands to on hover. Drawn as a foreground
/// overlay *over* the content — the central panel never reflows for a hover.
pub const RAIL_FLYOUT_W: f32 = 220.0;
/// At or above this window width the full 240px sidebar is drawn.
pub const BP_RAIL: f32 = 900.0;
/// Below this window width the sidebar disappears entirely and the title bar
/// grows a hamburger that opens a full-window menu.
pub const BP_MOBILE: f32 = 560.0;

// Compile-time invariants: the overlay has to be wider than the rail it covers
// (or it reveals nothing) and narrower than the full sidebar (or the collapsed
// form is the wider of the two the moment it opens).
const _: () = assert!(RAIL_FLYOUT_W > RAIL_W);
const _: () = assert!(RAIL_FLYOUT_W < SIDE_PANEL_W);
const _: () = assert!(BP_RAIL > BP_MOBILE);
// The rail is icon-only, so its whole width is the icon column.
const _: () = assert!(RAIL_W == 2.0 * NAV_ICON_X);
// Labels have to clear the icons they sit beside.
const _: () = assert!(NAV_LABEL_X > NAV_ICON_X);

/// Which form of the nav a window draws. Derived from the window width by
/// [`nav_mode`] every frame — never store it, and never branch on a raw width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavMode {
    /// 240px sidebar with logo, wordmark and labelled rows.
    Full,
    /// 72px icon rail; hover expands a labelled overlay.
    Rail,
    /// No sidebar; hamburger in the title bar opens a full-window menu.
    Mobile,
}

/// Pick the nav form for a window of `screen_w` logical pixels.
///
/// Deliberately has no hysteresis: the mode never feeds back into the window
/// width, so a drag that parks the edge exactly on a breakpoint can flip the
/// sidebar but cannot oscillate.
pub fn nav_mode(screen_w: f32) -> NavMode {
    if screen_w >= BP_RAIL {
        NavMode::Full
    } else if screen_w >= BP_MOBILE {
        NavMode::Rail
    } else {
        NavMode::Mobile
    }
}

impl NavMode {
    /// Width the sidebar occupies in the layout. Zero in [`NavMode::Mobile`];
    /// the hover overlay is *not* counted — it floats over the content.
    pub fn side_panel_w(self) -> f32 {
        match self {
            NavMode::Full => SIDE_PANEL_W,
            NavMode::Rail => RAIL_W,
            NavMode::Mobile => 0.0,
        }
    }
}

/// Horizontal page margin. Tightened on phone-sized windows, where 24px a side
/// is 13% of the window.
pub fn content_margin(mode: NavMode) -> f32 {
    match mode {
        NavMode::Mobile => 16.0,
        _ => CONTENT_MARGIN,
    }
}

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

    /// How often the background worker re-reads the portal.
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

    /// Refresh the cache. **Blocks** — it spawns `dbus-send` — so this only
    /// ever runs on the background worker or once at startup, never on the UI
    /// thread.
    fn refresh_blocking() {
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

    /// Read the portal once, then keep the cache fresh from a background
    /// thread.
    ///
    /// Reading it inline was a UI-thread stall waiting for a subprocess: an
    /// `exec` has to fault the binary in off disk, and a sector scan of the
    /// boot drive starves exactly that, freezing every window for seconds
    /// while the worker threads kept logging progress (KI-42). The UI now only
    /// ever reads memory.
    pub fn spawn_refresh_worker() {
        static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        STARTED.get_or_init(|| {
            // One synchronous read before the window exists, so the very first
            // frame already has the right theme.
            refresh_blocking();
            std::thread::Builder::new()
                .name("diskoria-portal".into())
                .spawn(|| loop {
                    std::thread::sleep(CACHE_FOR);
                    refresh_blocking();
                })
                .expect("spawn portal worker");
        });
    }

    /// Non-blocking: whatever the worker last saw.
    pub fn prefers_dark() -> Option<bool> {
        CACHE.lock().expect("portal cache poisoned").prefers_dark
    }

    /// Non-blocking: whatever the worker last saw.
    pub fn accent_color() -> Option<Color32> {
        CACHE.lock().expect("portal cache poisoned").accent
    }
}

#[cfg(target_os = "linux")]
pub fn os_accent_color() -> Option<Color32> {
    linux_portal::accent_color()
}

/// Start the background worker that keeps the desktop-appearance cache fresh.
/// Call once at startup; every later read is a memory read.
#[cfg(target_os = "linux")]
pub fn spawn_portal_refresh() {
    linux_portal::spawn_refresh_worker();
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
    fn rail_icons_do_not_move_when_the_overlay_opens() {
        // The rail's icon is centred in the rail; the overlay's is at
        // NAV_ICON_X from the same left edge. They must be the same point, or
        // hovering makes every icon jump sideways.
        let left = 0.0_f32;
        let rail_icon_x = left + RAIL_W * 0.5;
        let flyout_icon_x = left + NAV_ICON_X;
        assert_eq!(rail_icon_x, flyout_icon_x);
    }

    #[test]
    fn nav_mode_breakpoints_are_half_open_from_below() {
        // Boundaries belong to the *wider* mode: exactly 900 is Full, exactly
        // 560 is Rail. One pixel narrower drops to the next form down.
        assert_eq!(nav_mode(1920.0), NavMode::Full);
        assert_eq!(nav_mode(BP_RAIL), NavMode::Full);
        assert_eq!(nav_mode(BP_RAIL - 1.0), NavMode::Rail);
        assert_eq!(nav_mode(BP_MOBILE), NavMode::Rail);
        assert_eq!(nav_mode(BP_MOBILE - 1.0), NavMode::Mobile);
        // The narrowest window the platform will let us have, and a degenerate
        // width from a minimized/zero-sized surface.
        assert_eq!(nav_mode(380.0), NavMode::Mobile);
        assert_eq!(nav_mode(0.0), NavMode::Mobile);
    }

    #[test]
    fn every_mode_leaves_room_for_content() {
        // The point of the exercise: at the narrowest window each mode can be
        // asked to draw, the central panel still gets a usable width. 320px is
        // roughly two cards' worth of readable text at the card padding.
        for (mode, narrowest) in [
            (NavMode::Full, BP_RAIL),
            (NavMode::Rail, BP_MOBILE),
            (NavMode::Mobile, 380.0),
        ] {
            let content = narrowest - mode.side_panel_w() - 2.0 * content_margin(mode);
            assert!(
                content >= 320.0,
                "{mode:?} at {narrowest}px leaves only {content}px of content"
            );
        }
    }

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
