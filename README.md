# Diskoria

Windows and Linux utility for **storage health visibility**, **non-destructive sector scanning**, optional **full-disk verification writes**, and **volume speed benchmarks**. It is a single portable executable with a custom **egui** UI rendered on the CPU (**softbuffer**), so it does not depend on GPU drivers or OpenGL.

**See the [Diskoria wiki](https://github.com/steeb-k/diskoria/wiki) for full usage documentation**, including a walkthrough of every page, how to read SMART attributes and drive health, background monitoring and alerts, a glossary, and answers to common questions.

## Requirements

**Windows**

- **Windows 10 or later** (64-bit).
- **Run as Administrator.** The app manifest requests elevation; raw physical disk access and some volume operations fail without it.
- **WMI** is used for disk enumeration and SMART-style health. Minimal or custom environments (e.g. some Windows PE builds) may have reduced data; drive-letter mapping includes a Win32 **storage device number** fallback when WMI omits associations.

**Linux**

- **x86-64 or ARM64.** There is no universal binary; each architecture has its own build, and the updater will not offer one machine a binary built for the other.
- **polkit** for privileges. Diskoria relaunches itself through `pkexec` when it needs raw device access, and degrades rather than exits if you decline.
- **systemd** only for the optional background health collector — SMART needs root, and a desktop session should not have it, so collection runs as a separate service that unprivileged sessions read from.
- On stock **GNOME**, the tray icon needs an AppIndicator extension.

## Install on Linux

```sh
curl -fsSL https://raw.githubusercontent.com/steeb-k/diskoria/main/scripts/install-linux.sh | bash
```

Detects your architecture, installs the matching build to `/usr/local/bin`, and registers the desktop entry and polkit policy. Re-run it to upgrade in place.

To pass options, add `-s --`:

```sh
# specific version, or a different install root
curl -fsSL .../install-linux.sh | bash -s -- --version 1.7.0
curl -fsSL .../install-linux.sh | bash -s -- --prefix "$HOME/.local"

# also install the background health collector (a root systemd service)
curl -fsSL .../install-linux.sh | bash -s -- --with-service
```

Background monitoring is **not** installed by default: it is a system-wide unit running as root, which should be your decision rather than a side effect of installing the app. See [`linux/README.md`](linux/README.md) for what it installs and how to remove it.

If you would rather not pipe a script to a shell, the same releases carry a `diskoria-<ver>-portable-linux-<arch>.tar.gz` you can unpack yourself; it contains the binary, the desktop entry, the polkit policy, the systemd units and `install-service.sh`.

## Getting started

1. Launch **Diskoria** elevated (right-click → Run as administrator if Windows does not prompt automatically).
2. Wait for the **physical disk** list to populate (background query).
3. Use the sidebar to open **Drive Health**, **Sector Read Test**, **Sector Write Test**, **Benchmark**, **About**, or **Settings**.

**Development build** (from repository root):

```powershell
.\scripts\run-dev.ps1
```

This runs `cargo run` inside the `diskoria` crate. If elevation is required and `cargo run` cannot obtain it, build then start the binary with elevation:

```powershell
cd diskoria
cargo build
Start-Process .\target\debug\diskoria.exe -Verb RunAs
```

**Release builds:** see `scripts\build-portable.ps1` and `scripts\build-release.ps1`.

## What each page does

| Page | Target | Access | Data risk |
| ---- | ------ | ------ | --------- |
| Drive Health | Physical disk | Read-only telemetry | None |
| Sector Read Test | Physical disk | Read-only | None |
| Sector Write Test | Physical disk | Writes and verifies every sector | **Complete data loss** on that disk |
| Benchmark | Volume (temp file) | Read/write a temporary file | Only the temp file; needs free space |

- **Drive Health** — SMART, NVMe and UFS telemetry for the selected disk, with a temperature history chart and an exportable HTML report.
- **Sector Read Test** — reads the disk from start to end and marks bad and slow spans on a sector map and a performance chart. Does not modify disk contents.
- **Sector Write Test** — writes a pattern to every sector and reads it back to verify. All volumes on the target disk are locked and dismounted first. **This erases the disk and there is no undo.**
- **Benchmark** — sequential and random 4 KiB read/write throughput against a temporary file on the chosen volume. The workload profile is selected from the drive's bus and media type, so a USB stick is not given the same profile as an NVMe drive.

Background monitoring, tray alerts and the settings are covered in the wiki.
Settings are stored in `%ProgramData%\Diskoria\settings.txt`.

**Full documentation for every page — including how to read the sector map's
colouring and the SMART attribute table — is in the
[wiki](https://github.com/steeb-k/diskoria/wiki).**

## Updating from GitHub Releases

The app can compare its version to the latest release on a separate **binaries** repository. Configure `UPDATES_OWNER` and `UPDATES_REPO` in `diskoria/src/github_config.rs`. That repo should stay reachable without authentication for the check to succeed.

## License

See repository licensing files if present; third-party assets (fonts, Bootstrap Icons) retain their respective licenses.