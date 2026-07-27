# Diskoria Monitoring

> Architecture/reference for the Pro-Monitoring subsystem. This work has landed
> on `main`; the historical `Pro-Monitoring` branch workflow note was removed.

## Overview

Monitoring adds continuous, background drive health surveillance to Diskoria.
While the main window can be closed, the app keeps running in the system tray,
polling every internal drive for temperature and SMART/NVMe health data at a
configurable interval (default 3 minutes).

---

## Feature Summary

### System Tray Icons
One icon per internally-installed drive (NVMe and SATA only — USB and UFS
excluded), plus one app-level icon using `assets/trayicon.ico`. Each drive icon is a
32×32 RGBA thermometer that re-colors based on the last measured temperature:

| Color  | Range  |
|--------|--------|
| Green  | < 45°C |
| Yellow | 45–59°C |
| Orange | 60–69°C |
| Red    | ≥ 70°C |
| Gray   | No reading available |

Drive icons have no tooltip (disabled to avoid conflict with the health flyout).

### Close to Tray
Closing the **last** main window hides it instead of exiting — but only when the
**Settings → Window → "Close to system tray"** toggle is on. The process then
remains alive and monitoring continues silently. The main window can be restored
by:
- **Left-clicking** the app tray icon
- Selecting **Open Diskoria** from the right-click context menu

In both cases the window is raised to the foreground via `SetForegroundWindow` +
`BringWindowToTop`. With the toggle on, a true exit is only triggered by **Quit**
in the context menu.

With the toggle **off**, closing the last window exits the process outright and
background monitoring stops until the next launch. Closing a *non-last* window
always just drops that window, regardless of the setting.

The toggle's initial value is not a fixed default — it comes from
`install_mode::default_close_to_tray()`: **on** for a build installed by
`installer/diskoria.iss`, **off** for the portable `diskoria.exe` (see
[Installed vs. portable](#installed-vs-portable) below). Once the user flips it,
the persisted `close_to_tray=` line in `settings.txt` wins and reinstalling does
not stomp it.

### Installed vs. portable
`src/install_mode.rs` reports whether this process is an installed build or a
portable exe. Following the same "OS state is the source of truth" rule as
`autostart.rs`, there is no persisted flag: the installer writes
`HKLM\Software\Diskoria\InstallDir` (and removes it on uninstall), and a build
counts as **Installed** only when that value names the directory the running exe
actually sits in. Copying `diskoria.exe` out of the install folder therefore
reports **Portable**, even on a machine that also has Diskoria installed.

Consumers:
- `app_settings::load_settings` — seeds `close_to_tray` when the settings file
  has no entry for it.
- `about.rs` — draws an "Installed" / "Portable" chip beside the version, so it
  is always visible which build is running.
- `DiskoriaApp::updates_supported` — see [Updates](#updates) below.

The launch-at-startup default differs the same way, but is expressed
independently through the scheduled task (see `autostart.rs`).

### Updates

Updates are an **installed-build feature**. The release asset the updater picks
is the Inno installer, so applying one on a portable exe would silently convert
it into an installed copy — `DiskoriaApp::updates_supported()` gates the whole
subsystem on `install_mode`, and `update_check_button_enabled()` consults it, so
both the manual button and the automatic check inherit the restriction.

Flow, when **Settings → Updates → "Check for updates automatically"** is on
(the default):

1. **Check** — once per process, on the first draw of a *visible* window.
   `SharedAppState::claim_auto_update_check` makes it a singleton across windows;
   the visibility condition matters because a `--minimized` tray-only start still
   draws a hidden window, and a prompt there would be invisible. Opening the
   window from the tray later satisfies the condition and the check runs then.
2. **Silent unless actionable** — an automatic check that finds nothing, or fails
   (offline, rate-limited), logs and shows no UI. Only the manual About button
   reports "Up to date" / "Update check failed".
3. **Download + stage** — an available update downloads immediately, without
   asking, into `%TEMP%` (`update::update_temp_file_name` keeps the `setup`
   marker; see known-issues KI-22). The path is parked in
   `SharedAppState::staged_update` rather than applied.
4. **Prompt** — "Update ready": *Update now* runs the installer and exits;
   *Update on close* dismisses and leaves it staged. While any test is running
   the modal degrades to a single OK button explaining that it will apply on
   close — interrupting a destructive write+verify mid-pass would leave the drive
   in an unknown state.
5. **Apply on exit** — `App::exiting` takes the staged path, cancels tests, and
   launches the installer. That hook is reached from every *real* exit (tray Quit,
   or the last window closing with `close_to_tray` off). Hiding to the tray is not
   an exit, so a staged update simply keeps waiting.

**The manual About-page check does not prompt at all.** Clicking "Check for
updates" *is* the decision, so an available update downloads and installs without
further questions — it skips step 4 entirely and goes straight to running the
installer. The two exceptions both still stage and prompt: an automatic check
(the user did not ask for anything right now), and a manual check while a test is
running (step 4's single-OK variant). Manual checks still report "Up to date" and
"Update check failed", since with nothing to install those are the only feedback
there is.

#### Running the installer

`update::spawn_installer` always runs it **silently** — `/SILENT` (a progress
window, no wizard pages, nothing to click), `/SUPPRESSMSGBOXES`, `/NORESTART`.
Two switches carry session state across, both built in
`update::silent_install_args`:

- `/MERGETASKS=[!]startup,[!]desktopicon` — a silent install would otherwise fall
  back to the `[Tasks]` defaults, and **both are checked by default**, so every
  update would re-create a startup task or desktop icon the user had removed. The
  values are read from live state (`autostart::is_enabled()`, and whether the
  public-desktop `.lnk` exists), matching how `autostart` and `install_mode`
  treat the OS as the source of truth.
- `/RELAUNCH=1|0` — a custom parameter read by the installer's
  `RelaunchAfterSilent` check, which gates a silent-only `[Run]` entry. The
  interactive `[Run]` entry is `skipifsilent`, so without this a silent install
  would leave the app closed. It is **1** when applying mid-session (the user is
  sitting in front of Diskoria and expects it back) and **0** from the exit hook,
  where the user was closing the app and reopening it would be unwelcome.

### Custom Context Menu
Right-clicking the app tray icon opens a custom themed context menu (not the
native Windows shell menu). The menu follows the app's current dark/light theme
and accent color. Items:
- **Open Diskoria** — restores and raises the main window
- **Quit** — cleanly cancels the monitor thread and exits

The menu appears just above the cursor and dismisses automatically when the
cursor leaves its bounds (after having entered them at least once).

### Drive Health Flyout
Hovering over any drive tray icon opens a small (280×168px) borderless,
always-on-top popup window near the tray icon. The flyout shows:
- Drive model name
- Current temperature (large, color-coded)
- Wear level percentage (if available)
- One-line health summary (Healthy / Warning / Critical)
- Time since last update

The flyout dismisses when the cursor leaves the icon area.

### Background Health Monitoring
A background thread (`monitor.rs`) polls all internal drives at the configured
interval. For each drive it:
1. Reads SMART/NVMe data via existing `smart_reader::query_smart_detail()`
2. Extracts a `HealthSnapshot` (temperature, sector counts, wear, NVMe fields)
3. Persists the snapshot to SQLite
4. Checks alert conditions against the previous snapshot
5. Sends snapshots and any alerts back to the UI thread via mpsc channel

The thread is cancelled cleanly (via `Arc<AtomicBool>`) when the app exits,
monitoring is disabled, or the poll interval is changed in settings.

### SQLite History Database
Location: `%ProgramData%\Diskoria\history.db`

Stores health snapshots for up to 90 days. Pruning runs at monitor thread
startup.

**Schema highlights:**
- `health_snapshots` — serial, model, timestamp, temp_c, reallocated_sectors,
  pending_sectors, uncorrectable_sectors, wear_pct, available_spare_pct,
  available_spare_thresh, critical_warning, raw_json
- `alert_cooldowns` — persists cooldown state across restarts so the app does
  not re-alert immediately after a relaunch
- WAL journal mode for concurrent read/write from UI and monitor threads

On startup, after drives are enumerated, up to 7 days of temperature history
is loaded from the database into memory so the chart is populated immediately —
no need to wait for the first poll cycle.

### Temperature History Chart
The **Health Status** page includes a temperature history chart below the SMART
attribute table. Five range tabs are provided:

| Tab | Window |
|-----|--------|
| 1 h | Last hour |
| 6 h | Last 6 hours |
| 12 h | Last 12 hours |
| 24 h | Last 24 hours (default) |
| 7 d | Last 7 days |

The chart uses `egui_plot` with the accent color. X-axis labels show relative
time (e.g. "30m", "2h", "3d"). Hover tooltip shows temperature and time ago.

Periods when the app was closed appear as **gaps** in the line rather than a
flatline — any two consecutive points more than 15 minutes apart are rendered
as separate line segments.

Selecting a different drive updates the chart to show that drive's history.

### Alert Engine
Alerts are evaluated on every monitor cycle. All conditions:

| Condition | Level | Threshold | Cooldown |
|-----------|-------|-----------|----------|
| Temperature warning | Warning | ≥ 60°C (configurable) | 1 hour |
| Temperature critical | Critical | ≥ 70°C (configurable) | 1 hour |
| NVMe critical_warning bits newly set | Critical | Any bit | 1 hour |
| NVMe available spare below threshold | Critical | spare < thresh | 1 hour |
| Reallocated sector count increased | Warning | delta > 0 | None |
| Pending sector count increased | Warning | delta > 0 | None |
| Uncorrectable sector count increased | Critical | delta > 0 | None |
| Wear level high | Warning | ≥ 90% (configurable) | 24 hours |

Cooldown state is persisted in `alert_cooldowns` across restarts.

### Toast Notifications
Alerts are delivered as Windows toast notifications via the WinRT
`ToastNotification` API (`windows::UI::Notifications`). The app registers an
App User Model ID (AUMID) `"Diskoria"` in the registry on first launch so
notifications route to the action center correctly. If WinRT fails, a
`Shell_NotifyIconW` balloon tip is used as a fallback. Toast calls are made
on a separate background thread (WinRT requires a COM MTA context).

### Monitoring Settings
A **Monitoring** card in the Settings page provides:
- **Enable background monitoring** — toggle to start/stop the monitor thread
- **Poll interval** — segmented control: 1 min / 3 min (default) / 5 min / 10 min;
  changing this restarts the monitor thread immediately
- **Temperature warning threshold** — slider 30–80°C (default 60°C)
- **Temperature critical threshold** — slider 40–95°C (default 70°C)
- **Wear level alert threshold** — slider 50–100% (default 90%)

All settings are persisted to `%ProgramData%\Diskoria\settings.txt`.

---

## Architecture Notes

- All monitoring code is gated with `#[cfg(windows)]` — the crate still
  compiles on non-Windows without any monitoring functionality.
- The `Pro-Monitoring` branch is kept in sync with `main` via rebase:
  ```
  git fetch origin
  git rebase origin/main
  git push --force-with-lease
  ```
- New crate dependencies: `rusqlite 0.31` (bundled SQLite), `tray-icon 0.19`,
  `windows 0.58` (WinRT), `serde_json 1`

---

## New Source Files

| File | Purpose |
|------|---------|
| `src/monitor.rs` | Background thread, HealthSnapshot types, snapshot extraction |
| `src/alert_engine.rs` | Alert condition logic, cooldown tracking (pure, no I/O) |
| `src/history_db.rs` | SQLite persistence: open, insert, query, prune |
| `src/toast.rs` | WinRT toast + balloon tip fallback, AUMID registration |
| `src/tray.rs` | TrayManager: app icon (from `assets/trayicon.ico`), per-drive thermometer icons |
| `src/flyout.rs` | Drive health flyout window; custom themed context menu window |
| `src/tex_mgr.rs` | Shared texture manager for egui softbuffer rasterizer (flyout + context menu) |
| `src/install_mode.rs` | Installed-vs-portable detection; source of the `close_to_tray` default |

---

## Test Plan

### 1. Build Verification
- [ ] `cargo check` completes with no errors
- [ ] `cargo test` — all 3 SQLite unit tests pass
  (`insert_and_query`, `load_last_snapshot_returns_most_recent`, `prune_removes_old_rows`)
- [ ] `cargo build --release` completes cleanly

### 2. Tray Icon Appearance
- [ ] On launch, tray icons appear in the notification area: one per internal
  NVMe/SATA drive, plus the app icon
- [ ] The app icon matches `assets/trayicon.ico`
- [ ] USB drives do **not** get a tray icon
- [ ] Each drive icon shows a colored thermometer shape
- [ ] Drive icons have no tooltip (tooltip is suppressed)

### 3. Temperature Color Updates
- [ ] After the first poll cycle, drive icon colors update to reflect actual
  temperatures
- [ ] Temporarily lower the warning threshold below the drive's reported temp —
  verify the icon color changes on next poll cycle
- [ ] A drive with no temperature reading shows a gray icon

### 4. Close to Tray
- [ ] With **Settings → Window → Close to system tray** ON, clicking the window
  close button hides the window (process remains running)
- [ ] With it OFF, clicking close on the *last* window exits the process and the
  tray icons disappear
- [ ] With it OFF and two windows open, closing one leaves the other running
- [ ] A fresh profile (no `%PROGRAMDATA%\Diskoria\settings.txt`) starts with the
  toggle ON for an installed build and OFF for the portable exe
- [ ] Left-clicking the app tray icon restores and **raises** the main window
  (appears in front of other windows)
- [ ] Right-clicking the app tray icon shows the custom dark/light themed
  context menu (not the native Windows shell menu)
- [ ] Context menu follows the app's current theme and accent color
- [ ] **Open Diskoria** restores, raises, and focuses the main window
- [ ] **Quit** exits the process cleanly
- [ ] Context menu dismisses when cursor moves away from it

### 5. Drive Health Flyout
- [ ] Hovering a drive tray icon opens the flyout near that icon
- [ ] Flyout displays drive model, temperature (color-coded), wear %, health
  summary, and last-updated time
- [ ] Flyout dismisses when cursor leaves the icon area
- [ ] Hovering a different drive icon switches the flyout to that drive's data
- [ ] Flyout border follows the app accent color

### 6. Background Monitoring & SQLite
- [ ] After one poll cycle, `%ProgramData%\Diskoria\history.db` exists
- [ ] `health_snapshots` contains rows with correct serial, model, timestamp,
  temp_c, and valid raw_json
- [ ] Rows accumulate at the configured poll interval
- [ ] On next launch, history loads immediately without waiting for a poll cycle

### 7. Temperature History Chart
- [ ] Navigate to **Health Status** page
- [ ] Temperature History section is visible with tabs: 1 h / 6 h / 12 h / 24 h / 7 d
- [ ] Default tab is 24 h
- [ ] Switching tabs changes the displayed range
- [ ] Selecting a different drive updates the chart
- [ ] X-axis labels show relative time (minutes/hours for short ranges, days for 7d)
- [ ] Hover tooltip shows temperature and time ago
- [ ] After closing and reopening the app, history chart is populated immediately
- [ ] Periods when the app was closed appear as **gaps** (blank space), not flatlines

### 8. Toast Notifications
- [ ] Set temperature warning threshold below the drive's current temperature
- [ ] Wait for the next poll cycle
- [ ] A Windows toast notification appears in the action center
- [ ] Toast does not repeat within 1 hour (cooldown)
- [ ] Cooldown persists across restarts

### 9. Monitoring Settings
- [ ] Open **Settings** page; verify the "Monitoring" card is present with a
  border matching the Theme/Accent cards
- [ ] Toggle "Enable background monitoring" off; verify monitoring stops
- [ ] Toggle back on; verify monitoring resumes
- [ ] Change poll interval (e.g. 1 min); verify new rows appear at that interval
- [ ] Changing poll interval restarts the monitor thread immediately
- [ ] All settings persist after restarting the app
- [ ] 12px gap below the Monitoring card (does not press against window frame)

### 10. Clean Shutdown
- [ ] With monitoring running, select **Quit** from the context menu
- [ ] Process exits cleanly with no crash or zombie process
- [ ] No `SQLITE_BUSY` or lock errors in the debug log

### 11. Multi-Drive Systems
- [ ] Two+ internal drives each get a separate tray icon
- [ ] Flyout shows the correct drive's data for each icon
- [ ] Temperature history chart switches correctly between drives

### 12. Edge Cases
- [ ] No internal drives detected — no drive tray icons, no crash
- [ ] App icon right-clicked immediately after launch (before first poll) —
  context menu opens correctly
- [ ] History DB unavailable (permissions issue) — app logs a warning and
  continues without crashing
