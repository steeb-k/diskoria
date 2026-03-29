# Agent guidance — Diskoria

## What this is

**Diskoria** is a Windows desktop utility for **read-only sector scanning** of physical disks, optional **destructive write+verify** of an entire disk, **file-based speed benchmarks** on a chosen volume, and **SMART / storage health** hints via WMI. It targets IT-style workflows: physical disk list, Disk Management / DiskPart shortcuts, BitLocker awareness, and update checks against GitHub Releases.

Rendering stack: **winit** + **egui** + **softbuffer** (CPU rasterizer, no OpenGL). Non-Windows builds compile with limited functionality (no disk list or tests).

## Repository layout

| Path | Role |
|------|------|
| **`diskoria/`** | Rust crate root: `Cargo.toml`, `build.rs`, `app.manifest`, `appicon.ico` (PE + window icon via `winresource` / `lib.rs`). |
| **`diskoria/src/`** | Application code: `lib.rs` (event loop, rasterizer), `app.rs` (shell + all pages), `surface_test.rs`, `destructive_test.rs`, `speed_test.rs`, `drive_enumeration.rs`, `smart_health.rs`, `update.rs`, `about.rs`, etc. |
| **Repo root** | `applogo.png`, `appicon.png` — embedded at compile time from `app.rs` (`include_bytes!("../../…")`) for sidebar logo and About card. Keep these in sync with branding; **`diskoria/appicon.ico`** must match the shipped OS icon. |
| **`diskoria/src/github_config.rs`** | **Edit once:** `UPDATES_OWNER` and `UPDATES_REPO` (default `diskoria-binaries`). Drives the in-app updater API URL and the About page releases link. |

## UI navigation (`active_nav`)

Sidebar order (indices 0–4): **Sector Test**, **Destructive Test**, **Speed Test**, **About**, **Settings**. Alt mnemonics: e / d / p / a / s.

## GitHub: two repos

- **`diskoria`** (this source tree): typically **private** — application code.
- **`diskoria-binaries`**: **Releases** with tagged builds (e.g. `v0.1.0`) and **`diskoria.exe`** assets. The app calls `.../releases/latest` on this repo. Keep it **public** (or otherwise readable without auth) so the updater works without a PAT.

Publish flow: bump `diskoria/Cargo.toml` version → `.\build-release.ps1` → attach the built exe to a **Release on `diskoria-binaries`** with a matching `v*` tag.

## Build / run

- From repo root: `.\run-dev.ps1` → `cargo run` in `diskoria/`. **Note:** the manifest requests elevation; if `cargo run` fails with “requires elevation,” run the built exe elevated or use an elevated shell.
- Portable output: `.\build-portable.ps1` → `portable-build\diskoria.exe`.
- Versioned release folder: `.\build-release.ps1` → `releases\<version>\diskoria.exe`.
- Interactive version bump: `.\set-version.ps1`.

`cargo build` output lives under `diskoria/target/` (gitignored).

## Windows behavior

- **Administrator elevation** — `diskoria/app.manifest` requests `requireAdministrator` for raw disk access and related operations.
- **Drive / volume list** — Primarily WMI in `diskoria/src/drive_enumeration.rs`, queried off the UI thread. Includes a **fallback** using `IOCTL_STORAGE_GET_DEVICE_NUMBER` (via `CreateFileW` on volume paths) when WMI does not associate drive letters reliably (e.g. some VHDx / Windows PE scenarios).
- **Theme / settings** — persisted under `%ProgramData%\Diskoria\settings.txt` (`app_settings.rs`).

## Test modules (where logic lives)

| Module | Purpose |
|--------|---------|
| `surface_test.rs` | Read-only sequential scan of `\\.\PhysicalDriveN`: geometry via `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`, 1 MiB aligned reads, sector-level good / bad / slow classification (slow threshold ~200 ms per read). UI maps progress to a fixed number of “blocks.” |
| `destructive_test.rs` | Full-disk deterministic pattern write + immediate read-back verify; locks and dismounts listed volume letters first. **Data destruction** — same progress type as surface test for the UI. |
| `speed_test.rs` | File on a selected volume: sequential write/read (multi-pass, size depends on media/bus profile), then random 4 KiB I/O; uses unbuffered / write-through flags on Windows. Temporary file name prefix `Diskoria_SpeedTest_`. |
| `smart_health.rs` | WMI-backed summary (`MSFT_PhysicalDisk`, failure-predict status) for health display on sector/destructive views. |

## Style reference

UI uses **Inter** + **Bootstrap Icons** (WOFF2 → TTF in `build.rs`), Windows 11–style undecorated window and manual tab focus patterns. There is no bundled `rust-egui-winui-example` subtree in this repo.

## Updates (code)

- Check/download: `diskoria/src/update.rs`.
- About + “Check for updates”: `about.rs`, `app.rs`.
