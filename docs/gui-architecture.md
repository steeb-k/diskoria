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
   (`lib.rs:266-365`).
5. The buffer is presented via softbuffer/GDI (`lib.rs:367-376`).

**Why no GPU:** the target includes Windows PE / recovery-style environments
where OpenGL/D3D may be unavailable. A software rasterizer trades performance for
"runs anywhere." Consequence: rendering is comparatively expensive, which is why
the repaint model (§6) works hard to avoid unnecessary frames.

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
width of the three control buttons. A drag sends
`ViewportCommand::StartDrag`; a double-click toggles maximize.

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

> **KI-1:** the button geometry (`TITLEBAR_H`, `BTN_W`) lives both here (via
> `theme.rs`) and as hardcoded `i32`s in the resize wndproc (§4). Keep them in
> sync until Phase 3a unifies them.

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
`CLOSE_HOVER_BG`, spacing, dark/light + accent colors). The egui side imports
these. **The exception is the resize wndproc**, which currently redefines
`TITLEBAR_H`/`BTN_W`/`CONTROLS_W` as local `i32`s (`chrome.rs:191-193`) — see
KI-1. Until that's unified, treat `theme.rs` as the source of truth and update
both sites together.

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
