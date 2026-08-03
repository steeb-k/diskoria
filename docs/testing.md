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
- `theme.rs` — the responsive-nav breakpoints (`nav_mode`) and the invariant
  that every mode still leaves the content a usable width at the narrowest
  window it can be drawn in.
- `app.rs` — `rail_hover_step`, the icon rail's hover open/close delays. Pure
  so the timing is testable without a window: the interesting case is a pointer
  brushing past, which must never open the overlay.
- `smart_health_page.rs` — the Vitals label column and temperature caption at
  narrow card widths, asserting the desktop layout is *unchanged* as well as
  that the narrow one fits (KI-52).
- `modal_confirm.rs` — dialog fitting: capped at the window, grown to fit
  wrapped text, and never negative on a degenerate window size.

> Note: `rusqlite` is an unconditional dependency since the linux-support work, so every unit test runs on both CI jobs.

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

Run these by hand (with `RUST_LOG=diskoria=debug .\scripts\run-dev.ps1`) before shipping
changes that touch windowing, the tray, the monitor thread, or the disk-test
workers.

## What is *not* covered automatically

- The custom chrome / resize hit-testing (`chrome.rs`) — visual, manual only.
- The CPU rasterizer output (`lib.rs::paint`) — no pixel snapshot tests.
- Real SMART/IOCTL paths (`surface_test`, `destructive_test`, `speed_test`,
  the Windows branches of `smart_reader`) — require real hardware + elevation.
- **ARM64 has never been executed on any platform.** The Linux `aarch64` cross
  build is clean and produces a correct ELF, but no ARM hardware has run it, so
  nothing past "it links" is known. Treat an ARM64 artifact as unverified until
  someone boots one.
- Damage tracking under *real* input. The `DISKORIA_DAMAGE_VERIFY` sweeps on
  Windows were driven by synthesized `PostMessage` traffic, which the OS does
  not coalesce the way it does genuine mouse input, and no rendered frame was
  ever inspected by eye. "No mismatches" means the verifier found none, not that
  the UI looked right.

See `known-issues.md` for fragile areas worth extra manual attention.

## Elevated Linux verification

`sudo ./scripts/test-elevated.sh` runs everything that needs root, without
touching a real disk destructively: elevated enumeration (partition styles,
LUKS states), the real SMART/NVMe ioctls with a smartctl cross-check, the
hwmon fallback, and a surface scan + destructive write+verify (including the
unmount pre-flight) against a disposable 256 MB file-backed loop device,
finishing with an elevated GUI smoke on the invoking user's display. It
builds as $SUDO_USER so target/ stays user-owned.
