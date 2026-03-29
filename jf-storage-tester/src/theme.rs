//! Theme tokens aligned with `WINDOWS11_EGUI_STYLE_GUIDE.md` (winui_egui_example / Copynaut).

use egui::{Color32, Stroke};

pub const TITLEBAR_H: f32 = 32.0;
pub const BTN_W: f32 = 46.0;
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

pub struct Theme {
    pub bg_pri: Color32,
    pub bg_sec: Color32,
    pub sb_bg: Color32,
    pub accent: Color32,
    pub txt_pri: Color32,
    pub txt_sec: Color32,
    #[allow(dead_code)]
    pub txt_on_accent: Color32,
    pub border: Color32,
    pub hover: Color32,
}

impl Theme {
    pub fn new(dark: bool, accent: Color32) -> Self {
        let acc_r = accent.r() as f32 / 255.0;
        let acc_g = accent.g() as f32 / 255.0;
        let acc_b = accent.b() as f32 / 255.0;
        let lum = 0.2126 * acc_r + 0.7152 * acc_g + 0.0722 * acc_b;
        let txt_on_accent = if lum > 0.6 {
            Color32::from_rgb(0, 0, 0)
        } else {
            Color32::WHITE
        };

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
