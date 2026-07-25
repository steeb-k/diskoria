# Diskoria — Known issues & oddity log

A living list of bugs, fragile workarounds, and cleanup targets discovered while
working on the codebase. Append here whenever something new turns up so it isn't
lost. Each entry is tagged:

- `bug` — incorrect behavior or a latent crash.
- `fragile` — works today but depends on subtle, easily-broken invariants.
- `cleanup` — code smell / duplication / stale comment; no functional impact.
- `linux-blocker` — Windows-specific assumption that the planned Linux port must
  address.

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

### KI-15 — Benchmark page can silently change the shared drive selection `cleanup`
With the unified `selected_drive` (KI elsewhere), visiting the Benchmark page
with a partition-less drive selected triggers
`ensure_speed_volume_selection_valid` (`app.rs:889`), which can't keep a drive
that has no `(drive, partition)` pair and falls back to `pairs[0]` — repointing
`selected_drive` to the first drive that has a testable volume. Because the
selection is shared, that change persists on Drive Health / Sector pages too.
Only happens when another drive has a partition (else `pairs` is empty and the
selection is left alone). Natural fix: have the speed page derive a *display*
selection without mutating the shared `selected_drive`, or only adjust the
partition, never the drive. (Behavior accepted for now.)

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

### KI-18 — Manual card layout: `allocate_rect` rewinds the cursor, next card overlaps `fragile`
Every settings/page "card" hand-computes a `card_h`, calls
`ui.allocate_space(card_h + N)` to reserve vertical space, then paints its rows at
absolute coordinates. The footgun: any widget inside the card that uses
`ui.allocate_rect(...)` (e.g. the temperature/wear **sliders** in
`draw_settings_monitoring`, via `Sense::click_and_drag`) calls
`advance_cursor_after_rect`, which moves the layout cursor **backward** to that
rect's bottom — *discarding* the `allocate_space` reservation. The next card then
reads a stale, too-high cursor and overlaps the previous one (seen when the Test
Results card landed on top of the Monitoring card's "Test notifications" buttons).
Worked around in `draw_settings_monitoring` by calling
`ui.advance_cursor_after_rect(section_rect)` at the end to re-assert the reserved
bottom. This whole class of bug ("adding a new card overlaps an old one") has
recurred across pages during development; the real fix is a standardized card
builder that owns its `allocate_space` and reports its own bottom rect — see
`docs/refactor-roadmap.md` ("Standardized card layout").

---

## Resolved

Condensed; see git history for full detail.

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
