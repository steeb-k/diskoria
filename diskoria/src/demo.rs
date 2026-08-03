//! Demo / capture mode: the `--page` and `--demo-*` command-line flags.
//!
//! These exist so the wiki's reference screenshots can be taken from a real
//! build without exposing the author's hardware — and, for the Sector Write
//! Test, without going anywhere near a real write.
//!
//! **Nothing in this module touches a disk.** Every flag seeds UI state from
//! the invented machine below; the flags that show a test "running" set the
//! progress fields directly and leave the worker channel `None`, so the
//! `poll_*` loops early-return and no I/O is ever started. `--demo-confirm`
//! exists precisely so the destructive-test confirmation dialog can be
//! captured without arming a real write.
//!
//! ```text
//! --page <drive-health|sector-read|sector-write|benchmark|about|settings>
//! --demo-drives     canned physical-disk list (no WMI)
//! --demo-drive <N>  select canned drive N (0 NVMe, 1 SATA warning, 2 USB)
//! --demo-health     canned SMART reports: healthy NVMe, warning SATA, no-SMART USB
//! --demo-progress   a test mid-run: progress, counters, elapsed/remaining
//! --demo-heatmap    a populated Sector Map with good/slow/bad cells
//! --demo-chart      a populated Performance Chart (selects that tab)
//! --demo-result     the PASS/WARN/FAIL result overlay
//! --demo-toast      fire a sample Windows toast
//! --demo-alert      monitoring having raised an alert
//! --demo-confirm    the confirmation modal for the current page
//! --demo-nav-open   hold the collapsed nav open — the rail's hover overlay at
//!                   rail widths, the full-window menu at phone widths
//! --demo-theme <dark|light>   force the theme, ignoring the OS and settings
//! --demo-accent <RRGGBB>      force the accent, ignoring the live Windows one
//! --demo-export <dir>         write the Export Log report for every canned
//!                             drive into <dir>, then exit without a window
//! ```
//!
//! `--demo-theme` and `--demo-accent` exist so a reference capture is
//! reproducible on a machine other than the author's. By default Diskoria reads
//! the accent live from DWM and the theme from the OS, which makes two captures
//! of the same page on two machines differ for reasons that have nothing to do
//! with the page. Neither flag writes to the settings file.
//!
//! Any `--demo-*` flag implies `--demo-drives` and `--demo-health`: a seeded
//! page needs a drive list to hang off, and once that list is invented its
//! device paths and PNP IDs no longer name anything the health readers may
//! open. Any flag that implies a *running or finished* write test also unlocks
//! the Sector Write Test gate page, so `--page sector-write` on its own still
//! shows the gate (which is itself worth capturing).

use std::sync::OnceLock;

use crate::detected_drive::{BusKind, DetectedDrive, MediaKind, PartitionTableStyle};
use crate::partition_info::{EncryptionStatus, PartitionInfo};
use crate::smart_reader::{ata_attribute, AtaSmartData, NvmeHealthData, SmartReport};
use crate::test_result_overlay::TestResult;

// ── Config ───────────────────────────────────────────────────────────────────

/// Sidebar nav index for a `--page` name. Mirrors `app::NAV_TOP` / `NAV_BOTTOM`.
pub const PAGE_DRIVE_HEALTH: usize = 0;
pub const PAGE_SECTOR_READ: usize = 1;
pub const PAGE_SECTOR_WRITE: usize = 2;
pub const PAGE_BENCHMARK: usize = 3;
pub const PAGE_ABOUT: usize = 4;
pub const PAGE_SETTINGS: usize = 5;

// Not `Copy`: `export_dir` is a String. Callers take `&'static DemoConfig`
// from `config()` rather than a copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DemoConfig {
    /// Sidebar nav index to open on, from `--page`.
    pub page: Option<usize>,
    /// Drive index to select, from `--demo-drive`. Without this each page picks
    /// its own sensible default — see `DiskoriaApp::apply_demo_seed`.
    pub drive: Option<usize>,
    pub drives: bool,
    pub health: bool,
    pub progress: bool,
    pub heatmap: bool,
    pub chart: bool,
    pub result: Option<TestResult>,
    pub toast: bool,
    pub alert: bool,
    pub confirm: bool,
    /// Hold the collapsed nav open: the rail's hover overlay in `NavMode::Rail`,
    /// the full-window menu in `NavMode::Mobile`. Both are pointer-driven and
    /// so cannot otherwise be captured — same reason `--demo-confirm` exists.
    pub nav_open: bool,
    /// `Some(true)` = force dark, `Some(false)` = force light.
    pub dark: Option<bool>,
    /// Accent as `(r, g, b)`, from `--demo-accent`.
    pub accent: Option<(u8, u8, u8)>,
    /// Directory to write Export Log reports into, from `--demo-export`. This
    /// is a headless path: it writes and exits without opening a window.
    pub export_dir: Option<String>,
}

impl DemoConfig {
    /// True when any demo flag is set. `--page` alone does not count: it only
    /// picks a starting page and is safe to use against real hardware.
    pub fn seeding(&self) -> bool {
        self.drive.is_some()
            || self.dark.is_some()
            || self.accent.is_some()
            || self.export_dir.is_some()
            || self.drives
            || self.health
            || self.progress
            || self.heatmap
            || self.chart
            || self.result.is_some()
            || self.toast
            || self.alert
            || self.confirm
            || self.nav_open
    }

    /// The Sector Write Test gate is bypassed when a flag implies the test has
    /// already been started — otherwise the gate page would hide the very thing
    /// the flag seeded.
    pub fn unlocks_destructive(&self) -> bool {
        self.progress || self.heatmap || self.chart || self.result.is_some() || self.confirm
    }
}

fn parse_page(name: &str) -> Option<usize> {
    match name {
        "drive-health" => Some(PAGE_DRIVE_HEALTH),
        "sector-read" => Some(PAGE_SECTOR_READ),
        "sector-write" => Some(PAGE_SECTOR_WRITE),
        "benchmark" => Some(PAGE_BENCHMARK),
        "about" => Some(PAGE_ABOUT),
        "settings" => Some(PAGE_SETTINGS),
        _ => None,
    }
}

/// `RRGGBB` or `#RRGGBB`.
fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ))
}

fn parse_result(name: &str) -> Option<TestResult> {
    match name {
        "pass" => Some(TestResult::Pass),
        "warn" => Some(TestResult::Warn),
        "fail" => Some(TestResult::Fail),
        _ => None,
    }
}

/// Parse `--page` / `--demo-*` out of an argument list. Both `--flag value` and
/// `--flag=value` spellings are accepted.
pub fn parse(args: impl IntoIterator<Item = String>) -> DemoConfig {
    let args: Vec<String> = args.into_iter().collect();
    let mut cfg = DemoConfig::default();

    // Value for `--flag`, taken from `--flag=value` or the next argument when
    // that argument is not itself a flag.
    let value_at = |i: usize, arg: &str| -> Option<String> {
        if let Some((_, v)) = arg.split_once('=') {
            return Some(v.to_string());
        }
        args.get(i + 1)
            .filter(|n| !n.starts_with('-'))
            .map(|n| n.to_string())
    };

    for (i, arg) in args.iter().enumerate() {
        let name = arg.split_once('=').map(|(n, _)| n).unwrap_or(arg);
        match name {
            "--page" => {
                if let Some(v) = value_at(i, arg) {
                    cfg.page = parse_page(&v);
                }
            }
            "--demo-drives" => cfg.drives = true,
            "--demo-drive" => {
                if let Some(v) = value_at(i, arg) {
                    cfg.drive = v.parse::<usize>().ok().filter(|n| *n < drives().len());
                }
                cfg.drives = true;
            }
            "--demo-health" => cfg.health = true,
            "--demo-progress" => cfg.progress = true,
            "--demo-heatmap" => cfg.heatmap = true,
            "--demo-chart" => cfg.chart = true,
            // A bare `--demo-result` shows PASS; a value picks the variant.
            "--demo-result" => {
                cfg.result = value_at(i, arg)
                    .and_then(|v| parse_result(&v))
                    .or(Some(TestResult::Pass));
            }
            "--demo-theme" => {
                cfg.dark = match value_at(i, arg).as_deref() {
                    Some("dark") => Some(true),
                    Some("light") => Some(false),
                    _ => None,
                };
            }
            "--demo-accent" => {
                cfg.accent = value_at(i, arg).as_deref().and_then(parse_hex_rgb);
            }
            "--demo-export" => cfg.export_dir = value_at(i, arg),
            "--demo-toast" => cfg.toast = true,
            "--demo-alert" => cfg.alert = true,
            "--demo-confirm" => cfg.confirm = true,
            "--demo-nav-open" => cfg.nav_open = true,
            _ => {}
        }
    }

    if cfg.seeding() {
        // A seeded page needs drives to hang off.
        cfg.drives = true;
        // ...and once the drive list is invented, health *must* come from
        // canned data too. The demo drives' device paths and PNP IDs name the
        // host's real disks; querying them would both fail loudly on the page
        // and reach real storage, which a capture run must never do.
        cfg.health = true;
    }
    cfg
}

static CONFIG: OnceLock<DemoConfig> = OnceLock::new();

/// The process-wide demo configuration, parsed from `std::env::args()` on first
/// use. Call [`init`] early so the parse is logged once at startup.
pub fn config() -> &'static DemoConfig {
    CONFIG.get_or_init(|| parse(std::env::args().skip(1)))
}

/// Parse and log the demo configuration. Idempotent.
pub fn init() {
    let cfg = config();
    if cfg.seeding() || cfg.page.is_some() {
        log::info!(target: "diskoria::demo", "demo mode active: {cfg:?}");
    }
    if cfg.seeding() {
        log::warn!(
            target: "diskoria::demo",
            "demo mode seeds the UI with invented data; no disk is read or written"
        );
    }
}

/// True when demo seeding is on — background work that would hit real hardware
/// or the network (SMART polling, the startup update check) is skipped.
pub fn seeding() -> bool {
    config().seeding()
}

// ── The invented machine ─────────────────────────────────────────────────────
//
// One fictional machine, reused across every wiki image so the whole wiki reads
// as a single session. No real model, serial, volume label or drive letter of
// the author's appears here. Sizes are the exact byte counts the real vendors
// would report, so the app's own formatter produces plausible strings.

const GB: i64 = 1024 * 1024 * 1024;

fn partition(
    letter: &str,
    name: &str,
    total: i64,
    free: i64,
    fs: &str,
    system: bool,
) -> PartitionInfo {
    PartitionInfo {
        mount_point: letter.to_string(),
        volume_name: name.to_string(),
        total_size: total,
        free_space: free,
        file_system: fs.to_string(),
        is_system_partition: system,
        encryption: EncryptionStatus::NotEncrypted,
    }
}

/// The canned physical-disk list used by `--demo-drives`.
///
/// Three drives, chosen so one page can show every health state the app can
/// report: an NVMe SSD (healthy), a SATA HDD (SMART warning), and a USB stick
/// (SMART not available over USB).
pub fn drives() -> Vec<DetectedDrive> {
    vec![
        DetectedDrive {
            disk_number: 0,
            device_id: r"\\.\PhysicalDrive0".to_string(),
            model: "Aurora NX-1000 1TB".to_string(),
            serial: "AX1K2407000183".to_string(),
            pnp_device_id: r"SCSI\DISK&VEN_NVME&PROD_AURORA_NX-1000\DEMO&0".to_string(),
            size_bytes: 1_000_204_886_016,
            media: MediaKind::Ssd,
            bus: BusKind::Nvme,
            summary: "Disk 0 — Aurora NX-1000 1TB (932 GB, NVMe SSD)".to_string(),
            partitions: vec![
                partition("C:", "Workstation", 930 * GB, 412 * GB, "NTFS", true),
            ],
            partition_style: PartitionTableStyle::Gpt,
        },
        DetectedDrive {
            disk_number: 1,
            device_id: r"\\.\PhysicalDrive1".to_string(),
            model: "Meridian ST2000-CX".to_string(),
            serial: "MDN7742318".to_string(),
            pnp_device_id: r"SCSI\DISK&VEN_ATA&PROD_MERIDIAN_ST2000-CX\DEMO&1".to_string(),
            size_bytes: 2_000_398_934_016,
            media: MediaKind::Hdd,
            bus: BusKind::Sata,
            summary: "Disk 1 — Meridian ST2000-CX (1.82 TB, SATA HDD)".to_string(),
            partitions: vec![
                partition("D:", "Archive", 1862 * GB, 704 * GB, "NTFS", false),
            ],
            partition_style: PartitionTableStyle::Gpt,
        },
        DetectedDrive {
            disk_number: 2,
            device_id: r"\\.\PhysicalDrive2".to_string(),
            model: "Corvid Pocket 32GB".to_string(),
            serial: "CVD0093145".to_string(),
            pnp_device_id: r"USBSTOR\DISK&VEN_CORVID&PROD_POCKET\DEMO&2".to_string(),
            size_bytes: 31_037_849_600,
            media: MediaKind::Flash,
            bus: BusKind::Usb,
            summary: "Disk 2 — Corvid Pocket 32GB (29 GB, USB Flash)".to_string(),
            partitions: vec![
                partition("E:", "FIELDKIT", 28 * GB, 21 * GB, "exFAT", false),
            ],
            partition_style: PartitionTableStyle::Mbr,
        },
    ]
}

/// The canned SMART report for a demo drive index, used by `--demo-health`.
///
/// Disk 0 is a healthy NVMe, disk 1 a SATA drive that has started reallocating
/// sectors (so the warning styling is visible), and disk 2 a USB stick, which
/// reports the same "not available over USB" text the real reader returns.
pub fn health_report(drive_index: usize) -> SmartReport {
    match drive_index {
        0 => SmartReport::Nvme(NvmeHealthData {
            temperature_c: 41,
            percentage_used: 4,
            available_spare_pct: 100,
            available_spare_threshold: 10,
            power_on_hours: 2_137,
            power_cycles: 412,
            // ~14.3 TB written, in 1000×512-byte units.
            data_units_written: 27_985_216,
            unsafe_shutdowns: 7,
            media_errors: 0,
            critical_warning: 0,
        }),
        1 => SmartReport::Ata(AtaSmartData {
            attributes: vec![
                //             id     cur worst thr  raw
                // Real Seagate/WD drives report a large packed *rate* here, not
                // a count — copied from an ST2000DM008 so the invented drive
                // exercises the same path (KI-27).
                ata_attribute(0x01, 118, 99, 6, 120_202_145),
                ata_attribute(0x03, 96, 96, 0, 0),
                ata_attribute(0x04, 100, 100, 20, 1_883),
                // The reason this drive reads as a warning: 24 sectors have
                // already been swapped out for spares.
                ata_attribute(0x05, 96, 96, 10, 24),
                ata_attribute(0x07, 84, 60, 30, 0),
                // Power-On Hours as a real drive packs it: 14,226 in the low 32
                // bits with vendor data above. Displays as 14,226 either way —
                // that equality is the KI-27 fix.
                ata_attribute(0x09, 84, 84, 0, 0xCEF0_0000_0000 | 14_226),
                // Threshold kept low deliberately: `compute_status` warns when
                // current is within 10 % of threshold, so a high threshold here
                // would paint a perfectly healthy attribute amber and muddy the
                // two warnings this drive is meant to show (KI-24).
                ata_attribute(0x0A, 100, 100, 20, 0),
                ata_attribute(0x0C, 99, 99, 20, 1_883),
                ata_attribute(0xBB, 100, 100, 0, 0),
                ata_attribute(0xBE, 62, 45, 0, 38),
                ata_attribute(0xC1, 99, 99, 0, 2_014),
                // Sectors the drive cannot read but has not given up on yet.
                ata_attribute(0xC5, 100, 100, 0, 8),
                ata_attribute(0xC6, 100, 100, 0, 0),
                ata_attribute(0xC7, 200, 200, 0, 0),
                ata_attribute(0xF1, 100, 253, 0, 38_442_119_680),
                ata_attribute(0xF2, 100, 253, 0, 71_205_336_064),
            ],
            power_on_hours: Some(14_226),
            power_cycles: Some(1_883),
            temperature_c: Some(38),
        }),
        _ => SmartReport::Unavailable {
            reason: "SMART is not available over USB connections.".to_string(),
        },
    }
}

// ── Seeded test state ────────────────────────────────────────────────────────

/// Where the invented drive's trouble lives, as a fraction of its capacity.
///
/// Kept well inside the seeded scan progress (`SCANNED` in `app.rs`) on purpose:
/// a defect band sitting on the leading edge of the scan reads as "the scan has
/// just hit trouble", whereas one with clean blocks after it reads as "this
/// drive has a bad region", which is what the heat map is for.
const ROUGH_PATCH: std::ops::Range<f64> = 0.42..0.468;

/// A deterministic block-latency profile for the Sector Map, in milliseconds.
///
/// `total` cells are produced; `scanned` of them carry a result and the rest
/// stay pending. The pattern is a gentle ramp with a rough patch at
/// [`ROUGH_PATCH`] — the shape of an ageing drive. `None` means the block
/// failed to read.
///
/// Deterministic by construction (no clock, no RNG) so two captures of the same
/// flags produce the same picture.
pub fn block_latencies(total: usize, scanned: usize) -> Vec<Option<f64>> {
    let scanned = scanned.min(total);
    (0..scanned)
        .map(|i| {
            let pos = i as f64 / total as f64;
            // Cheap deterministic jitter — a hash, not an RNG.
            let n = (i as u64).wrapping_mul(6_364_136_223_846_793_005).rotate_left(17);
            let jitter = (n % 1000) as f64 / 1000.0;

            // The rough patch: a narrow band where reads stall and a handful of
            // blocks fail outright.
            let in_rough = ROUGH_PATCH.contains(&pos);
            if in_rough && n.is_multiple_of(11) {
                return None; // bad block
            }
            if in_rough {
                return Some(180.0 + jitter * 420.0);
            }
            // Elsewhere: a gentle rise from the fast outer tracks inward, with
            // an occasional single slow block.
            let base = 6.0 + pos * 9.0 + jitter * 4.0;
            if n.is_multiple_of(149) {
                Some(230.0 + jitter * 60.0)
            } else {
                Some(base)
            }
        })
        .collect()
}

/// Deterministic `(position_gb, speed_mbps)` samples for the Performance Chart.
///
/// Modelled on a spinning disk: fast on the outer tracks, tailing off toward
/// the spindle, with the same rough patch the heat map shows.
pub fn chart_samples(total_gb: f64, scanned_frac: f64) -> Vec<[f64; 2]> {
    let points = 240;
    let last = (points as f64 * scanned_frac.clamp(0.0, 1.0)) as usize;
    (0..last)
        .map(|i| {
            let frac = i as f64 / points as f64;
            let n = (i as u64).wrapping_mul(2_862_933_555_777_941_757).rotate_left(23);
            let jitter = (n % 1000) as f64 / 1000.0;
            let base = 182.0 - frac * 74.0 + (jitter - 0.5) * 7.0;
            // The same bad region the heat map shows, seen as a speed trough —
            // the two visualisations are two views of one scan and must agree.
            let speed = if ROUGH_PATCH.contains(&frac) {
                base * 0.18
            } else {
                base
            };
            [frac * total_gb, speed.max(1.0)]
        })
        .collect()
}

/// A deterministic 7-day temperature series for the Drive Health page's history
/// chart, as `(unix_seconds, celsius)` ending at `now_unix`.
///
/// Seeded by `--demo-alert`, which means "monitoring has been running and has
/// raised an alert" — and monitoring that has been running has history. Without
/// it the chart can only ever be captured in its empty state.
///
/// A daily cycle plus a slow climb, crossing the 55 °C warning line near the end
/// so the alert in [`alert_event`] has something visible behind it.
#[cfg_attr(not(windows), allow(dead_code))] // consumer (monitor seeding) is Windows-gated today
pub fn temperature_history(now_unix: i64, warm: bool) -> Vec<[f64; 2]> {
    // One sample every 5 minutes for 7 days.
    let step = 300_i64;
    let n = 7 * 24 * 12;
    (0..n)
        .map(|i| {
            let ago = (n - 1 - i) as f64;
            let t = now_unix - (n - 1 - i) as i64 * step;
            // Position within a 24 h cycle, and how far through the week.
            let day = (ago * step as f64 / 86_400.0) * std::f64::consts::TAU;
            let week = 1.0 - ago / n as f64;
            let base = if warm { 44.0 } else { 33.0 };
            let climb = if warm { 11.0 } else { 2.0 };
            let c = base + climb * week + 3.5 * day.cos();
            [t as f64, (c * 10.0).round() / 10.0]
        })
        .collect()
}

/// The WMI predict-fail verdict (and NVMe/UFS health %) for a demo drive.
///
/// Note that disk 1 reads **Healthy** here while [`health_report`] warns about
/// its reallocated sectors. That is not an oversight: Windows' predict-fail bit
/// really does stay clear on a drive that has started swapping in spares, and
/// telling the two signals apart is the whole point of the Drive Health page.
#[cfg_attr(not(windows), allow(dead_code))] // consumer (poll_smart_health) is Windows-gated today
pub fn wmi_health(drive_index: usize) -> (crate::smart_health::SmartHealth, Option<u8>) {
    use crate::smart_health::SmartHealth;
    match drive_index {
        0 => (SmartHealth::Healthy, Some(96)),
        1 => (SmartHealth::Healthy, None),
        _ => (SmartHealth::Disabled, None),
    }
}

/// `--demo-export`: write the Export Log report for every canned drive, then
/// return `true` so the caller exits before opening a window.
///
/// This produces the *real* report — the same `build_report_html` the button
/// calls — from invented data, so the wiki can document what users actually
/// get rather than a drawing of it.
pub fn write_export_reports() -> bool {
    let Some(dir) = config().export_dir.as_deref() else {
        return false;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("--demo-export: cannot create {dir}: {e}");
        return true;
    }
    for (i, drive) in drives().iter().enumerate() {
        let report = health_report(i);
        let html = crate::smart_health_page::report_html_for_demo(drive, &report);
        let path = std::path::Path::new(dir)
            .join(format!("SMART-{}.html", drive.safe_filename_stem()));
        match std::fs::write(&path, html.as_bytes()) {
            Ok(()) => println!("wrote {}", path.display()),
            Err(e) => eprintln!("--demo-export: {}: {e}", path.display()),
        }
    }

    // ...and the COMBINED report, the one a sector test produces when you
    // answer "Include report" to the chart export. Different document: same
    // health tables with the performance chart embedded above them.
    let drive = &drives()[1];
    let report = health_report(1);
    let total_gb = drive.size_bytes as f64 / 1_073_741_824.0;
    let points = chart_samples(total_gb, 1.0);
    let max_speed = points.iter().fold(0.0_f64, |m, p| m.max(p[1]));
    // 1920x920 and dark, exactly as app.rs renders it for the report: 2x so the
    // lightbox has real detail to zoom into, dark to match the report CSS.
    match crate::app::render_performance_chart_png_bytes(
        1920,
        920,
        &format!("Disk {} \u{2014} {}", drive.disk_number, drive.model),
        "Sector Read Test",
        &points,
        &points,
        max_speed,
        total_gb,
        true,
    ) {
        Ok(png) => {
            let html =
                crate::smart_health_page::build_chart_report_html(drive, &report, &png);
            let path = std::path::Path::new(dir).join(format!(
                "Performance-{}.html",
                drive.safe_filename_stem()
            ));
            match std::fs::write(&path, html.as_bytes()) {
                Ok(()) => println!("wrote {}", path.display()),
                Err(e) => eprintln!("--demo-export: {}: {e}", path.display()),
            }
        }
        Err(e) => eprintln!("--demo-export: chart render failed: {e}"),
    }
    true
}

/// Title and body for the `--demo-toast` sample notification. Same shape the
/// alert path builds in `DiskoriaApp::poll_monitor`, so the screenshot matches
/// what monitoring actually sends.
#[cfg_attr(not(windows), allow(dead_code))] // consumer (--demo-toast path) is Windows-gated today
pub fn sample_toast() -> (String, String) {
    let alert = alert_event();
    (
        format!("Diskoria \u{2014} Drive {} (Warning)", alert.model),
        alert.detail,
    )
}

/// The alert `--demo-alert` raises: the warning-state SATA drive running hot.
#[cfg_attr(not(windows), allow(dead_code))] // consumer (--demo-alert path) is Windows-gated today
pub fn alert_event() -> crate::alert_engine::AlertEvent {
    let d = &drives()[1];
    crate::alert_engine::AlertEvent {
        serial: d.serial.clone(),
        model: d.model.clone(),
        condition: crate::alert_engine::AlertCondition::TemperatureWarning,
        level: crate::alert_engine::AlertLevel::Warning,
        detail: format!("{} — temperature reached 58\u{b0}C (threshold: 55\u{b0}C)", d.model),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> DemoConfig {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_flags_is_inert() {
        let c = cfg(&["--minimized"]);
        assert_eq!(c, DemoConfig::default());
        assert!(!c.seeding());
    }

    #[test]
    fn page_accepts_both_spellings() {
        assert_eq!(cfg(&["--page", "benchmark"]).page, Some(PAGE_BENCHMARK));
        assert_eq!(cfg(&["--page=benchmark"]).page, Some(PAGE_BENCHMARK));
        assert_eq!(cfg(&["--page", "nonsense"]).page, None);
        // `--page` alone selects a page but seeds nothing, so it is safe
        // against real hardware.
        assert!(!cfg(&["--page", "benchmark"]).seeding());
        assert!(!cfg(&["--page", "benchmark"]).drives);
    }

    #[test]
    fn demo_result_variants() {
        assert_eq!(cfg(&["--demo-result"]).result, Some(TestResult::Pass));
        assert_eq!(cfg(&["--demo-result", "fail"]).result, Some(TestResult::Fail));
        assert_eq!(cfg(&["--demo-result=warn"]).result, Some(TestResult::Warn));
        // An unrecognised value still shows the overlay rather than silently
        // doing nothing.
        assert_eq!(cfg(&["--demo-result", "bogus"]).result, Some(TestResult::Pass));
    }

    #[test]
    fn a_flag_taking_no_value_does_not_swallow_the_next_arg() {
        let c = cfg(&["--demo-heatmap", "--demo-chart"]);
        assert!(c.heatmap && c.chart);
    }

    #[test]
    fn any_demo_flag_implies_canned_drives_and_health() {
        for flags in [
            &["--demo-drives"][..],
            &["--demo-progress"][..],
            &["--demo-confirm"][..],
            &["--demo-toast"][..],
        ] {
            let c = cfg(flags);
            assert!(c.drives, "{flags:?} must not leave the real drive list in");
            assert!(c.health, "{flags:?} must not let a health reader open a real disk");
        }
    }

    #[test]
    fn only_started_test_flags_unlock_the_destructive_gate() {
        assert!(!cfg(&["--page", "sector-write"]).unlocks_destructive());
        assert!(!cfg(&["--demo-health"]).unlocks_destructive());
        assert!(cfg(&["--demo-progress"]).unlocks_destructive());
        assert!(cfg(&["--demo-confirm"]).unlocks_destructive());
        assert!(cfg(&["--demo-result", "fail"]).unlocks_destructive());
    }

    #[test]
    fn demo_drives_are_distinct_and_lockable() {
        let d = drives();
        assert_eq!(d.len(), 3);
        let keys: std::collections::HashSet<String> =
            d.iter().map(|dr| dr.lock_key()).collect();
        assert_eq!(keys.len(), d.len(), "demo drives must not share a lock key");
        for (i, dr) in d.iter().enumerate() {
            assert_eq!(dr.disk_number as usize, i);
            assert!(!dr.partitions.is_empty());
        }
    }

    #[test]
    fn demo_theme_and_accent_pin_the_capture() {
        assert_eq!(cfg(&["--demo-theme", "light"]).dark, Some(false));
        assert_eq!(cfg(&["--demo-theme=dark"]).dark, Some(true));
        assert_eq!(cfg(&["--demo-theme", "sideways"]).dark, None);
        assert_eq!(cfg(&["--demo-accent", "8E44AD"]).accent, Some((142, 68, 173)));
        assert_eq!(cfg(&["--demo-accent=#8E44AD"]).accent, Some((142, 68, 173)));
        // Both put the run in demo mode on their own, so a capture that pins
        // only the theme still cannot reach real hardware.
        assert!(cfg(&["--demo-theme", "light"]).seeding());
        assert!(cfg(&["--demo-accent", "8E44AD"]).drives);
    }

    #[test]
    fn hex_rgb_rejects_junk() {
        assert_eq!(parse_hex_rgb("8E44AD"), Some((142, 68, 173)));
        assert_eq!(parse_hex_rgb("#8e44ad"), Some((142, 68, 173)));
        assert_eq!(parse_hex_rgb("8E44A"), None);
        assert_eq!(parse_hex_rgb("8E44ADD"), None);
        assert_eq!(parse_hex_rgb("ZZZZZZ"), None);
        assert_eq!(parse_hex_rgb(""), None);
    }

    #[test]
    fn demo_drive_selects_and_rejects_out_of_range() {
        assert_eq!(cfg(&["--demo-drive", "1"]).drive, Some(1));
        assert_eq!(cfg(&["--demo-drive=2"]).drive, Some(2));
        assert_eq!(cfg(&["--demo-drive", "9"]).drive, None);
        assert_eq!(cfg(&["--demo-drive", "x"]).drive, None);
        // Even a rejected index still puts the run in demo mode, so it cannot
        // fall through to the real drive list.
        assert!(cfg(&["--demo-drive", "9"]).seeding());
        assert!(cfg(&["--demo-drive", "1"]).drives);
    }

    #[test]
    fn temperature_history_is_deterministic_and_plausible() {
        let a = temperature_history(1_700_000_000, true);
        let b = temperature_history(1_700_000_000, true);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        assert_eq!(a.len(), 7 * 24 * 12);
        // Ends at the requested instant and runs forward in time.
        assert_eq!(a.last().unwrap()[0], 1_700_000_000.0);
        assert!(a.windows(2).all(|w| w[1][0] > w[0][0]));
        assert!(a.iter().all(|p| p[1] > 20.0 && p[1] < 80.0));
        // The warm drive crosses the 55 C warning line; the cool one never does.
        assert!(a.iter().any(|p| p[1] >= 55.0));
        let cool = temperature_history(1_700_000_000, false);
        assert!(cool.iter().all(|p| p[1] < 55.0));
    }

    #[test]
    fn demo_health_covers_every_report_shape() {
        assert!(matches!(health_report(0), SmartReport::Nvme(_)));
        assert!(matches!(health_report(1), SmartReport::Ata(_)));
        assert!(matches!(health_report(2), SmartReport::Unavailable { .. }));
    }

    #[test]
    fn the_warning_drive_actually_reads_as_a_warning() {
        use crate::smart_reader::AttrStatus;
        let SmartReport::Ata(d) = health_report(1) else {
            panic!("disk 1 should be an ATA report");
        };
        let realloc = d.attributes.iter().find(|a| a.id == 0x05).expect("0x05");
        assert_eq!(realloc.status, AttrStatus::Warning);
        assert!(realloc.is_critical);
        // ...and the healthy attributes must not, or the page is all warnings.
        let poh = d.attributes.iter().find(|a| a.id == 0x09).expect("0x09");
        assert_eq!(poh.status, AttrStatus::Info);
        let crc = d.attributes.iter().find(|a| a.id == 0xC7).expect("0xC7");
        assert_eq!(crc.status, AttrStatus::Good);
    }

    #[test]
    fn the_warning_drive_warns_about_exactly_two_things() {
        use crate::smart_reader::AttrStatus;
        let SmartReport::Ata(d) = health_report(1) else { panic!("ATA") };
        let warned: Vec<u8> = d
            .attributes
            .iter()
            .filter(|a| a.status == AttrStatus::Warning)
            .map(|a| a.id)
            .collect();
        // Reallocated (0x05) and pending (0xC5) sectors -- and nothing else, or
        // the page reads as "everything is broken" instead of teaching which
        // two numbers matter. See KI-24 for the trap that catches extra ones.
        assert_eq!(warned, vec![0x05, 0xC5], "unexpected warning attributes");
        assert!(d.attributes.iter().all(|a| a.status != AttrStatus::Failed));
    }

    #[test]
    fn block_latencies_are_deterministic_and_bounded() {
        let a = block_latencies(1000, 640);
        let b = block_latencies(1000, 640);
        let fmt = |v: &Vec<Option<f64>>| format!("{v:?}");
        assert_eq!(fmt(&a), fmt(&b), "rendering must not depend on a clock or RNG");
        assert_eq!(a.len(), 640);
        assert!(a.iter().any(|c| c.is_none()), "want some bad blocks");
        assert!(
            a.iter().flatten().any(|ms| *ms >= crate::surface_test::SLOW_THRESHOLD_MS),
            "want some slow blocks"
        );
        assert!(
            a.iter().flatten().any(|ms| *ms < crate::surface_test::SLOW_THRESHOLD_MS),
            "want some good blocks"
        );
        // Asking for more than exist clamps rather than panicking.
        assert_eq!(block_latencies(100, 500).len(), 100);
    }

    #[test]
    fn the_bad_region_sits_inside_the_seeded_scan_with_clean_blocks_after() {
        let total = 1000;
        let cells = block_latencies(total, 640);
        let bad_or_slow = |c: &Option<f64>| {
            c.is_none() || c.is_some_and(|ms| ms >= crate::surface_test::SLOW_THRESHOLD_MS)
        };
        let last_trouble = cells.iter().rposition(bad_or_slow).expect("some trouble");
        // Comfortably short of the 640 scanned, so the heat map shows the whole
        // band with good blocks after it rather than trouble at the leading edge.
        assert!(
            last_trouble < 600,
            "bad region ends at {last_trouble}, too close to the scan edge"
        );
        // ...and it is a contiguous region, not scattered noise.
        let first_trouble = cells.iter().position(bad_or_slow).expect("some trouble");
        assert!(first_trouble < last_trouble);
    }

    #[test]
    fn chart_samples_are_deterministic_and_in_range() {
        let a = chart_samples(931.5, 0.64);
        let b = chart_samples(931.5, 0.64);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        assert!(!a.is_empty());
        assert!(a.iter().all(|p| p[0] >= 0.0 && p[0] <= 931.5));
        assert!(a.iter().all(|p| p[1] > 0.0));
        // Positions advance monotonically, so the plot draws left to right.
        assert!(a.windows(2).all(|w| w[1][0] > w[0][0]));
        assert!(chart_samples(931.5, 0.0).is_empty());
    }
}
