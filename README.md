# Diskoria

Windows utility for **storage health visibility**, **non-destructive sector scanning**, optional **full-disk verification writes**, and **volume speed benchmarks**. It is a single portable executable with a custom **egui** UI rendered on the CPU (**softbuffer**), so it does not depend on GPU drivers or OpenGL.

## Requirements

- **Windows 10 or later** (64-bit).
- **Run as Administrator.** The app manifest requests elevation; raw physical disk access and some volume operations fail without it.
- **WMI** is used for disk enumeration and SMART-style health. Minimal or custom environments (e.g. some Windows PE builds) may have reduced data; drive-letter mapping includes a Win32 **storage device number** fallback when WMI omits associations.

## Getting started

1. Launch **Diskoria** elevated (right-click → Run as administrator if Windows does not prompt automatically).
2. Wait for the **physical disk** list to populate (background query).
3. Use the sidebar to open **Sector Test**, **Destructive Test**, **Speed Test**, **About**, or **Settings**.

**Development build** (from repository root):

```powershell
.\run-dev.ps1
```

This runs `cargo run` inside the `diskoria` crate. If elevation is required and `cargo run` cannot obtain it, build then start the binary with elevation:

```powershell
cd diskoria
cargo build
Start-Process .\target\debug\diskoria.exe -Verb RunAs
```

**Release builds:** see `build-portable.ps1` and `build-release.ps1` at the repo root.

## Usage by page

### Sector Test (read-only)

- Select a **physical disk**.
- **Start** opens the disk with **read-only** access and reads it **sequentially** from start to end in aligned chunks (typically 1 MiB).
- The UI reports **good**, **bad**, and **slow** sectors. Reads slower than about **200 ms** are counted as slow; failed reads increment bad sectors and use follow-up logic to isolate problem ranges where possible.
- **Cancel** stops the worker; partial progress is shown.
- A **SMART / health** summary for the selected disk appears when WMI data is available.

This test does **not** modify disk contents. It is safe for diagnosing media and cabling issues on a live system, though heavy I/O may affect performance of volumes on that disk.

#### Sector map (heatmap)

The **Sector Map** tab (and the same grid on **Destructive Test**) is a fixed **1000-cell** grid. Cells are **not** individual disk sectors: each cell represents a **contiguous span of the disk’s capacity** in address order. As the worker advances, the cell index is

`floor((bytes_scanned_so_far / total_disk_bytes) × 1000)`,

clamped so it always lies in `0 … 999`. When the scan moves into a new cell (or completes), the worker finalizes the **previous** cell and sends:

- **`block_read_time_ms`** — the **average** wall-clock time, in milliseconds, of every successful 1 MiB read that fell in that cell’s span.
- **`block_is_good`** — `false` if **any** read in that span failed (after error handling / sector retries).

The UI then classifies that cell:

| Appearance | Meaning |
|------------|---------|
| **Dark gray** | Not scanned yet (or run not started). |
| **Red** | At least one read error in that span (“bad / error” on Sector Test; “bad / mismatch” on Destructive). |
| **Amber** | No errors, but the cell’s **average** read time is **≥ 200 ms** (slow threshold). |
| **Green shades** | No errors, average read **under 200 ms**. These are the “good (heat)” cells. |

**Light vs dark green:** Among **good** cells under the slow threshold, color is a **relative** heat map. The app tracks the **minimum and maximum** average read times seen so far in that category and maps each cell linearly between two RGB endpoints:

- **Fastest** observed good average → brighter green (approximately RGB 76, 175, 80).
- **Slowest** observed good average → darker green (approximately RGB 15, 50, 18).

If all good samples are effectively the same speed, the scale uses a small floor on the range (0.1 ms) so the color still stabilizes. Because the scale is **per-run** and based on data seen so far, the same absolute read time can look slightly different early vs late in a scan as min/max updates. **Yellow/amber** always means “average at or above 200 ms,” regardless of that green scale.

**Destructive Test** reuses the same grid and coloring: each cell’s timing is still an **average** over the I/O performed in that span, and the same 200 ms boundary and green min/max scaling apply (with separate min/max state from Sector Test).

### Destructive Test

- Same physical-disk selection metaphor as Sector Test, but the tool **writes a deterministic pattern to every sector** and **reads it back** to verify.
- All volumes on the target disk are **locked and dismounted** before writing so the filesystem cannot interfere; handles stay open for the duration of the run.
- **This erases all data on the selected disk.** Confirm prompts are intentional; there is no undo.

Use only on disks you intend to wipe or fully retest at the block level.

### Speed Test

- Chooses a **volume** (drive letter / mount point), not raw `PhysicalDrive`.
- Creates a **temporary file** on that volume (`Diskoria_SpeedTest_*.tmp`), then runs:
  1. **Sequential write** — multi-pass, 1 MiB blocks, unbuffered + write-through.
  2. **Sequential read** — same file, multi-pass.
  3. **Random 4 KiB writes** — many random offsets within the file.
  4. **Random 4 KiB reads** — same.
- **Workload size** (file size, 4K iteration count, pass count) depends on detected **bus** (e.g. NVMe vs USB) and **media** (SSD vs HDD vs removable), so USB sticks are not hammered with the same profile as a local NVMe drive.
- The temporary file is removed when the test finishes or is cancelled (best effort).

Requires free space roughly equal to the configured test file size on the chosen volume.

### About

- Version, home URL, **Ko-fi** support link, and **Check for updates** (GitHub Releases API — see `diskoria/src/github_config.rs` for the binaries repo).

### Settings

- Theme (auto / dark / light), accent (Windows, palette, custom hex). Stored under `%ProgramData%\Diskoria\settings.txt`.

## How the tests differ (summary)


| Test             | Target        | Access                 | Data risk                                       |
| ---------------- | ------------- | ---------------------- | ----------------------------------------------- |
| Sector Test      | Physical disk | Read-only              | None                                            |
| Destructive Test | Physical disk | Read/write all sectors | **Complete data loss** on that disk             |
| Speed Test       | Volume (file) | Read/write temp file   | Only the temp file; normal use needs free space |


## Updating from GitHub Releases

The app can compare its version to the latest release on a separate **binaries** repository. Configure `UPDATES_OWNER` and `UPDATES_REPO` in `diskoria/src/github_config.rs`. That repo should stay reachable without authentication for the check to succeed.

## License

See repository licensing files if present; third-party assets (fonts, Bootstrap Icons) retain their respective licenses.