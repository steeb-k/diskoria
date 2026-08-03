# Windows verification handoff — `linux-support`

**Branch-only file. Delete before merging to `main`.**

Everything on this branch was written and tested on Linux. **Nothing here has
ever been compiled for Windows**, including changes to code Windows shares. This
is the list of what to check and in what order, with the reasoning behind each
so you can judge for yourself where the risk sits.

39 commits. Zero warnings and 151 tests passing on Linux; `cargo clippy
--all-targets` clean.

---

## 1. Start here: build and the rasterizer

The single highest-risk area. The CPU rasterizer was rewritten this branch and
**none of it is `cfg`-gated** — Windows compiles and runs the same code.

```powershell
cargo build --all-targets
cargo clippy --all-targets                    # zero-warning bar
$env:DISKORIA_SKIP_RESOURCE=1; cargo test     # KI-10
```

Then, before anything else, **resize a window repeatedly, fast**:

```powershell
$env:DISKORIA_DAMAGE_VERIFY=1
cargo run
```

Why this first: KI-44 was a hard panic (`range end index … out of range for
slice of length …`) that killed every rayon worker and took the process with
it. It came from replaying damage rectangles recorded at a *larger* window size
against a smaller framebuffer. The fix (`replay_damage`, `lib.rs`) clamps every
rectangle to the current buffer, and `damage_history` is dropped on resize. It
is fixed and regression-tested, but it was reachable on any platform and the
Windows resize path differs in one relevant way: `window_hiding_supported()` is
`true` there, and Windows resizes are driven by the `WM_NCHITTEST` subclass
rather than a compositor. Worth re-earning confidence.

`DISKORIA_DAMAGE_VERIFY=1` re-renders every frame in full into a scratch buffer
and logs any pixel that differs from what damage tracking produced. Zero
mismatches is the pass condition. Exercise hover, scrolling, dropdowns, modals,
theme switch, and a running test's live heatmap — those are the paths I could
never drive on Wayland, so they are unverified on *both* platforms.

Diagnostics available:

| Env var | Effect |
|---|---|
| `DISKORIA_DAMAGE_VERIFY=1` | full-frame reference compare, logs stale pixels |
| `DISKORIA_FULL_REPAINT=1` | disables damage tracking — A/B a suspected artifact |
| `DISKORIA_FRAME_STATS=1` | per-frame breakdown (UI, tessellate, rasterize, present, overdraw) |
| `DISKORIA_STALL_MS=<ms>` | stall-watchdog threshold; `0` disables |

### What changed in the rasterizer

- **Parallel banding** (`rayon`, now an unconditional dependency). The
  framebuffer is split into 32-row bands, one task each. Ordering is preserved
  because every band walks the primitive list in the same order.
- **Damage tracking**: only changed rectangles are redrawn, replayed against
  `Buffer::age` for stale buffers. Idle frames are skipped entirely.
- **`merge_damage` rewritten** (KI-43). The old one restarted an O(n²) scan
  after every merge with an O(n) `Vec::remove` — O(n³) — over the tens of
  thousands of rectangles a scroll produces. Measured 2.12 s at 100k rects
  versus 433 µs now. On Linux this froze the UI for seconds; **if Windows ever
  felt sluggish during scrolling or a sector scan, this was very likely why.**
- **`debug_assert!`** in the band loop pins the "damage is inside the
  framebuffer" invariant. Debug builds will catch a regression loudly.

Expected gain is large at high resolution: 150 ms → 9.5 ms per frame on a 4K
Linux display, before damage tracking. It should carry to Windows since it is
the same code, but that is a prediction, not a measurement.

---

## 2. The stall watchdog is on by default

New `watchdog.rs`, **not** `cfg`-gated. Each winit callback and paint stage
marks a phase; a background thread logs a `warn` when a non-idle phase outlasts
1 s (`DISKORIA_STALL_MS`). It cost two relaxed atomic stores per callback and it
is what finally identified KI-43 after two wrong diagnoses.

**Expect it to fire on Windows in at least one place I know of and did not
change:** the Settings → Startup card calls `crate::autostart::is_enabled()` on
first draw, which shells out to `schtasks` synchronously on the event-loop
thread. That is pre-existing behaviour, not something this branch introduced,
but the watchdog will now name it. If you see

```
event loop stalled 1.2 s in `paint:ui+poll`
```

when opening Settings, that is the likely cause and it is worth fixing (cache it
off-thread the way `service_control` does on Linux) — but it is not a
regression.

A stall warning naming `paint:surface-acquire`, `paint:rasterize` or
`paint:present` *would* be new and worth investigating.

---

## 3. Shared, non-gated changes Windows compiles

These are the ones that can bite without being obviously "the Linux port".

| File | Change | What to check |
|---|---|---|
| `shared_state.rs` | `update_settings` now writes `settings.txt` **outside** the write guard. Holding it across a file write parked every window while a disk was busy. | Change any setting in two windows; both stay responsive and the file is correct. |
| `history_db.rs` | `open_or_create` refactored through `open_rw_at(path, journal)`. Same PRAGMAs (`WAL`, `synchronous=NORMAL`, `busy_timeout=5000`). | History DB opens; temperature chart populates. |
| `app_settings.rs` | New `Settings::close_to_tray_effective()`. **On Windows it returns the raw preference — behaviour is unchanged by design.** | Close-to-tray on/off both behave exactly as before. |
| `monitor.rs` | New `report_from_json` (inverse of `report_to_json`). Unused on Windows but public API. | Nothing; covered by unit tests. |
| `smart_reader/mod.rs` | New `ata_attribute_from_parts`; ATA attribute table and NVMe log parser hoisted to shared code and their tests un-gated. | Drive Health attribute table renders identically to `main`. |
| `partition_info.rs`, `detected_drive.rs` | Renames: `drive_letter` → `mount_point`, `BitLockerStatus` → `EncryptionStatus`, `drive_letters_display` → `mounts_display`. Plus `benchmarkable_partitions` (KI-38). | Compile-time mostly — but confirm the UI still says **"BitLocker"** on Windows, and that the Benchmark page still picks a sensible volume. |
| `lib.rs` | `cancel_monitor()` on quit widened from `cfg(windows)` to both. | Windows path unchanged. |
| `tray/mod.rs` | New `host_present()`; returns `true` unconditionally on Windows. | Close-to-tray still works. |

---

## 4. Structural moves — Windows code relocated

Storage/tray/autostart modules became directories with a shared `mod.rs` (types
+ pure logic) and `windows.rs` / `linux.rs` transports behind identical
free-function signatures. **The Windows implementations moved close to verbatim,
but they moved.**

```
drive_enumeration.rs  → drive_enumeration/{mod,windows,linux}.rs
smart_reader.rs       → smart_reader/{mod,windows,linux}.rs
surface_test.rs       → surface_test/{mod,windows,linux}.rs
destructive_test.rs   → destructive_test/{mod,windows,linux}.rs
speed_test.rs         → speed_test/mod.rs
tray.rs               → tray/{mod,windows,linux}.rs
autostart.rs          → autostart/{mod,windows,linux}.rs
```

Worth an actual pass on hardware, because a silent behaviour change here would
not show up in any test:

- WMI drive enumeration, including hot-plug (`WM_DEVICECHANGE` debounce)
- SMART / NVMe via `DeviceIoControl`; cross-check one drive against CrystalDiskInfo
- Sector read scan + heatmap
- Destructive write+verify **on a scratch drive** — and confirm it still refuses
  the OS disk
- Benchmark, including temp-file cleanup
- Tray: per-drive thermometer icons, hover flyout, context menu, close-to-tray
- Autostart scheduled task create/remove, and `--minimized` launch
- Installed vs. portable detection, and the update check being installed-only

The `pages/` split (`app/pages/{sector,speed,destructive}.rs`) also picked up
edits removing the old `cfg!(not(windows))` "requires Windows" walls. Those
should be no-ops on Windows; confirm the pages render unchanged.

---

## 5. Linux-only — safe to ignore

`cfg`-gated out of the Windows build entirely: `service.rs`,
`service_control.rs`, `elevation.rs`, `device_events.rs`, `compositor_focus.rs`,
`single_instance.rs` (unix arm), `tray/linux.rs`, `autostart/linux.rs`,
`drive_enumeration/linux.rs`, `smart_reader/linux.rs`,
`{surface,destructive}_test/linux.rs`, the `linux/` directory, and the
arch-matching half of `update.rs` (`pick_exe_url` is untouched).

The root monitoring service, the systemd units, polkit/pkexec and the
StatusNotifierItem tray have no Windows equivalent and no Windows code path.

I added `#[cfg_attr(windows, allow(dead_code))]` / `#[cfg_attr(not(target_os =
"linux"), allow(dead_code))]` where a Linux-only helper would otherwise trip
`dead_code` on Windows (`update.rs::ARCH_ALIASES`,
`history_db::apply_schema_for_tests`). If clippy still finds dead code on
Windows, that is a miss on my part, not a design decision.

---

## 6. Known issues opened this branch

Full text in `docs/known-issues.md`.

- **KI-43** *(fixed)* — O(n²) damage merge. **Cross-platform**; see §1.
- **KI-44** *(fixed)* — resize crash from replayed damage. **Cross-platform**;
  see §1.
- **KI-42** *(fixed, diagnosis was wrong)* — kept because both fixes are real
  event-loop blocks, but neither was the reported freeze. The `dbus-send` half
  is Linux-only; the `buffer_mut()`-before-damage-check half is shared and is a
  genuine improvement on both.
- **KI-45, KI-46, KI-47** — Linux-only (tray watcher race, cross-arch update
  refusal, service/tray state rules).
- **KI-32…KI-39** — Linux parity gaps and port bugs.

## 7. Not verified anywhere

Being explicit so you do not inherit false confidence:

- **Windows: nothing.** Not compiled, not run.
- **ARM64 Linux:** cross-builds clean and produces a correct `aarch64` ELF, but
  has never been *executed* — no ARM hardware here.
- **Damage tracking under interactive input** (hover, scroll, dropdowns,
  modals): I could not drive input on Wayland. Unverified on both platforms.
  This is what `DISKORIA_DAMAGE_VERIFY=1` is for.
- **Linux, still open:** monitoring can run with no tray icon if the system
  service is started outside the app (`systemctl start diskoria-monitor`) while
  the tray unit is stopped. Every app- and installer-driven path is covered;
  that one is not. Closing it needs the root service to reach into the user's
  graphical session, which was left as a deliberate open decision. No Windows
  impact.
