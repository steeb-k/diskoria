# Diskoria — refactor roadmap

What's been done structurally, and what's deliberately deferred. Pair with
`known-issues.md` (bugs/oddities) and `gui-architecture.md` (how the UI works).

## Done — moderate cleanup (Phase 3)

- **Shared chrome constants** — `TITLEBAR_H`/`BTN_W`/`CONTROLS_W` live only in
  `theme.rs`; the Win32 resize wndproc reads them (KI-1).
- **Top-edge / top-corner resize** added to the wndproc (KI-9).
- **Path seam** — all `%PROGRAMDATA%\Diskoria\…` paths go through
  `src/paths.rs` (`data_dir`, `settings_file`, `history_db_file`). One file to
  change for the Linux port.
- **`app.rs` page split** — the big central-page draw functions moved out of the
  5,800-line `app.rs` into a `pages` **submodule of `app`**:
  - `src/app/pages/sector.rs` ← `draw_sector_page`
  - `src/app/pages/speed.rs` ← `draw_speed_page`
  - `src/app/pages/destructive.rs` ← `draw_destructive_page`

  `app.rs` dropped from ~6,220 to ~4,840 lines. The moves are **verbatim** (no
  logic change); behavior is unchanged.
- **Shared drive selector** — the refresh button + drive/volume dropdown was
  copy-pasted across Sector, Sector Write, Benchmark and Drive Health (each with
  its own chip helpers). It now lives once in `src/drive_selector.rs`
  (`two_row_combo`, `refresh_button`, `ChipSpec`/`DriveEntry`, plus the `ROW_H`/
  `REFRESH_W`/`REFRESH_GAP` layout constants). Pages build a `Vec<DriveEntry>`
  and call it; the per-page chip helpers in `app.rs`/`smart_health_page.rs` were
  deleted. New two-row design (name on row 1, media/bus/size chips on row 2,
  icon-only refresh that disables + grays while refreshing, with a 750 ms
  minimum-visible window). The open list is keyboard-navigable
  (arrow keys/Enter/Esc) via egui's geometric focus nav — see KI-16 for the
  load-bearing popup-id coupling that makes the focus binding work.

### How the page split works (pattern for future moves)

`pages` is declared `mod pages;` inside `app.rs`, so `app::pages::*` are
*descendant* modules of `app`. Descendants can access an ancestor's **private**
items, so each page file can still:

- read/write `DiskoriaApp`'s private fields, and
- call private helper methods left in `app.rs` (`draw_smart_health_card`,
  `draw_sector_test_panel`, `draw_tabbed_map_card`, etc.).

Each page file is:

```rust
use /* only what this page needs */;
use super::super::{/* app-private free fns: chip_width, chip_pill, … */};

impl crate::app::DiskoriaApp {
    pub(crate) fn draw_<x>_page(&mut self, /* … */) { /* verbatim body */ }
}
```

The method is `pub(crate)` so `draw_central` in `app.rs` can still dispatch to
it. Shared helpers (chips, the tabbed map card, the SMART card) intentionally
**stayed in `app.rs`** because several pages use them; only the top-level page
functions moved.

To move another page: cut its `fn` body, drop it into a new
`src/app/pages/<x>.rs` using the template above, add `mod <x>;` to
`src/app/pages.rs`, then let the compiler tell you which imports/app-private
helpers to pull in. Verify with `cargo build` + the boot-smoke + a manual nav.

## Deferred — aggressive restructure (future phase)

Not started; capture here so it's ready.

1. **Collapse duplicated test state.** `DiskoriaApp` holds parallel
   `surface_*`, `destructive_*`, `speed_*` field families (running flags,
   receivers, cancel handles, chart points, heat min/max, focus slots). Fold
   into an enum:
   ```rust
   enum ActiveTest {
       None,
       Surface(SurfaceState),
       Destructive(DestructiveState),
       Speed(SpeedState),
   }
   ```
   Biggest single shrink to `app.rs`; touches every `poll_*`/`start_*`/
   `cancel_*` path, so do it behind the test net.

2. **Shared sector-map + performance-chart widget.** `draw_tabbed_map_card`
   already serves surface and destructive, but the heat-grid/chart state
   (`*_chart_points`, `*_heat_min_ms`, …) is duplicated per test. Extract a
   reusable widget owning that state once.

3. **Unify per-page focus.** `sector_focus`/`speed_focus`/`health_focus`/
   `destructive_focus`/`settings_focus` + the manual Tab cycling could become a
   single per-page focus manager (see `focus.rs`).

4. **Move remaining page bodies.** `draw_about_page`, the settings sub-pages
   (`draw_settings_theme`, `draw_settings_monitoring`), and
   `draw_drive_health_page` (in `smart_health_page.rs`) can follow the same
   `pages/` pattern once the state consolidation above lands.

5. **Trait-based platform backend (only when Linux work starts).** Introduce a
   `StorageBackend` trait (enumerate drives, query SMART, run sector/speed
   tests) with a Windows impl, so the Linux port fills in a second impl instead
   of threading `#[cfg]` through call sites. Pair with the `paths` seam already
   in place. Until then, keep the per-module `#[cfg(windows)]` + stub pattern.

6. **Standardized card layout (cards report their own bottom).** *Pinned after
   the Test Results card overlapped the Monitoring card — see KI-18.* Today every
   card hand-computes a `card_h`, calls `ui.allocate_space(card_h + N)`, and
   paints rows at absolute `y` coordinates. Two recurring failure modes: (a) the
   hand-counted `card_h` drifts out of sync when rows are added/removed, and
   (b) a child `ui.allocate_rect` (sliders) rewinds the layout cursor so the next
   card overlaps. Replace the ad-hoc pattern with a small card helper that owns
   the bookkeeping, e.g.:
   ```rust
   // Reserves space as rows are added and returns the card's outer rect so the
   // next card auto-positions below it — no manual card_h, no cursor drift.
   let mut card = CardLayout::begin(ui, t, content_x, margin, section_w, "Test Results");
   card.row(ui, ROW_H, |ui, rect| { /* toggle */ });
   card.row(ui, ROW_H, |ui, rect| { /* preview buttons */ });
   card.end(ui); // paints bg/border at the measured height, advances the cursor
   ```
   `row()` hands out the row rect (so existing absolute-coord painting still
   works) while accumulating height; `end()` paints the frame at the true content
   height and does a single authoritative `advance_cursor_after_rect`, making the
   `allocate_rect` rewind harmless. Migrate `draw_settings_theme` /
   `draw_settings_monitoring` / `draw_settings_test_overlay` and the page cards to
   it; once in place the KI-18 workaround can be removed. Pairs naturally with the
   `PageLayout` struct idea under "Related known issues" (KI-12).

## Related known issues to fold in

- **KI-12** — clear all build warnings (the moved code carries a couple of
  pre-existing `too_many_arguments`/`needless_borrow` lints; address in the
  warning sweep, e.g. by passing a small `PageLayout` struct instead of 7 loose
  args).
- **KI-11** — NVMe health % on the sector pages (touches the moved
  `pages/*.rs`).
