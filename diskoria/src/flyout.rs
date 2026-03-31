//! Pro-Monitoring: drive health flyout popup window.
//!
//! A small borderless, always-on-top egui window that appears near the system
//! tray when the user clicks a drive icon.  It auto-dismisses on focus loss.

use std::num::NonZeroU32;
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowLevel};

use crate::monitor::DriveSnapshot;
use crate::tex_mgr::TextureManager;
use crate::theme::Theme;

// ── FlyoutRenderer ────────────────────────────────────────────────────────────

pub struct FlyoutRenderer {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    tex_mgr: TextureManager,
    pub snapshot: DriveSnapshot,
    pub accent_color: egui::Color32,
}

impl FlyoutRenderer {
    /// Create the flyout window.  Positions it just above the tray icon that was
    /// hovered, falling back to the bottom-right corner if no rect is provided.
    pub fn new(
        event_loop: &ActiveEventLoop,
        snapshot: DriveSnapshot,
        icon_rect: Option<tray_icon::Rect>,
        accent_color: egui::Color32,
    ) -> Option<Self> {
        let egui_ctx = egui::Context::default();
        crate::chrome::setup_fonts(&egui_ctx);
        use winit::platform::windows::WindowAttributesExtWindows;

        let attrs = Window::default_attributes()
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(280_u32, 168_u32))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_title("Diskoria Health");

        let window = Arc::new(event_loop.create_window(attrs).ok()?);

        // Set WS_EX_NOACTIVATE so the flyout never steals keyboard focus.
        // Without this, showing the window resets the tray icon TrackMouseEvent
        // registration, causing Leave events to stop firing reliably.
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
            };
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE as isize);
                }
            }
        }

        // Position just above the hovered tray icon, or fall back to the screen corner.
        let scale = window.current_monitor()
            .or_else(|| window.primary_monitor())
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let win_w = (280.0 * scale) as i32;
        let win_h = (168.0 * scale) as i32;
        let gap = (6.0 * scale) as i32;

        let pos = if let Some(rect) = icon_rect {
            // Center the flyout horizontally over the icon, above it.
            let icon_center_x = rect.position.x as i32 + rect.size.width as i32 / 2;
            let x = icon_center_x - win_w / 2;
            let y = rect.position.y as i32 - win_h - gap;
            // Clamp to keep flyout on screen.
            let screen_w = window.current_monitor()
                .or_else(|| window.primary_monitor())
                .map(|m| m.size().width as i32)
                .unwrap_or(1920);
            let x = x.clamp(0, (screen_w - win_w).max(0));
            winit::dpi::PhysicalPosition::new(x, y.max(0))
        } else if let Some(monitor) = window.primary_monitor().or_else(|| window.current_monitor()) {
            let screen = monitor.size();
            let taskbar_h = (40.0 * scale) as i32;
            let x = screen.width as i32 - win_w - 10;
            let y = screen.height as i32 - win_h - taskbar_h - 8;
            winit::dpi::PhysicalPosition::new(x.max(0), y.max(0))
        } else {
            winit::dpi::PhysicalPosition::new(0, 0)
        };
        window.set_outer_position(pos);

        let ctx_sb = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&ctx_sb, window.clone()).ok()?;

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::from_hash_of("flyout"),
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        Some(FlyoutRenderer {
            window,
            surface,
            egui_ctx,
            egui_state,
            tex_mgr: TextureManager::new(),
            snapshot,
            accent_color,
        })
    }

    /// Handle winit events for the flyout window.
    /// Returns `true` if the flyout should be closed (focus lost).
    pub fn handle_event(&mut self, event: &WindowEvent) -> bool {
        let resp = self.egui_state.on_window_event(&self.window, event);
        if resp.repaint {
            self.window.request_redraw();
        }
        match event {
            WindowEvent::RedrawRequested => {
                self.paint();
                false
            }
            _ => false,
        }
    }

    /// Paint the flyout content.
    pub fn paint(&mut self) {
        let size = self.window.inner_size();
        let w = size.width;
        let h = size.height;
        if w == 0 || h == 0 {
            return;
        }

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let dark = matches!(self.window.theme(), Some(winit::window::Theme::Dark));
        let t = Theme::new(dark, self.accent_color);

        let snap = self.snapshot.clone();

        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            draw_flyout_ui(ctx, &snap, &t, dark);
        });

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);

        self.tex_mgr.update(&full_output.textures_delta);

        let ppp = full_output.pixels_per_point;
        let clipped = self.egui_ctx.tessellate(full_output.shapes, ppp);

        let bg = t.bg_pri;
        let bg32 = crate::to_bgra(bg.r(), bg.g(), bg.b(), 255);
        let mut pixels: Vec<u32> = vec![bg32; (w * h) as usize];

        for prim in &clipped {
            let clip = prim.clip_rect;
            let clip_x0 = (clip.min.x * ppp).floor() as i32;
            let clip_y0 = (clip.min.y * ppp).floor() as i32;
            let clip_x1 = (clip.max.x * ppp).ceil() as i32;
            let clip_y1 = (clip.max.y * ppp).ceil() as i32;

            if let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive {
                let verts = &mesh.vertices;
                let indices = &mesh.indices;
                let is_font_tex = mesh.texture_id == egui::TextureId::default();

                let tri_count = indices.len() / 3;
                for tri in 0..tri_count {
                    let i0 = indices[tri * 3] as usize;
                    let i1 = indices[tri * 3 + 1] as usize;
                    let i2 = indices[tri * 3 + 2] as usize;
                    if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() {
                        continue;
                    }
                    let v0 = &verts[i0];
                    let v1 = &verts[i1];
                    let v2 = &verts[i2];

                    let p0 = (v0.pos.x * ppp, v0.pos.y * ppp);
                    let p1 = (v1.pos.x * ppp, v1.pos.y * ppp);
                    let p2 = (v2.pos.x * ppp, v2.pos.y * ppp);

                    let min_x = p0.0.min(p1.0).min(p2.0).floor() as i32;
                    let min_y = p0.1.min(p1.1).min(p2.1).floor() as i32;
                    let max_x = p0.0.max(p1.0).max(p2.0).ceil() as i32;
                    let max_y = p0.1.max(p1.1).max(p2.1).ceil() as i32;

                    let x0 = min_x.max(clip_x0).max(0);
                    let y0 = min_y.max(clip_y0).max(0);
                    let x1 = max_x.min(clip_x1).min(w as i32 - 1);
                    let y1 = max_y.min(clip_y1).min(h as i32 - 1);

                    let denom = (p1.1 - p2.1) * (p0.0 - p2.0) + (p2.0 - p1.0) * (p0.1 - p2.1);
                    if denom.abs() < 0.001 {
                        continue;
                    }

                    for py in y0..=y1 {
                        for px in x0..=x1 {
                            let fx = px as f32 + 0.5;
                            let fy = py as f32 + 0.5;

                            let w0 = ((p1.1 - p2.1) * (fx - p2.0) + (p2.0 - p1.0) * (fy - p2.1)) / denom;
                            let w1 = ((p2.1 - p0.1) * (fx - p2.0) + (p0.0 - p2.0) * (fy - p2.1)) / denom;
                            let w2 = 1.0 - w0 - w1;

                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }

                            let uv_x = v0.uv.x * w0 + v1.uv.x * w1 + v2.uv.x * w2;
                            let uv_y = v0.uv.y * w0 + v1.uv.y * w1 + v2.uv.y * w2;

                            let vr = v0.color.r() as f32 * w0 + v1.color.r() as f32 * w1 + v2.color.r() as f32 * w2;
                            let vg = v0.color.g() as f32 * w0 + v1.color.g() as f32 * w1 + v2.color.g() as f32 * w2;
                            let vb = v0.color.b() as f32 * w0 + v1.color.b() as f32 * w1 + v2.color.b() as f32 * w2;
                            let va = v0.color.a() as f32 * w0 + v1.color.a() as f32 * w1 + v2.color.a() as f32 * w2;

                            let (r, g, b, final_a) = if is_font_tex {
                                let cov = self.tex_mgr.sample_alpha_f(uv_x, uv_y);
                                (vr, vg, vb, va / 255.0 * cov)
                            } else {
                                let [tr, tg, tb, ta] = self.tex_mgr.sample_rgba(mesh.texture_id, uv_x, uv_y);
                                (tr * vr, tg * vg, tb * vb, ta * va / 255.0)
                            };

                            if final_a < 1.0 / 255.0 {
                                continue;
                            }

                            let idx = (py as u32 * w + px as u32) as usize;
                            let dst = pixels[idx];
                            let dr = ((dst >> 16) & 0xFF) as f32;
                            let dg = ((dst >> 8) & 0xFF) as f32;
                            let db = (dst & 0xFF) as f32;

                            let inv = 1.0 - final_a;
                            let r_lin = (r / 255.0) * (r / 255.0);
                            let g_lin = (g / 255.0) * (g / 255.0);
                            let b_lin = (b / 255.0) * (b / 255.0);
                            let dr_lin = (dr / 255.0) * (dr / 255.0);
                            let dg_lin = (dg / 255.0) * (dg / 255.0);
                            let db_lin = (db / 255.0) * (db / 255.0);
                            let out_r = ((r_lin * final_a + dr_lin * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_g = ((g_lin * final_a + dg_lin * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_b = ((b_lin * final_a + db_lin * inv).sqrt() * 255.0 + 0.5) as u8;

                            pixels[idx] = crate::to_bgra(out_r, out_g, out_b, 255);
                        }
                    }
                }
            }
        }

        if self
            .surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .is_ok()
        {
            if let Ok(mut buf) = self.surface.buffer_mut() {
                buf.copy_from_slice(&pixels);
                let _ = buf.present();
            }
        }
    }
}

// ── Flyout UI content ─────────────────────────────────────────────────────────

fn draw_flyout_ui(ctx: &egui::Context, snap: &DriveSnapshot, t: &Theme, _dark: bool) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(t.bg_pri))
        .show(ctx, |ui| {
            ui.set_min_size(Vec2::new(280.0, 168.0));
            let pad = 12.0;

            // Thin accent border
            ui.painter().rect_stroke(
                Rect::from_min_size(Pos2::ZERO, Vec2::new(280.0, 168.0)),
                0.0,
                egui::Stroke::new(1.5, t.accent),
                egui::StrokeKind::Inside,
            );

            // Drive model (truncated)
            let model = if snap.model.len() > 34 {
                format!("{}…", &snap.model[..33])
            } else {
                snap.model.clone()
            };
            ui.add_space(pad);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(RichText::new(model).size(11.0).color(t.txt_sec));
            });

            ui.add_space(4.0);

            // Temperature (large, color-coded)
            let (temp_str, temp_color) = match snap.temp_c {
                None => ("—°C".to_string(), t.txt_sec),
                Some(t_val) => {
                    let color = if t_val >= 70 {
                        Color32::from_rgb(231, 76, 60)
                    } else if t_val >= 60 {
                        Color32::from_rgb(230, 126, 34)
                    } else if t_val >= 45 {
                        Color32::from_rgb(241, 196, 15)
                    } else {
                        Color32::from_rgb(39, 174, 96)
                    };
                    (format!("{}°C", t_val), color)
                }
            };

            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    RichText::new(temp_str)
                        .font(FontId::new(36.0, egui::FontFamily::Proportional))
                        .color(temp_color),
                );
            });

            ui.add_space(4.0);

            // Wear level row
            if let Some(wear) = snap.wear_pct {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.label(RichText::new(format!("Wear: {}%", wear)).size(11.0).color(t.txt_sec));
                });
            }

            // Health summary
            let summary_color = if snap.health_summary.to_lowercase().contains("critical")
                || snap.health_summary.to_lowercase().contains("warn")
            {
                Color32::from_rgb(231, 76, 60)
            } else {
                Color32::from_rgb(39, 174, 96)
            };
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    RichText::new(&snap.health_summary)
                        .size(11.0)
                        .color(summary_color),
                );
            });

            // Last updated
            if snap.last_updated_unix > 0 {
                let ago = chrono::Utc::now().timestamp() - snap.last_updated_unix;
                let ago_str = if ago < 60 {
                    format!("Updated {}s ago", ago)
                } else if ago < 3600 {
                    format!("Updated {}m ago", ago / 60)
                } else {
                    format!("Updated {}h ago", ago / 3600)
                };
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.label(RichText::new(ago_str).size(10.0).color(t.txt_sec));
                });
            }
        });
}

// ── ContextMenuWindow ─────────────────────────────────────────────────────────

/// Action returned when the user clicks a context menu item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContextMenuAction {
    Open,
    Quit,
}

/// Themed right-click context menu for the app tray icon.
///
/// Uses the same egui + softbuffer rasterizer as `FlyoutRenderer` so it
/// follows the app's accent color and dark/light theme.
pub struct ContextMenuWindow {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    tex_mgr: TextureManager,
    pub accent_color: egui::Color32,
    /// Becomes true once the cursor has been inside the window bounds.
    /// Cursor-leave dismissal is suppressed until then, preventing the menu
    /// from being destroyed immediately after creation (cursor starts at the
    /// tray bar below the window).
    cursor_entered: bool,
}

// Logical dimensions of the context menu.
const CM_W: f32 = 180.0;
const CM_H: f32 = 92.0; // 6 + 36 + 8 + 36 + 6

impl ContextMenuWindow {
    /// Create the context menu window, positioned just above `cursor_pos`
    /// (physical-pixel coordinates from the tray-icon click event).
    pub fn new(
        event_loop: &ActiveEventLoop,
        cursor_pos: winit::dpi::PhysicalPosition<f64>,
        accent_color: egui::Color32,
    ) -> Option<Self> {
        let egui_ctx = egui::Context::default();
        crate::chrome::setup_fonts(&egui_ctx);

        use winit::platform::windows::WindowAttributesExtWindows;
        let attrs = Window::default_attributes()
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(CM_W as u32, CM_H as u32))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_title("Diskoria Menu");

        let window = Arc::new(event_loop.create_window(attrs).ok()?);

        // WS_EX_NOACTIVATE — must not steal focus from the main window.
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
            };
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE as isize);
                }
            }
        }

        // Position the menu above-left of the cursor, clamped to screen.
        let scale = window.current_monitor()
            .or_else(|| window.primary_monitor())
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let win_w = (CM_W as f64 * scale) as i32;
        let win_h = (CM_H as f64 * scale) as i32;
        let gap = (4.0 * scale) as i32;

        let cx = cursor_pos.x as i32;
        let cy = cursor_pos.y as i32;
        let mut x = cx;
        let mut y = cy - win_h - gap;

        // Clamp to screen bounds.
        if let Some(monitor) = window.current_monitor().or_else(|| window.primary_monitor()) {
            let sw = monitor.size().width as i32;
            let sh = monitor.size().height as i32;
            x = x.clamp(0, (sw - win_w).max(0));
            y = y.clamp(0, (sh - win_h).max(0));
        }
        window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));

        let ctx_sb = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&ctx_sb, window.clone()).ok()?;

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::from_hash_of("context_menu"),
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        Some(ContextMenuWindow {
            window,
            surface,
            egui_ctx,
            egui_state,
            tex_mgr: TextureManager::new(),
            accent_color,
            cursor_entered: false,
        })
    }

    /// Handle a winit window event. Returns the action if an item was clicked.
    pub fn handle_event(&mut self, event: &WindowEvent) -> Option<ContextMenuAction> {
        let resp = self.egui_state.on_window_event(&self.window, event);
        if resp.repaint {
            self.window.request_redraw();
        }
        match event {
            WindowEvent::RedrawRequested => self.paint(),
            _ => None,
        }
    }

    /// Call every poll tick with the current cursor position (physical pixels).
    /// Returns `true` when the menu should be dismissed (cursor left after having entered).
    pub fn poll_cursor(&mut self, cursor_x: i32, cursor_y: i32) -> bool {
        let pos = self.window.outer_position().unwrap_or_default();
        let size = self.window.outer_size();
        let left   = pos.x;
        let top    = pos.y;
        let right  = pos.x + size.width as i32;
        let bottom = pos.y + size.height as i32;
        let inside = cursor_x >= left && cursor_x < right && cursor_y >= top && cursor_y < bottom;
        if inside {
            self.cursor_entered = true;
        }
        // Only close once the cursor has actually been over the menu and then left.
        self.cursor_entered && !inside
    }

    fn paint(&mut self) -> Option<ContextMenuAction> {
        let size = self.window.inner_size();
        let w = size.width;
        let h = size.height;
        if w == 0 || h == 0 {
            return None;
        }

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let dark = matches!(self.window.theme(), Some(winit::window::Theme::Dark));
        let t = crate::theme::Theme::new(dark, self.accent_color);

        let mut action: Option<ContextMenuAction> = None;
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            action = draw_context_menu_ui(ctx, &t);
        });

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);
        self.tex_mgr.update(&full_output.textures_delta);

        let ppp = full_output.pixels_per_point;
        let clipped = self.egui_ctx.tessellate(full_output.shapes, ppp);

        let bg = t.bg_pri;
        let bg32 = crate::to_bgra(bg.r(), bg.g(), bg.b(), 255);
        let mut pixels: Vec<u32> = vec![bg32; (w * h) as usize];

        for prim in &clipped {
            let clip = prim.clip_rect;
            let clip_x0 = (clip.min.x * ppp).floor() as i32;
            let clip_y0 = (clip.min.y * ppp).floor() as i32;
            let clip_x1 = (clip.max.x * ppp).ceil() as i32;
            let clip_y1 = (clip.max.y * ppp).ceil() as i32;

            if let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive {
                let verts = &mesh.vertices;
                let indices = &mesh.indices;
                let is_font_tex = mesh.texture_id == egui::TextureId::default();

                for tri in 0..(indices.len() / 3) {
                    let i0 = indices[tri * 3] as usize;
                    let i1 = indices[tri * 3 + 1] as usize;
                    let i2 = indices[tri * 3 + 2] as usize;
                    if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() { continue; }
                    let v0 = &verts[i0]; let v1 = &verts[i1]; let v2 = &verts[i2];

                    let p0 = (v0.pos.x * ppp, v0.pos.y * ppp);
                    let p1 = (v1.pos.x * ppp, v1.pos.y * ppp);
                    let p2 = (v2.pos.x * ppp, v2.pos.y * ppp);

                    let min_x = p0.0.min(p1.0).min(p2.0).floor() as i32;
                    let min_y = p0.1.min(p1.1).min(p2.1).floor() as i32;
                    let max_x = p0.0.max(p1.0).max(p2.0).ceil() as i32;
                    let max_y = p0.1.max(p1.1).max(p2.1).ceil() as i32;

                    let x0 = min_x.max(clip_x0).max(0);
                    let y0 = min_y.max(clip_y0).max(0);
                    let x1 = max_x.min(clip_x1).min(w as i32 - 1);
                    let y1 = max_y.min(clip_y1).min(h as i32 - 1);

                    let denom = (p1.1 - p2.1) * (p0.0 - p2.0) + (p2.0 - p1.0) * (p0.1 - p2.1);
                    if denom.abs() < 0.001 { continue; }

                    for py in y0..=y1 {
                        for px in x0..=x1 {
                            let fx = px as f32 + 0.5;
                            let fy = py as f32 + 0.5;
                            let w0 = ((p1.1 - p2.1) * (fx - p2.0) + (p2.0 - p1.0) * (fy - p2.1)) / denom;
                            let w1 = ((p2.1 - p0.1) * (fx - p2.0) + (p0.0 - p2.0) * (fy - p2.1)) / denom;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 { continue; }

                            let uv_x = v0.uv.x * w0 + v1.uv.x * w1 + v2.uv.x * w2;
                            let uv_y = v0.uv.y * w0 + v1.uv.y * w1 + v2.uv.y * w2;
                            let vr = v0.color.r() as f32 * w0 + v1.color.r() as f32 * w1 + v2.color.r() as f32 * w2;
                            let vg = v0.color.g() as f32 * w0 + v1.color.g() as f32 * w1 + v2.color.g() as f32 * w2;
                            let vb = v0.color.b() as f32 * w0 + v1.color.b() as f32 * w1 + v2.color.b() as f32 * w2;
                            let va = v0.color.a() as f32 * w0 + v1.color.a() as f32 * w1 + v2.color.a() as f32 * w2;

                            let (r, g, b, final_a) = if is_font_tex {
                                let cov = self.tex_mgr.sample_alpha_f(uv_x, uv_y);
                                (vr, vg, vb, va / 255.0 * cov)
                            } else {
                                let [tr, tg, tb, ta] = self.tex_mgr.sample_rgba(mesh.texture_id, uv_x, uv_y);
                                (tr * vr, tg * vg, tb * vb, ta * va / 255.0)
                            };
                            if final_a < 1.0 / 255.0 { continue; }

                            let idx = (py as u32 * w + px as u32) as usize;
                            let dst = pixels[idx];
                            let dr = ((dst >> 16) & 0xFF) as f32;
                            let dg = ((dst >> 8) & 0xFF) as f32;
                            let db = (dst & 0xFF) as f32;
                            let inv = 1.0 - final_a;
                            let out_r = (((r / 255.0).powi(2) * final_a + (dr / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_g = (((g / 255.0).powi(2) * final_a + (dg / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_b = (((b / 255.0).powi(2) * final_a + (db / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            pixels[idx] = crate::to_bgra(out_r, out_g, out_b, 255);
                        }
                    }
                }
            }
        }

        if self.surface.resize(
            std::num::NonZeroU32::new(w).unwrap(),
            std::num::NonZeroU32::new(h).unwrap(),
        ).is_ok() {
            if let Ok(mut buf) = self.surface.buffer_mut() {
                buf.copy_from_slice(&pixels);
                let _ = buf.present();
            }
        }

        action
    }
}

// ── Context menu UI ───────────────────────────────────────────────────────────

fn draw_context_menu_ui(ctx: &egui::Context, t: &crate::theme::Theme) -> Option<ContextMenuAction> {
    let mut action: Option<ContextMenuAction> = None;

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(t.bg_pri))
        .show(ctx, |ui| {
            ui.set_min_size(Vec2::new(CM_W, CM_H));

            // Border
            ui.painter().rect_stroke(
                Rect::from_min_size(Pos2::ZERO, Vec2::new(CM_W, CM_H)),
                4.0,
                Stroke::new(1.0, t.border),
                StrokeKind::Inside,
            );

            let pad = 6.0;
            let item_h = 36.0;
            let text_x = 14.0;

            // ── "Open Diskoria" ───────────────────────────────────────────────
            let open_rect = Rect::from_min_size(
                Pos2::new(1.0, pad),
                Vec2::new(CM_W - 2.0, item_h),
            );
            let open_resp = ui.interact(open_rect, egui::Id::new("cm_open"), Sense::click());
            if open_resp.hovered() {
                ui.painter().rect_filled(open_rect, 3.0, t.hover);
            }
            ui.painter().text(
                Pos2::new(text_x, open_rect.center().y),
                Align2::LEFT_CENTER,
                "Open Diskoria",
                FontId::new(13.0, egui::FontFamily::Proportional),
                t.txt_pri,
            );
            if open_resp.clicked() {
                action = Some(ContextMenuAction::Open);
            }

            // ── Separator ────────────────────────────────────────────────────
            let sep_y = pad + item_h + 3.5;
            ui.painter().hline(
                8.0..=(CM_W - 8.0),
                sep_y,
                Stroke::new(1.0, t.border),
            );

            // ── "Quit" ───────────────────────────────────────────────────────
            let quit_rect = Rect::from_min_size(
                Pos2::new(1.0, sep_y + 3.5),
                Vec2::new(CM_W - 2.0, item_h),
            );
            let quit_resp = ui.interact(quit_rect, egui::Id::new("cm_quit"), Sense::click());
            if quit_resp.hovered() {
                ui.painter().rect_filled(quit_rect, 3.0, t.hover);
            }
            ui.painter().text(
                Pos2::new(text_x, quit_rect.center().y),
                Align2::LEFT_CENTER,
                "Quit",
                FontId::new(13.0, egui::FontFamily::Proportional),
                t.txt_pri,
            );
            if quit_resp.clicked() {
                action = Some(ContextMenuAction::Quit);
            }
        });

    action
}

// ── DriveContextMenuWindow ────────────────────────────────────────────────────

/// Returned when the user picks a suppression duration from a drive context menu.
#[derive(Debug, Clone)]
pub struct DriveContextMenuAction {
    pub serial: String,
    /// How many seconds to suppress alerts for this drive.
    pub suppress_secs: u64,
}

// Logical dimensions.
const DCM_W: f32 = 180.0;
// header(26) + sep(7) + 4×item(32) + top/bottom pad(6+6) = 173
const DCM_H: f32 = 173.0;

/// Themed right-click context menu for a drive tray icon that is currently flashing an alert.
/// Shows "Ignore alert for:" and four duration choices.
pub struct DriveContextMenuWindow {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    tex_mgr: TextureManager,
    accent_color: Color32,
    /// Serial of the drive this menu belongs to, echoed back in the action.
    serial: String,
    /// Prevents immediate close — same guard as `ContextMenuWindow`.
    cursor_entered: bool,
}

impl DriveContextMenuWindow {
    pub fn new(
        event_loop: &ActiveEventLoop,
        cursor_pos: winit::dpi::PhysicalPosition<f64>,
        accent_color: Color32,
        serial: String,
    ) -> Option<Self> {
        let egui_ctx = egui::Context::default();
        crate::chrome::setup_fonts(&egui_ctx);

        use winit::platform::windows::WindowAttributesExtWindows;
        let attrs = Window::default_attributes()
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(DCM_W as u32, DCM_H as u32))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_title("Diskoria Drive Menu");

        let window = Arc::new(event_loop.create_window(attrs).ok()?);

        // WS_EX_NOACTIVATE — must not steal focus.
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
            };
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE as isize);
                }
            }
        }

        // Position above-left of cursor, clamped to screen.
        let scale = window.current_monitor()
            .or_else(|| window.primary_monitor())
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let win_w = (DCM_W as f64 * scale) as i32;
        let win_h = (DCM_H as f64 * scale) as i32;
        let gap = (4.0 * scale) as i32;

        let cx = cursor_pos.x as i32;
        let cy = cursor_pos.y as i32;
        let mut x = cx;
        let mut y = cy - win_h - gap;

        if let Some(monitor) = window.current_monitor().or_else(|| window.primary_monitor()) {
            let sw = monitor.size().width as i32;
            let sh = monitor.size().height as i32;
            x = x.clamp(0, (sw - win_w).max(0));
            y = y.clamp(0, (sh - win_h).max(0));
        }
        window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));

        let ctx_sb = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&ctx_sb, window.clone()).ok()?;

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::from_hash_of("drive_context_menu"),
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        Some(DriveContextMenuWindow {
            window,
            surface,
            egui_ctx,
            egui_state,
            tex_mgr: TextureManager::new(),
            accent_color,
            serial,
            cursor_entered: false,
        })
    }

    pub fn handle_event(&mut self, event: &WindowEvent) -> Option<DriveContextMenuAction> {
        let resp = self.egui_state.on_window_event(&self.window, event);
        if resp.repaint {
            self.window.request_redraw();
        }
        match event {
            WindowEvent::RedrawRequested => self.paint(),
            _ => None,
        }
    }

    pub fn poll_cursor(&mut self, cursor_x: i32, cursor_y: i32) -> bool {
        let pos = self.window.outer_position().unwrap_or_default();
        let size = self.window.outer_size();
        let inside = cursor_x >= pos.x && cursor_x < pos.x + size.width as i32
            && cursor_y >= pos.y && cursor_y < pos.y + size.height as i32;
        if inside { self.cursor_entered = true; }
        self.cursor_entered && !inside
    }

    fn paint(&mut self) -> Option<DriveContextMenuAction> {
        let size = self.window.inner_size();
        let w = size.width;
        let h = size.height;
        if w == 0 || h == 0 { return None; }

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let dark = matches!(self.window.theme(), Some(winit::window::Theme::Dark));
        let t = crate::theme::Theme::new(dark, self.accent_color);

        let mut action: Option<DriveContextMenuAction> = None;
        let serial = self.serial.clone();
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            action = draw_drive_context_menu_ui(ctx, &t, &serial);
        });

        self.egui_state.handle_platform_output(&self.window, full_output.platform_output);
        self.tex_mgr.update(&full_output.textures_delta);

        let ppp = full_output.pixels_per_point;
        let clipped = self.egui_ctx.tessellate(full_output.shapes, ppp);

        let bg = t.bg_pri;
        let bg32 = crate::to_bgra(bg.r(), bg.g(), bg.b(), 255);
        let mut pixels: Vec<u32> = vec![bg32; (w * h) as usize];

        for prim in &clipped {
            let clip = prim.clip_rect;
            let clip_x0 = (clip.min.x * ppp).floor() as i32;
            let clip_y0 = (clip.min.y * ppp).floor() as i32;
            let clip_x1 = (clip.max.x * ppp).ceil() as i32;
            let clip_y1 = (clip.max.y * ppp).ceil() as i32;

            if let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive {
                let verts = &mesh.vertices;
                let indices = &mesh.indices;
                let is_font_tex = mesh.texture_id == egui::TextureId::default();

                for tri in 0..(indices.len() / 3) {
                    let i0 = indices[tri * 3] as usize;
                    let i1 = indices[tri * 3 + 1] as usize;
                    let i2 = indices[tri * 3 + 2] as usize;
                    if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() { continue; }
                    let v0 = &verts[i0]; let v1 = &verts[i1]; let v2 = &verts[i2];

                    let p0 = (v0.pos.x * ppp, v0.pos.y * ppp);
                    let p1 = (v1.pos.x * ppp, v1.pos.y * ppp);
                    let p2 = (v2.pos.x * ppp, v2.pos.y * ppp);

                    let min_x = p0.0.min(p1.0).min(p2.0).floor() as i32;
                    let min_y = p0.1.min(p1.1).min(p2.1).floor() as i32;
                    let max_x = p0.0.max(p1.0).max(p2.0).ceil() as i32;
                    let max_y = p0.1.max(p1.1).max(p2.1).ceil() as i32;

                    let x0 = min_x.max(clip_x0).max(0);
                    let y0 = min_y.max(clip_y0).max(0);
                    let x1 = max_x.min(clip_x1).min(w as i32 - 1);
                    let y1 = max_y.min(clip_y1).min(h as i32 - 1);

                    let denom = (p1.1 - p2.1) * (p0.0 - p2.0) + (p2.0 - p1.0) * (p0.1 - p2.1);
                    if denom.abs() < 0.001 { continue; }

                    for py in y0..=y1 {
                        for px in x0..=x1 {
                            let fx = px as f32 + 0.5;
                            let fy = py as f32 + 0.5;
                            let w0 = ((p1.1 - p2.1) * (fx - p2.0) + (p2.0 - p1.0) * (fy - p2.1)) / denom;
                            let w1 = ((p2.1 - p0.1) * (fx - p2.0) + (p0.0 - p2.0) * (fy - p2.1)) / denom;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 { continue; }

                            let uv_x = v0.uv.x * w0 + v1.uv.x * w1 + v2.uv.x * w2;
                            let uv_y = v0.uv.y * w0 + v1.uv.y * w1 + v2.uv.y * w2;
                            let vr = v0.color.r() as f32 * w0 + v1.color.r() as f32 * w1 + v2.color.r() as f32 * w2;
                            let vg = v0.color.g() as f32 * w0 + v1.color.g() as f32 * w1 + v2.color.g() as f32 * w2;
                            let vb = v0.color.b() as f32 * w0 + v1.color.b() as f32 * w1 + v2.color.b() as f32 * w2;
                            let va = v0.color.a() as f32 * w0 + v1.color.a() as f32 * w1 + v2.color.a() as f32 * w2;

                            let (r, g, b, final_a) = if is_font_tex {
                                let cov = self.tex_mgr.sample_alpha_f(uv_x, uv_y);
                                (vr, vg, vb, va / 255.0 * cov)
                            } else {
                                let [tr, tg, tb, ta] = self.tex_mgr.sample_rgba(mesh.texture_id, uv_x, uv_y);
                                (tr * vr, tg * vg, tb * vb, ta * va / 255.0)
                            };
                            if final_a < 1.0 / 255.0 { continue; }

                            let idx = (py as u32 * w + px as u32) as usize;
                            let dst = pixels[idx];
                            let dr = ((dst >> 16) & 0xFF) as f32;
                            let dg = ((dst >> 8)  & 0xFF) as f32;
                            let db = ( dst        & 0xFF) as f32;
                            let inv = 1.0 - final_a;
                            let out_r = (((r / 255.0).powi(2) * final_a + (dr / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_g = (((g / 255.0).powi(2) * final_a + (dg / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            let out_b = (((b / 255.0).powi(2) * final_a + (db / 255.0).powi(2) * inv).sqrt() * 255.0 + 0.5) as u8;
                            pixels[idx] = crate::to_bgra(out_r, out_g, out_b, 255);
                        }
                    }
                }
            }
        }

        if self.surface.resize(
            NonZeroU32::new(w).unwrap(),
            NonZeroU32::new(h).unwrap(),
        ).is_ok() {
            if let Ok(mut buf) = self.surface.buffer_mut() {
                buf.copy_from_slice(&pixels);
                let _ = buf.present();
            }
        }

        action
    }
}

// ── Drive context menu UI ─────────────────────────────────────────────────────

fn draw_drive_context_menu_ui(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    serial: &str,
) -> Option<DriveContextMenuAction> {
    const ITEMS: [(&str, u64); 4] = [
        ("1 hour",   3_600),
        ("6 hours",  21_600),
        ("24 hours", 86_400),
        ("7 days",   604_800),
    ];

    let mut action: Option<DriveContextMenuAction> = None;

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(t.bg_pri))
        .show(ctx, |ui| {
            ui.set_min_size(Vec2::new(DCM_W, DCM_H));

            // Border
            ui.painter().rect_stroke(
                Rect::from_min_size(Pos2::ZERO, Vec2::new(DCM_W, DCM_H)),
                4.0,
                Stroke::new(1.0, t.border),
                StrokeKind::Inside,
            );

            let pad  = 6.0;
            let text_x = 14.0;
            let header_h = 26.0;
            let sep_h = 7.0;
            let item_h = 32.0;

            // ── "Ignore alert for:" header (non-clickable) ────────────────────
            let header_rect = Rect::from_min_size(
                Pos2::new(1.0, pad),
                Vec2::new(DCM_W - 2.0, header_h),
            );
            ui.painter().text(
                Pos2::new(text_x, header_rect.center().y),
                Align2::LEFT_CENTER,
                "Ignore alert for:",
                FontId::new(11.0, egui::FontFamily::Proportional),
                t.txt_sec,
            );

            // ── Separator ─────────────────────────────────────────────────────
            let sep_y = pad + header_h + sep_h * 0.5;
            ui.painter().hline(
                8.0..=(DCM_W - 8.0),
                sep_y,
                Stroke::new(1.0, t.border),
            );

            // ── Duration items ────────────────────────────────────────────────
            let items_top = pad + header_h + sep_h;
            for (i, (label, secs)) in ITEMS.iter().enumerate() {
                let item_rect = Rect::from_min_size(
                    Pos2::new(1.0, items_top + i as f32 * item_h),
                    Vec2::new(DCM_W - 2.0, item_h),
                );
                let id = egui::Id::new(("dcm_item", i));
                let resp = ui.interact(item_rect, id, Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(item_rect, 3.0, t.hover);
                }
                ui.painter().text(
                    Pos2::new(text_x, item_rect.center().y),
                    Align2::LEFT_CENTER,
                    *label,
                    FontId::new(13.0, egui::FontFamily::Proportional),
                    t.txt_pri,
                );
                if resp.clicked() {
                    action = Some(DriveContextMenuAction {
                        serial: serial.to_string(),
                        suppress_secs: *secs,
                    });
                }
            }
        });

    action
}
