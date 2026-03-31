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
pub mod smart_reader;
mod smart_health_page;
mod theme;
mod update;
mod widgets;

// Pro-Monitoring modules
pub mod alert_engine;
#[cfg(windows)]
pub mod flyout;
pub mod history_db;
pub mod monitor;
pub mod toast;
#[cfg(windows)]
pub mod tray;

pub(crate) mod tex_mgr;

pub use app::DiskoriaApp;

use std::num::NonZeroU32;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::theme::Theme;

// ── User event type (tray + monitoring callbacks) ─────────────────────────────

/// Events that can be injected into the winit event loop from background threads
/// or the tray-icon callbacks.
#[derive(Debug)]
pub enum UserEvent {
    /// A tray icon was clicked / interacted with.
    #[cfg(windows)]
    TrayIconEvent(tray_icon::TrayIconEvent),
    /// Update a drive's tray icon to reflect a new temperature.
    #[cfg(windows)]
    TrayIconUpdate { serial: String, temp_c: Option<i32> },
    /// Flash a drive's tray icon to signal an alert condition.
    #[cfg(windows)]
    DriveAlert { serial: String, is_critical: bool },
    /// Graceful shutdown requested (from tray context menu "Quit").
    QuitRequested,
}

use crate::tex_mgr::TextureManager;

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
    fn new(event_loop: &ActiveEventLoop, pro_edition: bool) -> Self {
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

        let app = DiskoriaApp::new(&egui_ctx, system_dark, hwnd, pro_edition);

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
pub(crate) fn to_bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | (b as u32)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Show and forcefully bring a window to the foreground.
///
/// `set_visible(true)` alone leaves the window behind the current active window
/// on Windows because of focus-stealing prevention.  Pairing it with
/// `SetForegroundWindow` + `BringWindowToTop` (safe to call from the tray-icon
/// callback thread since we still hold foreground permission at that point)
/// makes the window actually appear on top.
#[cfg(windows)]
fn raise_window(window: &winit::window::Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    window.set_visible(true);
    if let Ok(handle) = window.window_handle() {
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
                BringWindowToTop(hwnd);
            }
        }
    }
}

// ── winit ApplicationHandler ──────────────────────────────────────────────────

struct App {
    renderer: Option<Renderer>,
    proxy: EventLoopProxy<UserEvent>,
    /// Whether the `--Pro-Edition` flag was passed at launch.
    pro_edition: bool,
    #[cfg(windows)]
    tray: Option<crate::tray::TrayManager>,
    #[cfg(windows)]
    flyout: Option<crate::flyout::FlyoutRenderer>,
    /// Physical-pixel rect of the icon that opened the current flyout.
    /// Polled every 50 ms to close the flyout when cursor leaves.
    #[cfg(windows)]
    flyout_icon_rect: Option<tray_icon::Rect>,
    /// Serial of the drive whose flyout is currently open.
    #[cfg(windows)]
    flyout_drive_serial: Option<String>,
    /// Custom themed context menu (shown on right-click of the app tray icon).
    #[cfg(windows)]
    context_menu: Option<crate::flyout::ContextMenuWindow>,
    /// Context menu shown on right-click of a drive icon that is currently flashing an alert.
    #[cfg(windows)]
    drive_context_menu: Option<crate::flyout::DriveContextMenuWindow>,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let mut renderer = Renderer::new(event_loop, self.pro_edition);
            #[cfg(windows)]
            if self.pro_edition {
                renderer.app.event_proxy = Some(self.proxy.clone());
                self.tray = crate::tray::TrayManager::new(self.proxy.clone());
            }
            self.renderer = Some(renderer);
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
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Dispatch to flyout first if the event belongs to it.
        #[cfg(windows)]
        if let Some(flyout) = &mut self.flyout {
            if flyout.window.id() == window_id {
                let should_close = flyout.handle_event(&event);
                if should_close {
                    self.flyout = None;
                }
                return;
            }
        }

        // Dispatch to context menu if the event belongs to it.
        #[cfg(windows)]
        if let Some(cm) = &mut self.context_menu {
            if cm.window.id() == window_id {
                if let Some(action) = cm.handle_event(&event) {
                    match action {
                        crate::flyout::ContextMenuAction::Open => {
                            if let Some(r) = &self.renderer {
                                raise_window(&r.window);
                            }
                        }
                        crate::flyout::ContextMenuAction::Quit => {
                            let _ = self.proxy.send_event(UserEvent::QuitRequested);
                        }
                    }
                    self.context_menu = None;
                }
                return;
            }
        }

        // Dispatch to drive context menu if the event belongs to it.
        #[cfg(windows)]
        if let Some(dcm) = &mut self.drive_context_menu {
            if dcm.window.id() == window_id {
                if let Some(action) = dcm.handle_event(&event) {
                    // Stop flashing and set suppression.
                    if let Some(tray) = &mut self.tray {
                        tray.clear_drive_flash(&action.serial);
                    }
                    if let Some(r) = &mut self.renderer {
                        r.app.suppress_drive_alerts(&action.serial, action.suppress_secs);
                    }
                    self.drive_context_menu = None;
                }
                return;
            }
        }

        let Some(renderer) = &mut self.renderer else { return };

        let resp = renderer.egui_state.on_window_event(&renderer.window, &event);
        if resp.repaint {
            renderer.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => {
                // Minimize to tray instead of exiting.
                // The app only exits via the tray context menu "Quit" → QuitRequested.
                if let Some(r) = &self.renderer {
                    r.window.set_visible(false);
                }
            }
            WindowEvent::RedrawRequested => {
                renderer.paint();
                if renderer.close_requested {
                    renderer.close_requested = false;
                    renderer.window.set_visible(false);
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::QuitRequested => {
                // Cancel the monitor thread before exiting.
                #[cfg(windows)]
                if let Some(renderer) = &self.renderer {
                    if let Some(cancel) = &renderer.app.monitor_cancel {
                        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                // Drop flyout, context menu, and tray icons cleanly.
                #[cfg(windows)]
                {
                    self.flyout = None;
                    self.context_menu = None;
                    self.drive_context_menu = None;
                    self.tray = None;
                }
                event_loop.exit();
            }
            #[cfg(windows)]
            UserEvent::TrayIconUpdate { serial, temp_c } => {
                if let Some(tray) = &mut self.tray {
                    tray.update_drive_icon(&serial, temp_c);
                }
            }
            #[cfg(windows)]
            UserEvent::DriveAlert { serial, is_critical } => {
                if let Some(tray) = &mut self.tray {
                    tray.set_drive_alert(&serial, is_critical);
                }
            }
            #[cfg(windows)]
            UserEvent::TrayIconEvent(e) => {
                log::debug!(target: "diskoria::tray", "TrayIconEvent: {:?}", e);
                let tray_ref = self.tray.as_ref();
                if let Some(tray) = tray_ref {
                    if let Some((serial, rect)) = tray.drive_serial_for_hover_event(&e) {
                        // Always update the rect so polling uses the freshest position.
                        self.flyout_icon_rect = Some(rect.clone());

                        // Only (re)create the flyout if it isn't already open for this drive.
                        let already_open = self.flyout.is_some()
                            && self.flyout_drive_serial.as_deref() == Some(serial.as_str());
                        if !already_open {
                            let snapshot = self.renderer
                                .as_ref()
                                .and_then(|r| r.app.last_snapshots.get(&serial))
                                .map(crate::monitor::DriveSnapshot::from_snapshot)
                                .unwrap_or_else(|| {
                                    let mut s = crate::monitor::DriveSnapshot::default();
                                    s.serial = serial.clone();
                                    s.model = serial.clone();
                                    s
                                });
                            self.flyout = None;
                            self.flyout_drive_serial = Some(serial.clone());
                            let accent = self.renderer.as_ref()
                                .map(|r| r.app.accent_color)
                                .unwrap_or(egui::Color32::from_rgb(61, 90, 128));
                            if let Some(flyout) = crate::flyout::FlyoutRenderer::new(event_loop, snapshot, Some(rect), accent) {
                                unsafe {
                                    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
                                    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                                    if let Ok(handle) = flyout.window.window_handle() {
                                        if let RawWindowHandle::Win32(h) = handle.as_raw() {
                                            let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                                        }
                                    }
                                }
                                flyout.window.request_redraw();
                                self.flyout = Some(flyout);
                            }
                        }
                    } else if tray.is_drive_leave_event(&e) {
                        // Fast path when Leave does fire correctly.
                        self.flyout = None;
                        self.flyout_icon_rect = None;
                        self.flyout_drive_serial = None;
                    } else if let Some((serial, pos)) = tray.alert_drive_right_click(&e) {
                        // Right-click on a flashing drive icon → show suppression menu.
                        let accent = self.renderer.as_ref()
                            .map(|r| r.app.accent_color)
                            .unwrap_or(egui::Color32::from_rgb(61, 90, 128));
                        self.drive_context_menu = None;
                        if let Some(dcm) = crate::flyout::DriveContextMenuWindow::new(
                            event_loop, pos, accent, serial,
                        ) {
                            unsafe {
                                use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
                                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                                if let Ok(handle) = dcm.window.window_handle() {
                                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                                        let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                                    }
                                }
                            }
                            dcm.window.request_redraw();
                            self.drive_context_menu = Some(dcm);
                        }
                    } else if tray.is_app_icon_left_click(&e) {
                        // Left-click on the app icon → restore and raise main window.
                        if let Some(r) = &self.renderer {
                            raise_window(&r.window);
                        }
                    } else if let Some(pos) = tray.app_icon_right_click_pos(&e) {
                        log::info!(target: "diskoria::tray", "App icon right-clicked at {:?}", pos);
                        // Right-click on the app icon → custom themed context menu.
                        let accent = self.renderer.as_ref()
                            .map(|r| r.app.accent_color)
                            .unwrap_or(egui::Color32::from_rgb(61, 90, 128));
                        self.context_menu = None;
                        if let Some(cm) = crate::flyout::ContextMenuWindow::new(event_loop, pos, accent) {
                            log::info!(target: "diskoria::tray", "Context menu window created");

                            unsafe {
                                use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
                                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                                if let Ok(handle) = cm.window.window_handle() {
                                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                                        let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                                    }
                                }
                            }
                            cm.window.request_redraw();
                            self.context_menu = Some(cm);
                        }
                    }
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(r) = &self.renderer {
            r.window.request_redraw();
        }

        // Rebuild per-drive tray icons if the drive list changed.
        #[cfg(windows)]
        {
            let needs_rebuild = self.renderer
                .as_ref()
                .map(|r| !r.app.drive_icons_built && !r.app.drives.is_empty())
                .unwrap_or(false);

            if needs_rebuild {
                if let (Some(tray), Some(renderer)) = (&mut self.tray, &mut self.renderer) {
                    tray.rebuild_drive_icons(&renderer.app.drives);
                    renderer.app.drive_icons_built = true;
                }
            }
        }

        // While a flyout or context menu is open, poll the cursor position every 50 ms.
        #[cfg(windows)]
        {
            // Advance tray icon flash animations; collect the desired poll interval.
            let flash_wait = self.tray.as_mut().and_then(|t| t.tick_flash());

            let poll_needed = self.flyout.is_some() || self.context_menu.is_some() || self.drive_context_menu.is_some() || flash_wait.is_some();

            if self.flyout.is_some() {
                if let Some(ref rect) = self.flyout_icon_rect {
                    unsafe {
                        use windows_sys::Win32::Foundation::POINT;
                        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
                        let mut pt = POINT { x: 0, y: 0 };
                        GetCursorPos(&mut pt);
                        let left   = rect.position.x as i32;
                        let top    = rect.position.y as i32;
                        let right  = (rect.position.x + rect.size.width  as f64) as i32;
                        let bottom = (rect.position.y + rect.size.height as f64) as i32;
                        if pt.x < left || pt.x >= right || pt.y < top || pt.y >= bottom {
                            self.flyout = None;
                            self.flyout_icon_rect = None;
                            self.flyout_drive_serial = None;
                        }
                    }
                }
            }

            if self.context_menu.is_some() || self.drive_context_menu.is_some() {
                let (close_cm, close_dcm) = unsafe {
                    use windows_sys::Win32::Foundation::POINT;
                    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    (
                        self.context_menu.as_mut().map(|cm| cm.poll_cursor(pt.x, pt.y)).unwrap_or(false),
                        self.drive_context_menu.as_mut().map(|dcm| dcm.poll_cursor(pt.x, pt.y)).unwrap_or(false),
                    )
                };
                if close_cm  { self.context_menu = None; }
                if close_dcm { self.drive_context_menu = None; }
            }

            if poll_needed {
                // Use the flash interval if it's shorter than the 50ms cursor-poll cadence.
                let base = std::time::Duration::from_millis(50);
                let wait = flash_wait.map_or(base, |fw| fw.min(base));
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    std::time::Instant::now() + wait,
                ));
            }
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

    let pro_edition = std::env::args().any(|a| a == "--Pro-Edition");
    log::info!(target: "diskoria", "Pro Edition: {pro_edition}");

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App {
        renderer: None,
        proxy,
        pro_edition,
        #[cfg(windows)]
        tray: None,
        #[cfg(windows)]
        flyout: None,
        #[cfg(windows)]
        flyout_icon_rect: None,
        #[cfg(windows)]
        flyout_drive_serial: None,
        #[cfg(windows)]
        context_menu: None,
        #[cfg(windows)]
        drive_context_menu: None,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!(target: "diskoria", "event loop error: {e}");
    }
}
