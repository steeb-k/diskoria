# CLAUDE.md — Diskoria

Guidance for working in this repo. Read this first; follow the `docs/` links for
depth. Keep new work consistent with the patterns below so features blend in.

## What Diskoria is

A desktop storage utility (Windows + Linux): **read-only sector scan**, **destructive
write+verify**, **file-based speed benchmark**, **SMART / NVMe / UFS health**,
and a Pro **background monitoring** mode (tray + temperature history + alerts).

Rendering stack: **winit + egui + softbuffer** with a **hand-written CPU
rasterizer** (no GPU/OpenGL — runs in Windows PE / recovery). The window is
**frameless** with custom title bar, resize hit-testing, and chrome.

Platform: Windows and Linux (the `linux-support` work). Storage, tests,
monitoring, tray, notifications, autostart and self-update all run on both;
platform code lives in per-module `windows.rs`/`linux.rs` submodules behind
shared free-function contracts (see `docs/refactor-roadmap.md` item 5). Other
unixes are a compile-only shell. Known Linux parity gaps are logged as
`linux`-tagged entries in `docs/known-issues.md` (KI-33…KI-37).

## Repository map

```
CLAUDE.md, AGENTS.md   This guide (AGENTS.md points here)
README.md              User-facing readme
docs/                  All developer docs (see "Docs" below)
diskoria/              Rust crate
  Cargo.toml, build.rs, app.manifest, .cargo/config.toml (crt-static)
  src/                 Application code (see "Code structure")
  tests/boot_smoke.rs  Ignored integration smoke (launches the real exe)
scripts/               run-dev.ps1|.sh / build-release.ps1 / build-portable.ps1|.sh
                       / set-version.ps1 + artifact-signing-metadata.json
linux/                 Linux desktop assets: polkit policy (pkexec), .desktop
                       entry, diskoria-monitor.service + install-service.sh
                       (the root health collector), README (privileges, tray,
                       service, architectures, data locations)
installer/             diskoria.iss — Inno Setup script (built by build-release.ps1
                       via ISCC; produces releases/<ver>/diskoria-<ver>-setup.exe).
                       Writes the startup scheduled task and the
                       HKLM\Software\Diskoria\InstallDir marker read by install_mode.rs
assets/                appicon2.ico (window + notification icon), trayicon.ico
                       (tray only — simplified for 16-24px; looks flat rendered
                       large, by design), applogo.png (in-app sidebar logo).
                       All embedded at compile time.
  source/              Editable/alternate originals (.xcf, alt png/ico) — not embedded
```

## Docs (read before changing the relevant area)

| Doc | What it covers |
|-----|----------------|
| `docs/gui-architecture.md` | Frameless chrome, CPU rasterizer, repaint model, multi-window, tray/flyout — the *why* behind the UI workarounds. |
| `docs/known-issues.md` | Living bug/oddity log (KI-1…). **Append here** when you find something; mark items resolved when fixed. |
| `docs/refactor-roadmap.md` | Done vs. deferred cleanup; the `pages/` split pattern; the planned `PageLayout` / enum-test-state / `StorageBackend` work. |
| `docs/testing.md` | What's tested and how to run it. |
| `docs/releasing.md` | Release checklist: version bump, unsigned build → manual test → sign, **tag the source repo**, artifacts on diskoria-binaries. |
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
  Anything drawn *on an accent fill* takes `Theme::txt_on_accent` (WCAG-contrast
  black or white) — never a hard-coded `Color32::WHITE`; the accent can be any
  color, including white (known-issues KI-19). **Keyboard focus rings are always
  `Stroke::new(2.0, t.accent)`** at `rect.expand(2..3)` — white rings vanish on
  the light theme's white cards (KI-21).
- **`card.rs`** — `CardLayout`, the builder every stacked card goes through.
  Rows are handed out by `row()` while it accumulates the real height; the frame
  is painted last into placeholder shapes reserved at `begin()`; `end()` does the
  **single** cursor advance. Never pre-compute a card height, never add your own
  `advance_cursor_after_rect` — that combination is what KI-18 was.
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
- **`demo.rs`** — the `--page` / `--demo-*` flags, which seed the UI from one
  invented machine so the wiki's reference screenshots can be taken without
  exposing real hardware. **Nothing in it touches a disk**: a "running" test sets
  the progress fields and leaves the worker channel `None`, so every `poll_*`
  early-returns. `--demo-confirm` exists so the destructive-test confirmation
  dialog can be captured without arming a write. Any `--demo-*` implies
  `--demo-drives` *and* `--demo-health` — once the drive list is invented its
  device paths (`\\.\PhysicalDrive0`) name the *host's* real disks, so the health
  readers must never be let near them. Demo mode also skips the single-instance
  guard, the monitor thread and the startup update check. Seams live in
  `shared_state::start_drive_enumeration`, `app::poll_smart_health`,
  `smart_health_page::spawn_health_poll_if_needed` and `app::apply_demo_seed`.
- **`single_instance.rs`** (unix) — the `$XDG_RUNTIME_DIR/diskoria.sock`
  single-instance guard; the primary decides raise-vs-new-window from its own
  renderer state (no KI-6-style flag race). Windows keeps the named
  mutex/events in `lib.rs`.
- **`service.rs`** (Linux) — the headless root collector behind `--service`
  (`diskoria-monitor.service`). Exists because SMART needs root but a desktop
  session should not have it: autostart launches `--minimized` *unelevated* on
  purpose (a polkit prompt every login is worse), which leaves it with hwmon
  temperatures only. The service polls health as root into
  `/var/lib/diskoria/history.db`; sessions read it unprivileged and prefer any
  reading younger than three poll intervals (`monitor::fresh_service_snapshot`).
  **Collection only** — no sockets, no commands, never writes to a block
  device; disk tests still go through pkexec. The system DB uses a rollback
  journal, not WAL, because a WAL reader must write the `-shm` sidecar and an
  unprivileged session cannot do that in a root-owned directory.
- **`watchdog.rs`** — event-loop stall detector. Every winit callback and paint
  stage marks a phase; a background thread warns when a non-idle phase outlasts
  `DISKORIA_STALL_MS` (default 1 s). On by default — it named KI-43 after two
  wrong diagnoses. Keep event-loop work non-blocking: anything waiting on a
  disk, subprocess, D-Bus peer or compositor belongs on a worker thread.
- **`elevation.rs`** (Linux) — pkexec self-relaunch with an env trampoline
  (DISPLAY/WAYLAND/D-Bus/XDG passthrough); skipped for smoke/demo/
  `--no-elevate`/`--minimized`; declined auth degrades instead of exiting;
  elevated startups re-own the XDG data dir to the invoking user.
- **`device_events.rs`** (Linux) — NETLINK_KOBJECT_UEVENT block-device watcher
  feeding the same debounced `DEVICE_CHANGE_PENDING` seam as `WM_DEVICECHANGE`.
- **`install_mode.rs`** — installed build vs. portable exe. Like `autostart.rs`,
  OS state is the source of truth (no persisted flag): the installer writes
  `HKLM\Software\Diskoria\InstallDir`, and a build is "Installed" only when that
  value names the running exe's own directory. Drives the `close_to_tray` default
  (installed ON, portable OFF, applied in `app_settings::load_settings` when the
  settings file has no entry), the About page's Installed/Portable chip, and
  `DiskoriaApp::updates_supported()` — **update checks are installed-only on
  Windows** (the asset the updater applies there is the installer, KI-23);
  the Linux portable binary self-replaces, so updates are always on there.
  The update flow (startup check → silent unless actionable → auto-download →
  stage → "Update now / on close" → apply in `App::exiting`) is documented in
  `docs/monitoring.md` § Updates.
  Note `HKLM\Software\Diskoria` is *shared* (an older build left an
  `AutoCheckUpdates` value there), so the installer deletes only its own
  `InstallDir` value on uninstall, never the whole key.
- **`autostart.rs`** (`#[cfg(windows)]`) — launch-at-startup via a Scheduled Task
  (`schtasks /RL HIGHEST`, no UAC prompt since the app is `requireAdministrator`).
  The task's existence *is* the state (no persisted setting): the installer creates
  it (installed default ON), the portable exe doesn't (OFF). The Settings toggle
  reads/writes it; the task launches Diskoria with `--minimized` → starts tray-only
  (parsed in `run()`, applied in `resumed()` in `lib.rs`).

## Build / run / test

```
.\scripts\run-dev.ps1                  # Windows: cargo run (needs elevation)
./scripts/run-dev.sh                   # Linux: cargo run (app pkexec-relaunches;
                                       #   --no-elevate skips the prompt)
.\scripts\build-release.ps1 / .\scripts\build-portable.ps1
./scripts/build-portable.sh            # Linux release: bare binary + portable tarball
.\scripts\set-version.ps1              # interactive version bump

cargo build                            # zero warnings is the bar — keep it there
cargo clippy --all-targets             # also zero warnings
DISKORIA_SKIP_RESOURCE=1 cargo test    # unit tests; the env flag avoids the
                                       # admin-manifest "requires elevation" on the
                                       # test exe (see known-issues KI-10) — a
                                       # Windows-only concern; plain `cargo test`
                                       # works on Linux. Several #[ignore]d
                                       # diagnostics inspect real hardware:
                                       # `cargo test <name> -- --ignored --nocapture`
                                       # (enumeration, hwmon, portal, speed run).
cargo test --test boot_smoke -- --ignored   # launches the real exe, renders, exits 0
DISKORIA_SMOKE=3 cargo run             # render N frames then exit (manual smoke)

# Demo / capture mode (see `demo.rs`) — seeds the UI, never touches a disk:
cargo run -- --page drive-health --demo-drive 1     # the SATA warning drive
cargo run -- --page sector-read --demo-progress --demo-heatmap
cargo run -- --page sector-write --demo-confirm     # the destructive dialog,
                                                    # WITHOUT arming a write
```

Tests are pure-logic unit tests (`alert_engine`, `smart_reader`, `monitor`
extraction, `app_settings`, `history_db`) plus the boot smoke. There is no GPU/
headless GUI test — window/tray/disk paths are verified manually (see
`docs/multi-window-smoke-tests.md`).

### CI

Two workflows run on push + PR. Neither builds or publishes a release — that
stays local and signed (`docs/releasing.md`).

- `.github/workflows/ci.yml` — `cargo build` + `cargo test` against
  `diskoria/Cargo.toml`, on **windows-latest and ubuntu-latest** (one job per
  OS: the Windows job covers the Win32/WMI/IOCTL half, the Linux job the
  sysfs/SG_IO/O_DIRECT half plus everything shared). Both set
  `DISKORIA_SKIP_RESOURCE=1` (KI-10, harmless on Linux) and pass `--locked`,
  which makes a stale `Cargo.lock` a CI failure instead of a silent
  re-resolve. The #[ignore]d tests (boot smoke, hardware diagnostics) don't
  run in CI.
- `.github/workflows/cargo-deny.yml` — supply-chain checks. `deny.toml`'s
  `ignore` list is the open-advisory backlog and **removing an entry is how one
  gets fixed**; each carries what it is and how to clear it. Re-run
  `cargo deny check advisories` after any dependency bump to see if one can go.

## Conventions to follow when adding features

- **New central page** → add a `draw_*_page` in `app/pages/<name>.rs` using the
  existing template (`impl crate::app::DiskoriaApp`, `pub(crate)` fn, minimal
  `use` + `use super::super::{…}` for app-private helpers); register it in
  `app/pages.rs` and dispatch from `draw_central`. See `refactor-roadmap.md`.
- **Drive selection is shared** across Drive Health / Sector / Benchmark via the
  single `selected_drive` field (new windows start at 0). Don't reintroduce
  per-page selection fields.
- **New card** → `CardLayout::builder(left, section_w).title(…).begin(ui, t)`,
  `card.row(h)` per row, `card.end(ui)`. See `card.rs` and the pattern in
  `refactor-roadmap.md`.
- **Paths** go through `paths.rs`, never inline `%PROGRAMDATA%`.
- **Layout constants** come from `theme.rs`.
- **Background work** posts results over an `mpsc` channel and is drained in a
  `poll_*` method; call `ctx.request_repaint()` from the worker so unfocused
  windows update (see the repaint model in `gui-architecture.md`).
- **Platform code**: storage/tray/autostart modules are directories with a
  shared `mod.rs` (types + pure logic + parsers) and `windows.rs`/`linux.rs`
  transports behind identical free-function signatures — extend both (or add a
  stub) when touching them. Smaller items use `#[cfg(windows)]` /
  `#[cfg(target_os = "linux")]` pairs; `#[cfg(any(windows, target_os =
  "linux"))]` marks features live on both but absent on other unixes.
- **Keep it warning-free.** `too_many_arguments` is allowed crate-wide (egui draw
  fns); everything else should be clean. Add unit tests for any new pure logic.
- **When you find a bug/oddity**, log it in `docs/known-issues.md` (next `KI-`
  number) even if you don't fix it.

## Gotchas

- The app manifest requests `requireAdministrator`; real disk ops need elevation,
  and `cargo test` needs `DISKORIA_SKIP_RESOURCE=1` in an unelevated shell.
- `selected_drive` is written **only** by an explicit dropdown pick. The
  Benchmark page needs a `(drive, partition)` pair, so it *derives* one via
  `speed_target_pair()` — clamping the partition, never moving the drive — and
  shows a grayed "no mounted volume" row with Start disabled when the shared
  selection has nothing to benchmark (known-issues KI-15). Don't reintroduce a
  "fix up the selection so this page works" pass.
- Drive list is enumerated at startup, on Refresh, and on `WM_DEVICECHANGE`
  (debounced); a 12 s watchdog recovers a hung WMI scan.
- Closing the **last** window only hides it to the tray when `close_to_tray` is
  on; otherwise the process exits and background monitoring stops. Closing a
  non-last window always just drops that window. Two things gate the installed
  defaults and they're kept independent: the scheduled task (`autostart.rs`) and
  the registry marker (`install_mode.rs`).
- One test per physical drive across all windows: each window publishes its
  drive-under-test to `SharedAppState::test_locks` every frame and clears it on
  close (via `cancel_all_tests`). Test-page dropdowns gray drives locked by
  other windows; Drive Health does not. See known-issues KI-17.
