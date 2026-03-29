# Rust UI conversion — JF Storage Tester

This document describes how the **egui / Rust** shell (`jf-storage-tester/`) relates to the legacy **WPF** app (`_original/`), how to set up a Windows development environment, how versioning interacts with the **Check for updates** flow, and where to find release helper scripts.

---

## Summary of the Rust direction

- **New UI stack:** [eframe](https://github.com/emilk/egui) + [egui](https://github.com/emilk/egui) (Glow), Windows-focused, styled to align with the **Windows 11–style egui** patterns (Inter, Bootstrap Icons PUA, card layout).
- **Crate location:** `jf-storage-tester/` at the repository root (alongside `_original/`).
- **Assets:** Window/branding images such as `applogo.png` and `appicon.png` live at the **repo root**; the crate loads them via `include_bytes!(...)` paths relative to `jf-storage-tester/src/`.
- **Platform:** Drive enumeration, surface test, speed test, SMART, and WMI-heavy code paths are `**Yes,`** 
- **Updates:** GitHub Releases API targets the same repo as the WPF app (`jjfbno/JF-Storage-Tester`). Download/apply logic lives in `jf-storage-tester/src/update.rs`; the About page triggers checks from `jf-storage-tester/src/about.rs` and `app.rs`.

---

## Windows development environment

Install the following on a clean Windows 10/11 machine (x64).

### 1. Git

- [Git for Windows](https://git-scm.com/download/win) — default options are fine; ensure `git` is on `PATH`.

### 2. Rust toolchain (MSVC)

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) or a full Visual Studio edition with the workload **“Desktop development with C++”** (MSVC, Windows SDK).
2. Install Rust via [rustup](https://rustup.rs/) — choose the **x86_64-pc-windows-msvc** default host triple.
3. Confirm in PowerShell:
  ```powershell
   rustc -V
   cargo -V
  ```
4. Optional: match CI or release builds explicitly:
  ```powershell
   rustup target add x86_64-pc-windows-msvc
  ```

### 3. Clone and run (debug)

From the repo root:

```powershell
.\run-dev.ps1
```

This runs `cargo run` inside `jf-storage-tester/`. You can set `RUST_LOG` for logging (the script sets a default if unset).

### 4. Release-style binary (local folder)

- **Portable output** (copies exe to `portable-build/`):
  ```powershell
  .\build-portable.ps1
  ```
- **Versioned release folder** (see scripts below):
  ```powershell
  .\build-release.ps1
  ```

### 5. Open / edit

- Any editor works; **Rust Analyzer** in VS Code / Cursor is recommended for `Cargo.toml`, `rustfmt`, and `cargo check` integration.

---

## Helper scripts (adapted from copynaut-rust)


| Script               | Purpose                                                                                                                                                                                                         |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `run-dev.ps1`        | Fast iteration: `cargo run` in `jf-storage-tester/`.                                                                                                                                                            |
| `build-portable.ps1` | `cargo build --release` and copy `jf-storage-tester.exe` to `portable-build/`.                                                                                                                                  |
| `set-version.ps1`    | Interactive bump of `**[package].version`** in `jf-storage-tester/Cargo.toml` only (prompts for new version, validates via `cargo metadata`).                                                                   |
| `build-release.ps1`  | Reads version from Cargo metadata, `cargo build --release`, copies exe to `**releases\<version>\jf-storage-tester.exe**`. **No code-signing step** (unlike the copynaut script’s optional Azure signing block). |


Run `set-version.ps1` and `build-release.ps1` from the **repository root** (same level as `jf-storage-tester/`).

---

## GitHub releases and semantic versioning (updater)

### Why “Unrecognized release tag: V1.0.4.2” appears

The Rust updater (`jf-storage-tester/src/update.rs`) compares the current app version to the **latest GitHub release** using the `[semver](https://crates.io/crates/semver)` crate:

- Release `**tag_name`** is normalized (leading `v` / `V` stripped) and parsed with `**Version::parse**`.
- **SemVer** expects forms like `**1.0.5`** or `**0.2.0-beta.1**`. Tags such as `**V1.0.4.2**` use **four numeric segments** (`1.0.4.2`), which **do not parse as standard SemVer** in this pipeline, so the check fails with an “unrecognized release tag” style error.

The legacy WPF `UpdateService` used a **custom numeric split** on `tag_name` and padded segment counts, so it was more tolerant of ad hoc tagging.

### Recommended policy for this rebuild

1. **Publish GitHub releases** with tags in **SemVer** form, e.g. `**v1.0.5`**, `**v2.0.0**`, optionally with pre-release labels: `**v1.1.0-beta.1**`.
2. Keep `**jf-storage-tester/Cargo.toml**` `version = "…"` aligned with what you ship (same `MAJOR.MINOR.PATCH` as the tag, ignoring the leading `v` on GitHub).
3. Attach `**Setup*.exe**` (Inno installer) when possible; the updater prefers assets whose names suggest a **Setup** installer, then falls back to other `**.exe`** assets (see `pick_exe_url` in `update.rs`).

This is a deliberate move to **one clear versioning scheme** while the UI is being rebuilt: **Cargo + SemVer + matching Git tags**.

---

## Feature parity: `_original/` (WPF) vs `jf-storage-tester/` (Rust)

High-level comparison. “**Parity**” means comparable behavior for typical IT workflows; UI layout differs (sidebar pages vs header tabs).


| Feature                                                                 | WPF (`_original/`)             | Rust (`jf-storage-tester/`)                       | Notes                                                                                                                              |
| ----------------------------------------------------------------------- | ------------------------------ | ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Physical disk list (WMI)                                                | Yes (`DriveDetectionService`)  | Yes (`drive_enumeration.rs`)                      | Rust uses sidebar + per-page combo; WPF uses global header combo.                                                                  |
| Media / bus / size chips                                                | Drive template in UI           | Yes (chips on Sector/Speed)                       |                                                                                                                                    |
| Sector / surface test (raw read, progress, map)                         | Yes                            | Yes (`surface_test.rs`, `app.rs`)                 |                                                                                                                                    |
| Heat map / slow threshold                                               | Yes                            | Yes                                               |                                                                                                                                    |
| Speed test (seq + 4K, MB/s)                                             | Yes                            | Yes (`speed_test.rs`)                             |                                                                                                                                    |
| BitLocker-aware speed test                                              | Yes (locks write path)         | Yes (partition metadata)                          |                                                                                                                                    |
| **SMART / health status**                                               | No                             | Yes (`smart_health.rs`)                           | **Rust-only enhancement.**                                                                                                         |
| **Open Disk Management** (`diskmgmt.msc`)                               | Yes                            | No                                                | **Missing** — **high value** for same audience as original.                                                                        |
| **Open DiskPart** (`cmd /k diskpart`)                                   | Yes                            | No                                                | **Missing** — **high value**, matches IT workflow.                                                                                 |
| Global drive info bar (capacity, partitions, FS, scheme, **usage bar**) | Yes                            | Partial                                           | Rust shows drive/partition context in cards; **no single usage progress bar** like WPF. **Medium value** for at-a-glance fullness. |
| **Include write tests** toggle (read-only benchmark)                    | Yes (`IncludeWriteTests`)      | No (writes always part of flow where implemented) | **Missing** — **medium value** for cautious users / read-only policy.                                                              |
| Theme: dark / light                                                     | Yes                            | Yes (`app_settings.rs`, `theme.rs`)               |                                                                                                                                    |
| Follow system theme                                                     | Yes                            | Yes                                               |                                                                                                                                    |
| Accent: palette / custom / Windows                                      | N/A (accent from theme XAML)   | Yes                                               | Rust has richer accent options.                                                                                                    |
| Settings UI                                                             | Modal window                   | In-app Settings page                              | Different UX; same role.                                                                                                           |
| About + version + link                                                  | In Settings                    | Dedicated About page + `appicon.png`              |                                                                                                                                    |
| Check for updates / download                                            | Settings (`UpdateService`)     | About + `update.rs`                               | SemVer tagging required (see above).                                                                                               |
| Update download **progress %** in UI                                    | Yes (progress bar in Settings) | Busy overlay text only                            | **Partial** — **medium value** for large installers.                                                                               |
| Single-instance mutex                                                   | Yes (`App.xaml.cs`)            | No                                                | **Missing** — **medium value** for installer/upgrades; egui can add named mutex if needed.                                         |
| Run as **Administrator** (manifest)                                     | Yes                            | Not enforced in Rust by default                   | **Gap** for raw disk parity — product decision.                                                                                    |
| Inno `**Setup*.exe`**-based update                                      | Yes                            | Picks Setup exe or portable replace               | Align asset names on releases.                                                                                                     |


### Suggested priority for missing items


| Gap                                    | Suggested priority | Rationale                                                            |
| -------------------------------------- | ------------------ | -------------------------------------------------------------------- |
| Disk Management + DiskPart shortcuts   | **High**           | Same differentiators as original; small Win32 shell-execute surface. |
| SemVer GitHub tags + Cargo version     | **High**           | Unblocks updater; zero UI work.                                      |
| Admin elevation / mutex                | **Medium–High**    | Matches shipping WPF assumptions; elevation especially for raw I/O.  |
| Usage / fullness bar for selected disk | **Medium**         | Nice parity with header bar; not blocking core tests.                |
| Write-test toggle                      | **Medium**         | Safety / policy for some users.                                      |
| Update download progress bar           | **Medium**         | UX polish on slow networks.                                          |


---

## Related docs

- `AGENTS.md` — agent-oriented architecture notes for this repo.
- `_original/installer/` — Inno Setup script for the WPF build (paths and version defines).

