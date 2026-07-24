# Multi-window refactor — smoke test plan

Companion to the multi-window refactor plan. One block per plan step; run the
checks at the end of each before moving on. The
refactor is long and mechanical, so the goal is to catch regressions *early* —
ideally in the step that introduced them — rather than during a single
end-of-project sweep.

All tests assume the `multi-window` branch, a clean `cargo build`, and
`.\scripts\run-dev.ps1` for launch unless noted. Treat any panic, crash, or hang as
a stop-the-line; do not advance to the next step until green.

---

## Per-step smoke tests

### Step 1 — SharedAppState scaffold (done)

Gate: one window renders as before; no behavioral change expected.

- [ ] `cargo build` → clean, no new warnings beyond the 2 pre-existing.
- [ ] App launches, drives enumerate, main window is usable.
- [ ] Theme toggle (Settings → Dark/Light/Auto) still works.
- [ ] Accent palette swatch click still recolors the UI.
- [ ] Close window → tray-minimize (Pro) behavior unchanged.

### Step 2 — Delete migrated fields (done)

Gate: every old field now read/written via `shared`. Single-window
behavior identical.

- [ ] `cargo build` → clean.
- [ ] Change theme from each UI path (Settings radio, Ctrl-click on swatch
      etc.) — every write actually persists. Restart app, confirm theme
      survives.
- [ ] Change accent to custom hex → hex edit commits on blur/enter; the
      next draw shows the new color.
- [ ] Settings file at `%PROGRAMDATA%\Diskoria\settings.txt` reflects each
      change within one frame.

### Step 3 — Monitor + drive-enum receivers on SharedAppState (done)

Gate: background-thread plumbing routes through `shared`; no dangling
receivers; monitor lifecycle unchanged.

- [ ] `cargo build` → clean.
- [ ] On launch: drive list populates within ~1 s.
- [ ] Pro-Monitoring ticks: `history.db` gets a new row at the configured
      poll interval (run with `poll_interval_mins = 1` to shorten the
      loop). Verify via:
      `sqlite3 %PROGRAMDATA%\Diskoria\history.db "select datetime(ts,'unixepoch'), serial from snapshots order by ts desc limit 5;"`
- [ ] Toggle "Enable monitoring" off → no new rows land within 2 ×
      poll interval. Toggle back on → rows resume.
- [ ] Change `poll_interval_mins` → monitor restarts with new cadence
      (watch `history.db` insert spacing).
- [ ] Tray "Quit" → process exits cleanly, no stderr panic from the
      monitor thread about a closed proxy.

### Step 4 — HashMap<WindowId, Renderer>

Gate: still creates exactly one window; routing by window id.

- [ ] `cargo build` → clean.
- [ ] Single window still opens on launch.
- [ ] Window events (resize, close, redraw) route correctly — manually
      resize the window, confirm the render follows.
- [ ] Close window → expected behavior (tray-minimize).
- [ ] Quick stress: resize the window rapidly for 10 s, no panic.

### Step 5 — UserEvent::OpenNewWindow + Ctrl+N

Gate: two windows open simultaneously; each is independently
interactive.

- [ ] `cargo build` → clean.
- [ ] Launch. Press Ctrl+N → second window appears.
- [ ] Both windows show the same drive list.
- [ ] Focus each window in turn; the keyboard focus follows.
- [ ] Navigate to different pages in each window (e.g. A on
      "Surface Test", B on "SMART Health"). Each retains its page.
- [ ] Resize window A independently of window B.
- [ ] Close window A (not last) → window B stays alive, tray intact.
- [ ] Close window B (last) → tray-minimize as expected (with
      Settings → Window → "Close to system tray" ON; with it OFF the process
      exits instead).

### Step 6 — SettingsChanged broadcast + live sync

Gate: changes in one window appear in every other window on next frame.

- [ ] `cargo build` → clean.
- [ ] Open two windows (Ctrl+N).
- [ ] In window A, switch theme Dark → Light. Window B flips within one
      frame (observe without needing to click it).
- [ ] Change accent palette swatch in A → B's accent updates.
- [ ] Toggle a Pro-Monitoring threshold (e.g. temp warn) in A → next
      monitor cycle uses new threshold; verify via a deliberately low
      warn threshold + alert toast fires.
- [ ] `settings.txt` on disk shows the final value exactly once (not
      double-written). Open from both windows simultaneously (race):
      change theme in A and accent in B within the same half-second →
      last write wins cleanly, no corruption.

### Step 7 — Close/Quit with multiple windows

Gate: per-window tests cancel on their own window's close; last-window
falls back to tray-minimize; tray "Quit" tears everything down.

- [ ] `cargo build` → clean.
- [ ] Open 2 windows. Start a Sector Read test in window A. Window B's
      Sector Read page shows "Idle".
- [ ] Start an independent Sector Read test in window B on a different
      drive. Both run in parallel. History rows arrive from both.
- [ ] Close window A mid-test → A's test thread exits within ~2 s
      (monitor via `RUST_LOG=diskoria=debug`). Window B's test
      continues.
- [ ] Close window B (last remaining) → hides to tray; test in B is
      either finished or cancelled cleanly.
- [ ] Relaunch visible from tray. Start a test. Right-click tray → Quit.
      Process exits within 3 s; no background-thread panic.

### Step 8 — Single-instance watcher: raise vs. new window

Gate: second exe launch raises when hidden, opens new window when
visible.

- [ ] `cargo build` → clean.
- [ ] Launch diskoria.exe. Window visible. Launch diskoria.exe again →
      **second window appears** in the same process (check Task
      Manager: still only one `diskoria.exe` PID).
- [ ] Close both windows to tray (tray-minimize). Launch diskoria.exe
      again → **one** existing window raises; no new window.
- [ ] Rapid double-launch stress: kick off 5 launches inside 2 s. Result
      is deterministic (either all raise, all spawn new, or some mix)
      but: no process leak (still exactly 1 `diskoria.exe`), no crash.

### Step 9 — Tray "New Window" menu item

Gate: tray right-click opens a new window identical to Ctrl+N.

- [ ] `cargo build` → clean.
- [ ] Right-click the app tray icon → context menu includes "New
      Window". Click → new window appears.
- [ ] New window is fully usable, shares shared state, closes
      independently.
- [ ] "Quit" still closes everything.

### Step 10 — End-to-end verification (plan's checklist)

Run the full 12-step list from `remove-the-flag-fancy-seal.md` §
"Verification" before closing the branch. In particular:

- [ ] 10.1 Start Sector Read in A. Close A mid-scan. Verify A's test
      thread exits within a couple of seconds via `RUST_LOG=diskoria=debug`.
- [ ] 10.2 Right-click drive tray icon → "Suppress alerts 10 min". No
      alert toast in *either* window during the suppression window.
      Suppression map is global.
- [ ] 10.3 Launch.exe a third/fourth time while running → each either
      raises hidden windows or opens new ones per rule. No proliferating
      processes.
- [ ] 10.4 Tray Quit → clean exit. Check Task Manager: no lingering
      `diskoria.exe`.

---

### Launch at startup (`autostart.rs` + `installer/diskoria.iss`)

Run elevated (the app is `requireAdministrator`).

- **`--minimized` flag.** Launch `diskoria.exe --minimized`. Expect: **no window**,
  tray icon present, monitoring runs (temperature icons appear; `history.db` grows).
  Left-click the tray icon / "Open" → the window appears. This is the exact state
  the logon task produces.
- **Options toggle → task create.** Settings page → "Launch at startup" ON. Confirm
  `schtasks /Query /TN "Diskoria Startup"` succeeds (task exists, Run Level = Highest,
  Trigger = At log on). Toggle OFF → the query fails (task deleted).
- **No UAC at logon.** With the task present, log off and back on: Diskoria auto-starts
  minimized to the tray with **no UAC consent prompt**.
- **Single-instance + `--minimized`.** With Diskoria already running (window hidden),
  run `diskoria.exe --minimized` again: the second process exits silently and does
  **not** raise the hidden window. (A plain second launch still raises it, as before.)
- **Installer.** Install with the "Launch Diskoria at startup" box checked → task
  created, in-app toggle reads ON. Uninstall → task removed
  (`schtasks /Query /TN "Diskoria Startup"` fails).

---

### Installed vs. portable + close-to-tray (`install_mode.rs`, Settings → Window)

Run elevated. Delete `%PROGRAMDATA%\Diskoria\settings.txt` before each
first-run check so the install-mode default is what's actually being observed.

- **Portable default.** Run a standalone `diskoria.exe` from a folder that is not
  the install dir. About page chip reads **Portable** (accent-filled);
  Settings → Window → "Close to system tray" defaults **OFF**. Closing the only
  window exits the process (Task Manager: no `diskoria.exe`, tray icons gone).
- **Installed default.** Install, then launch from the Start menu. Chip reads
  **Installed**; the toggle defaults **ON**; closing the last window hides it to
  the tray and monitoring keeps ticking (`history.db` grows).
- **Copied-out exe is portable.** Copy `diskoria.exe` out of
  `C:\Program Files\Diskoria` to e.g. the Desktop and run it *on the same
  machine*. Chip must still read **Portable** — the registry marker alone is not
  enough, the exe's directory has to match it.
- **Uninstall clears the marker.** Uninstall, then run a portable copy → chip
  reads **Portable** (`HKLM\Software\Diskoria` is gone).
- **User choice survives reinstall.** Flip the toggle OFF on an installed build,
  reinstall over the top → the toggle stays OFF (the installer writes only the
  registry marker, never `settings.txt`).
- **Non-last window ignores the setting.** With the toggle OFF, open two windows
  (Ctrl+N) and close one → the other stays alive; only closing the *last* one
  exits.

---

### Automatic updates (`Settings → Updates`)

Needs an **installed** build plus a *newer* release published to
`diskoria-binaries` — a same-version release exercises nothing. Publish a
throwaway higher tag (or temporarily lower the local `Cargo.toml` version and
rebuild) to drive the available-update path.

- **Portable is inert.** In the portable exe the Updates card reads
  "Unavailable — updates are handled by the installer", its toggle is greyed and
  skipped by Tab, and the About "Check for updates" button is greyed with a
  hover tooltip. Nothing hits the network — confirm with Fiddler/Wireshark or by
  watching for the `startup update check` log line (it must not appear).
- **Silent when current.** Installed build, latest release == running version →
  no modal at launch, no "Up to date" box. `RUST_LOG=diskoria=info` shows
  `startup update check` and nothing further.
- **Silent when offline.** Disconnect the network and launch → no modal; a
  `startup update check failed` warning in the log.
- **Fires once.** With two windows open (Ctrl+N), only one check runs — the log
  line appears exactly once per process.
- **Tray-only start defers it.** Launch with `--minimized`: no check while
  hidden. Open the window from the tray → the check runs then.
- **Download + stage + prompt.** With a newer release available: no "download?"
  prompt, the busy overlay appears, then "Update ready" with *Update now* /
  *Update on close*.
- **Update on close.** Choose it, keep working, then Quit from the tray → the
  installer launches as Diskoria exits. Closing to the tray must **not** trigger it.
- **Update now.** Choose it → installer runs immediately and the app exits; the
  exit hook must not launch a *second* copy.
- **Test running.** Start a sector scan, then trigger the update → the modal has
  only an OK button and says it will apply on close. Quit → installer runs.
- **Toggle off.** Turn "Check for updates automatically" off, relaunch → no check
  (no log line). The About button still works.

---

## Cross-cutting invariants (check after any step)

These should always be true. If any fails mid-refactor, pause and
diagnose before moving forward.

- **Single process.** Task Manager never shows two `diskoria.exe`, no
  matter how the user launched (double-click, tray, Ctrl+N, second
  `.\scripts\run-dev.ps1`).
- **Tray singletons.** Exactly one app tray icon; drive tray icons
  match the enumerated drive list (no duplicates from reopened
  windows).
- **`history.db` singleton.** Only the primary process writes rows.
  Verify via `lsof`/Handle that only one process holds the file open.
- **No `poisoned` panic.** If a `RwLock`/`Mutex` in `SharedAppState`
  panics, a later `expect("... poisoned")` will crash the app. Never
  acceptable.
- **No stale reads across windows.** If window B shows old theme/accent
  one second after A changed it, the broadcast is broken. File a bug
  before moving on.
- **Guards never cross `draw()`.** Enforced by inspection: every
  `settings.read()` / `drives.read()` inside `DiskoriaApp::draw` must
  drop its guard before the egui rendering calls begin. If we ever see
  mysterious frame hitches when another window mutates settings,
  inspect for a leaked guard.

## Tools / telemetry

- `RUST_LOG=diskoria=debug .\scripts\run-dev.ps1` — see monitor cadence, test
  lifecycle, proxy sends.
- `sqlite3 %PROGRAMDATA%\Diskoria\history.db` — confirm monitor writes.
- Task Manager → Details → `diskoria.exe` — confirm single-process
  invariant on every spawn path.
- `Get-Process diskoria | Select Id,HandleCount,Threads` — watch for
  thread/handle leaks across window open/close cycles.

## Known non-regressions

- `drive_enumeration.rs`: 2 pre-existing warnings (`releases_repo_page_url`
  dead code, `DriveType` unread field). These are OK; do not "fix" them
  in this branch.
- Windows with the same title: intentional — the "any visible" shared
  flag is the source of truth for the launch-#2 decision, not
  `FindWindowW`.
