# Diskoria — testing

Diskoria's test coverage has three layers, from cheapest/most reliable to most
involved. Most logic lives behind a GUI that needs admin rights, real disks, and
a window — so the strategy is to test the **pure logic** automatically and reserve
hardware/GUI checks for manual runs.

## 1. Unit tests (pure logic — run everywhere)

```text
cargo test
```

> **Unelevated / CI shells:** the app's admin manifest gets linked into the test
> exe, so a plain `cargo test` in a non-elevated shell fails with "requires
> elevation" (os error 740). Build the tests without the manifest:
>
> ```text
> DISKORIA_SKIP_RESOURCE=1 cargo test
> ```
>
> An elevated developer shell can run `cargo test` directly. See `known-issues.md`
> (KI-10) for the proper long-term fix.

These are `#[cfg(test)]` modules beside the code they cover. No hardware, no
display, no admin:

- `alert_engine.rs` — alert thresholds, delta-on-increase rules, cooldown
  windows and suppression (in-memory SQLite).
- `smart_reader.rs` — attribute name/criticality tables, `compute_status`
  classification, and the NVMe health-log byte parser (`parse_nvme_health_log`).
- `monitor.rs` — `HealthSnapshot` extraction from ATA/UFS reports, UFS lifetime
  encoding, and the flyout health-summary string.
- `app_settings.rs` — hex color parse/format, accent selection rules, and a
  settings save→load round-trip (uses a temp `PROGRAMDATA`).
- `history_db.rs` — insert/query/prune against an in-memory database
  (pre-existing).

> Note: `rusqlite` is a Windows-only dependency, so the DB-backed tests compile
> and run on Windows. The rest are platform-agnostic.

To confirm the suite actually catches regressions, break a parser (e.g. flip a
byte offset in `parse_nvme_health_log`) and re-run — a test should fail.

## 2. Boot-smoke test (full startup, real window — Windows, manual)

The real binary understands `DISKORIA_SMOKE`: when set, it renders that many
frames through the complete `draw()` path and exits 0. This exercises app
construction, fonts/textures, page dispatch, and the polling loops — surfacing
panics that unit tests can't.

Run the harness (ignored by default because it opens a window and needs an
interactive desktop session):

```text
cargo test --test boot_smoke -- --ignored
```

Or drive the binary directly:

```text
DISKORIA_SMOKE=3 cargo run        # renders 3 frames, then exits 0
```

The harness (`tests/boot_smoke.rs`) points `PROGRAMDATA` at a temp dir so it
never touches the real `history.db` / `settings.txt`, and fails if the process
panics or doesn't exit within 30 s.

## 3. Manual GUI / hardware matrix

Behavior that needs real drives, the tray, multiple windows, or admin rights is
covered by the manual checklist in `docs/`:

- `docs/multi-window-smoke-tests.md` — per-feature window/tray/monitor matrix.

Run these by hand (with `RUST_LOG=diskoria=debug .\run-dev.ps1`) before shipping
changes that touch windowing, the tray, the monitor thread, or the disk-test
workers.

## What is *not* covered automatically

- The custom chrome / resize hit-testing (`chrome.rs`) — visual, manual only.
- The CPU rasterizer output (`lib.rs::paint`) — no pixel snapshot tests.
- Real SMART/IOCTL paths (`surface_test`, `destructive_test`, `speed_test`,
  the Windows branches of `smart_reader`) — require real hardware + elevation.

See `known-issues.md` for fragile areas worth extra manual attention.
