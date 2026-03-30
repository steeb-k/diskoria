# TODO — Diskoria (Rust / egui)

Informal backlog. Priorities and designs can change.

---

## Enhanced SMART status / monitoring

Wishlist for clearer HDD/SSD health visibility beyond a simple pass/fail:

- **Readable attribute table** — Show standard SMART IDs with names, raw vs normalized values, and which attributes are “critical” for that device class (reallocated sectors, pending sectors, uncorrectable errors, etc.).
- **NVMe + ATA in one story** — Present health from both paths (SMART for SATA/ATA, log pages / ID namespace for NVMe) with labels that match what users see in vendor tools.
- **Temperature and wear cues** — Surface drive temperature, power-on hours, and SSD wear / lifetime indicators where the drive exposes them.
- **Self-test status** — Show last short/long SMART self-test result and optionally queue or remind about running periodic checks.
- **Trend or history (lightweight)** — Optional logging of key counters over time so gradual issues (slow climb in reallocated sectors) are easier to notice than a single snapshot.

---

## Better reports

Exportable, timestamped summaries of test runs so results can be shared or filed (IT documentation, RMA, resale checks):

- **Structured export** — HTML, PDF, and/or CSV with disk identity, test type, parameters, duration, and outcome counts (good / bad / slow sectors, speed-test numbers as applicable).
- **Heatmap in the report** — Embed or attach the sector-map visualization for the run, not only numeric totals.
- **Machine context (optional)** — OS build, host name, or other metadata users expect on a “machine report” (keep privacy in mind; make fields opt-in if added).

---

## Sector test speed optimization

**Preliminary work (done):** The surface / sector scan engine now uses **larger sequential read chunks** (1 MiB instead of 64 KiB in `diskoria/src/surface_test.rs`), cutting syscall overhead on the hot path. In practice this has produced **much faster full-disk scans on NVMe** versus the old chunk size. Cancel checks on the worker use `Relaxed` atomics (minor cleanup).

**Original observation:** The scan had felt **slower than typical dedicated drive testers** (vendor tools, mature open-source scanners, etc.). The chunk-size change addresses a large part of that for fast media; further gains may still exist.

**Further optimizations (backlog — not started unless noted):**

- **Overlapped I/O / queue depth:** Today the engine uses one **synchronous** `ReadFile` at a time (`OVERLAPPED` is null). NVMe benefits from multiple commands in flight; implementing **overlapped reads** (e.g. `FILE_FLAG_OVERLAPPED`, several buffers, `GetOverlappedResult` / completion ports) could raise throughput further at the cost of a **larger, riskier** refactor (ordering, cancellation, error paths, bad-sector drill-down must stay correct).

- **Read chunk size experiments:** 1 MiB is a good default; **2–4 MiB** might squeeze a bit more on very fast NVMe (diminishing returns; larger failed chunks mean more per-sector retries in `handle_read_error`). **Device-specific** caps (USB HDD vs NVMe) are optional if profiling shows a win.

- **UI / repaint cadence:** While a test runs, the app calls `ctx.request_repaint()` **every frame** (`any_test_running()`), so the GPU/UI cost is continuous even when the sector map only changes on ~1000 progress steps. Consider **repainting only when** progress messages arrive (or at a capped FPS) if profiling shows UI as non-trivial on weak GPUs.

- **Logging (small polish):** The disk **worker** does not log inside the read loop. On the UI side, `poll_surface_test` emits `log::debug!` whenever a non-empty batch arrives; with default `diskoria=debug` that can mean many lines over a run. Lowering the default filter (e.g. `diskoria=warn`), gating that line behind `trace`, or dropping it is **minor** vs disk I/O but reduces console noise and a bit of UI-thread work when a console is attached.

- **Comparability / targets:** Benchmark Diskoria vs one or two external tools on the **same** disk; record throughput (MB/s) or wall time for a fixed range so future changes don’t regress without notice.

**Goal:** Match or approach “good enough” industry-adjacent throughput on NVMe/SATA/USB where the hardware allows, without sacrificing correctness of bad/slow sector reporting.

---

## Possible Pro Features

_Ideas that could justify a paid tier without hiding safety-critical diagnostics from the free app._

- **CLI / automation** — Headless or scripted runs (sector scan, speed test, etc.), non-zero exit codes on failure thresholds, write report to a path. Targets labs and sysadmins who need unattended workflows.
- **History and compare** — Persist SMART (and optionally scan outcome) snapshots over time; show trends and “since last run” deltas. Free stays at a current snapshot; Pro adds the longitudinal view.
- **Professional / branded reporting** — Customizable or branded PDF/HTML (logo, cover fields) aimed at MSPs and resale documentation; can build on **Better reports** above.
- **Alerting** — Notify when SMART crosses user-defined thresholds (e.g. tray, email, webhook). Free shows status; Pro could own “tell me when it breaks.”
- **Batch / fleet workflow** — Queue multiple physical disks or save/load test profiles (thresholds, which tests) for repeat use.
- **Support / updates** — Priority support and/or a guaranteed update channel, if you want a business tier without splitting features.

---

## Other (scratch)

_Add items as they come up._
