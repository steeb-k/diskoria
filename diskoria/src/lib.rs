//! Diskoria — Windows storage scanning and benchmarking (egui).
//!
//! Rendering stack: winit + softbuffer + hand-written CPU rasterizer (no GPU / no OpenGL).
//! Style tokens live in `theme.rs`; the UI architecture is documented in
//! `docs/gui-architecture.md`.

// Many egui draw helpers take a handful of layout params (theme, dark flag,
// margins, content x/width). Bundling them into a `PageLayout` struct is tracked
// in docs/refactor-roadmap.md; until then, allow the arg count crate-wide.
#![allow(clippy::too_many_arguments)]

mod about;
mod app;
mod app_settings;
mod card;
#[cfg(windows)]
mod autostart;
mod chrome;
mod demo;
mod github_config;
mod focus;
mod install_mode;
mod modal_confirm;
mod partition_info;
mod paths;
mod detected_drive;
mod drive_enumeration;
mod drive_selector;
pub mod surface_test;
pub mod speed_test;
pub mod destructive_test;
mod shared_state;
mod shortcuts;
mod smart_health;
pub mod smart_reader;
mod smart_health_page;
mod test_result_overlay;
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
pub use shared_state::SharedAppState;

use std::collections::HashMap;
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
    /// Another exe launch signaled us via the single-instance named event.
    /// Raise the main window through winit so visibility state stays in sync.
    #[cfg(windows)]
    ShowWindowRequested,
    /// Open an additional Diskoria window in the primary process.  Sent
    /// by the in-app Ctrl+N shortcut (step 5) and — once implemented —
    /// the tray "New Window" menu item (step 9) and the secondary-exe
    /// new-window path (step 8).
    #[cfg(windows)]
    OpenNewWindow,
    /// Settings changed in some window.  Every renderer redraws so the
    /// next frame picks up the new shared values.  If `restart_monitor`
    /// is true, also tear down the monitor thread so the next draw
    /// respawns it with fresh thresholds.
    #[cfg(windows)]
    SettingsChanged { restart_monitor: bool },
    /// egui requested a repaint from a background thread or animation.
    /// Forwarded by each context's repaint callback to wake the winit loop,
    /// which otherwise sleeps in `ControlFlow::Wait` when the window is
    /// unfocused. `after` is the requested delay (ZERO = as soon as possible).
    Repaint { after: std::time::Duration },
}

/// Minimum spacing between repaints driven by an ASAP `request_repaint()`.
/// Caps a continuously-animating window at ~60 fps so it doesn't spin the CPU,
/// while still waking the loop frequently enough to keep every window and the
/// tray responsive.
const REPAINT_FRAME_CAP: std::time::Duration = std::time::Duration::from_millis(16);

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
    fn new(event_loop: &ActiveEventLoop, shared: Arc<SharedAppState>) -> Self {
        use winit::dpi::LogicalSize;
        use winit::window::Icon;

        const APPICON_ICO: &[u8] = include_bytes!("../../assets/appicon2.ico");

        let window_icon = image::load_from_memory(APPICON_ICO)
            .ok()
            .and_then(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).ok()
            });

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
                    h.hwnd.get()
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

        // Bridge egui's repaint requests into the winit loop. Background
        // threads (monitor, tests, drive enumeration) call
        // `ctx.request_repaint()`, which only sets an egui flag — it cannot
        // wake a loop sleeping in `ControlFlow::Wait`. This callback forwards
        // every request as a `UserEvent`, so unfocused windows still redraw.
        {
            let proxy = shared.event_proxy.clone();
            egui_ctx.set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::Repaint { after: info.delay });
            });
        }

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );

        let app = DiskoriaApp::new(&egui_ctx, system_dark, hwnd, shared);

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

        // A `--minimized` start still draws (that is what kicks drive
        // enumeration), so tell the app whether anyone can actually see it —
        // the startup update prompt must not fire into a hidden window.
        self.app.window_visible = self.window.is_visible().unwrap_or(true);

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
                        // The OS-driven move loop consumes the mouse-button
                        // release that ends the drag, so egui never sees it and
                        // its pointer stays stuck "down". Without a fresh
                        // up->down transition egui can't detect a new drag, so
                        // the window becomes undraggable after the first move.
                        // Synthesize the release so egui ends the interaction.
                        let pos = self
                            .egui_ctx
                            .input(|i| i.pointer.latest_pos())
                            .unwrap_or(egui::Pos2::ZERO);
                        self.egui_state.egui_input_mut().events.push(
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::default(),
                            },
                        );
                        self.window.request_redraw();
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

        let bg = Theme::new(self.app.dark, self.app.shared.accent_color()).bg_pri;
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

                            if w0 < -EDGE_EPS || w1 < -EDGE_EPS || w2 < -EDGE_EPS {
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

                            // A pixel admitted by EDGE_EPS sits marginally outside
                            // the triangle, so its interpolated alpha can land a
                            // hair outside [0, 1]. Left alone, a negative `inv`
                            // below can push the blend under zero, and `sqrt` of
                            // that is NaN — which casts to 0 and paints the black
                            // speck this whole epsilon exists to remove.
                            let final_a = final_a.clamp(0.0, 1.0);
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

        // Continuous repaints (animations, live test progress) are driven by
        // egui's repaint callback -> `UserEvent::Repaint` -> the timer-paced
        // paint in `about_to_wait`, so we deliberately do not re-`request_redraw`
        // here. Doing so would spin the focused window at unbounded CPU (bad for
        // Windows PE) and, for a background window, post a WM_PAINT that Windows
        // starves — the bug this path used to cause.
    }
}

#[inline]
pub(crate) fn to_bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | (b as u32)
}

/// Slack on the rasterizer's inside-triangle test, as a fraction of a triangle's
/// own size (the test runs on normalised barycentric weights).
///
/// egui antialiases by tessellating a solid core plus a 1px feathered ring that
/// share an edge, so on any shape edge landing on a whole pixel coordinate —
/// every panel, every button — that shared edge falls exactly down the middle of
/// a row of pixel centres. Exact arithmetic would put those centres on both
/// triangles; f32 puts them a few 1e-8 outside *both*, and the pixel is dropped,
/// letting whatever is underneath dot through the fill. Real geometry misses by
/// ~1e-1, so anything in between separates float noise from a genuine miss with
/// orders of magnitude to spare.
pub(crate) const EDGE_EPS: f32 = 1e-5;

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
    /// Open Diskoria windows, keyed by `WindowId`. Holds more than one entry
    /// once `UserEvent::OpenNewWindow` (Ctrl+N / tray "New Window") creates
    /// additional windows.
    renderers: HashMap<WindowId, Renderer>,
    proxy: EventLoopProxy<UserEvent>,
    shared: Arc<SharedAppState>,
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
    /// Writes the "any window visible" flag that secondary exe launches
    /// read to decide between "raise" and "open new window".  `None` only
    /// if the file mapping could not be created at startup.
    #[cfg(windows)]
    any_visible_flag: Option<AnyVisibleFlag>,
    /// Earliest pending delayed-repaint deadline (from `request_repaint_after`).
    /// Folded into `about_to_wait`'s `WaitUntil` so the loop wakes on time even
    /// when unfocused. `None` when no delayed repaint is pending.
    next_repaint: Option<std::time::Instant>,
    /// Boot-smoke mode (env `DISKORIA_SMOKE`): when `Some(n)`, render `n` frames
    /// through the full `draw()` path then exit cleanly. Used by the
    /// `tests/boot_smoke.rs` harness to catch startup/wiring regressions without
    /// a real display driver or admin rights. `None` in normal operation.
    smoke_remaining: Option<u32>,
    /// Debounce deadline for `WM_DEVICECHANGE`-driven drive re-enumeration.
    /// Device-tree changes arrive in bursts; we coalesce them and re-scan once
    /// the burst settles. `None` when no re-scan is pending.
    device_change_deadline: Option<std::time::Instant>,
    /// One-shot guard so the "still running in the tray" toast fires only the
    /// first time the last window is closed (hidden to tray) in a session.
    #[cfg(windows)]
    tray_toast_shown: bool,
    /// `--minimized` was passed (the scheduled logon task launches Diskoria this
    /// way): create the first window hidden so the app comes up tray-only.
    #[cfg(windows)]
    start_minimized: bool,
}

impl App {
    /// First/any renderer — used by tray-callback sites that need to reach into
    /// per-window state (accent, snapshots). With multiple windows open this is
    /// just "some window"; a future refinement could target the most-recently-
    /// focused window or broadcast to all.
    #[cfg(windows)]
    fn primary(&self) -> Option<&Renderer> {
        self.renderers.values().next()
    }

    #[cfg(windows)]
    fn primary_mut(&mut self) -> Option<&mut Renderer> {
        self.renderers.values_mut().next()
    }

    /// Re-run drive enumeration after a device-tree change. Uses any renderer's
    /// egui context to drive the worker's repaints; the per-window generation
    /// sync in `poll_drive_enumeration` then propagates the fresh list to every
    /// window. Intentionally silent — no loading spinner for an automatic
    /// refresh the user didn't request.
    fn auto_reenumerate_drives(&mut self) {
        let Some(ctx) = self.renderers.values().next().map(|r| r.egui_ctx.clone()) else {
            return;
        };
        log::debug!(target: "diskoria", "device change: re-enumerating drives");
        self.shared.start_drive_enumeration(&ctx);
    }

    /// Recompute and publish the "any window visible" bit so that a
    /// secondary exe launch picks the right follow-up (raise vs. new).
    /// Cheap to call — just walks `self.renderers` and writes 4 bytes.
    #[cfg(windows)]
    fn refresh_any_visible_flag(&self) {
        if let Some(flag) = &self.any_visible_flag {
            let visible = self
                .renderers
                .values()
                .any(|r| r.window.is_visible().unwrap_or(true));
            flag.set(visible);
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderers.is_empty() {
            let mut renderer = Renderer::new(event_loop, self.shared.clone());
            #[cfg(windows)]
            if self.shared.pro_edition {
                renderer.app.event_proxy = Some(self.proxy.clone());
                self.tray = crate::tray::TrayManager::new(self.proxy.clone());
            }
            // Auto-start (`--minimized`): come up tray-only. The window is
            // created visible by default; hide it before it maps. We still
            // request a redraw so the first `draw()` runs — that kicks drive
            // enumeration and, in turn, the background monitor thread — even
            // though no window is ever shown.
            #[cfg(windows)]
            if self.start_minimized {
                renderer.window.set_visible(false);
                renderer.window.request_redraw();
            }
            let id = renderer.window.id();
            self.renderers.insert(id, renderer);
            #[cfg(windows)]
            self.refresh_any_visible_flag();
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            for r in self.renderers.values() {
                r.window.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
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
                            for r in self.renderers.values() {
                                raise_window(&r.window);
                            }
                            self.refresh_any_visible_flag();
                        }
                        crate::flyout::ContextMenuAction::NewWindow => {
                            let _ = self.proxy.send_event(UserEvent::OpenNewWindow);
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
                    if let Some(r) = self.primary_mut() {
                        r.app.suppress_drive_alerts(&action.serial, action.suppress_secs);
                    }
                    self.drive_context_menu = None;
                }
                return;
            }
        }

        let window_count = self.renderers.len();
        let Some(renderer) = self.renderers.get_mut(&window_id) else { return };

        let resp = renderer.egui_state.on_window_event(&renderer.window, &event);
        if resp.repaint {
            renderer.window.request_redraw();
        }

        // `CloseRequested` / `RedrawRequested`-via-close both need to pick
        // between exit / drop-this-renderer / tray-minimize.  Set a flag
        // while the `&mut renderer` borrow is live, then act on it after
        // the borrow ends so we can mutate `self.renderers`.
        enum CloseDisposition { None, Exit, DropThis, HideThis }
        let mut disposition = CloseDisposition::None;
        // Set when we hid a window (collapsed to `None` so the outer match
        // doesn't double-handle it, but we still want to refresh the
        // any-visible flag for the single-instance watcher).
        let mut did_hide = false;
        // Closing the *last* window hides it to the tray only when the user has
        // opted in — installed builds default to on, the portable exe to off
        // (see `install_mode`). With it off the process exits, which stops
        // background monitoring until the next launch; that is the documented
        // trade-off shown under the Settings toggle.
        #[cfg(windows)]
        let can_hide_to_tray =
            self.shared.pro_edition && self.shared.settings_snapshot().close_to_tray;
        // No tray outside Windows, so hiding the last window would strand it.
        #[cfg(not(windows))]
        let can_hide_to_tray = false;
        let decide = |can_hide_to_tray: bool, window_count: usize| -> CloseDisposition {
            if window_count > 1 {
                CloseDisposition::DropThis
            } else if can_hide_to_tray {
                CloseDisposition::HideThis
            } else {
                CloseDisposition::Exit
            }
        };

        match event {
            WindowEvent::CloseRequested => {
                disposition = decide(can_hide_to_tray, window_count);
                if matches!(disposition, CloseDisposition::DropThis) {
                    renderer.app.cancel_all_tests();
                } else if matches!(disposition, CloseDisposition::HideThis) {
                    renderer.window.set_visible(false);
                    disposition = CloseDisposition::None;
                    did_hide = true;
                }
            }
            WindowEvent::RedrawRequested => {
                renderer.paint();
                if renderer.close_requested {
                    renderer.close_requested = false;
                    disposition = decide(can_hide_to_tray, window_count);
                    if matches!(disposition, CloseDisposition::DropThis) {
                        renderer.app.cancel_all_tests();
                    } else if matches!(disposition, CloseDisposition::HideThis) {
                        renderer.window.set_visible(false);
                        disposition = CloseDisposition::None;
                        did_hide = true;
                    }
                }
            }
            WindowEvent::Resized(size)
                if size.width > 0 && size.height > 0 => {
                    renderer.window.request_redraw();
                }
            _ => {}
        }

        match disposition {
            CloseDisposition::Exit => event_loop.exit(),
            CloseDisposition::DropThis => {
                self.renderers.remove(&window_id);
                #[cfg(windows)]
                self.refresh_any_visible_flag();
            }
            CloseDisposition::None | CloseDisposition::HideThis => {
                #[cfg(windows)]
                if did_hide {
                    self.refresh_any_visible_flag();
                }
            }
        }
        // First time the last window is hidden to the tray this session, let the
        // user know the app keeps running there. Once per process (tray_toast_shown).
        #[cfg(windows)]
        if did_hide && !self.tray_toast_shown {
            self.tray_toast_shown = true;
            // WinRT toasts require an MTA thread; the winit loop is STA.
            std::thread::spawn(|| {
                crate::toast::send_toast(
                    "Diskoria is still running",
                    "Diskoria is minimized to the system tray and keeps monitoring your drives. Right-click the tray icon to quit.",
                );
            });
        }
        #[cfg(not(windows))]
        let _ = did_hide;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            #[cfg(windows)]
            UserEvent::ShowWindowRequested => {
                for r in self.renderers.values() {
                    raise_window(&r.window);
                }
                self.refresh_any_visible_flag();
            }
            #[cfg(windows)]
            UserEvent::OpenNewWindow => {
                let mut renderer = Renderer::new(event_loop, self.shared.clone());
                if self.shared.pro_edition {
                    renderer.app.event_proxy = Some(self.proxy.clone());
                }
                raise_window(&renderer.window);
                let id = renderer.window.id();
                self.renderers.insert(id, renderer);
                self.refresh_any_visible_flag();
            }
            #[cfg(windows)]
            UserEvent::SettingsChanged { restart_monitor } => {
                if restart_monitor {
                    // Drop the monitor thread; the next draw in any window
                    // calls start_monitor_if_not_running and respawns it
                    // with the fresh thresholds.
                    self.shared.cancel_monitor();
                }
                for r in self.renderers.values() {
                    r.window.request_redraw();
                }
            }
            UserEvent::Repaint { after } => {
                // Record when the loop must next wake to repaint. `about_to_wait`
                // turns this into a `WaitUntil` and issues `request_redraw` to
                // every window. We never paint here directly: painting calls
                // `draw()`, which re-requests a repaint, which would re-enter
                // this handler in a tight loop that never yields to the OS
                // message pump — locking out other windows and the tray. An
                // ASAP request (`after == 0`) is clamped to one frame so a
                // continuously-animating window (e.g. running a test) settles
                // into a steady ~60 fps tick instead of spinning the CPU.
                let delay = if after.is_zero() { REPAINT_FRAME_CAP } else { after };
                let deadline = std::time::Instant::now() + delay;
                self.next_repaint =
                    Some(self.next_repaint.map_or(deadline, |d| d.min(deadline)));
            }
            UserEvent::QuitRequested => {
                // Cancel every window's in-flight tests so their worker
                // threads start unwinding before we drop the renderers.
                for r in self.renderers.values() {
                    r.app.cancel_all_tests();
                }
                // Cancel the monitor thread.
                #[cfg(windows)]
                self.shared.cancel_monitor();
                // Drop every renderer so per-window resources release before
                // the singletons below.
                self.renderers.clear();
                #[cfg(windows)]
                self.refresh_any_visible_flag();
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
                        self.flyout_icon_rect = Some(rect);

                        // Only (re)create the flyout if it isn't already open for this drive.
                        let already_open = self.flyout.is_some()
                            && self.flyout_drive_serial.as_deref() == Some(serial.as_str());
                        if !already_open {
                            let snapshot = self.shared
                                .snapshot_for(&serial)
                                .as_ref()
                                .map(crate::monitor::DriveSnapshot::from_snapshot)
                                .unwrap_or_else(|| crate::monitor::DriveSnapshot {
                                    serial: serial.clone(),
                                    model: serial.clone(),
                                    ..Default::default()
                                });
                            self.flyout = None;
                            self.flyout_drive_serial = Some(serial.clone());
                            let accent = self.primary()
                                .map(|r| r.app.shared.accent_color())
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
                        let accent = self.primary()
                            .map(|r| r.app.shared.accent_color())
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
                        // Left-click on the app icon → restore and raise every window.
                        for r in self.renderers.values() {
                            raise_window(&r.window);
                        }
                        self.refresh_any_visible_flag();
                    } else if let Some(pos) = tray.app_icon_right_click_pos(&e) {
                        log::info!(target: "diskoria::tray", "App icon right-clicked at {:?}", pos);
                        // Right-click on the app icon → custom themed context menu.
                        let accent = self.primary()
                            .map(|r| r.app.shared.accent_color())
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

    /// Last stop before the process ends: if an update was downloaded and left
    /// staged ("Update on close"), launch the installer now that Diskoria is
    /// releasing its exe. Reached from every real exit — the tray's Quit, and
    /// the last window closing when `close_to_tray` is off. Hiding to the tray
    /// is not an exit, so a staged update simply waits.
    #[cfg(windows)]
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(installer) = self.shared.take_staged_update() {
            log::info!(
                target: "diskoria",
                "applying staged update on exit: {}",
                installer.display()
            );
            // Stop worker threads first so nothing holds a device handle while
            // the installer swaps the exe.
            for r in self.renderers.values_mut() {
                r.app.cancel_all_tests();
            }
            crate::update::spawn_installer(&installer);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Boot-smoke mode: paint every window once per tick (driving the full
        // `draw()` path), then exit after the requested frame count. Bypasses the
        // normal repaint pacing — a few frames is enough to surface a panic.
        if let Some(n) = self.smoke_remaining {
            let ids: Vec<WindowId> = self.renderers.keys().copied().collect();
            for id in ids {
                if let Some(r) = self.renderers.get_mut(&id) {
                    r.paint();
                }
            }
            if n <= 1 {
                log::info!(target: "diskoria", "DISKORIA_SMOKE: render complete, exiting cleanly");
                self.smoke_remaining = None;
                event_loop.exit();
            } else {
                self.smoke_remaining = Some(n - 1);
                event_loop.set_control_flow(ControlFlow::Poll);
            }
            return;
        }

        // Earliest instant the loop must wake. Combines any pending repaint
        // (background data or animation, tracked in `next_repaint`) with the
        // Windows cursor/flash poll cadence below.
        let mut wake_at: Option<std::time::Instant> = None;

        // Service a due repaint by painting every window *directly*. We do not
        // use `request_redraw` here: it posts a low-priority WM_PAINT that
        // Windows starves for a background window whenever another window of
        // this process holds focus and pumps messages — the exact case of a
        // second window running a sector test. Painting is paced from this
        // timer (not per repaint-request event) so the loop keeps returning to
        // the OS message pump between frames, keeping every window and the tray
        // responsive. Repainting *all* windows propagates updates that a
        // background thread requested on just one window's context (e.g. the
        // monitor's shared snapshot data).
        let due = matches!(self.next_repaint, Some(d) if std::time::Instant::now() >= d);
        if due {
            self.next_repaint = None;
            let ids: Vec<WindowId> = self.renderers.keys().copied().collect();
            for id in ids {
                if let Some(r) = self.renderers.get_mut(&id) {
                    r.paint();
                    if r.close_requested {
                        // Route close disposition through the RedrawRequested path.
                        r.window.request_redraw();
                    }
                }
            }
        }
        if let Some(deadline) = self.next_repaint {
            wake_at = Some(deadline);
        }

        // Device-change auto-refresh: coalesce WM_DEVICECHANGE bursts behind a
        // short debounce, then re-enumerate drives so plug/unplug is picked up
        // without the user clicking Refresh.
        if crate::chrome::take_device_change_pending() {
            self.device_change_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
        }
        if let Some(dl) = self.device_change_deadline {
            if std::time::Instant::now() >= dl {
                self.device_change_deadline = None;
                self.auto_reenumerate_drives();
            } else {
                wake_at = Some(wake_at.map_or(dl, |w| w.min(dl)));
            }
        }

        // Rebuild per-drive tray icons if the drive list changed.  The
        // dirty flag is a singleton on SharedAppState, so opening a
        // second window does not retrigger a rebuild and wipe the
        // monitor-driven temperature rendering.
        #[cfg(windows)]
        {
            let has_drives = self.primary()
                .map(|r| !r.app.drives.is_empty())
                .unwrap_or(false);

            if has_drives && self.shared.take_drive_icons_dirty() {
                if let (Some(tray), Some(renderer)) = (&mut self.tray, self.renderers.values_mut().next()) {
                    tray.rebuild_drive_icons(&renderer.app.drives);
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
                let poll_at = std::time::Instant::now() + wait;
                wake_at = Some(wake_at.map_or(poll_at, |w| w.min(poll_at)));
            }
        }

        // Schedule the next wake-up. With a pending deadline, wake then; with
        // none, return to idle `Wait`. Resetting to `Wait` matters because
        // control flow is sticky: an elapsed `WaitUntil` left in place spins
        // the loop until the next OS event.
        match wake_at {
            Some(deadline) => event_loop
                .set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait),
        }
    }
}

// ── Single-instance guard ─────────────────────────────────────────────────────

/// Creates a named mutex to enforce a single running instance.
///
/// If another instance already holds the mutex, its window is raised and this
/// process exits immediately.  Returns the raw mutex HANDLE, which must be kept
/// alive for the duration of the process (dropping it releases the lock).
#[cfg(windows)]
const SHOW_WINDOW_EVENT_NAME: &str = "Local\\DiskoriaShowWindow\0";

#[cfg(windows)]
const NEW_WINDOW_EVENT_NAME: &str = "Local\\DiskoriaNewWindow\0";

/// Named 4-byte file mapping the primary process uses to publish whether
/// any of its windows is currently visible.  The secondary process reads
/// this on launch to decide which named event to pulse — raise hidden
/// windows, or open a new one alongside the visible ones.
#[cfg(windows)]
const ANY_VISIBLE_MAP_NAME: &str = "Local\\DiskoriaAnyVisible\0";

/// RAII writer for the `DiskoriaAnyVisible` file mapping.  Only the primary
/// process creates one; the mapping lives until the process exits because
/// the kernel destroys it when the last handle closes.
#[cfg(windows)]
struct AnyVisibleFlag {
    mapping: windows_sys::Win32::Foundation::HANDLE,
    ptr: *mut u32,
}

#[cfg(windows)]
impl AnyVisibleFlag {
    fn new() -> Option<Self> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MapViewOfFile, FILE_MAP_ALL_ACCESS, PAGE_READWRITE,
        };

        let name: Vec<u16> = ANY_VISIBLE_MAP_NAME.encode_utf16().collect();
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                4,
                name.as_ptr(),
            )
        };
        if mapping == 0 {
            return None;
        }
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, 4) };
        if view.Value.is_null() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(mapping) };
            return None;
        }
        let ptr = view.Value as *mut u32;
        unsafe { std::ptr::write_volatile(ptr, 0) };
        Some(Self { mapping, ptr })
    }

    fn set(&self, visible: bool) {
        unsafe { std::ptr::write_volatile(self.ptr, if visible { 1 } else { 0 }) };
    }
}

#[cfg(windows)]
impl Drop for AnyVisibleFlag {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{UnmapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS};
        unsafe {
            if !self.ptr.is_null() {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.ptr as *mut _,
                });
            }
            if self.mapping != 0 {
                CloseHandle(self.mapping);
            }
        }
    }
}

/// Secondary-process side: try to read the primary's `any visible` flag.
/// Returns `None` if the mapping isn't published yet (race at primary
/// startup) — the caller falls back to the old "always raise" behaviour.
#[cfg(windows)]
fn read_any_visible_flag() -> Option<bool> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
    };
    let name: Vec<u16> = ANY_VISIBLE_MAP_NAME.encode_utf16().collect();
    unsafe {
        let h = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if h == 0 {
            return None;
        }
        let view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, 4);
        if view.Value.is_null() {
            CloseHandle(h);
            return None;
        }
        let val = std::ptr::read_volatile(view.Value as *const u32);
        UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        CloseHandle(h);
        Some(val != 0)
    }
}

#[cfg(windows)]
fn acquire_single_instance_mutex(start_minimized: bool) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::{CreateMutexW, OpenEventW, SetEvent, EVENT_MODIFY_STATE};

    let mutex_name: Vec<u16> = "Local\\DiskoriaSingleInstance\0"
        .encode_utf16()
        .collect();

    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr()) };

    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        // Auto-start (`--minimized`) racing an already-running instance: the
        // user is already covered, so just exit quietly. Never raise the
        // existing (possibly hidden) instance — that would defeat "stay in the
        // tray".
        if start_minimized {
            std::process::exit(0);
        }
        // Another instance is running — signal it via a named event so its
        // own winit thread reacts (bypassing winit's state tracker causes
        // caption-button desync).  Which event we pulse depends on whether
        // the primary has any visible windows right now: all hidden means
        // "raise what's there", any visible means "open another window".
        let any_visible = read_any_visible_flag().unwrap_or(false);
        let target_event = if any_visible {
            NEW_WINDOW_EVENT_NAME
        } else {
            SHOW_WINDOW_EVENT_NAME
        };
        let event_name: Vec<u16> = target_event.encode_utf16().collect();
        unsafe {
            let ev = OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr());
            if ev != 0 {
                SetEvent(ev);
                CloseHandle(ev);
            }
        }
        std::process::exit(0);
    }

    handle
}

/// Create the named event used to signal duplicate launches, then spawn a
/// watcher thread that translates each signal into a `UserEvent` on the winit
/// event loop.  Returns the event HANDLE, which must be kept alive for the
/// process lifetime (dropping it destroys the kernel object).
#[cfg(windows)]
fn spawn_show_window_watcher(
    proxy: EventLoopProxy<UserEvent>,
) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

    let event_name: Vec<u16> = SHOW_WINDOW_EVENT_NAME.encode_utf16().collect();
    // Manual-reset = 0 (auto-reset), initially non-signaled = 0.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    if event == 0 {
        log::warn!(target: "diskoria", "failed to create show-window event");
        return 0;
    }

    let event_for_thread = event;
    std::thread::spawn(move || loop {
        let rc = unsafe { WaitForSingleObject(event_for_thread, INFINITE) };
        if rc != WAIT_OBJECT_0 {
            break;
        }
        if proxy.send_event(UserEvent::ShowWindowRequested).is_err() {
            break;
        }
    });

    event
}

/// Sibling of `spawn_show_window_watcher` for the new-window path.  The
/// secondary process signals `DiskoriaNewWindow` when any primary window
/// is currently visible; this watcher forwards the signal into the winit
/// loop as `UserEvent::OpenNewWindow`.
#[cfg(windows)]
fn spawn_new_window_watcher(
    proxy: EventLoopProxy<UserEvent>,
) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

    let event_name: Vec<u16> = NEW_WINDOW_EVENT_NAME.encode_utf16().collect();
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    if event == 0 {
        log::warn!(target: "diskoria", "failed to create new-window event");
        return 0;
    }

    let event_for_thread = event;
    std::thread::spawn(move || loop {
        let rc = unsafe { WaitForSingleObject(event_for_thread, INFINITE) };
        if rc != WAIT_OBJECT_0 {
            break;
        }
        if proxy.send_event(UserEvent::OpenNewWindow).is_err() {
            break;
        }
    });

    event
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("diskoria=debug,warn"),
    )
    .format_timestamp_millis()
    .try_init();

    // Install mode goes in the banner because the single-instance guard does not
    // distinguish builds: launching an installed copy while a *portable* one is
    // already running just raises the portable window, which then correctly
    // reports "Portable" and looks like the installer failed (known-issues
    // KI-28). Logging the exe path next to it makes that one line to check.
    log::info!(
        target: "diskoria",
        "Diskoria {} starting ({}, exe={})",
        env!("CARGO_PKG_VERSION"),
        install_mode::current().label(),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string()),
    );

    // Parse `--page` / `--demo-*` once, so the log records what a capture run
    // was seeded with. See `demo.rs` — none of it touches a disk.
    demo::init();

    // `--demo-export`: write the Export Log reports and quit. Headless, so it
    // never opens a window or reaches a real disk.
    if demo::write_export_reports() {
        return;
    }

    // Boot-smoke mode: render a few frames then exit. Defaults to 3 frames;
    // `DISKORIA_SMOKE=N` overrides the count.
    let smoke_remaining: Option<u32> = std::env::var("DISKORIA_SMOKE").ok().map(|v| {
        v.trim().parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(3)
    });
    if let Some(n) = smoke_remaining {
        log::info!(target: "diskoria", "DISKORIA_SMOKE active: rendering {n} frame(s) then exiting");
    }

    // Auto-start launches Diskoria with `--minimized` (via the logon scheduled
    // task) so it comes up tray-only.
    #[cfg(windows)]
    let start_minimized = std::env::args().skip(1).any(|a| a == "--minimized");

    // Skip the single-instance guard under smoke — and under demo seeding, so a
    // capture run does not just hand off to the author's real instance — leaving
    // the test run independent of any already-running Diskoria.
    #[cfg(windows)]
    let _single_instance_mutex = (smoke_remaining.is_none() && !demo::seeding())
        .then(|| acquire_single_instance_mutex(start_minimized));

    // `--demo-toast`: fire one sample notification so the toast can be
    // photographed. Backgrounded because WinRT needs a COM MTA context.
    #[cfg(windows)]
    if demo::config().toast {
        std::thread::spawn(|| {
            let (title, body) = demo::sample_toast();
            crate::toast::send_toast(&title, &body);
        });
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();

    let shared = Arc::new(SharedAppState::new(proxy.clone()));

    #[cfg(windows)]
    let _show_window_event = spawn_show_window_watcher(proxy.clone());
    #[cfg(windows)]
    let _new_window_event = spawn_new_window_watcher(proxy.clone());

    let mut app = App {
        renderers: HashMap::new(),
        proxy,
        shared,
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
        #[cfg(windows)]
        any_visible_flag: AnyVisibleFlag::new(),
        next_repaint: None,
        smoke_remaining,
        device_change_deadline: None,
        #[cfg(windows)]
        tray_toast_shown: false,
        #[cfg(windows)]
        start_minimized,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!(target: "diskoria", "event loop error: {e}");
    }
}
