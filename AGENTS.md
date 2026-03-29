# Agent guidance — Diskoria

## What this is

**Diskoria** is a Windows desktop utility for **storage surface scanning** (read entire physical disk) and **speed testing** (read/write benchmarks on a selected partition). It targets IT-style workflows: physical disk list, Disk Management / DiskPart shortcuts, BitLocker awareness, and update checks against GitHub Releases.

## Repository layout

| Path | Role |
|------|------|
| **`diskoria/`** | Rust app: `eframe` + **egui** (glow), Windows 11–style shell. |
| Repo root | `applogo.png`, `appicon.ico` — loaded via paths relative to the crate. |
| **`diskoria/src/github_config.rs`** | **Edit once:** `UPDATES_OWNER` (your GitHub user or org) and `UPDATES_REPO` (default `diskoria-binaries`). Drives the in-app updater API URL and the About page releases link. |

## GitHub: two repos

- **`diskoria`** (this source tree): typically **private** — application code.
- **`diskoria-binaries`**: **Releases** with tagged builds (e.g. `v0.1.0`) and **`diskoria.exe`** assets. The app calls `.../releases/latest` on this repo. Keep it **public** (or otherwise readable without auth) so the updater works without a PAT.

Publish flow: bump `diskoria/Cargo.toml` version → `.\build-release.ps1` → attach the built exe to a **Release on `diskoria-binaries`** with a matching `v*` tag.

## Build / run

- From repo root: `.\run-dev.ps1` → `cargo run` in `diskoria/`.
- Portable output: `.\build-portable.ps1` → `portable-build\diskoria.exe`.
- Versioned release folder: `.\build-release.ps1` → `releases\<version>\diskoria.exe`.
- Interactive version bump: `.\set-version.ps1`.

`cargo build` output lives under `diskoria/target/` (gitignored).

## Windows behavior

- **Administrator elevation** — `diskoria/app.manifest` requests `requireAdministrator` for raw disk access and related operations.
- **Drive list** — WMI (`diskoria/src/drive_enumeration.rs`), off the UI thread. Non-Windows builds show an error instead of listing disks.
- **Theme / settings** — persisted under `ProgramData\Diskoria\settings.txt` (`app_settings.rs`).

## Style reference

UI patterns may follow an external **rust-egui-winui-example** template on the maintainer machine (Inter + Bootstrap Icons, etc.); do not assume that folder lives inside this repo.

## Updates (code)

- Check/download: `diskoria/src/update.rs`.
- About + “Check for updates”: `about.rs`, `app.rs`.
