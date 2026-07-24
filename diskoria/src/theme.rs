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

#[cfg(windows)]
pub fn windows_accent_color() -> Option<Color32> {
    use windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor;
    unsafe {
        let mut colorization: u32 = 0;
        let mut opaque: i32 = 0;
        let hr = DwmGetColorizationColor(&mut colorization, &mut opaque);
        if hr != 0 {
            return None;
        }
        let r = ((colorization >> 16) & 0xFF) as u8;
        let g = ((colorization >> 8) & 0xFF) as u8;
        let b = (colorization & 0xFF) as u8;
        Some(Color32::from_rgb(r, g, b))
    }
}

#[cfg(not(windows))]
pub fn windows_accent_color() -> Option<Color32> {
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
    vis.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.txt_pri);
    vis.widgets.noninteractive.bg_stroke = Stroke::new(1.5, t.border);
    vis.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    vis.widgets.inactive.fg_stroke = Stroke::new(1.0, t.accent);
    vis.widgets.inactive.bg_stroke = Stroke::NONE;
    vis.widgets.hovered.bg_fill = t.hover;
    vis.widgets.hovered.fg_stroke = Stroke::new(1.0, t.accent);
    vis.widgets.hovered.bg_stroke = Stroke::NONE;
    vis.widgets.active.bg_fill = if dark {
        Color32::from_rgba_premultiplied(255, 255, 255, 30)
    } else {
        Color32::from_rgba_premultiplied(0, 0, 0, 25)
    };
    vis.widgets.active.fg_stroke = Stroke::new(1.0, t.accent);
    vis.selection.bg_fill =
        Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 120);
    vis.selection.stroke = Stroke::new(1.5, t.accent);
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
    fn theme_exposes_the_same_choice() {
        for &accent in crate::app_settings::ACCENT_PALETTE.iter() {
            for dark in [true, false] {
                assert_eq!(Theme::new(dark, accent).txt_on_accent, text_on(accent));
            }
        }
    }
}
