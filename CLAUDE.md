# CLAUDE.md — Diskoria

Guidance for working in this repo. Read this first; follow the `docs/` links for
depth. Keep new work consistent with the patterns below so features blend in.

## What Diskoria is

A Windows desktop storage utility: **read-only sector scan**, **destructive
write+verify**, **file-based speed benchmark**, **SMART / NVMe / UFS health**,
and a Pro **background monitoring** mode (tray + temperature history + alerts).

Rendering stack: **winit + egui + softbuffer** with a **hand-written CPU
rasterizer** (no GPU/OpenGL — runs in Windows PE / recovery). The window is
**frameless** with custom title bar, resize hit-testing, and chrome.

Platform: Windows-first. Non-Windows is a **compile-only shell** (no disk
enumeration/tests); keep it compiling but don't expect functionality there.

## Repository map

```
CLAUDE.md, AGENTS.md   This guide (AGENTS.md points here)
README.md              User-facing readme
docs/                  All developer docs (see "Docs" below)
diskoria/              Rust crate
  Cargo.toml, build.rs, app.manifest, .cargo/config.toml (crt-static)
  src/                 Application code (see "Code structure")
  tests/boot_smoke.rs  Ignored integration smoke (launches the real exe)
*.ps1 (repo root)      run-dev / build-release / build-portable / set-version
appicon*.png/.ico, applogo.png   Branding embedded at compile time
```

## Docs (read before changing the relevant area)

| Doc | What it covers |
|-----|----------------|
| `docs/gui-architecture.md` | Frameless chrome, CPU rasterizer, repaint model, multi-window, tray/flyout — the *why* behind the UI workarounds. |
| `docs/known-issues.md` | Living bug/oddity log (KI-1…). **Append here** when you find something; mark items resolved when fixed. |
| `docs/refactor-roadmap.md` | Done vs. deferred cleanup; the `pages/` split pattern; the planned `PageLayout` / enum-test-state / `StorageBackend` work. |
| `docs/testing.md` | What's tested and how to run it. |
| `docs/monitoring.md` | Pro-Monitoring subsystem (tray icons, flyout, alert engine, history DB, toasts, settings). |
| `docs/smart-telemetry-reference.md` | Windows SMART/NVMe IOCTL reference for extending telemetry. |
| `docs/TODO.md` | Forward-looking feature backlog. |
| `docs/multi-window-smoke-tests.md` | Manual window/tray/monitor regression matrix. |

## Code structure (`diskoria/src/`)

- **`lib.rs`** — winit event loop, the CPU rasterizer (`Renderer::paint`),
  multi-window (`HashMap<WindowId, Renderer>`), single-instance guard, tray/flyout
  routing, the repaint model, `DISKORIA_SMOKE` boot path, `WM_DEVICECHANGE`
  auto-refresh debounce.
- **`app.rs`** — `DiskoriaApp` (per-window UI state), `draw()` dispatch, all the
  `poll_*` loops. The big page bodies live in **`app/pages/{sector,speed,destructive}.rs`**
  (a submodule of `app`, so they can access `DiskoriaApp` privates).
- **`smart_health_page.rs`** — the Drive Health page.
- **`chrome.rs`** — title bar, the `WM_NCHITTEST` resize wndproc, DWM rounding,
  fonts, `WM_DEVICECHANGE` flag.
- **`theme.rs`** — colors + shared layout constants (`TITLEBAR_H`, `BTN_W`,
  `CONTROLS_W`, …). Single source of truth — don't redefine these elsewhere.
- **`focus.rs`, `shortcuts.rs`** — manual Tab order + Alt mnemonics (egui's
  auto-focus is not used).
- **`drive_selector.rs`** — the shared two-row drive/volume dropdown + icon
  refresh button (disables/grays while refreshing) used by all four drive-bearing
  pages (`two_row_combo`, `refresh_button`, `ChipSpec`/`DriveEntry`,
  `ROW_H`/`REFRESH_W`/`REFRESH_GAP`).
- **`shared_state.rs`** — `SharedAppState`: settings, drive list, monitor state,
  shared across windows. **Locking rule: never hold a guard across `draw()`.**
- **`paths.rs`** — the one place that builds `%PROGRAMDATA%\Diskoria\…` paths
  (the seam for a future Linux port).
- **`drive_enumeration.rs`** (WMI), **`surface_test.rs` / `destructive_test.rs` /
  `speed_test.rs`** (IOCTL disk workers), **`smart_reader.rs`** (raw SMART/NVMe/UFS
  via `DeviceIoControl`), **`smart_health.rs`** (WMI predict-fail).
- **`monitor.rs`, `alert_engine.rs`, `history_db.rs`, `tray.rs`, `flyout.rs`,
  `toast.rs`** — Pro-Monitoring (mostly `#[cfg(windows)]`).
- Support: `app_settings.rs`, `detected_drive.rs`, `partition_info.rs`,
  `widgets.rs`, `modal_confirm.rs`, `tex_mgr.rs`, `about.rs`, `update.rs`,
  `github_config.rs`.

## Build / run / test

```
.\run-dev.ps1                          # cargo run (needs elevation for disk I/O)
.\build-release.ps1 / build-portable.ps1
.\set-version.ps1                      # interactive version bump

cargo build                            # zero warnings is the bar — keep it there
cargo clippy --all-targets             # also zero warnings
DISKORIA_SKIP_RESOURCE=1 cargo test    # unit tests; the env flag avoids the
                                       # admin-manifest "requires elevation" on the
                                       # test exe (see known-issues KI-10)
cargo test --test boot_smoke -- --ignored   # launches the real exe, renders, exits 0
DISKORIA_SMOKE=3 cargo run             # render N frames then exit (manual smoke)
```

Tests are pure-logic unit tests (`alert_engine`, `smart_reader`, `monitor`
extraction, `app_settings`, `history_db`) plus the boot smoke. There is no GPU/
headless GUI test — window/tray/disk paths are verified manually (see
`docs/multi-window-smoke-tests.md`).

## Conventions to follow when adding features

- **New central page** → add a `draw_*_page` in `app/pages/<name>.rs` using the
  existing template (`impl crate::app::DiskoriaApp`, `pub(crate)` fn, minimal
  `use` + `use super::super::{…}` for app-private helpers); register it in
  `app/pages.rs` and dispatch from `draw_central`. See `refactor-roadmap.md`.
- **Drive selection is shared** across Drive Health / Sector / Benchmark via the
  single `selected_drive` field (new windows start at 0). Don't reintroduce
  per-page selection fields.
- **Paths** go through `paths.rs`, never inline `%PROGRAMDATA%`.
- **Layout constants** come from `theme.rs`.
- **Background work** posts results over an `mpsc` channel and is drained in a
  `poll_*` method; call `ctx.request_repaint()` from the worker so unfocused
  windows update (see the repaint model in `gui-architecture.md`).
- **Windows-only code** is `#[cfg(windows)]` with a non-Windows stub, so the
  shell keeps compiling.
- **Keep it warning-free.** `too_many_arguments` is allowed crate-wide (egui draw
  fns); everything else should be clean. Add unit tests for any new pure logic.
- **When you find a bug/oddity**, log it in `docs/known-issues.md` (next `KI-`
  number) even if you don't fix it.

## Gotchas

- The app manifest requests `requireAdministrator`; real disk ops need elevation,
  and `cargo test` needs `DISKORIA_SKIP_RESOURCE=1` in an unelevated shell.
- `selected_drive` can be silently repointed by the Benchmark page when a
  partition-less drive is selected (known-issues KI-15).
- Drive list is enumerated at startup, on Refresh, and on `WM_DEVICECHANGE`
  (debounced); a 12 s watchdog recovers a hung WMI scan.
- One test per physical drive across all windows: each window publishes its
  drive-under-test to `SharedAppState::test_locks` every frame and clears it on
  close (via `cancel_all_tests`). Test-page dropdowns gray drives locked by
  other windows; Drive Health does not. See known-issues KI-17.
