//! Diskoria — Windows storage scanning and benchmarking (egui).
//!
//! Rendering stack: winit + softbuffer + hand-written CPU rasterizer (no GPU / no OpenGL).
//! Style tokens follow `WINDOWS11_EGUI_STYLE_GUIDE.md`.

mod about;
mod app;
mod app_settings;
mod chrome;
mod github_config;
mod focus;
mod modal_confirm;
mod partition_info;
mod detected_drive;
mod drive_enumeration;
pub mod surface_test;
pub mod speed_test;
pub mod destructive_test;
mod shortcuts;
mod smart_health;
mod theme;
mod update;
mod widgets;

pub use app::DiskoriaApp;

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::theme::Theme;

// ── Texture manager (CPU rasterizer atlas) ────────────────────────────────────

struct TexEntry {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl TexEntry {
    fn sample(&self, uv_x: f32, uv_y: f32) -> [f32; 4] {
        let px = (uv_x * self.width as f32).floor().clamp(0.0, self.width as f32 - 1.0) as usize;
        let py = (uv_y * self.height as f32).floor().clamp(0.0, self.height as f32 - 1.0) as usize;
        let idx = (py * self.width + px) * 4;
        if idx + 3 >= self.rgba.len() {
            return [1.0, 0.0, 1.0, 1.0];
        }
        [
            self.rgba[idx] as f32 / 255.0,
            self.rgba[idx + 1] as f32 / 255.0,
            self.rgba[idx + 2] as f32 / 255.0,
            self.rgba[idx + 3] as f32 / 255.0,
        ]
    }
}

struct TextureManager {
    font_atlas: Option<TexEntry>,
    textures: HashMap<egui::TextureId, TexEntry>,
}

impl TextureManager {
    fn new() -> Self {
        Self {
            font_atlas: None,
            textures: HashMap::new(),
        }
    }

    fn update(&mut self, delta: &egui::TexturesDelta) {
        for (id, image_delta) in &delta.set {
            let (w, h, rgba) = match &image_delta.image {
                egui::ImageData::Font(font_img) => {
                    let w = font_img.width();
                    let h = font_img.height();
                    let rgba: Vec<u8> = font_img
                        .pixels
                        .iter()
                        .flat_map(|&cov| {
                            let v = (cov * 255.0 + 0.5) as u8;
                            [v, v, v, v]
                        })
                        .collect();
                    (w, h, rgba)
                }
                egui::ImageData::Color(color_img) => {
                    let w = color_img.width();
                    let h = color_img.height();
                    let rgba: Vec<u8> = color_img
                        .pixels
                        .iter()
                        .flat_map(|p| [p.r(), p.g(), p.b(), p.a()])
                        .collect();
                    (w, h, rgba)
                }
            };

            let entry = if let Some([ox, oy]) = image_delta.pos {
                // Partial update
                if *id == egui::TextureId::default() {
                    if let Some(atlas) = &mut self.font_atlas {
                        for row in 0..h {
                            for col in 0..w {
                                let src = (row * w + col) * 4;
                                let dst = ((oy + row) * atlas.width + (ox + col)) * 4;
                                if dst + 3 < atlas.rgba.len() && src + 3 < rgba.len() {
                                    atlas.rgba[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                                }
                            }
                        }
                    }
                    continue;
                } else if let Some(entry) = self.textures.get_mut(id) {
                    for row in 0..h {
                        for col in 0..w {
                            let src = (row * w + col) * 4;
                            let dst = ((oy + row) * entry.width + (ox + col)) * 4;
                            if dst + 3 < entry.rgba.len() && src + 3 < rgba.len() {
                                entry.rgba[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                            }
                        }
                    }
                    continue;
                } else {
                    TexEntry { width: w, height: h, rgba }
                }
            } else {
                TexEntry { width: w, height: h, rgba }
            };

            if *id == egui::TextureId::default() {
                self.font_atlas = Some(entry);
            } else {
                self.textures.insert(*id, entry);
            }
        }

        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    // Alpha coverage for font atlas — returns [0, 1].
    fn sample_alpha_f(&self, uv_x: f32, uv_y: f32) -> f32 {
        let Some(atlas) = &self.font_atlas else { return 1.0 };
        let px = (uv_x * atlas.width as f32).floor().clamp(0.0, atlas.width as f32 - 1.0) as usize;
        let py = (uv_y * atlas.height as f32).floor().clamp(0.0, atlas.height as f32 - 1.0) as usize;
        let idx = (py * atlas.width + px) * 4 + 3;
        atlas.rgba.get(idx).copied().unwrap_or(0) as f32 / 255.0
    }

    // RGBA sample for image textures — bilinear, returns [r, g, b, a] in [0, 1].
    fn sample_rgba(&self, id: egui::TextureId, uv_x: f32, uv_y: f32) -> [f32; 4] {
        self.textures
            .get(&id)
            .map(|e| e.sample(uv_x, uv_y))
            .unwrap_or([1.0, 0.0, 1.0, 1.0])
    }
}

// ── Renderer ──────────────────────────────────────────────────────────────────

struct Renderer {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    app: DiskoriaApp,
    tex_mgr: TextureManager,
    close_requested: bool,
}

impl Renderer {
    fn new(event_loop: &ActiveEventLoop) -> Self {
        use winit::dpi::LogicalSize;
        use winit::window::Icon;

        const APPICON_ICO: &[u8] = include_bytes!("../appicon.ico");

        let window_icon = image::load_from_memory(APPICON_ICO)
            .ok()
            .map(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).ok()
            })
            .flatten();

        let mut attrs = Window::default_attributes()
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(800_u32, 600_u32))
            .with_min_inner_size(LogicalSize::new(780_u32, 580_u32))
            .with_resizable(true)
            .with_title("Diskoria");
        if let Some(icon) = window_icon {
            attrs = attrs.with_window_icon(Some(icon));
        }

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let system_dark = matches!(window.theme(), Some(winit::window::Theme::Dark));

        // Extract HWND for DWM rounded corners and resize border hook.
        #[cfg(windows)]
        let hwnd: isize = {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    h.hwnd.get() as isize
                } else {
                    0
                }
            } else {
                0
            }
        };
        #[cfg(not(windows))]
        let hwnd: isize = 0;

        let ctx = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&ctx, window.clone()).expect("softbuffer surface");

        let egui_ctx = egui::Context::default();

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        let app = DiskoriaApp::new(&egui_ctx, system_dark, hwnd);

        Renderer {
            window,
            surface,
            egui_ctx,
            egui_state,
            app,
            tex_mgr: TextureManager::new(),
            close_requested: false,
        }
    }

    fn paint(&mut self) {
        let size = self.window.inner_size();
        let w = size.width;
        let h = size.height;
        if w == 0 || h == 0 {
            return;
        }

        let raw_input = self.egui_state.take_egui_input(&self.window);

        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            self.app.draw(ctx);
        });

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);

        // Handle viewport commands (drag, minimize, maximize, close).
        if let Some(viewport_output) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
            for cmd in &viewport_output.commands {
                match cmd {
                    egui::ViewportCommand::StartDrag => {
                        let _ = self.window.drag_window();
                    }
                    egui::ViewportCommand::Minimized(true) => {
                        self.window.set_minimized(true);
                    }
                    egui::ViewportCommand::Maximized(b) => {
                        self.window.set_maximized(*b);
                    }
                    egui::ViewportCommand::Close => {
                        self.close_requested = true;
                    }
                    _ => {}
                }
            }
        }

        self.tex_mgr.update(&full_output.textures_delta);

        let ppp = full_output.pixels_per_point;
        let clipped = self.egui_ctx.tessellate(full_output.shapes, ppp);

        let bg = Theme::new(self.app.dark, self.app.accent_color).bg_pri;
        let bg32 = to_bgra(bg.r(), bg.g(), bg.b(), 255);
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

                            // Gamma-correct blend (gamma ≈ 2.0): linearise → blend → encode.
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

                            pixels[idx] = to_bgra(out_r, out_g, out_b, 255);
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

        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        if repaint_delay == std::time::Duration::ZERO {
            self.window.request_redraw();
        }
    }
}

#[inline]
fn to_bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | (b as u32)
}

// ── winit ApplicationHandler ──────────────────────────────────────────────────

struct App {
    renderer: Option<Renderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() {
            self.renderer = Some(Renderer::new(event_loop));
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(r) = &self.renderer {
                r.window.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = &mut self.renderer else { return };

        let resp = renderer.egui_state.on_window_event(&renderer.window, &event);
        if resp.repaint {
            renderer.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                renderer.paint();
                if renderer.close_requested {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    renderer.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(r) = &self.renderer {
            r.window.request_redraw();
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("diskoria=debug,warn"),
    )
    .format_timestamp_millis()
    .try_init();

    log::info!(
        target: "diskoria",
        "Diskoria {} starting",
        env!("CARGO_PKG_VERSION")
    );

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App { renderer: None };
    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!(target: "diskoria", "event loop error: {e}");
    }
}
