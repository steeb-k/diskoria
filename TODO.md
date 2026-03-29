# TODO — JF Storage Tester (Rust / egui)

Informal backlog. Priorities and designs can change.

---

## Destructive test (sector read + write)

**Idea:** Add a test mode that exercises sectors with **both reads and writes** (unlike the current surface test, which is read-only). This would catch a wider class of media/controller issues at the cost of **destroying all data** on the target device.

**UX / safety (high level — not a spec):**

- Treat this as a **separate flow** from the normal Sector Test, not a hidden toggle.
- Before the user can start, show a **blocker / gate page** (or equivalent modal-first step) that states clearly that **any drive selected for destructive testing will be wiped** (full-disk overwrite semantics — exact scope TBD when implemented).
- Require **explicit confirmation** (e.g. typed acknowledgement, checkbox + Continue, or both) so this cannot be started accidentally.
- Consider: admin elevation, physical disk vs partition targeting, and alignment with how the read-only surface test selects devices — details deferred.

**Implementation:** Not planned here; capture requirements and UI copy when picking up the work.

---

## Sector test speed optimization

**Preliminary work (done):** The surface / sector scan engine now uses **larger sequential read chunks** (1 MiB instead of 64 KiB in `jf-storage-tester/src/surface_test.rs`), cutting syscall overhead on the hot path. In practice this has produced **much faster full-disk scans on NVMe** versus the old chunk size. Cancel checks on the worker use `Relaxed` atomics (minor cleanup).

**Original observation:** The scan had felt **slower than typical dedicated drive testers** (vendor tools, mature open-source scanners, etc.). The chunk-size change addresses a large part of that for fast media; further gains may still exist.

**Further optimizations (backlog — not started unless noted):**

- **Overlapped I/O / queue depth:** Today the engine uses one **synchronous** `ReadFile` at a time (`OVERLAPPED` is null). NVMe benefits from multiple commands in flight; implementing **overlapped reads** (e.g. `FILE_FLAG_OVERLAPPED`, several buffers, `GetOverlappedResult` / completion ports) could raise throughput further at the cost of a **larger, riskier** refactor (ordering, cancellation, error paths, bad-sector drill-down must stay correct).

- **Read chunk size experiments:** 1 MiB is a good default; **2–4 MiB** might squeeze a bit more on very fast NVMe (diminishing returns; larger failed chunks mean more per-sector retries in `handle_read_error`). **Device-specific** caps (USB HDD vs NVMe) are optional if profiling shows a win.

- **UI / repaint cadence:** While a test runs, the app calls `ctx.request_repaint()` **every frame** (`any_test_running()`), so the GPU/UI cost is continuous even when the sector map only changes on ~1000 progress steps. Consider **repainting only when** progress messages arrive (or at a capped FPS) if profiling shows UI as non-trivial on weak GPUs.

- **Logging (small polish):** The disk **worker** does not log inside the read loop. On the UI side, `poll_surface_test` emits `log::debug!` whenever a non-empty batch arrives; with default `jf_surface=debug` that can mean many lines over a run. Lowering the default filter (e.g. `jf_surface=warn`), gating that line behind `trace`, or dropping it is **minor** vs disk I/O but reduces console noise and a bit of UI-thread work when a console is attached.

- **Parity with WPF:** `_original/Services/SurfaceTestService.cs` still uses **64 KiB** chunks; bumping it to **1 MiB** (same alignment rules as Rust) would align legacy and Rust behavior and performance if the WPF build remains in use.

- **Comparability / targets:** Benchmark Rust vs `_original` WPF and vs one or two external tools on the **same** disk; record throughput (MB/s) or wall time for a fixed range so future changes don’t regress without notice.

**Goal:** Match or approach “good enough” industry-adjacent throughput on NVMe/SATA/USB where the hardware allows, without sacrificing correctness of bad/slow sector reporting.

---

## Other (scratch)

_Add items as they come up._
