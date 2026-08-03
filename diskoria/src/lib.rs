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
#[cfg(any(windows, target_os = "linux"))]
mod autostart;
mod chrome;
mod demo;
#[cfg(target_os = "linux")]
mod compositor_focus;
#[cfg(target_os = "linux")]
mod device_events;
#[cfg(target_os = "linux")]
mod elevation;
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
#[cfg(unix)]
mod single_instance;
mod smart_health;
pub mod smart_reader;
mod smart_health_page;
mod test_result_overlay;
mod theme;
#[cfg(target_os = "linux")]
mod service;
#[cfg(target_os = "linux")]
mod service_control;
mod update;
mod watchdog;
mod widgets;

// Pro-Monitoring modules
pub mod alert_engine;
#[cfg(windows)]
pub mod flyout;
pub mod history_db;
pub mod monitor;
pub mod toast;
#[cfg(any(windows, target_os = "linux"))]
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
    #[cfg(any(windows, target_os = "linux"))]
    TrayIconUpdate { serial: String, temp_c: Option<i32> },
    /// Flash (Windows) / flag (Linux SNI attention) a drive alert on the tray.
    #[cfg(any(windows, target_os = "linux"))]
    DriveAlert { serial: String, is_critical: bool },
    /// Graceful shutdown requested (from tray context menu "Quit").
    QuitRequested,
    /// Another exe launch signaled us via the single-instance channel (a named
    /// event on Windows, the unix socket elsewhere). Raise the main window
    /// through winit so visibility state stays in sync.
    ShowWindowRequested,
    /// A second launch connected to the unix single-instance socket. The
    /// handler decides raise-vs-new-window from its own renderer state —
    /// Windows makes that call in the *secondary* process via the shared
    /// visibility flag instead, and pulses one of two named events.
    #[cfg(unix)]
    SecondLaunch,
    /// Open an additional Diskoria window in the primary process.  Sent by the
    /// in-app Ctrl+N shortcut, the tray "New Window" menu item, and the
    /// secondary-exe new-window path.
    OpenNewWindow,
    /// Settings changed in some window.  Every renderer redraws so the
    /// next frame picks up the new shared values.  If `restart_monitor`
    /// is true, also tear down the monitor thread so the next draw
    /// respawns it with fresh thresholds.
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

use rayon::prelude::*;

use crate::tex_mgr::TextureManager;


// ── Damage tracking ───────────────────────────────────────────────────────────

/// An inclusive rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RectPx {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl RectPx {
    fn is_empty(&self) -> bool {
        self.x1 < self.x0 || self.y1 < self.y0
    }

    fn union(self, o: Self) -> Self {
        Self {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// Overlapping *or touching*: merging both keeps damage regions disjoint,
    /// which matters because a translucent primitive drawn twice blends twice.
    fn touches(&self, o: &Self) -> bool {
        self.x0 <= o.x1 + 1 && o.x0 <= self.x1 + 1 && self.y0 <= o.y1 + 1 && o.y0 <= self.y1 + 1
    }

    fn clamped(self, w: u32, h: u32) -> Self {
        Self {
            x0: self.x0.max(0),
            y0: self.y0.max(0),
            x1: self.x1.min(w as i32 - 1),
            y1: self.y1.min(h as i32 - 1),
        }
    }
}

/// Damage regions are merged until disjoint, then capped: past a handful of
/// rects the per-rect overhead beats the pixels saved, and one bounding box is
/// simpler to reason about than fifty slivers.
const MAX_DAMAGE_RECTS: usize = 8;

/// Above this many *raw* rectangles, skip straight to the bounding box.
///
/// `frame_damage` emits two rectangles per changed triangle, so a scroll or a
/// heatmap recolor arrives here with thousands. Those frames repaint nearly
/// everything regardless, and the result is capped at [`MAX_DAMAGE_RECTS`]
/// anyway, so there is nothing to be gained by merging them precisely.
const RAW_DAMAGE_LIMIT: usize = 4096;

/// Above this many rectangles *after* the sweep below, likewise.
const MERGE_BUDGET: usize = 256;

/// Reduce damage rectangles to a small disjoint set covering every input.
///
/// Disjointness is the correctness requirement: the rasterizer blends, so a
/// pixel inside two damage rectangles is composited twice and comes out wrong.
/// Everything else here is about cost.
///
/// Cost is the whole story, in fact. This used to be a fixpoint loop that
/// restarted its entire O(n²) scan after *every* merge and removed from the
/// middle of the `Vec` each time — O(n³), on the event-loop thread, over the
/// thousands of rectangles a scroll produces. That was the freeze in KI-43:
/// seconds of work to compute a set that the `MAX_DAMAGE_RECTS` cap (8) was
/// about to collapse into a single rectangle anyway, because the cap was
/// applied *after* the loop rather than before it.
///
/// Now the work is bounded up front and done in three tiers:
/// 1. Too many raw rectangles → bounding box, no merging at all.
/// 2. A linear sweep over y-then-x-sorted rectangles, which collapses the
///    common case cheaply: text damage arrives one rectangle per glyph, and
///    neighbouring glyphs touch, so a line of text becomes one rectangle.
/// 3. The exact pairwise fixpoint on what survives — correct, and now only
///    ever run on a set small enough for it to be free.
fn merge_damage(mut rects: Vec<RectPx>) -> Vec<RectPx> {
    rects.retain(|r| !r.is_empty());
    if rects.len() <= 1 {
        return rects;
    }
    if rects.len() > RAW_DAMAGE_LIMIT {
        return bounding_box(rects);
    }

    // Sweep: sort by row then column and fuse touching neighbours in one pass.
    rects.sort_unstable_by_key(|r| (r.y0, r.x0));
    let mut swept: Vec<RectPx> = Vec::with_capacity(rects.len());
    for r in rects {
        match swept.last_mut() {
            Some(last) if last.touches(&r) => *last = last.union(r),
            _ => swept.push(r),
        }
    }
    if swept.len() > MERGE_BUDGET {
        return bounding_box(swept);
    }

    // Exact fixpoint. The sweep only fuses neighbours in sort order, so
    // rectangles that touch "backwards" (a tall rectangle overlapping several
    // later rows) are still outstanding; repeat full passes until a pass
    // changes nothing, which is what makes the result pairwise disjoint.
    // `swap_remove` is O(1) and order does not matter here.
    loop {
        let mut changed = false;
        let mut i = 0;
        while i < swept.len() {
            let mut j = i + 1;
            while j < swept.len() {
                if swept[i].touches(&swept[j]) {
                    swept[i] = swept[i].union(swept[j]);
                    swept.swap_remove(j);
                    changed = true;
                    // `j` now holds a rectangle swapped in from the end, which
                    // has not been tested against `i` yet — do not advance.
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
        if !changed {
            break;
        }
    }

    if swept.len() > MAX_DAMAGE_RECTS {
        return bounding_box(swept);
    }
    swept
}

/// The one rectangle covering them all. Trivially disjoint, and always a valid
/// (if pessimistic) answer — worst case it means a full repaint.
fn bounding_box(rects: Vec<RectPx>) -> Vec<RectPx> {
    rects.into_iter().reduce(RectPx::union).into_iter().collect()
}

/// What to actually redraw: this frame's own damage, plus what earlier frames
/// changed when softbuffer hands back a stale buffer (`Buffer::age`).
///
/// **Every rectangle is clamped to the current framebuffer**, which is the
/// whole reason this is a function. History entries were clamped to the size of
/// the frame that recorded them, so once the window shrinks they can reach past
/// the end of a row — and the rasterizer's `band[row + x0..=row + x1]` then
/// panics with an out-of-range slice index, killing every rayon worker at once.
/// That was KI-44, hit by resizing the window rapidly.
fn replay_damage(
    own: &[RectPx],
    history: &[Vec<RectPx>],
    buffer_age: u8,
    w: u32,
    h: u32,
) -> Vec<RectPx> {
    let full_rect = RectPx { x0: 0, y0: 0, x1: w as i32 - 1, y1: h as i32 - 1 };
    let mut to_draw: Vec<RectPx> = own.to_vec();
    if buffer_age == 0 {
        // Undefined contents — everything is stale.
        to_draw.push(full_rect);
    } else {
        for past in history.iter().take(buffer_age.saturating_sub(1) as usize) {
            to_draw.extend_from_slice(past);
        }
    }
    merge_damage(to_draw.into_iter().map(|r| r.clamped(w, h)).collect())
}

/// `DISKORIA_FULL_REPAINT=1` disables damage tracking, for comparing a
/// suspected artifact against a known-good full redraw.
fn force_full_repaints() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("DISKORIA_FULL_REPAINT").is_some())
}

/// `DISKORIA_DAMAGE_VERIFY=1` re-renders every frame in full into a scratch
/// buffer and compares it against what damage tracking actually produced,
/// logging any mismatch.
///
/// This is the check that matters for partial redraw: the failure mode is a
/// stale pixel nobody notices, and it depends on the *sequence* of frames, so
/// comparing two separate runs cannot catch it. Doubles the work while on.
fn verify_damage() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("DISKORIA_DAMAGE_VERIFY").is_some())
}

#[inline]
fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^ (x >> 33)
}

/// One triangle's fingerprint and the pixels it covers.
///
/// Damage is tracked per *triangle*, not per primitive: egui batches an entire
/// page into a handful of meshes, so a primitive-level diff reports "the whole
/// page changed" the moment one label ticks over, which is no better than
/// repainting everything.
#[derive(Clone, Copy)]
struct TriPrint {
    hash: u64,
    area: RectPx,
}

/// A clipped primitive's identity plus its triangles.
#[derive(Clone, Default)]
struct PrimPrint {
    /// Texture and clip rect — if either changes the whole primitive is
    /// suspect, since every triangle in it may sample or clip differently.
    key: u64,
    tris: Vec<TriPrint>,
}

impl PrimPrint {
    /// Every pixel this primitive can touch.
    fn area(&self) -> Option<RectPx> {
        self.tris.iter().map(|t| t.area).reduce(RectPx::union)
    }
}

/// Fingerprint one clipped primitive, triangle by triangle.
fn prim_print(prim: &egui::ClippedPrimitive, ppp: f32, w: u32, h: u32) -> PrimPrint {
    let clip = prim.clip_rect;
    let clip_px = RectPx {
        x0: (clip.min.x * ppp).floor() as i32,
        y0: (clip.min.y * ppp).floor() as i32,
        x1: (clip.max.x * ppp).ceil() as i32,
        y1: (clip.max.y * ppp).ceil() as i32,
    };

    let mut key = mix64(0x9e37_79b9_7f4a_7c15);
    for v in [clip.min.x, clip.min.y, clip.max.x, clip.max.y] {
        key = mix64(key ^ v.to_bits() as u64);
    }

    let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive else {
        // Not something this renderer draws; make it always-changed so it can
        // never leave stale pixels behind.
        return PrimPrint {
            key: mix64(key ^ 0xdead_beef),
            tris: vec![TriPrint {
                hash: mix64(key),
                area: clip_px.clamped(w, h),
            }],
        };
    };
    key = mix64(key ^ format!("{:?}", mesh.texture_id).len() as u64);
    key = mix64(key ^ mesh.indices.len() as u64);

    let verts = &mesh.vertices;
    let mut tris = Vec::with_capacity(mesh.indices.len() / 3);
    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (Some(v0), Some(v1), Some(v2)) = (verts.get(i0), verts.get(i1), verts.get(i2)) else {
            continue;
        };
        let mut hash = mix64(0x517c_c1b7_2722_0a95);
        let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
        let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
        for v in [v0, v1, v2] {
            hash = mix64(hash ^ ((v.pos.x.to_bits() as u64) << 32 | v.pos.y.to_bits() as u64));
            hash = mix64(hash ^ ((v.uv.x.to_bits() as u64) << 32 | v.uv.y.to_bits() as u64));
            hash = mix64(hash ^ u32::from_le_bytes(v.color.to_array()) as u64);
            min_x = min_x.min(v.pos.x);
            min_y = min_y.min(v.pos.y);
            max_x = max_x.max(v.pos.x);
            max_y = max_y.max(v.pos.y);
        }
        // One pixel of slack each way covers the edge epsilon and rounding.
        let area = RectPx {
            x0: (min_x * ppp).floor() as i32 - 1,
            y0: (min_y * ppp).floor() as i32 - 1,
            x1: (max_x * ppp).ceil() as i32 + 1,
            y1: (max_y * ppp).ceil() as i32 + 1,
        };
        let area = RectPx {
            x0: area.x0.max(clip_px.x0),
            y0: area.y0.max(clip_px.y0),
            x1: area.x1.min(clip_px.x1),
            y1: area.y1.min(clip_px.y1),
        }
        .clamped(w, h);
        if !area.is_empty() {
            tris.push(TriPrint { hash, area });
        }
    }

    PrimPrint { key, tris }
}

/// Rectangles to redraw: every triangle whose fingerprint changed, in both its
/// old and its new position.
fn frame_damage(prev: &[PrimPrint], cur: &[PrimPrint]) -> Vec<RectPx> {
    let mut out = Vec::new();
    for i in 0..prev.len().max(cur.len()) {
        match (prev.get(i), cur.get(i)) {
            (Some(p), Some(c)) if p.key == c.key && p.tris.len() == c.tris.len() => {
                for (pt, ct) in p.tris.iter().zip(&c.tris) {
                    if pt.hash != ct.hash {
                        out.push(pt.area);
                        out.push(ct.area);
                    }
                }
            }
            (p, c) => {
                // Different texture, clip rect or triangle count: the whole
                // primitive is repainted, old position and new.
                out.extend(p.and_then(PrimPrint::area));
                out.extend(c.and_then(PrimPrint::area));
            }
        }
    }
    out
}

// ── Renderer ──────────────────────────────────────────────────────────────────

struct Renderer {
    /// Per-primitive fingerprints from the previous frame; the diff against
    /// this frame's is the damage (see `frame_damage`).
    prev_prims: Vec<PrimPrint>,
    /// Damage applied in recent frames, newest first. softbuffer can hand back
    /// a buffer that is several frames old (`Buffer::age`), in which case
    /// everything drawn since then has to be drawn again.
    damage_history: Vec<Vec<RectPx>>,
    /// Set when the next frame cannot be trusted to a diff — first frame,
    /// resize, theme change, or a texture-atlas update that can repaint
    /// pixels without changing any mesh.
    force_full_repaint: bool,
    last_bg: u32,
    last_size: (u32, u32),
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

        // app_id (Wayland) / WM_CLASS (X11). Without it compositors cannot tie
        // the window to linux/diskoria.desktop — no icon in docks or alt-tab,
        // and window rules cannot target it. Both are set: whichever backend
        // is not in use ignores its attribute. Fully qualified because the two
        // extension traits both spell the setter `with_name`.
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            let a = WindowAttributesExtWayland::with_name(
                Window::default_attributes(),
                "diskoria",
                "",
            );
            WindowAttributesExtX11::with_name(a, "diskoria", "Diskoria")
        };
        #[cfg(not(target_os = "linux"))]
        let attrs = Window::default_attributes();

        let mut attrs = attrs
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(800_u32, 600_u32))
            // Far below the old 780x580: the nav collapses to an icon rail and
            // then to a hamburger menu as the window narrows, so there is no
            // longer a width at which the sidebar alone squeezes the content
            // out (theme::nav_mode).
            .with_min_inner_size(LogicalSize::new(380_u32, 480_u32))
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
            prev_prims: Vec::new(),
            damage_history: Vec::new(),
            force_full_repaint: true,
            last_bg: 0,
            last_size: (0, 0),
            window,
            surface,
            egui_ctx,
            egui_state,
            app,
            tex_mgr: TextureManager::new(),
            close_requested: false,
        }
    }

    /// Show and raise this window, forcing the next frame to be a full repaint.
    ///
    /// Damage tracking asks "what changed since the last frame we drew", and a
    /// window coming back from the tray answers "nothing" — the UI really is
    /// identical. But hiding a window discards what was on screen, so an empty
    /// damage set meant `paint` skipped the frame entirely and the window came
    /// back see-through, filling in only where a later hover happened to damage
    /// something (KI-50).
    ///
    /// `Buffer::age` cannot catch this: softbuffer keeps handing back buffers
    /// and reports age 1, which is true of the buffer and false of the screen.
    /// Visibility is the app's own knowledge, so the app has to say so.
    fn raise(&mut self) {
        self.force_full_repaint = true;
        // Nothing on screen survived the hide, so recorded damage no longer
        // describes anything a replay could catch up from.
        self.damage_history.clear();
        raise_window(&self.window);
        self.window.request_redraw();
    }

    fn paint(&mut self) {
        // Per-stage frame timing, off unless DISKORIA_FRAME_STATS is set.
        // Rendering is a hand-written CPU rasterizer, so "where did the frame
        // go" is a question worth being able to answer on any machine.
        static STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let stats = *STATS.get_or_init(|| std::env::var_os("DISKORIA_FRAME_STATS").is_some());
        let t_start = std::time::Instant::now();

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

        crate::watchdog::enter(crate::watchdog::Phase::PaintUi);
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            self.app.draw(ctx);
        });
        let t_ui = t_start.elapsed();

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
                    // Sent by `chrome::handle_edge_resize` (the non-Windows
                    // resize path; Windows resizes via the WM_NCHITTEST
                    // subclass and never emits this). The compositor-driven
                    // resize loop swallows the release just like drag_window
                    // (KI-2), so synthesize it the same way.
                    #[cfg(not(windows))]
                    egui::ViewportCommand::BeginResize(dir) => {
                        use egui::viewport::ResizeDirection as Erd;
                        use winit::window::ResizeDirection as Wrd;
                        let wdir = match dir {
                            Erd::North => Wrd::North,
                            Erd::South => Wrd::South,
                            Erd::East => Wrd::East,
                            Erd::West => Wrd::West,
                            Erd::NorthEast => Wrd::NorthEast,
                            Erd::NorthWest => Wrd::NorthWest,
                            Erd::SouthEast => Wrd::SouthEast,
                            Erd::SouthWest => Wrd::SouthWest,
                        };
                        if let Err(e) = self.window.drag_resize_window(wdir) {
                            log::debug!(target: "diskoria", "drag_resize_window: {e}");
                        }
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

        // A hidden window still runs the egui pass above — that is what keeps
        // drive enumeration and the monitor alive in tray-only mode — but must
        // not reach the compositor. On Wayland "visible" *is* "has a committed
        // buffer", so presenting here re-mapped the window a frame after
        // `--minimized` hid it, and the app came up on screen anyway.
        if !self.app.window_visible {
            return;
        }

        let t_before_tess = std::time::Instant::now();
        crate::watchdog::enter(crate::watchdog::Phase::PaintTessellate);
        let ppp = full_output.pixels_per_point;
        let clipped = self.egui_ctx.tessellate(full_output.shapes, ppp);
        let t_tess = t_before_tess.elapsed();
        let t_before_raster = std::time::Instant::now();

        let bg = Theme::new(self.app.dark, self.app.shared.accent_color()).bg_pri;
        let bg32 = to_bgra(bg.r(), bg.g(), bg.b(), 255);


        // What actually needs redrawing this frame.
        //
        // Worked out *before* touching the surface on purpose: acquiring a
        // buffer blocks until the compositor releases the back buffer
        // (softbuffer's Wayland backend loops on `blocking_dispatch`), so a
        // frame with nothing to draw must not ask for one — otherwise an idle
        // window takes a Wayland round trip thousands of times a second, on the
        // event-loop thread, which is what froze every window during a sector
        // scan (KI-42).
        crate::watchdog::enter(crate::watchdog::Phase::PaintDamage);
        let prints: Vec<PrimPrint> = clipped
            .iter()
            .map(|p| prim_print(p, ppp, w, h))
            .collect();

        let full_rect = RectPx { x0: 0, y0: 0, x1: w as i32 - 1, y1: h as i32 - 1 };
        let size_changed = self.last_size != (w, h);
        let bg_changed = self.last_bg != bg32;
        // Only a *replaced* texture can change pixels already on screen, and
        // that cannot be diffed from the meshes — repaint everything. egui
        // appends newly rasterized glyphs to the font atlas most frames
        // (`pos: Some(..)`), leaving existing glyphs where they were; any mesh
        // that starts using them has a changed fingerprint of its own.
        let textures_changed = full_output
            .textures_delta
            .set
            .iter()
            .any(|(_, delta)| delta.pos.is_none())
            || !full_output.textures_delta.free.is_empty();
        let forced_flag = self.force_full_repaint;
        let force_full = forced_flag
            || size_changed
            || bg_changed
            || textures_changed
            || force_full_repaints();

        // This frame's own changes. History holds *these*, not the rectangles
        // finally redrawn: a catch-up region belongs to the frame that
        // originally changed, and folding applied damage back in would make one
        // full repaint propagate forever.
        let own_damage: Vec<RectPx> = if force_full {
            vec![full_rect]
        } else {
            merge_damage(
                frame_damage(&self.prev_prims, &prints)
                    .into_iter()
                    .map(|r| r.clamped(w, h))
                    .collect(),
            )
        };
        let raw_damage = own_damage.len();

        self.prev_prims = prints;
        self.last_size = (w, h);
        self.last_bg = bg32;
        self.force_full_repaint = false;

        // A resize reallocates the surface's buffers, so `Buffer::age` cannot
        // refer to anything drawn at the old size and those rectangles are in
        // the old coordinate space. Drop them — `force_full` already covers
        // this frame. `replay_damage` clamps regardless, but not replaying
        // meaningless damage is the actual intent (KI-44).
        if size_changed {
            self.damage_history.clear();
        }

        if own_damage.is_empty() {
            // Nothing changed: leave the surface alone entirely, and do *not*
            // record a history entry. `Buffer::age` counts presents, so the
            // history has to hold one entry per frame actually drawn —
            // recording skipped frames pushed the real ones out of reach of
            // the replay and left most of the window stale (caught by
            // DISKORIA_DAMAGE_VERIFY).
            if stats {
                log::info!(target: "diskoria::frame", "{w}x{h}: nothing damaged, frame skipped");
            }
            return;
        }

        crate::watchdog::enter(crate::watchdog::Phase::PaintSurfaceAcquire);
        if self
            .surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .is_err()
        {
            return;
        }
        let Ok(mut buf) = self.surface.buffer_mut() else {
            return;
        };

        // The buffer handed back is `age` frames old, so it also needs whatever
        // the frames in between changed; age 0 means undefined contents.
        let buffer_age = buf.age();
        let damage = replay_damage(&own_damage, &self.damage_history, buffer_age, w, h);

        self.damage_history.insert(0, own_damage);
        self.damage_history.truncate(4);

        if stats && (force_full || buffer_age == 0) {
            log::info!(
                target: "diskoria::frame",
                "full repaint: first/forced={forced_flag} resized={size_changed} \
                 theme={bg_changed} textures={textures_changed} buffer_age={buffer_age}"
            );
        }

        // Rasterize in horizontal bands, one rayon task each, restricted to the
        // damaged rectangles. Bands are disjoint slices of the framebuffer, and
        // every band walks the primitive list in the same order, so
        // painter's-algorithm ordering is preserved. Damage rects are merged
        // until disjoint so no pixel is blended twice.
        //
        // 32 rows keeps tasks small enough for rayon to balance a frame whose
        // work sits mostly in a few large panels.
        const BAND_ROWS: u32 = 32;
        crate::watchdog::enter(crate::watchdog::Phase::PaintRasterize);
        let tex_mgr = &self.tex_mgr;
        let damage_ref = &damage;
        let px_tested: u64 = buf
            .par_chunks_mut((BAND_ROWS * w) as usize)
            .enumerate()
            .map(|(band_idx, band)| {
                let band_y0 = band_idx as i32 * BAND_ROWS as i32;
                let rows = (band.len() / w as usize) as i32;
                let band_y1 = band_y0 + rows - 1;
                let mut tested = 0u64;
                for d in damage_ref {
                    if d.y1 < band_y0 || d.y0 > band_y1 {
                        continue;
                    }
                    let rect = RectPx {
                        x0: d.x0,
                        y0: d.y0.max(band_y0),
                        x1: d.x1,
                        y1: d.y1.min(band_y1),
                    };
                    // Damage must already be inside the framebuffer — the
                    // slicing below indexes a row directly, and an escaped
                    // rectangle panics every rayon worker at once (KI-44).
                    debug_assert!(
                        rect.x0 >= 0 && rect.x1 < w as i32,
                        "damage {rect:?} escapes a {w}px-wide framebuffer"
                    );
                    // Clear only the damaged span of each row: the rest of the
                    // row is still valid from an earlier frame.
                    for py in rect.y0..=rect.y1 {
                        let row = ((py - band_y0) as usize) * w as usize;
                        band[row + rect.x0 as usize..=row + rect.x1 as usize].fill(bg32);
                    }
                    tested += rasterize_band(band, band_y0, rect, w, &clipped, ppp, tex_mgr);
                }
                tested
            })
            .sum();

        if verify_damage() {
            let mut reference: Vec<u32> = vec![bg32; (w * h) as usize];
            let full = RectPx { x0: 0, y0: 0, x1: w as i32 - 1, y1: h as i32 - 1 };
            reference
                .par_chunks_mut((BAND_ROWS * w) as usize)
                .enumerate()
                .for_each(|(band_idx, band)| {
                    let band_y0 = band_idx as i32 * BAND_ROWS as i32;
                    let rows = (band.len() / w as usize) as i32;
                    let rect = RectPx {
                        x0: 0,
                        y0: band_y0,
                        x1: w as i32 - 1,
                        y1: (band_y0 + rows - 1).min(full.y1),
                    };
                    band.fill(bg32);
                    rasterize_band(band, band_y0, rect, w, &clipped, ppp, tex_mgr);
                });
            let mismatched = buf
                .iter()
                .zip(&reference)
                .filter(|(a, b)| a != b)
                .count();
            if mismatched > 0 {
                let first = buf
                    .iter()
                    .zip(&reference)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                log::error!(
                    target: "diskoria::frame",
                    "DAMAGE MISMATCH: {mismatched} px differ from a full repaint \
                     (first at {},{}); damage was {:?}",
                    first as u32 % w,
                    first as u32 / w,
                    damage,
                );
            }
        }

        let t_raster = t_before_raster.elapsed();
        let t_before_present = std::time::Instant::now();
        crate::watchdog::enter(crate::watchdog::Phase::PaintPresent);

        let present_rects: Vec<softbuffer::Rect> = damage
            .iter()
            .filter_map(|r| {
                Some(softbuffer::Rect {
                    x: r.x0.max(0) as u32,
                    y: r.y0.max(0) as u32,
                    width: NonZeroU32::new((r.x1 - r.x0 + 1).max(0) as u32)?,
                    height: NonZeroU32::new((r.y1 - r.y0 + 1).max(0) as u32)?,
                })
            })
            .collect();
        let _ = buf.present_with_damage(&present_rects);

        let frame_total = t_start.elapsed();
        // A frame this long is not slow rendering, it is the UI thread blocked
        // on something it should not be touching — a lock, a subprocess, a
        // disk read (KI-42). Say so rather than leaving it to look like a hang.
        if frame_total > std::time::Duration::from_millis(250) {
            log::warn!(
                target: "diskoria",
                "UI thread stalled for {:.0} ms in one frame (ui {:.0} ms, rasterize {:.0} ms) \
                 — something blocking is running on the event loop",
                frame_total.as_secs_f64() * 1000.0,
                t_ui.as_secs_f64() * 1000.0,
                t_raster.as_secs_f64() * 1000.0,
            );
        }

        if stats {
            let t_present = t_before_present.elapsed();
            log::info!(
                target: "diskoria::frame",
                "{w}x{h} ({} px): ui {:.1}ms, tessellate {:.1}ms, rasterize {:.1}ms, \
                 present {:.1}ms, total {:.1}ms ({} prims, {:.1}x overdraw, \
                 {} damage rect(s) (from {raw_damage} raw) = {:.1}% of the window{}, \
                 buffer age {})",
                w as u64 * h as u64,
                t_ui.as_secs_f64() * 1000.0,
                t_tess.as_secs_f64() * 1000.0,
                t_raster.as_secs_f64() * 1000.0,
                t_present.as_secs_f64() * 1000.0,
                t_start.elapsed().as_secs_f64() * 1000.0,
                clipped.len(),
                px_tested as f64 / (w as f64 * h as f64),
                damage.len(),
                damage
                    .iter()
                    .map(|r| ((r.x1 - r.x0 + 1) as f64) * ((r.y1 - r.y0 + 1) as f64))
                    .sum::<f64>()
                    / (w as f64 * h as f64)
                    * 100.0,
                if force_full { ", full" } else { "" },
                buffer_age,
            );
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

/// Whether the platform can keep a window alive but off-screen.
///
/// Wayland cannot: winit's `set_visible` is a documented no-op there and
/// `is_visible()` returns `None`, because a Wayland window *is* its mapped
/// surface. Hiding therefore means destroying the window and recreating it on
/// demand — which is what every Wayland app with a tray does.
#[cfg(not(windows))]
fn window_hiding_supported() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none()
}

#[cfg(windows)]
fn window_hiding_supported() -> bool {
    true
}

/// Show and bring a window to the foreground, best-effort.
///
/// On X11 `focus_window()` maps to `_NET_ACTIVE_WINDOW` and generally works.
/// On Wayland a client cannot focus itself: activation requires a token from
/// the surface the user actually clicked, and the StatusNotifierItem protocol
/// gives a tray host no way to pass one, so `focus_window()` is a no-op there
/// (KI-35). What *does* work is mapping a hidden window — compositors focus
/// newly mapped windows — which covers the case that matters: raising Diskoria
/// after it was closed to the tray. A same-frame hide/show "remap" of an
/// already-visible window was tried and does not help (winit coalesces it, and
/// re-mapping would also disturb a tiling layout), so a visible-but-unfocused
/// window only gets an attention request.
#[cfg(not(windows))]
fn raise_window(window: &winit::window::Window) {
    let was_visible = window.is_visible().unwrap_or(true);
    window.set_minimized(false);

    if !was_visible {
        window.set_visible(true);
        window.focus_window();
        return;
    }

    window.focus_window();
    window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
    // Wayland ignores `focus_window`; ask the compositor's own IPC instead.
    // Off-thread because it spawns a helper, and the UI thread must never wait
    // on an exec (KI-42).
    #[cfg(target_os = "linux")]
    std::thread::spawn(|| {
        crate::compositor_focus::focus_own_window();
    });
}


// ── winit ApplicationHandler ──────────────────────────────────────────────────

struct App {
    /// Open Diskoria windows, keyed by `WindowId`. Holds more than one entry
    /// once `UserEvent::OpenNewWindow` (Ctrl+N / tray "New Window") creates
    /// additional windows.
    renderers: HashMap<WindowId, Renderer>,
    // Every sender today (tray, monitor, single-instance watchers) is
    // `#[cfg(windows)]`; the Linux port's shell phase starts using it.
    #[cfg_attr(not(windows), allow(dead_code))]
    proxy: EventLoopProxy<UserEvent>,
    shared: Arc<SharedAppState>,
    #[cfg(any(windows, target_os = "linux"))]
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
    /// Set while a quit is waiting for the root monitoring service to stop.
    /// Quitting promises nothing is still collecting, but stopping the service
    /// needs authentication — exiting underneath the polkit prompt would kill
    /// it and leave the service running. Holds the give-up time so a prompt
    /// nobody answers cannot wedge the app forever.
    #[cfg(target_os = "linux")]
    quit_deadline: Option<std::time::Instant>,
    /// Debounce deadline for `WM_DEVICECHANGE`-driven drive re-enumeration.
    /// Device-tree changes arrive in bursts; we coalesce them and re-scan once
    /// the burst settles. `None` when no re-scan is pending.
    device_change_deadline: Option<std::time::Instant>,
    /// One-shot guard so the "still running in the tray" toast fires only the
    /// first time the last window is closed (hidden to tray) in a session.
    #[cfg(any(windows, target_os = "linux"))]
    tray_toast_shown: bool,
    /// `--minimized` was passed (the logon task / autostart entry launches
    /// Diskoria this way): create the first window hidden so the app comes up
    /// tray-only.
    #[cfg(any(windows, target_os = "linux"))]
    start_minimized: bool,
}

impl App {
    /// First/any renderer — used by tray-callback sites that need to reach into
    /// per-window state (accent, snapshots). With multiple windows open this is
    /// just "some window"; a future refinement could target the most-recently-
    /// focused window or broadcast to all.
    #[cfg(any(windows, target_os = "linux"))]
    fn primary(&self) -> Option<&Renderer> {
        self.renderers.values().next()
    }

    #[cfg(windows)]
    fn primary_mut(&mut self) -> Option<&mut Renderer> {
        self.renderers.values_mut().next()
    }

    /// Keep the tray alive while no window exists.
    ///
    /// Monitoring itself is a detached thread, so it keeps polling after the
    /// last window goes away (which is what "close to tray" means on Wayland,
    /// where a window cannot be hidden — only destroyed). Its messages are
    /// normally drained by `DiskoriaApp::poll_monitor` during a frame; with no
    /// frames to run, this drains them so tray temperatures and alerts still
    /// update. Returns how long to wait before checking again.
    #[cfg(any(windows, target_os = "linux"))]
    fn pump_headless_monitor(&mut self) -> Option<std::time::Duration> {
        if !self.renderers.is_empty() || self.tray.is_none() {
            return None;
        }
        let (msgs, _connected) = self.shared.drain_monitor_rx();
        for msg in msgs {
            match msg {
                crate::monitor::MonitorMsg::Snapshots(snaps) => {
                    for snap in snaps {
                        let _ = self.proxy.send_event(UserEvent::TrayIconUpdate {
                            serial: snap.serial.clone(),
                            temp_c: snap.temp_c,
                        });
                        self.shared.insert_snapshot(snap);
                    }
                }
                crate::monitor::MonitorMsg::AlertFired(alert) => {
                    let _ = self.proxy.send_event(UserEvent::DriveAlert {
                        serial: alert.serial.clone(),
                        is_critical: matches!(
                            alert.level,
                            crate::alert_engine::AlertLevel::Critical
                        ),
                    });
                    let (title, body) = (
                        format!("Diskoria \u{2014} {} ({:?})", alert.model, alert.level),
                        alert.detail.clone(),
                    );
                    std::thread::spawn(move || crate::toast::send_toast(&title, &body));
                }
            }
        }
        // Nothing here is interactive; a lazy tick is plenty.
        Some(std::time::Duration::from_secs(5))
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
        crate::watchdog::enter(crate::watchdog::Phase::Resumed);
        if self.renderers.is_empty() {
            #[cfg_attr(not(windows), allow(unused_mut))]
            let mut renderer = Renderer::new(event_loop, self.shared.clone());
            #[cfg(any(windows, target_os = "linux"))]
            if self.shared.pro_edition {
                renderer.app.event_proxy = Some(self.proxy.clone());
                self.tray = crate::tray::TrayManager::new(self.proxy.clone());
            }
            // Auto-start (`--minimized`): come up tray-only. The window is
            // created visible by default; hide it before it maps. We still
            // request a redraw so the first `draw()` runs — that kicks drive
            // enumeration and, in turn, the background monitor thread — even
            // though no window is ever shown.
            #[cfg(any(windows, target_os = "linux"))]
            if self.start_minimized {
                if window_hiding_supported() {
                    renderer.window.set_visible(false);
                } else {
                    log::warn!(
                        target: "diskoria",
                        "--minimized: this compositor cannot start a window hidden \
                         (Wayland has no way to unmap a window); showing it instead"
                    );
                }
                renderer.window.request_redraw();
            }
            let id = renderer.window.id();
            self.renderers.insert(id, renderer);
            #[cfg(windows)]
            self.refresh_any_visible_flag();
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        crate::watchdog::enter(crate::watchdog::Phase::NewEvents);
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
        crate::watchdog::enter(crate::watchdog::Phase::WindowEvent);
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
                            for r in self.renderers.values_mut() {
                                r.raise();
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
        #[derive(Debug)]
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
        #[cfg(any(windows, target_os = "linux"))]
        let can_hide_to_tray = self.shared.pro_edition
            && self.shared.settings_snapshot().close_to_tray_effective()
            && self.tray.is_some()
            // A tray object is not the same as a visible icon: the SNI service
            // stays alive with no watcher on the bus so it can register later
            // (KI-45), and Wayland *destroys* the window rather than hiding it,
            // so trusting `is_some()` alone could strand the app with no icon
            // to bring it back (KI-47).
            && crate::tray::host_present();
        // No tray on other platforms, so hiding the last window would strand it.
        #[cfg(not(any(windows, target_os = "linux")))]
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
                log::debug!(
                    target: "diskoria",
                    "close requested: windows={window_count} can_hide={can_hide_to_tray} \
                     hiding_supported={} -> {:?}",
                    window_hiding_supported(),
                    disposition
                );
                if matches!(disposition, CloseDisposition::DropThis) {
                    renderer.app.cancel_all_tests();
                } else if matches!(disposition, CloseDisposition::HideThis) {
                    if window_hiding_supported() {
                        renderer.window.set_visible(false);
                        disposition = CloseDisposition::None;
                    } else {
                        // Wayland: destroy it instead. The tray keeps the
                        // process alive and re-creates a window on demand.
                        renderer.app.cancel_all_tests();
                        disposition = CloseDisposition::DropThis;
                    }
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
                        if window_hiding_supported() {
                            renderer.window.set_visible(false);
                            disposition = CloseDisposition::None;
                        } else {
                            renderer.app.cancel_all_tests();
                            disposition = CloseDisposition::DropThis;
                        }
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
        #[cfg(any(windows, target_os = "linux"))]
        if did_hide && !self.tray_toast_shown {
            self.tray_toast_shown = true;
            // Off the winit thread: WinRT toasts require an MTA thread, and the
            // Linux path blocks on the session bus.
            std::thread::spawn(|| {
                crate::toast::send_toast(
                    "Diskoria is still running",
                    "Diskoria is minimized to the system tray and keeps monitoring your drives. Right-click the tray icon to quit.",
                );
            });
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = did_hide;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        crate::watchdog::enter(crate::watchdog::Phase::UserEvent);
        match event {
            UserEvent::ShowWindowRequested => {
                if self.renderers.is_empty() {
                    // Nothing to raise (every window was dropped) — the intent
                    // is "show me Diskoria", so make one.
                    log::debug!(target: "diskoria", "show requested with no windows; opening one");
                    self.user_event(event_loop, UserEvent::OpenNewWindow);
                    return;
                }
                log::debug!(
                    target: "diskoria",
                    "show requested; raising {} window(s)",
                    self.renderers.len()
                );
                for r in self.renderers.values_mut() {
                    r.raise();
                }
                #[cfg(windows)]
                self.refresh_any_visible_flag();
            }
            #[cfg(unix)]
            UserEvent::SecondLaunch => {
                // Same policy as Windows: a visible instance gets a second
                // window; a hidden (tray-only) or window-less one is raised.
                let any_visible = self
                    .renderers
                    .values()
                    .any(|r| r.window.is_visible().unwrap_or(true));
                let follow_up = if any_visible && !self.renderers.is_empty() {
                    UserEvent::OpenNewWindow
                } else {
                    UserEvent::ShowWindowRequested
                };
                self.user_event(event_loop, follow_up);
            }
            UserEvent::OpenNewWindow => {
                #[cfg_attr(not(windows), allow(unused_mut))]
                let mut renderer = Renderer::new(event_loop, self.shared.clone());
                #[cfg(windows)]
                if self.shared.pro_edition {
                    renderer.app.event_proxy = Some(self.proxy.clone());
                }
                renderer.raise();
                let id = renderer.window.id();
                self.renderers.insert(id, renderer);
                #[cfg(windows)]
                self.refresh_any_visible_flag();
            }
            UserEvent::SettingsChanged { restart_monitor } => {
                if restart_monitor {
                    // Drop the monitor thread; the next draw in any window
                    // calls start_monitor_if_not_running and respawns it
                    // with the fresh thresholds.
                    #[cfg(windows)]
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
                // Cancel the monitor thread. Quitting means nothing of ours is
                // still collecting — the process exit would take the thread
                // with it anyway, but saying so keeps the shutdown ordered.
                #[cfg(any(windows, target_os = "linux"))]
                self.shared.cancel_monitor();
                // Same promise for the root service: quit means nothing is
                // collecting. `stop`, not `disable` — closing a window is not
                // a decision to undo the boot-time setup. This needs
                // authentication, so the exit waits for it below rather than
                // killing the prompt by exiting underneath it.
                #[cfg(target_os = "linux")]
                if crate::service_control::status().is_some_and(|s| s.running) {
                    log::info!(
                        target: "diskoria::service",
                        "quitting — stopping background collection"
                    );
                    crate::service_control::stop_now();
                    self.quit_deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(90));
                }
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
                }
                // The tray outlives the renderers while a stop is pending, so
                // there is still something on screen saying Diskoria is
                // finishing up rather than a silently lingering process.
                #[cfg(target_os = "linux")]
                if self.quit_deadline.is_some() {
                    return;
                }
                #[cfg(any(windows, target_os = "linux"))]
                {
                    self.tray = None;
                }
                event_loop.exit();
            }
            #[cfg(any(windows, target_os = "linux"))]
            UserEvent::TrayIconUpdate { serial, temp_c } => {
                if let Some(tray) = &mut self.tray {
                    crate::watchdog::scope(crate::watchdog::Phase::TrayUpdate, || {
                        tray.update_drive_icon(&serial, temp_c)
                    });
                }
            }
            #[cfg(any(windows, target_os = "linux"))]
            UserEvent::DriveAlert { serial, is_critical } => {
                if let Some(tray) = &mut self.tray {
                    crate::watchdog::scope(crate::watchdog::Phase::TrayUpdate, || {
                        tray.set_drive_alert(&serial, is_critical)
                    });
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
                            // Straight from shared state, not from the primary
                            // window: tray-only runs (`--minimized`, or the last
                            // window closed to tray) have no primary, and used to
                            // fall back to a hard-coded blue (known-issues KI-31).
                            let accent = self.shared.accent_color();
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
                        let accent = self.shared.accent_color();
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
                        for r in self.renderers.values_mut() {
                            r.raise();
                        }
                        self.refresh_any_visible_flag();
                    } else if let Some(pos) = tray.app_icon_right_click_pos(&e) {
                        log::info!(target: "diskoria::tray", "App icon right-clicked at {:?}", pos);
                        // Right-click on the app icon → custom themed context menu.
                        let accent = self.shared.accent_color();
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
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        crate::watchdog::enter(crate::watchdog::Phase::Exiting);
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(staged) = self.shared.take_staged_update() {
            log::info!(
                target: "diskoria",
                "applying staged update on exit: {}",
                staged.display()
            );
            // Stop worker threads first so nothing holds a device handle while
            // the update swaps the exe.
            for r in self.renderers.values_mut() {
                r.app.cancel_all_tests();
            }
            // relaunch = false: the user was closing Diskoria, not restarting it.
            #[cfg(windows)]
            crate::update::spawn_installer(&staged, false);
            #[cfg(target_os = "linux")]
            match std::env::current_exe() {
                Ok(exe) => {
                    if let Err(e) = crate::update::replace_exe(&staged, &exe) {
                        log::warn!(target: "diskoria", "staged update failed: {e}");
                    }
                }
                Err(e) => {
                    log::warn!(target: "diskoria", "staged update: current_exe failed: {e}");
                }
            }
        }
        // Drop every renderer (window, softbuffer surface, egui-winit state —
        // including the smithay-clipboard worker on Wayland) while the event
        // loop's display connection is still alive. `run_app` consumes the
        // `EventLoop`, so anything still held in `App` afterwards is destroyed
        // *after* the connection — on Wayland the clipboard worker then frees
        // proxies of a dead `wl_display` and segfaults at exit.
        self.renderers.clear();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        crate::watchdog::enter(crate::watchdog::Phase::AboutToWait);
        self.about_to_wait_inner(event_loop);
        // Every exit path lands here, so the watchdog sees "parked waiting for
        // the next event" rather than a phase that never ends.
        crate::watchdog::idle();
    }
}

impl App {
    /// The real `about_to_wait` body. Split out because it returns early in
    /// several places and the watchdog must be told the loop went idle on all
    /// of them.
    fn about_to_wait_inner(&mut self, event_loop: &ActiveEventLoop) {
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
        // Finish a quit that is waiting on the service to stop.
        #[cfg(target_os = "linux")]
        if let Some(deadline) = self.quit_deadline {
            let timed_out = std::time::Instant::now() >= deadline;
            if !crate::service_control::busy() || timed_out {
                self.quit_deadline = None;
                let still_running =
                    crate::service_control::status().is_some_and(|s| s.running);
                if still_running {
                    // Declined, failed, or timed out. Say so — the whole point
                    // of the control is that collection never continues
                    // unnoticed, and there will be no tray left to check.
                    let why = crate::service_control::last_error()
                        .unwrap_or_else(|| "authentication was not completed".to_string());
                    log::warn!(
                        target: "diskoria::service",
                        "quitting with background collection still running: {why}"
                    );
                    crate::toast::send_toast(
                        "Diskoria — background collection still running",
                        &format!(
                            "The monitoring service was not stopped ({why}). Stop it with: systemctl stop {}",
                            crate::service_control::UNIT
                        ),
                    );
                } else {
                    log::info!(target: "diskoria::service", "background collection stopped");
                }
                self.tray = None;
                event_loop.exit();
                return;
            }
            // Keep the loop turning while we wait for the prompt.
            let soon = std::time::Instant::now() + std::time::Duration::from_millis(250);
            wake_at = Some(wake_at.map_or(soon, |w: std::time::Instant| w.min(soon)));
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
        #[cfg(any(windows, target_os = "linux"))]
        {
            let has_drives = self
                .primary()
                .map(|r| !r.app.drives.is_empty())
                .unwrap_or(false);

            if has_drives && self.shared.take_drive_icons_dirty() {
                if let (Some(tray), Some(renderer)) = (&mut self.tray, self.renderers.values_mut().next()) {
                    crate::watchdog::scope(crate::watchdog::Phase::TrayUpdate, || {
                        tray.rebuild_drive_icons(&renderer.app.drives)
                    });
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

        // Tray-only (no windows): nothing paints, so drain the monitor here and
        // keep a slow tick going.
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(wait) = self.pump_headless_monitor() {
            let at = std::time::Instant::now() + wait;
            wake_at = Some(wake_at.map_or(at, |w: std::time::Instant| w.min(at)));
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
    // egui-winit initializes *both* clipboard backends on Linux and warns when
    // the X11 one is unreachable — expected in a pure-Wayland session, where
    // the smithay backend it prefers is the one actually in use. Quiet by
    // default; `RUST_LOG` still overrides.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("diskoria=debug,warn,egui_winit::clipboard=error"),
    )
    .format_timestamp_millis()
    .try_init();

    // The headless root collector (`diskoria-monitor.service`). Handled before
    // anything GUI-related: no window, no tray, no single-instance guard, no
    // elevation logic — systemd already starts it as root.
    #[cfg(target_os = "linux")]
    if service::requested() {
        std::process::exit(service::run());
    }

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
    // task on Windows / the autostart .desktop entry on Linux) so it comes up
    // tray-only.
    let start_minimized = std::env::args().skip(1).any(|a| a == "--minimized");

    // Skip the single-instance guard under smoke — and under demo seeding, so a
    // capture run does not just hand off to the author's real instance — leaving
    // the test run independent of any already-running Diskoria.
    #[cfg(windows)]
    let _single_instance_mutex = (smoke_remaining.is_none() && !demo::seeding())
        .then(|| acquire_single_instance_mutex(start_minimized));
    // Unix flavour: binding the socket claims primary; a connect-to-existing
    // hands off and exits inside `acquire`. The listener starts once the event
    // loop proxy exists, below.
    #[cfg(unix)]
    let single_instance = (smoke_remaining.is_none() && !demo::seeding())
        .then(|| single_instance::acquire(start_minimized))
        .flatten();
    #[cfg(not(any(windows, unix)))]
    let _ = start_minimized;

    // pkexec self-relaunch (Linux): runs *after* the single-instance check so
    // a second launch hands off to the running primary without an auth
    // prompt, and *before* the event loop so no unelevated window flashes.
    // On success the elevated child runs the whole session and this process
    // just forwards its exit code; a declined auth degrades to an unelevated
    // run (per-operation errors where root is required).
    #[cfg(target_os = "linux")]
    let single_instance = {
        let mut si = single_instance;
        if elevation::should_elevate(smoke_remaining.is_some(), start_minimized) {
            // Give the socket to the elevated child.
            if let Some(a) = si.take() {
                a.release();
            }
            match elevation::relaunch_elevated() {
                Ok(code) => std::process::exit(code),
                Err(reason) => {
                    log::warn!(target: "diskoria", "continuing unelevated: {reason}");
                    si = (smoke_remaining.is_none() && !demo::seeding())
                        .then(|| single_instance::acquire(start_minimized))
                        .flatten();
                }
            }
        }
        // Heal root-owned files in the user's data dir (created by this or a
        // previous elevated session) so unelevated runs keep write access.
        elevation::fix_data_dir_ownership();
        si
    };

    // `--demo-toast`: fire one sample notification so the toast can be
    // photographed. Backgrounded because WinRT needs a COM MTA context (and
    // the Linux path blocks on the session bus).
    #[cfg(any(windows, target_os = "linux"))]
    if demo::config().toast {
        std::thread::spawn(|| {
            let (title, body) = demo::sample_toast();
            crate::toast::send_toast(&title, &body);
        });
    }

    // Warm the desktop-appearance cache and keep it fresh off-thread; the UI
    // must never block on a subprocess (KI-42).
    #[cfg(target_os = "linux")]
    crate::theme::spawn_portal_refresh();

    // Keep the monitoring service's state visible in the tray and Settings.
    // Polled off-thread for the same reason as the portal: `systemctl` is a
    // subprocess and must never run on the event loop (KI-42).
    #[cfg(target_os = "linux")]
    crate::service_control::spawn_status_worker();

    // Notice — and name — anything that blocks the event-loop thread from here
    // on (KI-42, KI-43).
    crate::watchdog::start();

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
    // Serve unix activation requests; the guard unlinks the socket on drop.
    #[cfg(unix)]
    let _single_instance_guard = single_instance.map(|a| a.spawn(proxy.clone()));

    // Block-device hotplug → debounced auto-re-enumeration (the Linux
    // counterpart of WM_DEVICECHANGE in the wndproc).
    #[cfg(target_os = "linux")]
    device_events::spawn_uevent_watcher(proxy.clone());

    let mut app = App {
        renderers: HashMap::new(),
        proxy,
        shared,
        #[cfg(any(windows, target_os = "linux"))]
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
        #[cfg(target_os = "linux")]
        quit_deadline: None,
        device_change_deadline: None,
        #[cfg(any(windows, target_os = "linux"))]
        tray_toast_shown: false,
        #[cfg(any(windows, target_os = "linux"))]
        start_minimized,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!(target: "diskoria", "event loop error: {e}");
    }
}

/// Rasterize every primitive into one horizontal band of the framebuffer.
///
/// `band` covers rows `band_y0..=band_y1` and is indexed relative to
/// `band_y0`. Bands are disjoint and each walks the primitives in the same
/// order, so painter's-algorithm ordering is preserved exactly and the output
/// is identical to rasterizing the whole frame on one thread. Returns the
/// number of pixels visited, for the overdraw metric.
#[allow(clippy::too_many_arguments)]
fn rasterize_band(
    band: &mut [u32],
    band_y0: i32,
    rect: RectPx,
    w: u32,
    clipped: &[egui::ClippedPrimitive],
    ppp: f32,
    tex_mgr: &TextureManager,
) -> u64 {
    let mut tested: u64 = 0;
for prim in clipped {
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

                let x0 = min_x.max(clip_x0).max(rect.x0);
                let y0 = min_y.max(clip_y0).max(rect.y0);
                let x1 = max_x.min(clip_x1).min(rect.x1);
                let y1 = max_y.min(clip_y1).min(rect.y1);

                let denom = (p1.1 - p2.1) * (p0.0 - p2.0) + (p2.0 - p1.0) * (p0.1 - p2.1);
                if denom.abs() < 0.001 {
                    continue;
                }

                // Flat triangle: one colour and one UV at every vertex, which
                // is what egui emits for panel fills, card backgrounds, button
                // bodies — the large areas. Then the texture sample and the
                // blend inputs are the same for every pixel, so they are
                // computed once here instead of per pixel. Coverage is still
                // decided per pixel by the untouched barycentric test below,
                // so the rasterized shape is identical.
                let flat = v0.color == v1.color
                    && v1.color == v2.color
                    && v0.uv == v1.uv
                    && v1.uv == v2.uv;
                let mut flat_src: Option<(f32, f32, f32, f32)> = None;
                let mut flat_opaque: Option<u32> = None;
                if flat {
                    let (vr, vg, vb, va) = (
                        v0.color.r() as f32,
                        v0.color.g() as f32,
                        v0.color.b() as f32,
                        v0.color.a() as f32,
                    );
                    let (r, g, b, a) = if is_font_tex {
                        let cov = tex_mgr.sample_alpha_f(v0.uv.x, v0.uv.y);
                        (vr, vg, vb, va / 255.0 * cov)
                    } else {
                        let [tr, tg, tb, ta] =
                            tex_mgr.sample_rgba(mesh.texture_id, v0.uv.x, v0.uv.y);
                        (tr * vr, tg * vg, tb * vb, ta * va / 255.0)
                    };
                    let a = a.clamp(0.0, 1.0);
                    if a < 1.0 / 255.0 {
                        // Fully transparent everywhere — nothing to draw.
                        continue;
                    }
                    if a >= 1.0 {
                        flat_opaque = Some(to_bgra(
                            (r + 0.5) as u8,
                            (g + 0.5) as u8,
                            (b + 0.5) as u8,
                            255,
                        ));
                    }
                    flat_src = Some((r, g, b, a));
                }

                tested += ((y1 - y0 + 1).max(0) as u64) * ((x1 - x0 + 1).max(0) as u64);
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

                        // Flat triangle: colour, sample and alpha were all
                        // resolved once above.
                        if let Some(packed) = flat_opaque {
                            let idx = ((py - band_y0) as u32 * w + px as u32) as usize;
                            band[idx] = packed;
                            continue;
                        }

                        let (r, g, b, final_a) = if let Some(src) = flat_src {
                            src
                        } else {
                        let uv_x = v0.uv.x * w0 + v1.uv.x * w1 + v2.uv.x * w2;
                        let uv_y = v0.uv.y * w0 + v1.uv.y * w1 + v2.uv.y * w2;

                        let vr = v0.color.r() as f32 * w0 + v1.color.r() as f32 * w1 + v2.color.r() as f32 * w2;
                        let vg = v0.color.g() as f32 * w0 + v1.color.g() as f32 * w1 + v2.color.g() as f32 * w2;
                        let vb = v0.color.b() as f32 * w0 + v1.color.b() as f32 * w1 + v2.color.b() as f32 * w2;
                        let va = v0.color.a() as f32 * w0 + v1.color.a() as f32 * w1 + v2.color.a() as f32 * w2;

                        let (r, g, b, final_a) = if is_font_tex {
                            let cov = tex_mgr.sample_alpha_f(uv_x, uv_y);
                            (vr, vg, vb, va / 255.0 * cov)
                        } else {
                            let [tr, tg, tb, ta] = tex_mgr.sample_rgba(mesh.texture_id, uv_x, uv_y);
                            (tr * vr, tg * vg, tb * vb, ta * va / 255.0)
                        };

                        // A pixel admitted by EDGE_EPS sits marginally outside
                        // the triangle, so its interpolated alpha can land a
                        // hair outside [0, 1]. Left alone, a negative `inv`
                        // below can push the blend under zero, and `sqrt` of
                        // that is NaN — which casts to 0 and paints the black
                        // speck this whole epsilon exists to remove.
                        let final_a = final_a.clamp(0.0, 1.0);
                            (r, g, b, final_a)
                        };
                        if final_a < 1.0 / 255.0 {
                            continue;
                        }

                        let idx = ((py - band_y0) as u32 * w + px as u32) as usize;

                        // Fully opaque: the blend below reduces to
                        // sqrt(src_lin) == src, so skip the framebuffer read,
                        // six multiplies and three sqrt. Most of a UI frame is
                        // opaque panel fill.
                        if final_a >= 1.0 {
                            band[idx] = to_bgra(
                                (r + 0.5) as u8,
                                (g + 0.5) as u8,
                                (b + 0.5) as u8,
                                255,
                            );
                            continue;
                        }

                        let dst = band[idx];
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

                        band[idx] = to_bgra(out_r, out_g, out_b, 255);
                    }
                }
            }
        }
    }

    tested
}

#[cfg(test)]
mod damage_tests {
    use super::{merge_damage, RectPx, MAX_DAMAGE_RECTS};

    fn r(x0: i32, y0: i32, x1: i32, y1: i32) -> RectPx {
        RectPx { x0, y0, x1, y1 }
    }

    fn overlaps(a: &RectPx, b: &RectPx) -> bool {
        a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
    }

    fn covers(outer: &RectPx, inner: &RectPx) -> bool {
        outer.x0 <= inner.x0 && outer.y0 <= inner.y0 && outer.x1 >= inner.x1 && outer.y1 >= inner.y1
    }

    /// The two things every result must satisfy: nothing is composited twice,
    /// and nothing that changed is left unpainted.
    fn assert_disjoint_and_covering(input: &[RectPx], out: &[RectPx]) {
        for i in 0..out.len() {
            for j in (i + 1)..out.len() {
                assert!(
                    !overlaps(&out[i], &out[j]),
                    "output rects overlap: {:?} and {:?} — those pixels blend twice",
                    out[i],
                    out[j]
                );
            }
        }
        for r in input.iter().filter(|r| !r.is_empty()) {
            assert!(
                out.iter().any(|o| covers(o, r)),
                "input {r:?} is not covered by any output rect — stale pixels"
            );
        }
        assert!(out.len() <= MAX_DAMAGE_RECTS, "cap exceeded: {}", out.len());
    }

    #[test]
    fn far_apart_rects_stay_separate() {
        let input = vec![r(0, 0, 10, 10), r(500, 500, 510, 510)];
        let out = merge_damage(input.clone());
        assert_eq!(out.len(), 2);
        assert_disjoint_and_covering(&input, &out);
    }

    #[test]
    fn touching_rects_fuse() {
        // Adjacent, not overlapping: `touches` treats a 1px gap as contact so
        // the merged region stays disjoint.
        let input = vec![r(0, 0, 10, 10), r(11, 0, 20, 10)];
        let out = merge_damage(input.clone());
        assert_eq!(out, vec![r(0, 0, 20, 10)]);
        assert_disjoint_and_covering(&input, &out);
    }

    #[test]
    fn empty_rects_are_dropped() {
        let out = merge_damage(vec![r(5, 5, 4, 4), r(0, 0, 3, 3)]);
        assert_eq!(out, vec![r(0, 0, 3, 3)]);
    }

    /// The case the cheap sweep alone gets wrong: sorted by row, A and C do not
    /// touch, but once B joins them the union does. Only the exact pass after
    /// the sweep catches it — without it the output would overlap.
    #[test]
    fn transitively_touching_rects_fuse_across_sort_order() {
        let input = vec![r(0, 0, 5, 100), r(0, 40, 200, 45), r(195, 0, 200, 100)];
        let out = merge_damage(input.clone());
        assert_eq!(out.len(), 1);
        assert_disjoint_and_covering(&input, &out);
    }

    #[test]
    fn a_line_of_glyph_rects_collapses() {
        // Text damage arrives one rect per glyph, side by side.
        let input: Vec<RectPx> = (0..60).map(|i| r(i * 8, 20, i * 8 + 7, 32)).collect();
        let out = merge_damage(input.clone());
        assert_eq!(out, vec![r(0, 20, 479, 32)]);
        assert_disjoint_and_covering(&input, &out);
    }

    /// KI-43: a scroll changes every triangle, so thousands of rects arrive at
    /// once. The old fixpoint was O(n^3) here and took over a second on the
    /// event-loop thread; the answer is a bounding box either way.
    #[test]
    fn a_scroll_sized_pile_is_bounded_not_merged() {
        let mut input = Vec::new();
        for i in 0..6000i32 {
            let x = (i % 100) * 19;
            let y = (i / 100) * 31;
            input.push(r(x, y, x + 15, y + 25));
        }
        let started = std::time::Instant::now();
        let out = merge_damage(input.clone());
        let took = started.elapsed();
        assert_eq!(out.len(), 1);
        assert_disjoint_and_covering(&input, &out);
        assert!(
            took < std::time::Duration::from_millis(50),
            "merging {} rects took {took:?} — this runs on the event-loop thread",
            input.len()
        );
    }

    /// KI-44: the window shrank, so a rectangle recorded by an earlier, wider
    /// frame reaches past the end of a row in the new buffer. Unclamped, the
    /// rasterizer's `band[row + x0..=row + x1]` panicked out of range and took
    /// every rayon worker with it.
    #[test]
    fn replayed_history_from_a_larger_window_is_clamped() {
        // Previous frame at 2423 px wide; current buffer is 2403 px.
        let history = vec![vec![r(0, 0, 2422, 500)]];
        let own = vec![r(10, 10, 20, 20)];
        let out = super::replay_damage(&own, &history, 2, 2403, 1000);
        for rect in &out {
            assert!(
                rect.x1 < 2403 && rect.y1 < 1000 && rect.x0 >= 0 && rect.y0 >= 0,
                "{rect:?} escapes a 2403x1000 framebuffer"
            );
        }
        assert!(!out.is_empty());
    }

    #[test]
    fn an_undefined_buffer_forces_a_full_repaint() {
        let out = super::replay_damage(&[r(1, 1, 2, 2)], &[], 0, 800, 600);
        assert_eq!(out, vec![r(0, 0, 799, 599)]);
    }

    #[test]
    fn a_fresh_buffer_replays_only_this_frames_damage() {
        // age 1 means the buffer is the one presented last frame, so nothing
        // from the history needs replaying.
        let history = vec![vec![r(500, 500, 600, 600)]];
        let out = super::replay_damage(&[r(10, 10, 20, 20)], &history, 1, 800, 600);
        assert_eq!(out, vec![r(10, 10, 20, 20)]);
    }

    /// Between the two tiers: enough rects to exceed the cap after merging,
    /// few enough to go through the exact pass.
    #[test]
    fn many_separate_regions_collapse_to_the_cap() {
        let input: Vec<RectPx> = (0..40).map(|i| r(i * 50, i * 30, i * 50 + 10, i * 30 + 10)).collect();
        let out = merge_damage(input.clone());
        assert_disjoint_and_covering(&input, &out);
    }
}
