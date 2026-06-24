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
