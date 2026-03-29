//! Window chrome: fonts, logo textures, custom title bar, Win32 resize + DWM rounding.

use egui::{Color32, FontFamily, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::theme::{Theme, BTN_W, CLOSE_HOVER_BG, TITLEBAR_H};

pub fn decode_png(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let decoder = png::Decoder::new(data);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let bytes = &buf[..info.buffer_size()];
    let w = info.width as usize;
    let h = info.height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => bytes.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        _ => return None,
    };
    Some((w, h, rgba))
}

pub fn setup_fonts(egui_ctx: &egui::Context) {
    static INTER_REGULAR: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
    static INTER_BOLD: &[u8] = include_bytes!("../fonts/Inter-Bold.ttf");
    static BOOTSTRAP_ICONS: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/bootstrap-icons.ttf"));

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        egui::FontData::from_static(INTER_REGULAR).into(),
    );
    fonts.font_data.insert(
        "InterBold".into(),
        egui::FontData::from_static(INTER_BOLD).into(),
    );
    fonts.font_data.insert(
        "BootstrapIcons".into(),
        egui::FontData::from_static(BOOTSTRAP_ICONS).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .splice(0..0, ["Inter".into(), "BootstrapIcons".into()]);
    fonts.families.insert(
        FontFamily::Name("InterBold".into()),
        vec!["InterBold".into(), "BootstrapIcons".into()],
    );
    egui_ctx.set_fonts(fonts);
}

/// Dark-mode logo plus RGB-inverted "light" variant.
pub fn load_logo_textures(
    ctx: &egui::Context,
    png_bytes: &[u8],
) -> (
    Option<egui::TextureHandle>,
    Option<egui::TextureHandle>,
    [usize; 2],
) {
    let Some((w, h, rgba)) = decode_png(png_bytes) else {
        return (None, None, [0, 0]);
    };
    let size = [w, h];
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    let logo = ctx.load_texture("diskoria_logo", color_image, egui::TextureOptions::LINEAR);
    let rgba_inv: Vec<u8> = rgba
        .chunks(4)
        .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]])
        .collect();
    let img_inv = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba_inv);
    let logo_light = ctx.load_texture("diskoria_logo_light", img_inv, egui::TextureOptions::LINEAR);
    (Some(logo), Some(logo_light), size)
}

/// Single texture from PNG bytes (e.g. `appicon.png` for About page).
pub fn load_appicon_texture(
    ctx: &egui::Context,
    png_bytes: &[u8],
) -> Option<egui::TextureHandle> {
    let (w, h, rgba) = decode_png(png_bytes)?;
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    Some(ctx.load_texture(
        "diskoria_about_appicon",
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

#[cfg(windows)]
pub fn shell_open_uri(uri: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", uri])
        .spawn();
}

#[cfg(not(windows))]
pub fn shell_open_uri(_uri: &str) {}

pub const INTERACT_MANUAL_FOCUS: Sense = Sense::CLICK;

/// Apply Win11 rounded corners via DWM (silent no-op on older OS or Windows PE).
#[cfg(windows)]
pub fn apply_win11_rounded_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    const DWMWCP_ROUND: u32 = 2;
    let pref = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn apply_win11_rounded_corners(_hwnd: isize) {}

#[cfg(windows)]
mod win32_resize {
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GetSystemMetrics, GetWindowLongW, GetWindowRect, IsZoomed,
        SetWindowLongPtrW, SetWindowLongW, SetWindowPos, WM_NCHITTEST,
    };

    const GWL_STYLE: i32 = -16;
    const GWLP_WNDPROC: i32 = -4;

    const WS_THICKFRAME: u32 = 0x00040000;
    const WS_MAXIMIZEBOX: u32 = 0x00010000;
    const WS_MINIMIZEBOX: u32 = 0x00020000;

    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_FRAMECHANGED: u32 = 0x0020;

    const SM_CXSIZEFRAME: i32 = 32;
    const SM_CYSIZEFRAME: i32 = 33;

    const HTLEFT: isize = 10;
    const HTRIGHT: isize = 11;
    const HTBOTTOM: isize = 15;
    const HTBOTTOMLEFT: isize = 16;
    const HTBOTTOMRIGHT: isize = 17;

    type PrevWndProc = Option<unsafe extern "system" fn(isize, u32, usize, isize) -> isize>;
    static PREV_WNDPROC: OnceLock<PrevWndProc> = OnceLock::new();

    #[inline]
    fn x_from_lparam(lparam: isize) -> i32 {
        let lo = (lparam & 0xFFFF) as u16;
        lo as i16 as i32
    }

    #[inline]
    fn y_from_lparam(lparam: isize) -> i32 {
        let hi = ((lparam >> 16) & 0xFFFF) as u16;
        hi as i16 as i32
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: isize, lparam: isize) -> isize {
        let prev = PREV_WNDPROC.get().copied().unwrap_or(None);
        if msg == WM_NCHITTEST && IsZoomed(hwnd) == 0 {
            const HTCLIENT: isize = 1;

            let cx = x_from_lparam(lparam);
            let cy = y_from_lparam(lparam);
            let mut rect_for_controls: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rect_for_controls) != 0 {
                const TITLEBAR_H: i32 = 32;
                const BTN_W: i32 = 46;
                const CONTROLS_W: i32 = 3 * BTN_W;
                let in_top = cy >= rect_for_controls.top && cy < rect_for_controls.top + TITLEBAR_H;
                let in_controls =
                    cx >= rect_for_controls.right - CONTROLS_W && cx < rect_for_controls.right;
                if in_top && in_controls {
                    return HTCLIENT;
                }
            }

            let def_ht = DefWindowProcW(hwnd, msg, wparam as usize, lparam);
            if def_ht != 0 && def_ht != HTCLIENT {
                return def_ht;
            }

            let x = x_from_lparam(lparam);
            let y = y_from_lparam(lparam);
            let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rect) != 0 {
                let border_x = GetSystemMetrics(SM_CXSIZEFRAME).max(1);
                let border_y = GetSystemMetrics(SM_CYSIZEFRAME).max(1);
                const CORNER_PAD: i32 = 2;
                let corner_border_x = (border_x + CORNER_PAD).max(1);
                let corner_border_y = (border_y + CORNER_PAD).max(1);

                let left_edge = rect.left + border_x;
                let right_edge = rect.right - border_x;
                let bottom_edge = rect.bottom - border_y;

                let on_left_edge = x >= rect.left && x < left_edge;
                let on_right_edge = x < rect.right && x >= right_edge;
                let on_bottom_edge = y < rect.bottom && y >= bottom_edge;

                let left_corner = rect.left + corner_border_x;
                let right_corner = rect.right - corner_border_x;
                let bottom_corner = rect.bottom - corner_border_y;

                let on_left_corner = x >= rect.left && x < left_corner;
                let on_right_corner = x < rect.right && x >= right_corner;
                let on_bottom_corner = y < rect.bottom && y >= bottom_corner;

                let ht = if on_left_corner && on_bottom_corner {
                    HTBOTTOMLEFT
                } else if on_right_corner && on_bottom_corner {
                    HTBOTTOMRIGHT
                } else if on_left_edge {
                    HTLEFT
                } else if on_right_edge {
                    HTRIGHT
                } else if on_bottom_edge {
                    HTBOTTOM
                } else {
                    0
                };

                if ht != 0 {
                    return ht;
                }
            }
        }

        if let Some(prev_fn) = prev {
            CallWindowProcW(Some(prev_fn), hwnd, msg, wparam as usize, lparam)
        } else {
            DefWindowProcW(hwnd, msg, wparam as usize, lparam)
        }
    }

    pub(crate) unsafe fn install(hwnd: HWND) {
        if hwnd == 0 {
            return;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let desired = style | WS_THICKFRAME | WS_MAXIMIZEBOX | WS_MINIMIZEBOX;
        if desired != style {
            let _ = SetWindowLongW(hwnd, GWL_STYLE, desired as i32);
            let _ = SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }

        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as isize);
        let prev_fn: PrevWndProc = if prev == 0 {
            None
        } else {
            Some(std::mem::transmute(prev))
        };
        let _ = PREV_WNDPROC.set(prev_fn);
    }
}

/// Hook WM_NCHITTEST for resize borders on an undecorated window.
#[cfg(windows)]
pub fn install_win32_resize(hwnd: isize) {
    use windows_sys::Win32::Foundation::HWND;
    unsafe {
        win32_resize::install(hwnd as HWND);
    }
}

pub fn draw_titlebar(ctx: &egui::Context, t: &Theme) {
    let maximized = ctx
        .input(|i| i.viewport().maximized)
        .unwrap_or(false);

    let screen = ctx.screen_rect();
    let controls_w = 3.0 * BTN_W;
    let close_rect = Rect::from_min_size(
        Pos2::new(screen.right() - BTN_W, screen.top()),
        Vec2::new(BTN_W, TITLEBAR_H),
    );
    let max_rect_area = Rect::from_min_size(
        Pos2::new(screen.right() - 2.0 * BTN_W, screen.top()),
        Vec2::new(BTN_W, TITLEBAR_H),
    );
    let min_rect = Rect::from_min_size(
        Pos2::new(screen.right() - controls_w, screen.top()),
        Vec2::new(BTN_W, TITLEBAR_H),
    );
    let drag_rect = Rect::from_min_size(
        screen.left_top(),
        Vec2::new(screen.width() - controls_w, TITLEBAR_H),
    );

    egui::Area::new(Id::new("diskoria_tb_drag"))
        .fixed_pos(drag_rect.min)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let drag_r = ui.interact(drag_rect, Id::new("diskoria_tb_drag_i"), Sense::click_and_drag());
            if drag_r.dragged() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag_r.double_clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
        });

    egui::Area::new(Id::new("diskoria_tb_controls"))
        .fixed_pos(min_rect.left_top())
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let min_r = ui.interact(min_rect, Id::new("diskoria_tb_min"), INTERACT_MANUAL_FOCUS);
            let max_r = ui.interact(max_rect_area, Id::new("diskoria_tb_max"), INTERACT_MANUAL_FOCUS);
            let close_r = ui.interact(close_rect, Id::new("diskoria_tb_close"), INTERACT_MANUAL_FOCUS);

            if min_r.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            if max_r.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            if close_r.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }

            if min_r.hovered() {
                ui.painter().rect_filled(min_rect, 0.0, t.hover);
            }
            if max_r.hovered() {
                ui.painter().rect_filled(max_rect_area, 0.0, t.hover);
            }
            if close_r.hovered() {
                ui.painter().rect_filled(close_rect, 0.0, CLOSE_HOVER_BG);
            }

            let mc = min_rect.center();
            ui.painter().line_segment(
                [Pos2::new(mc.x - 5.0, mc.y), Pos2::new(mc.x + 5.0, mc.y)],
                Stroke::new(1.5, t.txt_pri),
            );

            let mxc = max_rect_area.center();
            if maximized {
                let back =
                    Rect::from_min_size(Pos2::new(mxc.x - 3.0, mxc.y - 5.0), Vec2::splat(8.0));
                let front =
                    Rect::from_min_size(Pos2::new(mxc.x - 5.0, mxc.y - 3.0), Vec2::splat(8.0));
                ui.painter().rect_stroke(
                    back,
                    0.0,
                    Stroke::new(1.5, t.txt_pri),
                    StrokeKind::Middle,
                );
                ui.painter().rect_filled(front, 0.0, t.bg_pri);
                ui.painter().rect_stroke(
                    front,
                    0.0,
                    Stroke::new(1.5, t.txt_pri),
                    StrokeKind::Middle,
                );
            } else {
                let sq = Rect::from_center_size(mxc, Vec2::splat(10.0));
                ui.painter().rect_stroke(
                    sq,
                    0.0,
                    Stroke::new(1.5, t.txt_pri),
                    StrokeKind::Middle,
                );
            }

            let close_col = if close_r.hovered() {
                Color32::WHITE
            } else {
                t.txt_pri
            };
            let cc = close_rect.center();
            let d = 5.0_f32;
            ui.painter().line_segment(
                [Pos2::new(cc.x - d, cc.y - d), Pos2::new(cc.x + d, cc.y + d)],
                Stroke::new(1.5, close_col),
            );
            ui.painter().line_segment(
                [Pos2::new(cc.x + d, cc.y - d), Pos2::new(cc.x - d, cc.y + d)],
                Stroke::new(1.5, close_col),
            );
        });
}
