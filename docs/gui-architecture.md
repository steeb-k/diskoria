# Diskoria — GUI architecture & oddities

Diskoria is an [egui](https://github.com/emilk/egui) app that has been forced
into a **frameless, custom-chromed, CPU-rendered** window. It does not use
`eframe`; it drives `winit` + `softbuffer` + a hand-written rasterizer directly,
and re-implements everything the OS title bar would normally provide (drag,
resize, min/max/close, rounded corners). This document explains how that fits
together and, just as importantly, *why* the non-obvious workarounds exist, so
future changes don't silently undo them.

Companion docs:
- `known-issues.md` — the running bug/oddity log (cross-referenced below as `KI-n`).
- `multi-window-smoke-tests.md` — manual test matrix for window/tray behavior.

Line anchors (`file:line`) are approximate — verify against the current source.

---

## 1. Rendering pipeline (no GPU)

`lib.rs` top-of-file doc and `Renderer::paint` (`lib.rs:200-385`).

The window is rendered entirely on the **CPU**:

1. winit drives the event loop and owns the OS window.
2. `softbuffer` gives a raw pixel buffer for that window (`lib.rs:161-162`).
3. egui tessellates the UI into triangle meshes (`lib.rs:260`).
4. A hand-written rasterizer walks each triangle, does barycentric
   interpolation, samples the font/texture atlas, and blends per-pixel
   (`rasterize_band` in `lib.rs`).
5. The buffer is presented via softbuffer/GDI.

**Why no GPU:** the target includes Windows PE / recovery-style environments
where OpenGL/D3D may be unavailable. A software rasterizer trades performance for
"runs anywhere." Rendering is therefore comparatively expensive, which is why the
repaint model (§6) works hard to avoid unnecessary frames.

**Parallel + flat-triangle fast path.** The framebuffer is split into 32-row
bands, one rayon task each, and every band walks the primitive list in the same
order — bands are disjoint slices, so painter's-algorithm ordering is preserved
structurally rather than by convention. Triangles whose vertices share one
colour and one UV (panel fills, card backgrounds, button bodies — the large
areas) resolve their texture sample and blend inputs once per triangle instead
of per pixel, and fully opaque ones reduce to a precomputed `u32` store.
Coverage is still decided per pixel by the barycentric test, so shapes are
unchanged; text and gradients take the generic path.

Cost scales with *pixels visited*, which is the display's business, not the
app's: on a 2.25x HiDPI panel with a tiled full-height window (3.58 Mpx, 3.4x
overdraw) a frame was 150 ms single-threaded before this work and is ~9.5 ms
after — 42 ms with rayon pinned to one thread, so most of the win survives on a
low-core PE box. rayon sizes its pool from `available_parallelism`, so no cap is
configured.

**Damage tracking.** A frame only redraws what changed. Each clipped primitive
is fingerprinted **per triangle** — egui batches a whole page into a handful of
meshes, so a primitive-level diff reports "everything changed" the moment one
label ticks over — and the diff against last frame gives the damaged rectangles,
in both the old and the new position. Those are merged until disjoint (a
translucent primitive drawn twice would blend twice) and capped, then only the
damaged spans are cleared, rasterized, and handed to
`Buffer::present_with_damage`, which on Windows becomes per-rect `BitBlt`.

A frame falls back to a full repaint when it cannot be trusted to a diff: first
frame, resize, theme change, a *replaced* texture (an appended font-atlas patch
is fine — existing glyphs keep their pixels), or `Buffer::age() == 0`, meaning
softbuffer handed back a buffer with undefined contents. Because the buffer may
be several frames old, `damage_history` keeps each frame's *own* damage and the
last `age - 1` frames are replayed; storing the *applied* damage instead makes
one full repaint propagate forever.

Typical steady state on the Drive Health page: tens of thousands of frames where
nothing changed and painting is skipped outright, a partial redraw per second at
~1 ms, and a full repaint only when something structural happens (~16 ms).

**Merging must stay cheap (KI-43).** `frame_damage` emits two rectangles per
changed triangle, so a scroll or a heatmap recolor hands `merge_damage` tens of
thousands of them — and the result is capped at 8 rectangles regardless. An
exact pairwise merge over that many inputs is quadratic and cost seconds on the
event-loop thread. The cost is therefore bounded *before* any merging: over
`RAW_DAMAGE_LIMIT` raw rectangles it returns the bounding box; otherwise a
linear y-then-x sweep fuses touching neighbours (a line of text arrives one
rectangle per glyph and collapses to one), and only the survivors go through the
exact fixpoint. Coarser damage is always safe — it is a superset — so when in
doubt, widen rather than merge harder. The one property that is not negotiable
is **disjointness**: the rasterizer blends, so a pixel inside two damage
rectangles composites twice and comes out wrong.

**Measuring and verifying:** `DISKORIA_FRAME_STATS=1` logs a per-frame breakdown
(UI pass, tessellate, rasterize, present, total, primitive count, overdraw,
damage rectangles, buffer age). `DISKORIA_FULL_REPAINT=1` disables damage
tracking to A/B a suspected artifact. `DISKORIA_DAMAGE_VERIFY=1` re-renders each
frame in full into a scratch buffer and logs any pixel that differs from what
damage tracking produced — the failure mode of partial redraw is a stale pixel
that depends on the *sequence* of frames, which comparing two separate runs
cannot catch. All three are free when unset.

**Stall watchdog (`watchdog.rs`):** the UI is single-threaded — winit callbacks,
egui's layout pass and the rasterizer all run on the event-loop thread — so any
blocking call that reaches it freezes *every* window at once. That symptom looks
identical whatever the cause, and it only shows up under load that is awkward to
reproduce (KI-42, KI-43). So each callback and each paint stage marks a phase
(`watchdog::enter` / `scope`, with `idle()` when the loop parks), and a
background thread warns when a non-idle phase outlasts `DISKORIA_STALL_MS`
(default 1000 ms; `0` disables), naming the phase and, on recovery, the total
stall. Unlike the three flags above this is **on by default** — two relaxed
atomic stores per callback, and the bugs it catches only occur on real hardware
under real load, where an opt-in flag would be off. When adding work to the
event loop, keep it non-blocking: anything that waits on a disk, a subprocess,
a D-Bus peer or a compositor belongs on a worker thread feeding an `mpsc`
channel.

**Gamma note (KI-5):** the blend approximates gamma 2.0 (square / sqrt) rather
than true sRGB ~2.2 (`lib.rs:348-358`). Acceptable, but not color-exact.

`to_bgra` (`lib.rs:387-390`) packs pixels in the BGRA order softbuffer expects.

---

## 2. Frameless window

`Renderer::new` (`lib.rs:130-135`):

```rust
Window::default_attributes()
    .with_decorations(false)      // no OS title bar / border / buttons
    .with_inner_size(800x600)
    .with_min_inner_size(780x580) // floor so the custom layout never collapses
    .with_resizable(true)         // still resizable — via our own hit-testing
```

`with_decorations(false)` removes *all* OS chrome. Everything below (§3–§5) is
the app re-implementing what was removed. The minimum size matters: the custom
title bar + sidebar have no graceful collapse, so the floor prevents a broken
layout.

---

## 3. Custom title bar & window controls

`chrome.rs::draw_titlebar` (`chrome.rs:298-421`). The title bar is a transparent
egui `Area` in the `Foreground` order, drawn every frame.

**Drag region** (`chrome.rs:324-340`): the top `TITLEBAR_H` strip, minus the
reserves at each end — `chrome::titlebar_hit_reserve(mode)`. A drag sends
`ViewportCommand::StartDrag`; a double-click toggles maximize.

**Two bars, not one.** Below the mobile breakpoint (§10) `draw_titlebar` hands
off to `draw_titlebar_mobile`: a hamburger and the app name, and **nothing on
the right at all**. There is no room for five controls at 380px, and phone-style
navigation puts what survives in the menu rather than the chrome — Quit is the
last row of the nav menu. Maximize/restore is still a double-click on the drag
strip, so a window dragged down to that size can always be restored, and Alt+F4
is unaffected.

**Control buttons** (`chrome.rs:342-420`): minimize, maximize/restore, close are
**painter-drawn** (lines, rects, an X) — not egui `Button` widgets. Each is an
`interact` rect that, on click, sends a `ViewportCommand`
(`Minimized` / `Maximized` / `Close`). Hover fills are drawn manually; close uses
a dedicated `CLOSE_HOVER_BG` red and turns its glyph white on hover.

**Maximize source of truth:** on Windows the maximized state is read from Win32
`IsZoomed()` directly (`chrome.rs:299-303`), not from egui's viewport state,
because the latter isn't reliable for an undecorated window. Non-Windows falls
back to egui's `viewport().maximized` (`chrome.rs:304-307`).

The viewport commands are consumed in `Renderer::paint` (`lib.rs:217-255`), which
maps them onto winit calls (`set_minimized`, `set_maximized`, `drag_window`, and
a `close_requested` flag).

> **KI-1 (resolved):** the button geometry is no longer duplicated. Both the
> title bar and the resize wndproc (§4) read `theme.rs` and call the same
> `titlebar_hit_reserve(mode)`, so the strip you can click and the strip that
> swallows the resize border cannot disagree. The wndproc derives `mode` from
> the window's own width, and scales the logical constants by the window DPI
> before comparing them with physical hit-test coordinates (KI-51).

---

## 4. Custom resize handles (`WM_NCHITTEST` hook)

`chrome.rs` `win32_resize` module (`chrome.rs:136-296`); installed from the app
via `install_win32_resize(hwnd)`.

Because the window is undecorated, Windows won't resize it. The fix is to
**subclass the window procedure** and answer `WM_NCHITTEST` ourselves:

- `install` (`chrome.rs:260-286`): re-adds `WS_THICKFRAME | WS_MAXIMIZEBOX |
  WS_MINIMIZEBOX` to the window style (so the OS allows resize/snap), then swaps
  in our `wndproc` via `SetWindowLongPtrW(GWLP_WNDPROC, …)`, stashing the
  previous proc in a `OnceLock` for chaining.
- `wndproc` (`chrome.rs:182-258`): only acts when the message is `WM_NCHITTEST`
  **and** the window is not maximized (`IsZoomed == 0` — a maximized window fills
  the monitor and shouldn't resize).
  1. **Controls carve-out** (`chrome.rs:187-200`): if the cursor is in the top
     `TITLEBAR_H` strip within the right-hand `CONTROLS_W` region, return
     `HTCLIENT` so the min/max/close buttons get the click instead of a resize
     edge. *(These constants are the KI-1 duplicates.)*
  2. **Edge/corner math** (`chrome.rs:207-250`): using
     `GetSystemMetrics(SM_CXSIZEFRAME/SM_CYSIZEFRAME)` plus a `CORNER_PAD` of 2px
     for easier corner grabbing, classify the cursor into `HTLEFT`, `HTRIGHT`,
     `HTBOTTOM`, `HTBOTTOMLEFT`, `HTBOTTOMRIGHT`. (Top edge is intentionally not
     a resize edge — that's the drag bar.)
  3. Anything unhandled chains to the previous wndproc / `DefWindowProcW`
     (`chrome.rs:253-257`).

Rounded corners are applied separately via DWM
(`apply_win11_rounded_corners`, `chrome.rs:118-134`) — a silent no-op on older
Windows or where DWM is unavailable.

---

## 5. Input workarounds

**Drag-release synthesis (KI-2)** — `lib.rs:221-241`. `window.drag_window()`
hands control to the OS modal move-loop, which consumes the mouse-up that ends
the drag. egui never sees the release, so its pointer stays "down" and the next
drag attempt fails. The handler pushes a synthetic
`PointerButton { pressed: false }` at the last known pointer position to end the
interaction cleanly. Fragile but necessary.

**Manual focus / Tab order** — `focus.rs` (+ `shortcuts.rs` for Alt-mnemonics).
Pages have custom Tab order across non-contiguous widgets and Alt-shortcut
underlines, so the app drives focus with explicit "focus slots"
(`bind_text_focus_slot` etc.) and a Tab-cycle loop rather than relying on egui's
automatic focus traversal. `modal_confirm.rs` implements its own two-/one-button
modal focus handling for the same reason.

---

## 6. Repaint model (the subtle part)

Software rendering is costly, and the app is multi-window, so naive repainting
either spins the CPU or starves a background window of paints. The model:

- **No `request_redraw` in `paint`** (`lib.rs:378-383`). Painting calls `draw()`,
  which re-requests a repaint; redrawing from within paint would loop without
  yielding to the OS pump.
- **Repaint requests become a deadline.** egui's repaint callback
  (`lib.rs:171-176`) forwards `UserEvent::Repaint { after }`. The handler
  (`lib.rs:685-699`) folds it into `next_repaint`, clamping ASAP requests
  (`after == 0`) to `REPAINT_FRAME_CAP` (16 ms ≈ 60 fps, `lib.rs:98`) so a
  continuously animating window settles into a steady tick instead of spinning.
- **`about_to_wait` paints, on a timer, all windows directly** (`lib.rs:841-873`).
  When the deadline is due it calls `r.paint()` for every renderer. It uses
  direct painting rather than `request_redraw` because Windows starves the
  low-priority `WM_PAINT` of a background window while another window of the same
  process is pumping messages (e.g. a second window running a sector test —
  KI-3). Painting *all* windows propagates updates a background thread requested
  on just one window's context (e.g. the monitor's shared snapshot).
- **Control flow is idle by default** (`lib.rs:944-952`): `ControlFlow::Wait`
  (0% CPU) when nothing is pending, `WaitUntil(deadline)` only when a repaint or
  the tray/cursor poll needs it. Resetting back to `Wait` matters — `WaitUntil`
  is sticky and an elapsed one spins the loop.

---

## 7. Multi-window

The app is single-process, multi-window. State that must be shared lives in one
`Arc<SharedAppState>`; everything per-window lives in a `Renderer`.

- **`App` struct** (`lib.rs:423-457`): `renderers: HashMap<WindowId, Renderer>`,
  the event-loop `proxy`, shared state, and the Windows-only tray/flyout/context
  menus + the `any_visible_flag`. *(Field comment ~424-427 is stale — KI-8.)*
- **`Renderer`** (`lib.rs:104-198`): its own `Window`, softbuffer surface, egui
  context + winit state, `DiskoriaApp`, and texture manager. Each window keeps
  independent UI state (current page, scroll, per-window tests).
- **Opening windows**: Ctrl+N (in `app.rs`) and the tray "New Window" item both
  send `UserEvent::OpenNewWindow`, handled at `lib.rs:663-672` (creates a
  `Renderer`, installs the proxy, raises it, inserts into the map).
- **Live settings sync**: a settings change sends
  `UserEvent::SettingsChanged { restart_monitor }` (`lib.rs:673-684`), which
  redraws every window (and optionally cancels the monitor so the next draw
  respawns it with fresh thresholds). Windows read shared values at frame start.
- **Close / quit semantics** (`window_event`, `lib.rs:586-648`):
  - Free edition → `CloseRequested` exits the app.
  - Pro, multiple windows → drop just this renderer (after cancelling its tests).
  - Pro, last window → hide to tray (`set_visible(false)`), process stays alive.
  - Tray "Quit" → `UserEvent::QuitRequested` (`lib.rs:700-…`) cancels all tests,
    cancels the monitor, clears all renderers and tray resources, exits.
- **Guard discipline (KI-7)**: shared-state guards must be dropped before
  rendering begins in `draw()` (see `shared_state.rs:7-10`). By convention only.

### Single-instance & raise-vs-new
`lib.rs:956-end`. A named mutex enforces one process. The primary publishes an
"any window visible" bit into a named 4-byte file mapping
(`DiskoriaAnyVisible`). A secondary launch reads it and pulses one of two named
events — `DiskoriaShowWindow` (raise hidden windows, → `ShowWindowRequested`) or
`DiskoriaNewWindow` (open another window, → `OpenNewWindow`). `refresh_any_visible_flag`
(`lib.rs:477-486`) recomputes the bit on every window show/hide/close. Startup
race noted in KI-6. `raise_window` (`lib.rs:401-419`) combines
`set_visible` + `ShowWindow(SW_RESTORE)` + `SetForegroundWindow` +
`BringWindowToTop` to defeat Windows focus-stealing prevention.

---

## 8. Tray & flyout (Windows only)

`tray.rs`, `flyout.rs`, plus event routing in `lib.rs`.

- **App tray icon + per-drive icons**: one app icon, plus one 32×32 thermometer
  icon per NVMe/SATA drive, color-graded by temperature. Per-drive icons are
  rebuilt only when the drive list changes, gated by a `take_drive_icons_dirty`
  singleton so opening a second window doesn't wipe monitor-driven temperatures
  (`lib.rs:875-890`).
- **Alert flashing**: a drive in alert flashes its icon (faster when critical),
  advanced by `tray.tick_flash()` from `about_to_wait` (`lib.rs:896`).
- **Flyout & context menus**: the hover flyout and right-click menus are separate
  `WS_EX_NOACTIVATE` windows (`flyout.rs:58-71`) so they don't steal focus. They
  are dismissed by 50 ms cursor polling because tray `Leave` events are
  unreliable (KI-4, `lib.rs:892-933`). They're shown with `SW_SHOWNOACTIVATE`
  (`lib.rs:822-831`).
- **Event routing**: tray clicks arrive as `UserEvent::TrayIconEvent`; flyout /
  context-menu windows get first dibs on `window_event` (`lib.rs:519-572`) and
  return whether they should close.

---

## 9. Layout constants

Shared style/layout tokens live in `theme.rs` (`TITLEBAR_H`, `BTN_W`,
`CLOSE_HOVER_BG`, the `RAIL_*` / `BP_*` responsive constants, spacing,
dark/light + accent colors). Everything else imports them, the resize wndproc
included (§4). `theme.rs` is the source of truth — don't redefine a layout
number anywhere else.

---

## 10. Responsive nav

The sidebar has three forms. Which one a window draws depends **only on that
window's own width**, so two windows of different sizes disagree happily — the
mode is recomputed once per frame in `draw` and passed down, never stored.

| Mode | Window width | Sidebar |
|------|--------------|---------|
| `NavMode::Full` | ≥ `BP_RAIL` (900) | 240px, mark + wordmark + labelled rows |
| `NavMode::Rail` | 560–899 | 48px icon rail; hover expands a 220px overlay |
| `NavMode::Mobile` | < `BP_MOBILE` (560) | none; hamburger in the title bar |

`theme::nav_mode(screen_w)` is the only place that maps width to form. It has
**no hysteresis on purpose**: the mode never feeds back into the window width,
so parking a drag exactly on a breakpoint can flip the sidebar but cannot
oscillate. `NavMode::side_panel_w()` gives the width the layout reserves, and
the `SidePanel` is built from that same number.

**`RAIL_W` is exactly `2 × NAV_ICON_X`, and that is not a coincidence.** A nav
row draws its icon at `NAV_ICON_X` from the row's left edge in every form, so
making the rail twice that width puts the icon simultaneously at the rail's
centre and at the same x the expanded overlay uses. Any other rail width makes
every icon visibly jump sideways the moment the overlay opens. The rail was
72px at first, sized to fit the `applogo.png` wordmark; switching the header to
`appicon2.ico` — drawn for icon sizes, where the wordmark logo is mush — let it
shrink to where the icons line up.

**The header mark** is `draw_nav_header`, shared by all three forms. `name_at`
adds the app name beside the icon, which the expanded overlay passes
`NAV_LABEL_X` for so the wordmark sits on the same left edge as the row labels
below it. The rail passes `None` — the same icon, in the same place, with the
name appearing as the panel opens. `FULL_SIDEBAR_USES_APP_ICON` (a plain `const`
branch, both sides compiled, so no dead code either way) decides whether the
240px sidebar shows that mark too or goes back to `applogo.png`.

**Two sizes of the mark, because the rasterizer point-samples.**
`tex_mgr::TexEntry::sample` is one floored texel lookup (§1) — the
`TextureOptions::LINEAR` handed to egui never reaches a GPU sampler, so it does
nothing. A texture drawn well below its native size therefore keeps one texel
per NxN block and drops the rest: at 512 → 32 the icon's contact pins vanish
into blobs. `paint_nav_icon` picks `appicon2-64.png` at or below
`NAV_ICON_SMALL_MAX` and `appicon2.png` above it. **Any future art drawn much
smaller than its source needs the same treatment** — there is no filtering to
fall back on.

**The hover overlay is an `Area`, not a wider panel** (`draw_rail_flyout`).
Widening the `SidePanel` would reflow the whole page every time the pointer
crossed the rail — text moving under the cursor, and a full relayout per hover
on a CPU rasterizer. Instead the labelled panel is painted in `Order::Foreground`
over the content, which keeps the width it had. It claims its own rect with
`allocate_rect` so clicks don't fall through to the page beneath.

Open and close are delayed (`NAV_RAIL_OPEN_DELAY` 120ms, `NAV_RAIL_CLOSE_DELAY`
200ms) so a pointer crossing the rail on its way somewhere else doesn't flash a
panel over the page. The decision is `app::rail_hover_step`, a pure function so
the delays are unit-tested without a window; the caller feeds it the pointer
state and schedules `request_repaint_after` for the pending transition — a
pointer that has come to rest generates no further events, so the transition has
to book its own frame.

**The pointer is the only thing that closes it.** Picking a row does *not*
collapse the overlay — `rail_hover_step` alone decides, so it stays until the
pointer leaves and the close delay elapses. It used to close on click, on the
reasoning that the page behind is what the user just asked to see; in the hand
that reads as the labels being yanked away from a pointer that never moved, and
a second choice costs another hover plus another open delay. The mobile menu is
the opposite case and still closes on pick: it covers the whole window, so there
is nowhere to move out to.

**The mobile menu covers the whole window**, title bar included, and is drawn
*after* the content so it wins the input — but *before* the modals, which still
have to be able to cover it. Its header strip keeps the drag gesture so the
window can still be moved while the menu is up. Esc closes it, as does picking a
row.

Its last row is **Quit**, standing in for the close button the phone-width title
bar gives up. It sends `UserEvent::QuitRequested` — the tray's Quit path, which
cancels in-flight tests and the monitor — rather than
`ViewportCommand::Close`: with `close_to_tray` on, closing hides the window and
keeps collecting, which is not what a row labelled Quit should do. Like the
button it replaces it stays live while a test is running. Row chrome comes from
`paint_nav_row`, shared with the page rows so the two cannot drift apart.

`sync_nav_mode` resets the state belonging to a mode as soon as the window
leaves it, so a resize cannot strand an expanded overlay or an open menu on
screen. `--demo-nav-open` pins whichever collapsed form the current width uses:
both are pointer-driven and a capture has no other way to reach them (same
reason `--demo-confirm` exists).

**Content below 780px.** That used to be the minimum window width, so no page
had ever been laid out narrower and several clipped rather than wrapped when the
minimum dropped to 380. Page body text goes through `widgets::page_text_line`
(a bare `ui.horizontal` has egui's wrap mode set to `Extend`, which is what
produced the clipping); dialogs are fitted to the window by
`modal_confirm::fit_dialog`. See KI-52 for the full list.

---

## Quick file map

| File | Role in the GUI |
|------|-----------------|
| `lib.rs` | winit event loop, `Renderer`, CPU rasterizer, repaint model, multi-window, single-instance, tray/flyout routing |
| `chrome.rs` | fonts/logo textures, custom title bar + controls, `WM_NCHITTEST` resize hook, DWM rounding |
| `theme.rs` | colors, spacing, layout constants |
| `focus.rs`, `shortcuts.rs` | manual focus slots, Tab order, Alt-mnemonics |
| `modal_confirm.rs`, `widgets.rs`, `toast.rs` | custom modal/widget/notification drawing |
| `flyout.rs`, `tray.rs` | tray icons, hover flyout, context menus (Windows) |
| `app.rs` | `DiskoriaApp`, page dispatch, all page draw functions |

## Linux notes (linux-support)

- **Resize**: the `WM_NCHITTEST` subclass is Windows-only; Linux uses an
  egui-side edge hit-test (`chrome::handle_edge_resize`, same `theme.rs`
  geometry) driving `ViewportCommand::BeginResize` →
  `winit::Window::drag_resize_window`, with the same KI-2 release synthesis.
- **Single instance**: a unix socket in `$XDG_RUNTIME_DIR`
  (`single_instance.rs`); the primary decides raise-vs-new-window from its own
  renderer state, so the KI-6 flag race has no unix counterpart. Raise is
  best-effort under Wayland (KI-35).
- **Device change**: a netlink uevent watcher (`device_events.rs`) feeds the
  same debounced `DEVICE_CHANGE_PENDING` seam as `WM_DEVICECHANGE`.
- **Theme/accent**: winit reports no system theme on X11/Wayland, so
  `ThemePref::Auto` and the "System accent" option read the XDG settings
  portal (`org.freedesktop.appearance` via `dbus-send`, cached in `theme.rs`).
- The rendering pipeline (softbuffer + CPU rasterizer) is unchanged; frameless
  windows have no shadow/rounding on some compositors.
