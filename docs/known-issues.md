# Diskoria — Known issues & oddity log

A living list of bugs, fragile workarounds, and cleanup targets discovered while
working on the codebase. Append here whenever something new turns up so it isn't
lost. Each entry is tagged:

- `bug` — incorrect behavior or a latent crash.
- `fragile` — works today but depends on subtle, easily-broken invariants.
- `cleanup` — code smell / duplication / stale comment; no functional impact.
- `linux-blocker` — Windows-specific assumption that the planned Linux port must
  address. (Historical: the port landed on the `linux-support` branch; new
  platform gaps use `linux`.)
- `linux` — a deliberate parity gap or platform limitation of the Linux build.

Line anchors are approximate — verify against the current file before acting.
KI numbers are stable identifiers (referenced from commits/comments) — never
renumber; resolved items move to the bottom but keep their number.

---

## Open

### KI-2 — Window-drag release synthesis `fragile`
`lib.rs` ~221-241 (`ViewportCommand::StartDrag` handling): after
`window.drag_window()` the OS move-loop swallows the mouse-button release, so
egui's pointer stays logically "down" and the window can't be dragged again. The
code synthesizes a `PointerButton { pressed: false }` event to unstick it. If the
synthetic position is wrong or the event is dropped, dragging silently breaks
until the next click. Document; consider a smoke assertion if feasible.

### KI-3 — Focus-independent redraw depends on per-window callback `fragile`
Background threads (monitor, tests, drive enumeration) call
`ctx.request_repaint()`, which only sets an egui flag and cannot wake a loop in
`ControlFlow::Wait`. Each `Renderer` installs a repaint callback
(`lib.rs` ~171-176) that forwards `UserEvent::Repaint`; `about_to_wait`
(~841-873) then paints **all** windows directly on a timer. If a newly created
window ever skips installing this callback, background updates won't appear in it
until the user interacts. Verify the callback is installed for every renderer,
including `OpenNewWindow` ones.

### KI-4 — Tray `Leave` events unreliable; cursor polling fallback `fragile`
tray-icon's `TrackMouseEvent` sometimes fails to deliver `Leave`, so the flyout
and context menus are closed by polling `GetCursorPos` every 50 ms in
`about_to_wait` (`lib.rs` ~892-933). Flyout windows also carry `WS_EX_NOACTIVATE`
(`flyout.rs` ~58-71) so they don't steal focus and reset tray mouse tracking.
This coupling is load-bearing and non-obvious.

### KI-5 — CPU rasterizer uses gamma ≈ 2.0, not 2.2 `cleanup`
`lib.rs` ~348-358: the software blend linearizes/encodes with a squared/sqrt
(gamma 2.0) approximation instead of true sRGB (~2.2). Blended edges and
antialiased text may be marginally off. Low priority; document the choice.

### KI-6 — Single-instance "any visible" flag startup race `fragile`
`lib.rs` ~963-end: a secondary exe launch reads the `DiskoriaAnyVisible` file
mapping to choose raise-vs-new-window. If it reads before the primary publishes
the flag, it falls back to "raise". Benign today but order-dependent.

### KI-7 — Shared-state guard discipline not enforced `fragile`
`shared_state.rs` ~7-10 documents that `RwLock`/`Mutex` guards must be dropped
before `DiskoriaApp::draw` begins rendering, to avoid frame hitches when another
window mutates settings. This is by convention only — no compile-time check. A
leaked guard would show up as mysterious stalls when two windows interact.

### KI-16 — Two-row drive dropdown: custom popup reuses egui's combo popup id `fragile`
`drive_selector::two_row_combo` paints the collapsed card by hand and drives the
dropdown via `popup_below_widget` opened on **`combo_id.with("popup")`** — the
exact id `ComboBox::is_open` / `Memory::is_popup_open` use — so `focus.rs`'s
manual-focus machinery (`bind_combo_focus_slot`) keeps binding focus to the card
unchanged. This coupling is load-bearing: if egui ever changes
`ComboBox::widget_to_popup_id` (currently `id.with("popup")`), focus binding for
the selector silently breaks. Keyboard access works by relying on egui's
**geometric** arrow-key focus nav (`Memory::find_widget_in_direction`, which is
layer-agnostic): opening focuses the current row, ↑/↓ move between the focusable
`SelectableLabel` rows, Enter/Space selects, Esc closes. Verify keyboard nav
still works after any egui upgrade. Also note the selector row grew from 34 px to
`drive_selector::ROW_H` (56 px) to fit two rows + chips.

### KI-17 — Cross-window per-drive test lock depends on close-path cleanup `fragile`
To stop two windows running tests on one physical drive, `SharedAppState`
holds `test_locks: HashMap<window_token, drive_key>`. Each window *publishes*
its lock every frame in `DiskoriaApp::draw` via `publish_test_lock` —
`Some(lock_key(selected_drive))` while `any_test_running()`, else `None`. This
relies on the invariant that the dropdown is disabled during a test
(`add_enabled_ui`), so `selected_drive` can't drift off the drive actually under
test. Natural finish/stop clears the lock automatically (next frame publishes
`None`), so the scattered `*_test_running = false` sites need no edits — **but a
window that is dropped without `cancel_all_tests()` would leak its lock**
(graying that drive forever until process exit). Today every close path
(`CloseRequested`/`RedrawRequested` `DropThis`, `QuitRequested`) calls
`cancel_all_tests`, which calls `clear_window_test_lock`; keep that wiring if the
window lifecycle changes. Lock changes broadcast `UserEvent::Repaint` so other
windows re-gray live. Drive Health never disables items (read-only); test pages
gray drives locked elsewhere and also gate Start via `selected_drive_busy_elsewhere`.
Identity is `DetectedDrive::lock_key()` (serial, else `disk{n}`).

### KI-28 — Single-instance handoff crosses build boundaries `fragile`
The single-instance guard (`lib.rs`, `acquire_single_instance_mutex`) is keyed on
a machine-wide named mutex and does **not** distinguish which `diskoria.exe`
holds it. So launching the *installed* build while a *portable* copy is still
running raises the portable window instead of starting the installed one — and
that window then correctly reports "Portable", shows the About page's Portable
chip, and disables the update controls (`updates_supported()` is installed-only,
KI-23). The installer looks broken when nothing is wrong.

Hit for real while validating 1.6.x: a portable exe left running from a test
session swallowed the launch of a freshly installed build. Diagnosis is now one
line — the startup banner logs the install mode and the exe path:

```text
Diskoria 1.6.2 starting (Portable, exe=C:\...\releases\1.6.2\diskoria.exe)
```

Arguably correct behaviour (one Diskoria per machine, and the raise is the whole
point of the guard), so not "fixed" — but if it ever warrants one, the fix is to
include the exe directory in the mutex name, or to refuse the handoff when the
running instance's path differs from the launching one. Worth remembering
whenever an installed build seems to ignore the installer: **close the portable
copy, including its tray icon, first.**

---

### KI-33 — No hover flyout on Linux `linux`
The per-drive tray flyout (`flyout.rs`) is Windows-only. Wayland offers no
global window positioning and the StatusNotifierItem protocol exposes no icon
geometry to anchor to, so the Linux tray carries the same information in its
tooltip and actions in its D-Bus menu instead. An X11-only near-cursor popup
would be possible later.

### KI-34 — Single aggregated tray icon on Linux `linux`
Windows shows one thermometer icon per internal drive; the Linux SNI item is a
single icon (hottest drive's thermometer) with per-drive temperatures in the
tooltip. Multiple SNI items are technically possible but most desktops collapse
or hide extra items. Alert flashing maps to the SNI `NeedsAttention` status.

### KI-35 — A *visible* window cannot be focused on Wayland `linux`
`raise_window` uses `focus_window()` + `request_user_attention`. X11 honors it;
on Wayland a client cannot focus itself at all — activation needs a token from
the surface the user clicked, and neither a second launch nor a
StatusNotifierItem host provides one. What does work is *mapping* a window:
compositors focus newly mapped windows, so raising after close-to-tray (where
the window was destroyed, see KI-39) creates one and it gets focus. A
same-frame hide/show "remap" of an already-visible window was tried and does
not work (winit coalesces it) and would disturb a tiling layout anyway.

### KI-39 — Wayland cannot hide a window; close-to-tray destroys it `linux`
winit's Wayland `set_visible` is a documented no-op and `is_visible()` returns
`None` — a Wayland window *is* its mapped surface. Close-to-tray therefore
drops the window (`window_hiding_supported()` in `lib.rs` picks the path) and
the tray recreates one on demand, which is what Qt/Electron apps do too. Two
consequences: `--minimized` cannot start hidden on Wayland (it logs a warning
and shows the window), and with no window there are no frames, so
`App::pump_headless_monitor` drains monitor messages on a 5 s tick to keep tray
temperatures and alerts live.

### KI-36 — `--minimized` autostart runs unelevated; no elevate-on-open hand-off `linux`
A polkit prompt at every login is unacceptable, so the autostart launch skips
the pkexec relaunch and monitors via hwmon only (temperatures, no SMART
attributes). Opening a window from the tray keeps the unelevated session;
health pages show permission-aware errors until the user relaunches normally.
A hand-off (re-exec elevated on open, migrating the single-instance socket) is
designed but not implemented.

### KI-37 — GNOME needs an AppIndicator extension for the tray `linux`
Stock GNOME ships no StatusNotifierWatcher; ksni then logs "no system tray
available" and Diskoria runs without a tray (close-to-tray disables itself).
KDE, XFCE, LXQt and most others work out of the box. See `linux/README.md`.

### KI-38 — Benchmark accepted an unmounted volume as a target `bug` `linux`
Found on the first real elevated Linux run: the Benchmark page let a test
start against a partition with no mount point. `speed_volume_pairs` /
`speed_target` counted **every** partition as a volume, which held on Windows
(WMI only reports partitions that have a drive letter) but not on Linux, where
enumeration lists unmounted partitions too. The empty mount point then flowed
into the temp-path builder and produced a path on an unrelated filesystem —
`/Diskoria_SpeedTest_<n>.tmp` on the root disk — so the benchmark measured the
wrong drive and wrote a 1 GB file to `/` as root (deleted at the end of the
run, which is why it left no trace). Fixed at three levels:
`partition_info::benchmarkable_partitions` (mounted + unlocked) is now the
single definition of a target, `speed_target` picks from those indices instead
of clamping a count, and both temp-path builders return `None` for an empty or
non-absolute mount so `start_speed_test` refuses rather than inventing a path.

### KI-40 — Temperature history was per-window and only loaded at monitor start `bug`
`load_history_from_db` ran only inside `start_monitor_if_not_running`, which
early-returns once the monitor is up. Any window created *after* that — on
Windows a second window, on Linux any window reopened from the tray, since
close-to-tray destroys it (KI-39) — therefore drew an empty chart reading "No
history data yet" until the whole app was restarted. `ensure_history_loaded`
now fills the map once per window from the shared DB (and seeds the latest
snapshot so the card is populated before the next monitor cycle).

### KI-42 — UI froze during a sector scan; blocking work on the event-loop thread `bug`
Reported as: the GUI freezes partway through a sector read, worker threads keep
logging progress, *both* windows freeze together, and the display catches up in
one burst. Both windows freezing at once means the event-loop thread is blocked,
not the rendering.

Two blocking calls were on that thread, both introduced by the Linux port:

1. **`Surface::buffer_mut()` on every frame.** softbuffer's Wayland backend is
   double buffered and loops on `blocking_dispatch` until the compositor
   releases the back buffer, so acquiring is a blocking Wayland round trip. The
   paint path acquired the buffer *before* deciding whether anything had
   changed, so tens of thousands of no-op frames a second each took one — worst
   while a test runs, which is when repaints are continuous. Damage is now
   computed first and the surface is touched only when there is something to
   draw.
2. **XDG portal reads by subprocess.** The theme/accent poll ran `dbus-send`
   from `draw()` and waited for it. An `exec` has to fault a binary in off disk,
   which a sector scan of the boot drive is well placed to starve. Those reads
   now happen on a background thread and the UI only ever reads the cached
   value; the tray's compositor-IPC focus call was moved off-thread for the same
   reason.

A frame taking over 250 ms now logs a warning naming the phase, so this class of
bug announces itself instead of looking like a hang.

Also fixed while verifying: `damage_history` recorded an entry per *paint
attempt*, but `Buffer::age` counts *presents*, so once frames could be skipped
the replay read empty entries instead of the frames actually drawn and left
most of the window stale. `DISKORIA_DAMAGE_VERIFY=1` caught it (3.4 M pixels
adrift); history now records only drawn frames.

**This diagnosis was wrong.** The freeze persisted, and the actual cause was the
O(n²) damage merge — see KI-43. The two fixes above are real event-loop blocks
and worth keeping, but neither was what the reporter hit.

### KI-43 — UI froze for seconds during scans and scrolling: O(n²) damage merge `bug` `fixed`
The real cause of this *and* of the freeze filed as KI-42 — whose diagnosis was
wrong. Both were `merge_damage`, introduced with damage tracking, which is also
when the freezes started.

The watchdog below caught it on the first try: `event loop stalled 1.1 s in
`paint:damage``, reproduced immediately by scrolling.

`frame_damage` emits **two rectangles per changed triangle**. A scroll, or the
heatmap recoloring all 1000 cells mid-scan, changes essentially every triangle
on screen, so `merge_damage` was handed tens of thousands of rectangles. Its
fixpoint loop restarted its entire O(n²) scan after *every* merge and removed
from the middle of the `Vec` each time, and the `MAX_DAMAGE_RECTS` cap (8) was
applied **after** the loop rather than before it — so nothing bounded the work,
and all of it was thrown away when the result collapsed to one rectangle anyway.

Measured on the same input, old vs new:

| rects | old | new |
|------:|----:|----:|
| 4 000 | 2.55 ms | 34 µs |
| 20 000 | 69 ms | 104 µs |
| 50 000 | 383 ms | 180 µs |
| 100 000 | **2.12 s** | 433 µs |

The quadratic curve puts the reported 1.1 s stall at roughly 70 000 rectangles,
which is what a full-window scroll produces.

Now bounded up front, in three tiers: over `RAW_DAMAGE_LIMIT` (4096) raw
rectangles go straight to the bounding box; a linear sweep over y-then-x-sorted
rectangles fuses touching neighbours (text arrives one rectangle per glyph, so a
line of text collapses to one); and the exact pairwise fixpoint runs only on
what survives, which is now always small. Disjointness is still guaranteed —
that is the correctness requirement, since the rasterizer blends and a pixel
covered twice composites twice. Unit tests assert both disjointness *and*
coverage of every input, and the scroll-sized case is asserted to stay under
50 ms. `DISKORIA_DAMAGE_VERIFY=1` reports no stale pixels across all four pages.

Why the earlier diagnoses missed it: the symptom (GUI dead, workers still
logging, both windows frozen together) is consistent with *any* block on the
shared event-loop thread, and the reporter's note that the scan's throughput
never dipped was read as "the thread is parked" when in fact it was spinning —
the scan runs on its own thread, so its chart says nothing about the UI thread.
The fix for that reasoning gap is instrumentation, not more inference.

Two genuine event-loop blocks were found and fixed along the way, neither of
them the cause:

1. **`ksni::blocking::Handle::update` called from winit callbacks.** It is a
   `block_on` of an async-mutex acquire plus a oneshot round trip with the D-Bus
   service loop, so it parks the caller until the tray service *and* the panel
   are done — and it ran once per drive per monitor tick. Tray mutations now
   queue to a dedicated thread.
2. **`SharedAppState::update_settings` wrote settings under the write guard**
   that every window reads during `draw()`, parking the whole UI for the length
   of a disk write. The write moved outside the guard.

**Stall watchdog (`watchdog.rs`), kept.** Each winit callback and paint stage
marks a phase; a background thread warns when a non-idle phase outlasts
`DISKORIA_STALL_MS` (default 1000, `0` disables), naming the phase and the
recovered duration. It turned a freeze that had survived two wrong diagnoses
into a one-line answer. Two relaxed atomic stores per callback, so it stays on
in release builds.

### KI-44 — Crash while resizing: damage replayed from a larger window `bug` `fixed`
```
thread '<unnamed>' panicked at src/lib.rs:829:
range end index 76916 out of range for slice of length 76896
```
Dozens of these at once (one per rayon worker), then the process died. Hit by
resizing the window rapidly; intermittent enough to look like a fluke at first.

The numbers name the bug. 76896 = 32 rows x 2403 px, a full rasterizer band, and
76916 - 74493 (the band's last row) = an `x1` of 2422 against a width of 2403 —
20 px past the end of the row.

`Buffer::age` can hand back a buffer several frames old, so `paint` replays
`damage_history` to catch it up. Those entries were clamped to the size of the
frame that recorded them, and were never re-clamped: once the window shrank they
described a wider framebuffer than the one being drawn, and
`band[row + x0..=row + x1]` indexed out of range. Every band hits the same bad
rectangle, so all rayon workers panic together.

Latent since damage tracking landed; KI-43's fix exposed it by making resize
frames cheap enough to hit the stale-buffer replay path constantly.

Fixed in `replay_damage`, which is now the single place the redraw set is built
and clamps every rectangle to the current framebuffer. `damage_history` is also
dropped on resize — a reallocated surface makes older ages meaningless and those
coordinates belong to a different window size. A `debug_assert` in the band loop
documents the invariant for future changes.

Verified by driving 240 rapid resizes through the compositor's IPC
(`scripts/`-less, niri `set-window-width`/`set-window-height`): without the fix
that reproduces 64 panics and a segfault; with it, no panic, no assertion, and
`DISKORIA_DAMAGE_VERIFY=1` reports no stale pixels. The unit test pins the exact
geometry from the report and fails without the clamp.

### KI-45 — Tray icon missing for the whole session when Diskoria starts before the panel `bug` `linux` `fixed`
Classic login race: ksni's `spawn` registers with `org.kde.StatusNotifierWatcher`
immediately, and if the panel has not claimed that name yet the call fails,
`TrayManager::new` returns `None`, and there is **no service loop left to ever
recover**. Autostart made this likely — Diskoria is often up before the bar is.

`assume_sni_available(true)` is exactly the switch for this: the missing-watcher
case is routed to `Tray::watcher_offline` (returning `true` keeps the service
alive) instead of failing the spawn, so the item sits on the bus and ksni
re-registers on the watcher's `NameOwnerChanged`. Nothing about it needs
privileges.

Verified A/B against a private session bus with no watcher: without the fix,
"no system tray available" and **zero** `org.kde.StatusNotifierItem-*` names on
the bus; with it, the name is owned and the log says it is waiting for the
desktop to provide a watcher.

### KI-46 — Self-updater could install the wrong architecture's binary `bug` `linux` `fixed`
`pick_linux_url` preferred an asset matching `std::env::consts::ARCH` but fell
back to `linux.first()`, so an ARM64 machine offered a release that only shipped
an x86-64 build would download it and `rename` it over the running binary —
replacing a working install with one the kernel cannot exec. Windows hides this
class of mistake behind x64-on-ARM emulation; Linux has no equivalent.

The fallback is now restricted to assets that name *no* architecture (what old
single-build releases look like). An asset built for a different machine is
never chosen, so a release missing this architecture means "no update" rather
than a brick. Arch tokens cover the usual spellings (`x86_64`/`amd64`,
`aarch64`/`arm64`, …), and the matcher takes the architecture as a parameter so
the ARM cases are unit-tested from an x86-64 host.

### KI-47 — Incoherent states around the tray, monitoring and the service `bug` `linux` `fixed`
Adding the root service left several combinations that should not have been
reachable. Fixed together, since they are one question: *nothing collects
without a userspace presence that can see and stop it.*

1. **Close-to-tray could be turned off with monitoring on.** Closing the last
   window then either stopped monitoring silently, or — with the service
   installed — left it collecting with nothing on screen to stop it. It is now
   forced on while monitoring is enabled (`Settings::close_to_tray_effective`,
   the single rule both the settings card and the close decision read; the
   stored preference is left untouched so it returns when monitoring goes off).
   The toggle is greyed with the reason, and drops out of the tab order rather
   than trapping focus on a control that cannot change.
2. **`assume_sni_available` made "has a tray" mean less than it used to.**
   After KI-45 the SNI service stays alive with no watcher on the bus, so
   `tray.is_some()` no longer implies a *visible* icon — and on Wayland
   close-to-tray destroys the window rather than hiding it. Closing the last
   window could therefore strand the app with no icon to bring it back. The
   decision now also requires `tray::host_present()`, tracked from ksni's
   `watcher_online`/`watcher_offline`.
3. **Two switches both meaning "background monitoring".** Settings' *Enable
   background monitoring* is now the master: it governs the in-process thread
   *and* the service (`enable/disable --now`, so a settings change survives a
   reboot). The Background service card is subordinate — inert and unfocusable
   while the master is off — and the tray offers *Start* only when the master
   agrees. Stopping is never conditional; an off switch should always work.
4. **Quitting left the service collecting.** Quit now runs `systemctl stop` —
   stop, not disable, because closing a window is not a decision to undo the
   boot-time setup. That needs authentication, so the exit waits for it (90 s
   cap) instead of exiting underneath the polkit prompt and killing it; the
   tray stays up meanwhile. If the stop is declined, fails or times out, a
   notification says so with the command to run, since there will be no tray
   left to check.
5. **The service could run with no tray at all.** Two wrong answers before the
   right one, both from treating the tray as something the *user* launches:

   - Enabling autostart only from the in-app toggle missed the documented
     install path (`install-service.sh` / `systemctl enable --now`), so a
     normal install had no tray after a reboot.
   - Keying it off the observed service state fixed the reboot, but not *now*:
     enable the service while Diskoria is closed and collection runs with
     nothing in the session to show or stop it until the next login. An
     autostart entry cannot start anything today.

   The tray is now a **systemd user unit** (`linux/diskoria-tray.service`)
   installed and started alongside the system service by `install-service.sh`,
   so the pair moves together: enable collection and an icon appears
   immediately and at every login, and it comes back if it crashes
   (`Restart=on-failure`, not `always` — quitting from the tray is deliberate
   and also stops the collector).

   `autostart::is_enabled` counts the user unit as enabled and
   `set_enabled` defers to it, because two mechanisms would launch Diskoria
   twice at login and the second launch raises a window. The installer removes
   any stale XDG entry for the same reason. The XDG entry remains the fallback
   when the unit is not installed; it records `current_exe()`, so run the copy
   you intend to keep.
6. **`cancel_monitor()` on quit was `#[cfg(windows)]`.** The thread died with
   the process anyway, but the shutdown is now explicit on both.

Windows is deliberately untouched: monitoring there is in-process with no
service, and "closing the last window stops monitoring" is a documented,
intentional trade-off shown under the toggle.

### KI-48 — `fallback_partitions_for_disk`'s `already_found` argument is always empty `oddity` `windows`
In `drive_enumeration/windows.rs`, the WMI-association fallback is entered only
when `out.is_empty()`, but `already` is then built by iterating that same empty
`out`:

```rust
if out.is_empty() {
    let already: Vec<String> = out.iter().map(|p| p.mount_point.clone()).collect();
    let fallback = fallback_partitions_for_disk(disk_index, encryption, &already);
```

So the `already_found` filter inside `fallback_partitions_for_disk` — the
`already_found.iter().any(...)` skip at the top of the drive-letter scan — can
never skip anything. Harmless today (the fallback only runs when nothing was
found, so there is genuinely nothing to exclude), but the parameter reads as
live de-duplication that isn't there, and it would silently do nothing if the
guard were ever relaxed to "top up a partial result".

**Pre-existing, not a `linux-support` regression** — `main` has the identical
code, with only `drive_letter` → `mount_point` renamed. Left as-is rather than
fixed blind, because the right fix depends on which behaviour was intended:
drop the parameter, or move the `already` computation above the `is_empty()`
guard and let the fallback top up partial results.

## Resolved

Condensed; see git history for full detail.

- **KI-18 — Manual card layout: `allocate_rect` rewinds the cursor, next card
  overlaps** `fragile`. Every card hand-computed a `card_h`, reserved it with
  `ui.allocate_space(card_h + N)`, then painted rows at absolute coordinates.
  Two failure modes: **(a)** the hand-counted height drifted from the real
  content (`draw_settings_monitoring` reserved six rows but early-returned after
  two when monitoring was off, leaving ~136 px of empty frame); **(b)** any
  widget inside the card that allocated — the temperature/wear **sliders** via
  `ui.allocate_rect`, and the accent hex **`TextEdit`** via `ui.put` — moved the
  layout cursor *backward*, discarding the reservation so the next card painted
  on top. Confirmed against egui 0.31.1: `advance_cursor_after_rect` and
  `allocate_space` both call `Placer::advance_after_rects`, which sets the cursor
  unconditionally from the rect, while `expand_to_include_rect` only ever *grows*
  `min_rect` — hence a cursor bug that never showed up as a scroll-height bug.
  Fixed by **`card::CardLayout`**: rows come from `row()`, which accumulates the
  true height; the frame is painted last into two `ShapeIdx` placeholders
  reserved at `begin()` (so it still renders *under* the content while being
  sized *after* it); and `end()` performs the single authoritative cursor
  advance, which — being last — overrides any rewind from inside the card. The
  sliders and hex field still rewind; it is simply harmless now. Geometry
  constants live in `theme.rs` (`CARD_PAD`/`CARD_GAP`/`CARD_RADIUS`/…).
  Migrated: the seven Settings cards, the four Drive Health cards (deleting the
  `smart_health_page::card_rect` precursor), `draw_smart_health_card` (which had
  been laying every galley out twice — once to measure, once to paint), and
  `draw_tabbed_map_card`. Both `advance_cursor_after_rect` workarounds are gone.
  Two deliberate deviations from the plan in `refactor-roadmap.md`: `row()` takes
  neither `ui` nor a closure (card bodies mutate `DiskoriaApp` throughout, and a
  closure capturing `&mut self` while the card is alive fights the borrow checker
  at every call site — `CardLayout` borrows nothing, so `card.row(h)` and
  `self.whatever()` coexist), and the card never allocates up front, which is
  what makes it safe in `draw_tabbed_map_card` — the old note there was right
  that reserving `total_h` *before* `allocate_new_ui` corrupts layout state and
  panics on the next frame. Settings card gaps normalized to one `CARD_GAP`
  (Monitoring had 27 px below it against 15 px elsewhere) and the Monitoring card
  now shrinks to its two real rows when monitoring is off. ✓ verified by capture:
  all six Settings cards, both themes, all three sliders dragged with no reflow,
  Drive Health across NVMe/ATA/UFS row counts, and seven map/chart tab switches
  with no panic. **Note the Settings wiki captures need retaking** — the
  normalized gaps shift that page.

- **KI-15 — Benchmark page silently changed the shared drive selection**
  `cleanup`. `ensure_speed_volume_selection_valid` ran on every Benchmark frame;
  when `selected_drive` named a disk with no `(drive, partition)` pair it fell
  back to `pairs[0]`, repointing the *shared* selection at the first drive that
  had a mounted volume — so merely visiting Benchmark moved what Drive Health /
  Sector / Sector Write showed. Replaced with a derived
  `DiskoriaApp::speed_target_pair()`: the selected drive is kept and only the
  partition (a Benchmark-only field) is clamped; a volume-less selection yields
  `None` rather than jumping. The mutation is gone entirely —
  `sync_speed_partition_after_drives_refresh` was deleted too, since clamping on
  read means a refresh can't rewrite the selection either. Picking a volume in
  the Benchmark dropdown remains the one place the page writes `selected_drive`,
  because that is an explicit user choice.

  Three knock-on changes were required rather than optional:
  - The volume-less selection is now shown in the dropdown as a grayed, in-drive-
    order row ("Drive N — model" + a "No mounted volume" chip) and the page says
    *"No mounted volume on this disk. Choose a volume above to benchmark."* Start
    stays disabled — which is what `can_start_speed_test` always intended; the
    old repointing just made that branch unreachable.
  - `DriveEntry::disabled` became `Option<&'static str>` (the tooltip reason).
    The popup previously hard-coded "Testing in another window" for every grayed
    row, which would have been a lie on the new one. `BUSY_ELSEWHERE` in
    `drive_selector.rs` is the shared constant for the KI-17 case.
  - `two_row_combo` now focuses the first *enabled* row when the popup opens if
    the current row is disabled. Previously nothing gained focus, which also
    affected the pre-existing case of a drive that got locked by another window
    while it was your selection.
  - `publish_test_lock` derives its key from the running test's `*_test_target`
    (falling back to the selection) instead of from `selected_drive`. With the
    Benchmark target derived rather than stored, the selection alone can no
    longer be trusted to name the disk actually under test.

  Verified in demo mode with drive 1's partitions temporarily stripped: before,
  Benchmark → Drive Health showed Drive 0; after, it stays on Drive 1. Note the
  canned `demo::drives()` all have partitions (asserted by
  `demo_drives_are_distinct_and_lockable`), so reproducing this state needs
  either that temporary patch or real unformatted hardware.
- **KI-27 — Vendor-packed SMART raws shown and judged as scalar counts** `bug`.
  Several ATA attributes pack more than one field into the 48-bit raw. Found on
  a real Seagate ST2000DM008-2FR102, which reports Power-On Hours as
  `0xCEF0_0000_27F7` — 10,231 hours in the low 32 bits, vendor data in the high
  word. `query_ata` masked that when extracting the vitals, so the **Vitals card
  read 10,231 while the attribute grid printed 227,530,187,483,127** for the same
  fact on the same page. The exported HTML report had it too.

  Two changes, both in `smart_reader.rs`:
  - `display_raw(id, raw)` decodes the packed forms — low 32 bits for the hour
    and cycle counters (0x09, 0x0C, 0xF0), low byte for the temperature
    attributes (0xC2, 0xBE) — and `AtaAttribute::display_raw()` exposes it. The
    grid (`smart_health_page.rs`) and the Export Log now use it, and
    `query_ata`'s own vitals extraction routes through the *same* function, so
    the two can't drift apart again. `AtaAttribute::raw` still holds the
    untouched 48-bit value: the history-DB snapshot archives it, and a
    diagnostic must not lose the vendor's bytes.
  - **0x01 Read Error Rate removed from `is_critical`.** Its raw is a packed
    *rate*, not a count, and is large and non-zero on healthy Seagate/WD drives
    (that same disk: raw 120,202,145 with a healthy normalised 81 against a
    threshold of 6). The `is_critical(id) && raw > 0` rule therefore flagged
    every such drive amber forever. The normalised current/worst-vs-threshold
    checks still catch a genuinely failing read-error rate — which is how
    smartctl and CrystalDiskInfo judge this attribute. 0x07 Seek Error Rate was
    never in the list for the same reason.

  Audited every other `attr.raw` consumer while here; the rest are correct:
  `monitor.rs` reads 0x05/0xC5/0xC6 (genuine sector counts) and already masks
  0xE7 to its low byte, its `"raw"` JSON field is an archive and should stay
  unpacked, and the 0xC7 cable-warning icon tests a plain count.
  `demo.rs`'s SATA drive now carries the real packed values so the invented
  machine exercises the same path; it still displays 14,226 and still warns
  about exactly the two things it is meant to.

  Note this drive also shows **KI-24** live on attributes 0x0A (`cur 100 /
  thr 97`) and 0xB8 (`cur 100 / thr 99`), both amber while healthy. That is the
  separate proximity-band issue and is still unfixed.
- **KI-8 — Stale doc comments after multi-window landed** `cleanup`. The
  `renderers` field and `App::primary()` comments in `lib.rs` (and a couple of
  dangling references to a non-existent `WINDOWS11_EGUI_STYLE_GUIDE.md` and a
  local plan file) were corrected to reflect the shipped multi-window code and
  point at `docs/`.
- **KI-1 — Title-bar constants duplicated** `cleanup` `bug`. `TITLEBAR_H` /
  `BTN_W` / `CONTROLS_W` unified in `theme.rs`; the `win32_resize` wndproc reads
  them instead of redefining `32`/`46`.
- **KI-9 — Top edge / top-left corner don't resize** `bug`. wndproc now returns
  `HTTOP` / `HTTOPLEFT`; the controls strip and top-right corner stay `HTCLIENT`
  so the window buttons remain clickable. ✓ verified.
- **KI-10 — Admin manifest forced elevation on the test exe** `cleanup`. The
  `requireAdministrator` manifest leaked into the `cargo test` exe (os error
  740). `build.rs` now skips the icon+manifest when `DISKORIA_SKIP_RESOURCE=1`,
  so `DISKORIA_SKIP_RESOURCE=1 cargo test` runs unelevated. (A future clean fix
  would bin-scope the resource link rather than the global `rustc-link-lib`, but
  the workaround is accepted.)
- **KI-11 — NVMe "S.M.A.R.T. Status" should show a health %** `bug`. NVMe/UFS
  drives (no ATA predict-fail) now show "Drive Health: {pct}% remaining" via
  `monitor::health_pct_from_report`; SATA/USB keep "S.M.A.R.T. Status". ✓ verified.
- **KI-12 — Clear all build warnings** `cleanup`. `cargo build` and
  `cargo clippy --all-targets` are zero-warning (clippy `--fix` + targeted fixes;
  `too_many_arguments` allowed crate-wide with the `PageLayout` refactor tracked
  in `refactor-roadmap.md`).
- **KI-13 — Refresh could spin forever; drives weren't auto-detected** `bug`.
  `WM_DEVICECHANGE` (`DBT_DEVNODES_CHANGED`) drives a debounced auto re-enumerate;
  a 12 s watchdog abandons a hung scan; a `generation` counter syncs every window
  so draining the one-shot receiver can't strand the refreshing window. Refresh
  kept as a manual fallback. _Pending a final plug/unplug verification._
- **KI-14 — Mojibake-encoded characters in `smart_health_page.rs`** `bug`. Fixed
  by exact codepoint replacement (`â”€`→`─`, `â€¦`→`…`); legitimate `─`/`—`/`°`
  preserved; no other source file affected.
- **KI-19 — Hard-coded white text on accent fills** `bug`. The title bar's
  "New Window" button (and the segmented controls, toggle knobs, modal confirm
  buttons, and the plot export button's hover state) painted `Color32::WHITE`
  onto an accent-colored fill. With a pale accent — a white Windows accent, or
  the Cha-Cha / Quickstep palette swatches — that rendered white-on-white and the
  caption vanished. `theme.rs` already computed a `txt_on_accent` but nothing in
  the chrome used it, and its luminance formula weighted raw sRGB channels as if
  they were linear, which misjudged saturated mid-tones. Replaced with WCAG
  relative luminance + contrast ratio (`theme::text_on`, white unless it drops
  below 3.0:1), and every accent-fill call site now reads `t.txt_on_accent`. The
  three duplicated Settings toggles were folded into `widgets::paint_toggle`, so
  the knob follows the same rule. Covered by `theme::tests`.
- **KI-20 — `minimize_to_tray` setting was persisted but never read** `bug`.
  `Settings::minimize_to_tray` was saved to and parsed from `settings.txt` since
  the monitoring work landed, but no code ever consulted it: `lib.rs`'s close
  disposition hid the last window to the tray whenever `pro_edition` was set,
  which is unconditionally true. Renamed to `close_to_tray` (a new key, so every
  profile re-derives its default once instead of inheriting a value that never
  meant anything), wired into the close path, and given a Settings → Window
  toggle. The old `minimize_to_tray=` line is ignored and dropped on next save.
- **KI-21 — Focus rings were inconsistently white vs. accent** `bug`. Found while
  fixing KI-19. Most keyboard-focus rings were `Stroke::new(2.0, t.accent)`, but
  14 were a hard-coded `Color32::WHITE` (`about.rs` ×3, `drive_selector.rs` ×2,
  `smart_health_page.rs` ×2, `app/pages/{sector,speed}.rs` ×2 each,
  `destructive.rs` ×3). Every one is drawn at `rect.expand(2..3)` — *outside* the
  widget, on the card background — and `t.bg_sec` is pure white in the light
  theme, so keyboard focus was invisible there. Unlike KI-19 this was a *theme*
  contrast bug, not an accent one, so `txt_on_accent` did not apply. All 14 now
  use `t.accent`, matching what the segmented controls already did (including on
  accent-filled buttons, where the ring reads as a halo separated from the fill
  by the expand gap). A dedicated `Theme::focus_ring` token remains an option if
  that accent-on-accent case ever needs differentiating.
- **KI-22 — Self-update copied the installer over `diskoria.exe`** `bug`.
  `update::pick_exe_url` deliberately prefers the `*setup*.exe` release asset, and
  `poll_update_download` branches on the downloaded file's *name*: `setup` → run
  the Inno installer, otherwise → copy the exe over ourselves. But the download
  was always saved as `Diskoria_update_<nanos>.exe` (`app.rs`), so the marker
  never survived and the installer branch was unreachable — every update ran
  `spawn_apply_update_and_exit`, clobbering the running `diskoria.exe` with the
  Inno setup stub. Fixed with `update::update_temp_file_name`, which keeps a
  `setup` token when `url_is_installer` says the asset is an installer; covered by
  `update::tests`. **The bug lives in shipped 1.6.0 as well**, so the 1.6.0 →
  1.6.1 hop still misbehaves — that upgrade must be done by running the installer
  manually. 1.6.1 onward self-updates correctly.
- **KI-23 — Update checks were available to portable builds** `bug`. The updater
  installs an Inno installer, so letting a portable exe apply one silently
  converted it into an installed copy (and, pre-KI-22, corrupted it). The check is
  now gated on `install_mode::current().is_installed()` via
  `DiskoriaApp::updates_supported()`, which `update_check_button_enabled()` and
  therefore `on_about_check_updates_clicked` both consult — so any future trigger
  (startup or periodic auto-check) inherits the restriction. The About button is
  greyed for portable builds with a hover tooltip explaining why. Note for the
  record: despite appearances there has never been an *automatic* update check in
  this repo — the About button is the only entry point (verified across the full
  git history and the `diskoria.bak` archive). The stale
  `HKLM\Software\Diskoria\AutoCheckUpdates` value predates this repo and is read
  by nothing.
- **KI-24 — SMART attribute status warns on healthy high-threshold attributes**
  `oddity`. `smart_reader::compute_status` raises `AttrStatus::Warning` when
  `current <= threshold + max(threshold / 10, 2)`, meant as a "close to failing"
  band. For attributes whose threshold is *high* the band swallows healthy
  values: Spin Retry Count (0x0A) is commonly `current 100 / threshold 97` on a
  perfectly good drive, and 100 ≤ 97+9 flags it amber. Same shape for any
  attribute with a threshold above ~91. Found while picking demo values for
  `--demo-health` (`demo.rs`); worked around there by giving 0x0A a low
  threshold, so the invented drive warns only about the two things it should.
  A real fix would make the proximity band relative to the *headroom*
  (`current - threshold` against `100 - threshold`) rather than to the
  threshold's own magnitude. Not fixed: it only over-warns, never under-warns,
  and changing it silently changes what every real drive reports.
- **KI-25 — Selected nav row uses `from_rgba_premultiplied` with unmultiplied RGB**
  `oddity`. `app.rs` fills the active sidebar row with
  `Color32::from_rgba_premultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 38)`.
  Premultiplied means the RGB should already be scaled by the alpha; passing
  full-intensity accent channels with alpha 38 is the unmultiplied form, so the
  constructor is misnamed for what is being passed. Found while measuring tokens
  for the wiki mockups: a pixel scan of three pinned accents (#8E44AD, #3498DB,
  #F1C40F) shows the row composites at an effective alpha of ~0.128 in linear
  light, not the 38/255 = 0.149 the call reads like. Cosmetic — the result looks
  fine and is stable across accents — but the number in the source does not
  describe the wash that lands on screen, so anyone tuning it by editing 38 will
  be surprised. `from_rgba_unmultiplied` is almost certainly what was meant;
  changing it now would visibly lighten every selected nav row, so it is left
  alone and recorded here instead.
- **KI-26 — Rasterizer dropped pixels on whole-pixel shape edges** `bug`. Reported
  as black/blank specks around the title bar's "New Window" pill. egui antialiases
  by tessellating a solid core plus a 1px feathered ring that *share* an edge, so
  any shape edge landing on a whole pixel coordinate puts that shared edge exactly
  down the middle of a row of pixel centres. Exact arithmetic would put those
  centres on both triangles; f32 puts them ~1e-8 outside *both*, so the
  `w0 < 0.0 || w1 < 0.0 || w2 < 0.0` inside-triangle test dropped them and the
  background dotted through the fill. The pill is only the most visible victim —
  its bottom edge sits at exactly `TITLEBAR_H` over a contrasting page, and it is
  saturated accent — but every panel and button was exposed. Fixed by admitting
  weights down to `-EDGE_EPS` (`lib.rs`, `1e-5` — float noise misses by ~1e-8,
  real geometry by ~1e-1, so the threshold sits between with orders of magnitude
  to spare) in all four rasterizer copies (`lib.rs` + the three in `flyout.rs`).
  Clamping `final_a` to `[0, 1]` is part of the fix, not tidying: an admitted
  pixel sits marginally outside its triangle, so its interpolated alpha can exceed
  1, driving the gamma blend negative and `sqrt()` to NaN — which casts to 0 and
  paints a black speck, the very artifact being removed. ✓ verified by A/B capture
  at 100% scale: 6 background-gray pixels along the pill's bottom row (y=31)
  became accent, plus 6 more on other panel edges; 0 remain. Ported from
  phoenix-simulacra `e40a0a3`, which shares this rasterizer's lineage.
- **KI-29 — Tabbing past the custom-hex field silently pinned the accent to purple**
  `bug`. Reported as "the accent is set to Windows but it's always purple". The
  `TextEdit` draft `DiskoriaApp::accent_custom_hex` is seeded from
  `Settings::accent_custom_hex`, whose default is **`"#8E44AD"`** — the same purple
  as `ACCENT_PALETTE[0]`. The `te_resp.lost_focus()` handler committed whatever
  the buffer held without checking that the user had typed anything, setting
  `accent_use_custom = true`, persisting the hex and (source == Palette) calling
  `set_accent_color`. So the field committed its own placeholder — and focus gets
  there with no click, because `focus.rs`'s `bind_text_focus_slot` binds egui
  focus to it at the hex tab slot. **Tab-ing through the Settings page was enough
  to discard a chosen swatch and repaint everything purple.** It also looked
  stuck rather than overridden: a swatch only draws as selected when
  `!accent_use_custom`, so afterwards nothing was ringed. This is how a profile
  ends up with `accent_source=palette / accent_use_custom=true /
  accent_custom_hex=#8E44AD` in `settings.txt` without anyone typing a hex code —
  confirmed on the author's own profile. Fixed by gating the commit on a real
  edit: `accent_hex_edited` is set from `te_resp.changed()` and cleared on
  commit; an unedited blur now re-seeds the draft from the setting instead of
  writing to it.
- **KI-30 — "Windows accent" read the DWM colorization color, not the accent**
  `bug`. `theme::windows_accent_color` used `DwmGetColorizationColor`, which
  returns the composited **colorization** color — the accent blended per
  `ColorizationColorBalance`/afterglow — not the accent itself. Measured on a
  stock Windows 11 profile (balance 89, `ColorPrevalence=0`): accent `#0078D4`,
  colorization `0xE3006FC4`, so Diskoria painted `#006FC4`. Plausible enough to
  pass a glance, never a match. Worse, it is not guaranteed to track the accent
  at all, and in a session without DWM composition — **including Windows PE, a
  target environment** — it fails outright, `windows_accent_color()` returns
  `None`, and `initial_accent_color` silently falls back to `ACCENT_PALETTE[0]`:
  the Settings page reads "Windows accent" while the UI is purple, permanently,
  with nothing to explain it. The segment handlers had the mirror hole — on
  `None` they skipped `set_accent_color` entirely, stranding the previous
  source's color under a "Windows accent" label. Fixed by reading
  `HKCU\Software\Microsoft\Windows\DWM\AccentColor` (a `REG_DWORD` in **ABGR**,
  the reverse of DWM's ARGB — `accent_from_abgr`/`accent_from_argb` are unit
  tested precisely because mixing them up yields a plausible wrong color), with
  `DwmGetColorizationColor` kept as fallback. ✓ verified: resolves to `#0078D4`,
  matching the OS. The fallback is no longer silent — `SharedAppState::
  accent_os_available` is set at startup and by the accent poll, logged once at
  startup, and shown on the Settings page where the swatches would be; the
  segment handlers now go through `reapply_accent_source`, which falls back
  explicitly.
- **KI-41 — withdrawn: `trayicon.ico` is not a placeholder.** Filed in error
  after rendering it at 256 px, where its flat two-tone shapes look unfinished.
  It is a deliberately simplified, high-contrast design for the 16–24 px a tray
  actually draws at, which is exactly why it differs from `appicon2.ico`. Both
  platforms keep using it for the tray; `tray::app_icon_rgba` carries a comment
  so it does not get "fixed" again. Number retired rather than reused.
- **KI-32 — Segfault at exit on Wayland: clipboard worker outlived the display
  connection** `bug`. Found the moment the Linux shell first ran: `run_app`
  consumes the winit `EventLoop` (and with it the Wayland connection), but `App`
  — holding every `Renderer`, whose egui-winit state owns the smithay-clipboard
  worker thread — was dropped *after* `run_app` returned. The worker then called
  `wl_proxy_destroy` on objects of a dead `wl_display` and the process died with
  SIGSEGV after the clean-exit log line (boot smoke saw exit 139, not 0).
  Windows never hit it because nothing in a renderer holds display-connection
  objects with a cross-thread teardown. Fixed by clearing `self.renderers` in
  `ApplicationHandler::exiting` (`lib.rs`), which the loop invokes while the
  connection is still alive; the previously Windows-only `exiting` hook is now
  unconditional with the staged-update half behind `#[cfg(windows)]`.
- **KI-31 — Tray windows used a hard-coded accent when no main window existed**
  `bug`. The flyout and both tray context menus (`lib.rs`) resolved their accent
  as `self.primary().map(|r| r.app.shared.accent_color()).unwrap_or(Color32::from_rgb(61, 90, 128))`.
  `primary()` is `None` in exactly the configuration where the tray *is* the whole
  UI — launched `--minimized` from the startup scheduled task, or after the last
  window closed to tray — so those surfaces painted a slate blue matching neither
  the user's accent nor any palette entry. `self.shared` is in scope at all three
  sites and `SharedAppState::accent_color()` is always populated, so the fallback
  was never needed: all three now read it directly.
