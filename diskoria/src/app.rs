//! Main shell: sidebar + central pages.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use egui::{
    Align2, Color32, FontFamily, FontId, Frame, Id, Key,
    Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use egui::{CornerRadius, LayerId, Modifiers, Order};
use egui_plot::{Line, MarkerShape, Plot, PlotPoints, Points};
use egui::text::{LayoutJob, TextFormat};

use crate::app_settings::{
    accent_from_palette, color_to_hex_6, parse_hex_color_6,
    AccentSourcePref, ThemePref, ACCENT_PALETTE, ACCENT_PALETTE_LABELS,
};
use crate::card::CardLayout;
use crate::chrome::{
    apply_win11_rounded_corners, draw_titlebar, load_logo_textures, setup_fonts,
    INTERACT_MANUAL_FOCUS,
};
use crate::detected_drive::{BusKind, DetectedDrive};
use crate::shortcuts::{handle_alt_shortcuts, alt_pressed, ShortcutBinding};
use crate::surface_test::{
    self, SurfaceTestMsg, SurfaceTestProgress, TOTAL_UI_BLOCKS,
};
use crate::modal_confirm::{
    two_button_modal, ModalConfirmPrimary, ModalConfirmResult, TwoButtonModalParams,
};
use crate::modal_confirm::{one_button_modal, OneButtonModalParams};
use crate::smart_health::SmartHealth;
use crate::speed_test;
use crate::speed_test::SpeedTestMsg;
use crate::theme::{
    apply_visuals, os_accent_color, Theme, CONTENT_MARGIN, MAX_CONTENT_W,
    SIDE_PANEL_W, TITLEBAR_H,
};
use crate::focus::apply_manual_focus_event_filter;
use crate::widgets::show_tooltip_text;

/// Central page draw functions, split out of this file in the Phase 3c cleanup.
/// `pages` is a descendant module of `app`, so its functions can still reach
/// `DiskoriaApp`'s private fields and helper methods. See
/// `docs/refactor-roadmap.md`.
mod pages;

/// Bold label + proportional value for S.M.A.R.T. card drive info lines.
fn smart_health_kv_line_job(label: &str, value: &str, inner_w: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let bold = FontId::new(13.0, FontFamily::Name("InterBold".into()));
    let regular = FontId::proportional(13.0);
    job.append(label, 0.0, TextFormat::simple(bold, color));
    job.append(value, 0.0, TextFormat::simple(regular, color));
    job.wrap.max_width = inner_w;
    job
}

/// Watchdog limit for a single drive enumeration before the spinner gives up.
const DRIVE_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

// Bootstrap Icons PUA (v1.11).
const NAV_TOP: &[(&str, &str)] = &[
    ("\u{f473}", "Drive Health"),
    ("\u{f3f8}", "Sector Read Test"),
    ("\u{f33b}", "Sector Write Test"),
    ("\u{f57f}", "Benchmark"),
];
const NAV_BOTTOM: &[(&str, &str)] = &[
    ("\u{f431}", "About"),
    ("\u{f3e5}", "Settings"),
];

/// Sidebar nav mnemonics (uppercase = the capitalized letter that gets underlined):
/// **H** Drive Health, **R** Sector Read, **W** Sector Write, **B** Benchmark, **A** About, **S** Settings.
fn nav_mnemonic_char(nav_index: usize) -> Option<char> {
    match nav_index {
        0 => Some('H'),
        1 => Some('R'),
        2 => Some('W'),
        3 => Some('B'),
        4 => Some('A'),
        5 => Some('S'),
        _ => None,
    }
}

/// Prefix width before the mnemonic character, and that character’s width (for Alt underline).
fn nav_mnemonic_prefix_and_char_width(
    label: &str,
    mnemonic: char,
    ctx: &egui::Context,
    font: &FontId,
) -> Option<(f32, f32)> {
    let m_upper = mnemonic.to_ascii_uppercase();
    let mut n_before = 0usize;
    let mut matched_ch = None;
    for ch in label.chars() {
        if ch == m_upper {
            matched_ch = Some(ch);
            break;
        }
        n_before += 1;
    }
    let ch = matched_ch?;
    let prefix: String = label.chars().take(n_before).collect();
    let w_before = ctx.fonts(|f| {
        f.layout_no_wrap(prefix, font.clone(), Color32::WHITE)
            .rect
            .width()
    });
    let w_ch = ctx.fonts(|f| f.glyph_width(font, ch));
    Some((w_before, w_ch))
}

const SECTOR_GRID_COLS: usize = 50;
const SECTOR_CELL_GAP: f32 = 1.0;
const SECTOR_MAP_PAD: f32 = 16.0;
/// Space between sector grid and legend row inside the map card.
const SECTOR_LEGEND_GAP: f32 = 10.0;
const SECTOR_LEGEND_ROW_H: f32 = 22.0;
/// Speed Test: spacing between Start/Stop, progress card, and results grid.
const SPEED_PAGE_SECTION_GAP: f32 = 14.0;
/// Speed Test: padding below the results grid before scroll end.
const SPEED_PAGE_BOTTOM_PAD: f32 = 20.0;

#[derive(Clone, Copy)]
enum SectorCell {
    Pending,
    Bad,
    Slow,
    /// Good read; heat uses `surface_test::SLOW_THRESHOLD_MS` vs min/max calibration.
    Heat(f64),
}

/// Message from the SMART-health worker thread: (disk number, WMI predict-fail
/// health, optional NVMe/UFS health % = 100 − wear).
type SmartHealthMsg = (u32, Result<SmartHealth, String>, Option<u8>);

pub struct DiskoriaApp {
    /// Current resolved dark state — updated at start of each `draw()` call.
    /// Exposed so the softbuffer renderer can fill the background the right colour.
    pub dark: bool,
    /// Win32 HWND — used to query `IsZoomed` for reliable maximized state.
    /// Always 0 off-Windows, where `chrome::is_maximized` falls back to egui's
    /// viewport state instead.
    hwnd: isize,
    /// Per-window draft buffer for the custom-accent hex TextEdit.  Committed
    /// to `shared.settings.accent_custom_hex` on `lost_focus`; otherwise this
    /// is the in-progress text the user is typing.
    accent_custom_hex: String,
    accent_custom_te_id: Option<Id>,
    /// Whether the hex field has actually been typed into since it last gained
    /// focus. The draft is seeded from the *default* setting (`#8E44AD`), so a
    /// blind commit on `lost_focus` made merely Tab-ing through the Settings page
    /// enable the custom accent and repaint everything purple (known-issues
    /// KI-29). Only a real edit may commit.
    accent_hex_edited: bool,
    /// Last 10%-boundary already logged for the running sector scan, so a
    /// 1000-block run produces ten log lines rather than thousands.
    surface_logged_decile: u8,
    /// Settings page (`active_nav == 4`): manual keyboard focus slots (see example app).
    settings_focus: Option<usize>,
    /// Cached "Launch at startup" state for the Settings toggle. `None` until the
    /// Settings page is first drawn, then filled from `autostart::is_enabled()`
    /// (the scheduled task / autostart entry is the source of truth). Set
    /// eagerly on toggle.
    #[cfg(any(windows, target_os = "linux"))]
    startup_enabled: Option<bool>,
    pub(crate) scroll_focus_frames: u8,
    pub(crate) pending_scroll_rect: Option<Rect>,
    active_nav: usize,
    alt_pressed: bool,
    logo: Option<egui::TextureHandle>,
    logo_light: Option<egui::TextureHandle>,
    logo_size: [usize; 2],
    pub(crate) drives: Vec<DetectedDrive>,
    pub(crate) drives_loading: bool,
    /// Earliest time the refresh button may re-enable after a refresh, so a very
    /// fast enumeration still shows the disabled/working state long enough to be
    /// noticed. Set in `spawn_drive_enumeration`; read by `refresh_busy`.
    pub(crate) refresh_min_until: Option<Instant>,
    pub(crate) drives_error: Option<String>,
    /// Generation of the shared drive list this window has applied to `drives`.
    /// Lets every window converge to the shared list regardless of which one
    /// drained the enumeration receiver. See [`SharedAppState`] generation.
    drives_generation: u64,
    /// Shared selected-drive index across the Drive Health, Sector Read, Sector
    /// Write, and Benchmark pages, so the selection persists when switching
    /// pages within a window. New windows default to the first drive not under
    /// test in another window (per-`DiskoriaApp`).
    pub(crate) selected_drive: usize,

    /// Unique per-window token for the cross-window test-lock registry in
    /// [`SharedAppState`]. Identifies this window's "drive under test" entry.
    window_token: u64,

    /// Active PASS / WARN / FAIL result overlay (shared by both block-grid tests;
    /// only one test runs per window at a time). Dismissed by any key or click.
    test_result_overlay: Option<crate::test_result_overlay::TestResult>,
    /// Set once a FAIL overlay has been raised for the current run so it does not
    /// re-trigger on every subsequent bad block. Reset when a test starts.
    fail_overlay_shown: bool,

    /// Surface test (sector scan).
    surface_test_running: bool,
    surface_test_rx: Option<mpsc::Receiver<SurfaceTestMsg>>,
    surface_test_cancel: Option<Arc<AtomicBool>>,
    /// Disk under test when `surface_test_running` (for refresh/removal parity).
    surface_test_target: Option<(u32, String)>,
    sector_cells: Vec<SectorCell>,
    heat_min_ms: f64,
    heat_max_ms: f64,
    surface_progress_pct: f64,
    surface_avg_speed_mbps: f64,
    surface_bad_sectors: i64,
    surface_slow_blocks: i64,
    surface_test_started: Option<Instant>,
    surface_elapsed_label: String,
    surface_remaining_label: String,
    surface_last_error: Option<String>,
    surface_drive_removed_msg: Option<String>,
    /// Performance chart: averaged (position_gb, speed_mbps) points for display.
    surface_chart_points: Vec<[f64; 2]>,
    /// Raw (position_gb, speed_mbps) samples for scatter dot overlay.
    surface_chart_raw_points: Vec<[f64; 2]>,
    /// Running maximum speed seen during surface scan (for Y-axis scaling).
    surface_chart_max_speed: f64,
    /// Total disk size in GB (for X-axis range).
    surface_chart_total_gb: f64,
    /// Active visualization tab: 0 = Sector Map, 1 = Performance Chart.
    surface_chart_tab: usize,
    /// Bucket averaging state for chart smoothing.
    surface_chart_bucket_sum: f64,
    surface_chart_bucket_count: u32,
    surface_chart_bucket_idx: usize,

    /// Manual Tab order on Sector Test (copynaut-style): idle 0=Refresh, 1=Drive combo, 2=Start; running 0=Stop.
    sector_focus: Option<usize>,
    /// Manual Tab order on Speed Test: idle 0=Refresh, 1=Volume combo, 2=Start (Windows); running 0=Stop.
    speed_focus: Option<usize>,
    /// Widget ids from last frame (for `focus::bind_text_focus_slot`).
    sector_refresh_id: Option<Id>,
    sector_combo_id: Option<Id>,
    speed_refresh_id: Option<Id>,
    speed_volume_combo_id: Option<Id>,

    /// Confirm before stopping an in-progress sector scan (Copynaut-style modal).
    show_stop_test_confirm: bool,
    /// `Some(0)` = No, `Some(1)` = Yes, Stop — Tab cycles while modal is open.
    modal_two_button_focus: Option<usize>,

    /// SMART / storage health (WMI) for the drive currently selected in the sector combo.
    smart_health: Option<SmartHealth>,
    /// Estimated drive health % (100 − wear) for NVMe/UFS drives, which don't
    /// expose ATA SMART predict-fail. `None` for SATA/USB or when unavailable.
    smart_health_pct: Option<u8>,
    smart_health_err: Option<String>,
    smart_health_disk: Option<u32>,
    // Only the `#[cfg(windows)]` poll arm reads this today (Linux never spawns
    // the health thread); un-gated by the port's SMART phase.
    #[cfg_attr(not(windows), allow(dead_code))]
    smart_health_rx: Option<mpsc::Receiver<SmartHealthMsg>>,
    smart_health_inflight: bool,

    /// Speed test (file benchmark on a volume).
    speed_test_running: bool,
    speed_test_rx: Option<mpsc::Receiver<SpeedTestMsg>>,
    speed_test_cancel: Option<Arc<AtomicBool>>,
    /// Disk number + drive letter (`C:`) for removal checks.
    speed_test_target: Option<(u32, String)>,
    selected_speed_partition: usize,
    speed_progress_op: String,
    speed_progress_pct: f64,
    speed_current_mbps: f64,
    speed_seq_read_mbps: f64,
    speed_seq_write_mbps: f64,
    speed_r4_read_mbps: f64,
    speed_r4_write_mbps: f64,
    speed_error_msg: Option<String>,
    speed_drive_removed_msg: Option<String>,

    /// Destructive test (write+verify all sectors).
    /// Per-run gate: `false` at startup, set `true` by the blocker page — never saved.
    destructive_unlocked: bool,
    destructive_test_running: bool,
    destructive_test_rx: Option<mpsc::Receiver<crate::destructive_test::DestructiveTestMsg>>,
    destructive_test_cancel: Option<Arc<AtomicBool>>,
    /// `(disk_number, device_id)` of the drive under test (for removal detection).
    destructive_test_target: Option<(u32, String)>,
    destructive_cells: Vec<SectorCell>,
    destructive_heat_min_ms: f64,
    destructive_heat_max_ms: f64,
    destructive_progress_pct: f64,
    destructive_avg_speed_mbps: f64,
    destructive_bad_sectors: i64,
    destructive_slow_blocks: i64,
    destructive_test_started: Option<Instant>,
    destructive_elapsed_label: String,
    destructive_remaining_label: String,
    destructive_last_error: Option<String>,
    destructive_drive_removed_msg: Option<String>,
    /// Performance chart: averaged (position_gb, speed_mbps) points for display.
    destructive_chart_points: Vec<[f64; 2]>,
    /// Raw (position_gb, speed_mbps) samples for scatter dot overlay.
    destructive_chart_raw_points: Vec<[f64; 2]>,
    /// Running maximum speed seen during destructive scan (for Y-axis scaling).
    destructive_chart_max_speed: f64,
    /// Total disk size in GB (for X-axis range).
    destructive_chart_total_gb: f64,
    /// Active visualization tab: 0 = Sector Map, 1 = Performance Chart.
    destructive_chart_tab: usize,
    /// Bucket averaging state for chart smoothing.
    destructive_chart_bucket_sum: f64,
    destructive_chart_bucket_count: u32,
    destructive_chart_bucket_idx: usize,
    /// Tab order: gate page 0=Continue; test page idle 0=Refresh, 1=Combo, 2=Start; running 0=Stop.
    destructive_focus: Option<usize>,
    destructive_refresh_id: Option<Id>,
    destructive_combo_id: Option<Id>,
    /// Confirm before starting (data destruction warning).
    show_destructive_start_confirm: bool,
    destructive_start_confirm_focus: Option<usize>,
    /// Confirm before stopping an in-progress destructive test.
    show_destructive_stop_confirm: bool,
    destructive_stop_confirm_focus: Option<usize>,

    /// Pending performance-chart export awaiting the "include health report?"
    /// modal choice. `Some` shows the modal; the worker spawns on confirm/cancel.
    pending_chart_export: Option<ChartExportRequest>,
    chart_export_focus: Option<usize>,

    /// Health Status page state.
    pub(crate) health_report: Option<crate::smart_reader::SmartReport>,
    /// Drive index the current `health_report` belongs to, so the page re-polls
    /// when the shared selection changed on another page.
    pub(crate) health_report_drive: Option<usize>,
    pub(crate) health_poll_rx: Option<std::sync::mpsc::Receiver<crate::smart_reader::SmartReport>>,
    pub(crate) health_poll_running: bool,
    /// When the last completed SMART poll finished (used for live refresh).
    pub(crate) health_last_poll: Option<std::time::Instant>,
    /// Manual Tab order on Drive Health: 0=Refresh, 1=Drive combo, 2=Export Log.
    pub(crate) health_focus: Option<usize>,
    pub(crate) health_refresh_id: Option<Id>,
    pub(crate) health_combo_id: Option<Id>,
    /// Manual Tab order on About: 0=Check updates, 1=URL, 2=Ko-fi.
    pub(crate) about_focus: Option<usize>,

    pub(crate) about_appicon: Option<egui::TextureHandle>,
    #[cfg(any(windows, target_os = "linux"))]
    update_check_rx: Option<mpsc::Receiver<Result<crate::update::UpdateCheckResult, String>>>,
    #[cfg(any(windows, target_os = "linux"))]
    update_download_rx: Option<mpsc::Receiver<Result<std::path::PathBuf, String>>>,
    #[cfg(any(windows, target_os = "linux"))]
    pending_update_version: String,
    #[cfg(any(windows, target_os = "linux"))]
    show_update_alert: bool,
    #[cfg(any(windows, target_os = "linux"))]
    update_alert_title: String,
    #[cfg(any(windows, target_os = "linux"))]
    update_alert_body: String,
    #[cfg(any(windows, target_os = "linux"))]
    update_check_busy: bool,
    #[cfg(any(windows, target_os = "linux"))]
    update_download_busy: bool,
    #[cfg(any(windows, target_os = "linux"))]
    update_alert_focus: Option<usize>,
    /// The in-flight check was started automatically rather than by the About
    /// button. An automatic check stays silent unless it finds something — a
    /// "you are up to date" box on every launch would be noise.
    #[cfg(any(windows, target_os = "linux"))]
    update_check_is_auto: bool,
    /// An installer has been downloaded and is waiting to run; the modal asks
    /// whether to apply it now or on exit.
    #[cfg(any(windows, target_os = "linux"))]
    show_update_staged_modal: bool,
    #[cfg(any(windows, target_os = "linux"))]
    staged_update_version: String,
    #[cfg(any(windows, target_os = "linux"))]
    update_staged_focus: Option<usize>,
    /// Whether this window is currently on screen. `--minimized` still draws a
    /// hidden window (that is what kicks drive enumeration), so anything that
    /// shows UI to the user has to check this first.
    pub(crate) window_visible: bool,

    /// `--demo-alert` seeded an alert that still has to be announced. It cannot
    /// fire from `new()` because `event_proxy` is attached afterwards, so the
    /// first draw with a proxy consumes this. See `demo.rs`.
    #[cfg(any(windows, target_os = "linux"))]
    demo_alert_pending: bool,

    // ── Pro-Monitoring ────────────────────────────────────────────────────────
    // Latest per-drive snapshots live on `SharedAppState` (see
    // `insert_snapshot`/`snapshot_for`) so multiple windows don't split the set.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) pending_alerts: Vec<crate::alert_engine::AlertEvent>,
    /// Per-drive alert suppression: serial → Instant until which alerts are silenced.
    #[cfg(any(windows, target_os = "linux"))]
    alert_suppressions: std::collections::HashMap<String, std::time::Instant>,
    /// Temperature history master map (up to 7 days); chart filters per selected range.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) temp_history: std::collections::HashMap<String, Vec<[f64; 2]>>,
    /// Active history range tab: 0=1h 1=6h 2=12h 3=24h 4=7d.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) health_chart_range: usize,
    /// Proxy for sending tray icon update events back to the event loop.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) event_proxy: Option<winit::event_loop::EventLoopProxy<crate::UserEvent>>,
    // Pro-Monitoring settings (mirrored from app_settings for live UI editing)
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) monitoring_enabled: bool,
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) poll_interval_mins: u8,
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) alert_temp_warn: i32,
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) alert_temp_critical: i32,
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) alert_wear_threshold: u8,

    /// Process-wide shared state (settings, drives, accent, monitor).
    /// See `SharedAppState` — lives across every window, kept in sync live.
    pub(crate) shared: Arc<crate::SharedAppState>,
}

impl DiskoriaApp {
    pub fn new(
        ctx: &egui::Context,
        system_dark: bool,
        hwnd: isize,
        shared: Arc<crate::SharedAppState>,
    ) -> Self {
        setup_fonts(ctx);
        apply_win11_rounded_corners(hwnd);
        #[cfg(windows)]
        crate::chrome::install_win32_resize(hwnd);

        static LOGO_PNG: &[u8] = include_bytes!("../../assets/applogo.png");
        let (logo, logo_light, logo_size) = load_logo_textures(ctx, LOGO_PNG);

        static ABOUT_ICO: &[u8] = include_bytes!("../../assets/appicon2.ico");
        let about_appicon = crate::chrome::load_appicon_texture(ctx, ABOUT_ICO);

        let s = shared.settings_snapshot();
        // `--demo-accent` pins the accent for reproducible reference captures;
        // published to shared state so every window and the tray agree.
        if let Some((r, g, b)) = crate::demo::config().accent {
            shared.set_accent_color(Color32::from_rgb(r, g, b));
        }
        let dark = match crate::demo::config().dark {
            Some(d) => d,
            None => match s.theme {
                ThemePref::Auto => system_dark,
                ThemePref::Dark => true,
                ThemePref::Light => false,
            },
        };
        apply_visuals(ctx, dark, shared.accent_color());

        shared.ensure_drive_enumeration(ctx);

        // Unique token for this window's test-lock entry.
        static WINDOW_TOKEN: AtomicU64 = AtomicU64::new(0);
        let window_token = WINDOW_TOKEN.fetch_add(1, Ordering::Relaxed);

        // Default the selection to the first drive not under test elsewhere, so a
        // window opened while another is testing drive 0 lands on a free drive.
        let busy = shared.busy_keys_excluding(window_token);
        let selected_drive = shared.drives_read(|d| {
            d.list
                .iter()
                .position(|dr| !busy.contains(&dr.lock_key()))
                .unwrap_or(0)
        });

        let mut app = Self {
            dark,
            hwnd,
            accent_custom_hex: s.accent_custom_hex.clone(),
            accent_custom_te_id: None,
            accent_hex_edited: false,
            surface_logged_decile: u8::MAX,
            settings_focus: None,
            #[cfg(any(windows, target_os = "linux"))]
            startup_enabled: None,
            scroll_focus_frames: 0,
            pending_scroll_rect: None,
            active_nav: 0,
            alt_pressed: false,
            logo,
            logo_light,
            logo_size,
            drives: Vec::new(),
            drives_loading: true,
            refresh_min_until: None,
            drives_error: None,
            drives_generation: 0,
            selected_drive,
            window_token,
            test_result_overlay: None,
            fail_overlay_shown: false,
            surface_test_running: false,
            surface_test_rx: None,
            surface_test_cancel: None,
            surface_test_target: None,
            sector_cells: (0..TOTAL_UI_BLOCKS).map(|_| SectorCell::Pending).collect(),
            heat_min_ms: f64::MAX,
            heat_max_ms: f64::MIN,
            surface_progress_pct: 0.0,
            surface_avg_speed_mbps: 0.0,
            surface_bad_sectors: 0,
            surface_slow_blocks: 0,
            surface_test_started: None,
            surface_elapsed_label: "00:00:00".to_string(),
            surface_remaining_label: "--:--:--".to_string(),
            surface_last_error: None,
            surface_drive_removed_msg: None,
            surface_chart_points: Vec::new(),
            surface_chart_raw_points: Vec::new(),
            surface_chart_max_speed: 0.0,
            surface_chart_total_gb: 0.0,
            surface_chart_tab: 0,
            surface_chart_bucket_sum: 0.0,
            surface_chart_bucket_count: 0,
            surface_chart_bucket_idx: 0,
            sector_focus: None,
            speed_focus: None,
            sector_refresh_id: None,
            sector_combo_id: None,
            speed_refresh_id: None,
            speed_volume_combo_id: None,
            show_stop_test_confirm: false,
            modal_two_button_focus: None,
            smart_health: None,
            smart_health_pct: None,
            smart_health_err: None,
            smart_health_disk: None,
            smart_health_rx: None,
            smart_health_inflight: false,
            speed_test_running: false,
            speed_test_rx: None,
            speed_test_cancel: None,
            speed_test_target: None,
            selected_speed_partition: 0,
            speed_progress_op: "Ready".to_string(),
            speed_progress_pct: 0.0,
            speed_current_mbps: 0.0,
            speed_seq_read_mbps: -1.0,
            speed_seq_write_mbps: -1.0,
            speed_r4_read_mbps: -1.0,
            speed_r4_write_mbps: -1.0,
            speed_error_msg: None,
            speed_drive_removed_msg: None,
            destructive_unlocked: false,
            destructive_test_running: false,
            destructive_test_rx: None,
            destructive_test_cancel: None,
            destructive_test_target: None,
            destructive_cells: (0..crate::destructive_test::TOTAL_UI_BLOCKS)
                .map(|_| SectorCell::Pending)
                .collect(),
            destructive_heat_min_ms: f64::MAX,
            destructive_heat_max_ms: f64::MIN,
            destructive_progress_pct: 0.0,
            destructive_avg_speed_mbps: 0.0,
            destructive_bad_sectors: 0,
            destructive_slow_blocks: 0,
            destructive_test_started: None,
            destructive_elapsed_label: "00:00:00".to_string(),
            destructive_remaining_label: "--:--:--".to_string(),
            destructive_last_error: None,
            destructive_drive_removed_msg: None,
            destructive_chart_points: Vec::new(),
            destructive_chart_raw_points: Vec::new(),
            destructive_chart_max_speed: 0.0,
            destructive_chart_total_gb: 0.0,
            destructive_chart_tab: 0,
            destructive_chart_bucket_sum: 0.0,
            destructive_chart_bucket_count: 0,
            destructive_chart_bucket_idx: 0,
            destructive_focus: None,
            destructive_refresh_id: None,
            destructive_combo_id: None,
            show_destructive_start_confirm: false,
            destructive_start_confirm_focus: None,
            show_destructive_stop_confirm: false,
            destructive_stop_confirm_focus: None,
            pending_chart_export: None,
            chart_export_focus: None,
            health_report: None,
            health_report_drive: None,
            health_poll_rx: None,
            health_poll_running: false,
            health_last_poll: None,
            health_focus: None,
            health_refresh_id: None,
            health_combo_id: None,
            about_focus: None,
            about_appicon,
            #[cfg(any(windows, target_os = "linux"))]
            update_check_rx: None,
            #[cfg(any(windows, target_os = "linux"))]
            update_download_rx: None,
            #[cfg(any(windows, target_os = "linux"))]
            pending_update_version: String::new(),
            #[cfg(any(windows, target_os = "linux"))]
            show_update_alert: false,
            #[cfg(any(windows, target_os = "linux"))]
            update_alert_title: String::new(),
            #[cfg(any(windows, target_os = "linux"))]
            update_alert_body: String::new(),
            #[cfg(any(windows, target_os = "linux"))]
            update_check_busy: false,
            #[cfg(any(windows, target_os = "linux"))]
            update_download_busy: false,
            #[cfg(any(windows, target_os = "linux"))]
            update_alert_focus: None,
            #[cfg(any(windows, target_os = "linux"))]
            update_check_is_auto: false,
            #[cfg(any(windows, target_os = "linux"))]
            show_update_staged_modal: false,
            #[cfg(any(windows, target_os = "linux"))]
            staged_update_version: String::new(),
            #[cfg(any(windows, target_os = "linux"))]
            update_staged_focus: None,
            // Corrected by `Renderer::paint` before the first draw that matters.
            window_visible: true,
            #[cfg(any(windows, target_os = "linux"))]
            demo_alert_pending: false,
            // Pro-Monitoring
            #[cfg(any(windows, target_os = "linux"))]
            pending_alerts: Vec::new(),
            #[cfg(any(windows, target_os = "linux"))]
            alert_suppressions: std::collections::HashMap::new(),
            #[cfg(any(windows, target_os = "linux"))]
            temp_history: std::collections::HashMap::new(),
            #[cfg(any(windows, target_os = "linux"))]
            health_chart_range: 3, // default to 24h tab
            #[cfg(any(windows, target_os = "linux"))]
            event_proxy: None,
            #[cfg(any(windows, target_os = "linux"))]
            monitoring_enabled: s.monitoring_enabled,
            #[cfg(any(windows, target_os = "linux"))]
            poll_interval_mins: s.poll_interval_mins,
            #[cfg(any(windows, target_os = "linux"))]
            alert_temp_warn: s.alert_temp_warn,
            #[cfg(any(windows, target_os = "linux"))]
            alert_temp_critical: s.alert_temp_critical,
            #[cfg(any(windows, target_os = "linux"))]
            alert_wear_threshold: s.alert_wear_threshold,
            shared,
        };
        app.apply_demo_seed();
        app
    }

    /// Seed UI state from the `--page` / `--demo-*` flags (see `demo.rs`).
    ///
    /// A "running" test is faked by setting the progress fields and leaving the
    /// worker channel `None` — every `poll_*` early-returns on a missing
    /// receiver, so no disk I/O is started and none of the seeded numbers move.
    fn apply_demo_seed(&mut self) {
        let cfg = crate::demo::config();
        if let Some(page) = cfg.page {
            self.active_nav = page;
        }
        if !cfg.seeding() {
            return;
        }

        // Which test the test-shaped flags describe. Without `--page` the flags
        // seed the sector read test, the one that is safe in every sense.
        let page = cfg.page.unwrap_or(crate::demo::PAGE_SECTOR_READ);
        let write_test = page == crate::demo::PAGE_SECTOR_WRITE;
        let benchmark = page == crate::demo::PAGE_BENCHMARK;

        if cfg.unlocks_destructive() {
            self.destructive_unlocked = true;
        }

        // Which drive the page opens on. `--demo-drive` wins; otherwise the
        // write test points at the USB stick, because a wiki screenshot of the
        // destructive test aimed at the machine's system drive teaches exactly
        // the wrong reflex.
        self.selected_drive = cfg.drive.unwrap_or(if write_test { 2 } else { 0 });

        // Roughly two thirds of the way through a scan: far enough in for the
        // heat map to have a shape and for the rough patch to be visible.
        const SCANNED: f64 = 0.64;
        let total_gb = crate::demo::drives()[0].size_bytes as f64 / 1_073_741_824.0;

        if (cfg.progress || cfg.heatmap || cfg.chart) && !benchmark {
            let blocks = crate::demo::block_latencies(
                TOTAL_UI_BLOCKS,
                (TOTAL_UI_BLOCKS as f64 * SCANNED) as usize,
            );
            let mut cells: Vec<SectorCell> = Vec::with_capacity(TOTAL_UI_BLOCKS);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            let (mut bad, mut slow) = (0i64, 0i64);
            for ms in &blocks {
                match ms {
                    None => {
                        bad += 1;
                        cells.push(SectorCell::Bad);
                    }
                    Some(ms) if *ms >= surface_test::SLOW_THRESHOLD_MS => {
                        slow += 1;
                        cells.push(SectorCell::Slow);
                    }
                    Some(ms) => {
                        lo = lo.min(*ms);
                        hi = hi.max(*ms);
                        cells.push(SectorCell::Heat(*ms));
                    }
                }
            }
            cells.resize(TOTAL_UI_BLOCKS, SectorCell::Pending);
            let points = crate::demo::chart_samples(total_gb, SCANNED);
            let max_speed = points.iter().fold(0.0_f64, |m, p| m.max(p[1]));
            let pct = SCANNED * 100.0;
            let tab = usize::from(cfg.chart);

            if write_test {
                self.destructive_cells = cells;
                self.destructive_heat_min_ms = lo;
                self.destructive_heat_max_ms = hi;
                self.destructive_bad_sectors = bad;
                self.destructive_slow_blocks = slow;
                self.destructive_progress_pct = pct;
                self.destructive_avg_speed_mbps = 121.4;
                self.destructive_elapsed_label = "01:12:40".to_string();
                self.destructive_remaining_label = "00:40:52".to_string();
                self.destructive_chart_raw_points = points.clone();
                self.destructive_chart_points = points;
                self.destructive_chart_max_speed = max_speed;
                self.destructive_chart_total_gb = total_gb;
                self.destructive_chart_tab = tab;
            } else {
                self.sector_cells = cells;
                self.heat_min_ms = lo;
                self.heat_max_ms = hi;
                self.surface_bad_sectors = bad;
                self.surface_slow_blocks = slow;
                self.surface_progress_pct = pct;
                self.surface_avg_speed_mbps = 154.8;
                self.surface_elapsed_label = "00:57:18".to_string();
                self.surface_remaining_label = "00:32:14".to_string();
                self.surface_chart_raw_points = points.clone();
                self.surface_chart_points = points;
                self.surface_chart_max_speed = max_speed;
                self.surface_chart_total_gb = total_gb;
                self.surface_chart_tab = tab;
            }
        }

        if cfg.progress {
            if benchmark {
                self.speed_test_running = true;
                self.speed_test_target = Some((0, "C:".to_string()));
                self.speed_progress_op = "Random 4K read".to_string();
                self.speed_progress_pct = 62.0;
                self.speed_current_mbps = 71.3;
                self.speed_seq_read_mbps = 3_412.7;
                self.speed_seq_write_mbps = 2_988.1;
                // -1.0 is the app's "not measured yet" sentinel.
                self.speed_r4_read_mbps = -1.0;
                self.speed_r4_write_mbps = -1.0;
            } else {
                let sel = self.selected_drive;
                let target = Some((sel as u32, crate::demo::drives()[sel].device_id.clone()));
                if write_test {
                    self.destructive_test_running = true;
                    self.destructive_test_target = target;
                } else {
                    self.surface_test_running = true;
                    self.surface_test_target = target;
                }
            }
        }

        // A completed benchmark: all four numbers filled in. The PASS/WARN/FAIL
        // overlay does not exist on this page, so `--demo-result` means "the
        // finished result" here rather than "the result overlay".
        if benchmark && cfg.result.is_some() && !cfg.progress {
            self.speed_progress_op = "Complete".to_string();
            self.speed_progress_pct = 100.0;
            self.speed_seq_read_mbps = 3_412.7;
            self.speed_seq_write_mbps = 2_988.1;
            self.speed_r4_read_mbps = 74.6;
            self.speed_r4_write_mbps = 191.2;
        } else if let Some(r) = cfg.result {
            self.test_result_overlay = Some(r);
        }

        if cfg.confirm {
            if write_test {
                // Mid-run the meaningful confirmation is "stop?"; before the run
                // it is the data-destruction warning the flag exists to capture.
                if cfg.progress {
                    self.show_destructive_stop_confirm = true;
                    self.destructive_stop_confirm_focus = Some(0);
                } else {
                    self.show_destructive_start_confirm = true;
                    self.destructive_start_confirm_focus = Some(0);
                }
            } else {
                self.show_stop_test_confirm = true;
                self.modal_two_button_focus = Some(0);
            }
        }

        #[cfg(any(windows, target_os = "linux"))]
        if cfg.alert {
            self.pending_alerts.push(crate::demo::alert_event());
            self.demo_alert_pending = true;
            // Monitoring that has raised an alert has been running, so it has
            // history. The alerting drive runs warm; the others do not.
            let now = chrono::Utc::now().timestamp();
            for (i, d) in crate::demo::drives().iter().enumerate() {
                self.temp_history
                    .insert(d.serial.clone(), crate::demo::temperature_history(now, i == 1));
            }
        }
    }

    /// Announce the `--demo-alert` alert once a proxy exists, so the tray icon
    /// enters its alert state and the toast fires exactly as a real alert would
    /// drive them. `poll_monitor` cannot do this: with no monitor thread running
    /// it returns before it reaches the drain.
    #[cfg(any(windows, target_os = "linux"))]
    fn poll_demo_alert(&mut self) {
        if !self.demo_alert_pending {
            return;
        }
        let Some(proxy) = &self.event_proxy else { return };
        for alert in std::mem::take(&mut self.pending_alerts) {
            let _ = proxy.send_event(crate::UserEvent::DriveAlert {
                serial: alert.serial.clone(),
                is_critical: matches!(alert.level, crate::alert_engine::AlertLevel::Critical),
            });
            // WinRT is MTA; the main winit thread is STA.
            std::thread::spawn(move || {
                crate::toast::send_toast(
                    &format!("Diskoria \u{2014} Drive {} (Warning)", alert.model),
                    &alert.detail,
                );
            });
        }
        self.demo_alert_pending = false;
    }

    fn any_test_running(&self) -> bool {
        self.surface_test_running || self.destructive_test_running || self.speed_test_running
    }

    /// Flip every in-flight test's cancel flag.  Called when this window
    /// is closed (worker threads are per-window; they must not outlive
    /// their originating `DiskoriaApp`).  The worker threads poll their
    /// cancel flag between block reads and exit within ~1 block of work.
    pub fn cancel_all_tests(&self) {
        if let Some(c) = &self.surface_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(c) = &self.speed_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(c) = &self.destructive_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        // This window is going away — release any drive it had locked so other
        // windows stop graying it out.
        self.shared.clear_window_test_lock(self.window_token);
    }

    /// The `lock_key` of the drive at `idx` in this window's drive list.
    fn drive_lock_key_at(&self, idx: usize) -> Option<String> {
        self.drives.get(idx).map(|d| d.lock_key())
    }

    /// The `lock_key` of the currently-selected physical drive, if any.
    fn selected_drive_lock_key(&self) -> Option<String> {
        self.drive_lock_key_at(self.selected_drive.min(self.drives.len().saturating_sub(1)))
    }

    /// The `lock_key` of the drive a *running* test is actually working on.
    ///
    /// Each `*_test_target` carries the disk number the worker was started
    /// against, which is the authoritative answer — the Benchmark page's target
    /// is derived rather than stored in `selected_drive` (KI-15), so the
    /// selection alone can name a different disk than the one under test.
    fn running_test_lock_key(&self) -> Option<String> {
        let disk = if self.speed_test_running {
            self.speed_test_target.as_ref().map(|&(n, _)| n)
        } else if self.surface_test_running {
            self.surface_test_target.as_ref().map(|&(n, _)| n)
        } else if self.destructive_test_running {
            self.destructive_test_target.as_ref().map(|&(n, _)| n)
        } else {
            None
        }?;
        self.drives
            .iter()
            .find(|d| d.disk_number == disk)
            .map(|d| d.lock_key())
    }

    /// Publish this window's drive-under-test to the shared registry (or clear
    /// it when idle). Called once per frame from `draw`.
    fn publish_test_lock(&self) {
        let key = if self.any_test_running() {
            // Fall back to the selection if a target hasn't been recorded yet:
            // publishing the wrong key is far better than publishing none, which
            // would let another window start on the same disk.
            self.running_test_lock_key()
                .or_else(|| self.selected_drive_lock_key())
        } else {
            None
        };
        self.shared.set_window_test_lock(self.window_token, key);
    }

    /// Drive keys locked by *other* windows (used to gray out dropdown items).
    pub(crate) fn drives_busy_elsewhere(&self) -> std::collections::HashSet<String> {
        self.shared.busy_keys_excluding(self.window_token)
    }

    fn drive_busy_elsewhere_at(&self, idx: usize) -> bool {
        match self.drive_lock_key_at(idx) {
            Some(k) => self.shared.drive_busy_elsewhere(self.window_token, &k),
            None => false,
        }
    }

    /// Whether this window's currently-selected drive is under test elsewhere.
    pub(crate) fn selected_drive_busy_elsewhere(&self) -> bool {
        self.drive_busy_elsewhere_at(self.selected_drive.min(self.drives.len().saturating_sub(1)))
    }

    /// Whether the drive the Benchmark page would test is under test elsewhere.
    /// Same as [`Self::selected_drive_busy_elsewhere`] except when the selection
    /// has no mounted volume, in which case there is nothing to run and nothing
    /// to warn about.
    pub(crate) fn speed_target_busy_elsewhere(&self) -> bool {
        match self.speed_target_pair() {
            Some((di, _)) => self.drive_busy_elsewhere_at(di),
            None => false,
        }
    }

    /// Returns the drive index that the currently-active page has selected.
    /// Used to drive SMART health polling and the shared health card.
    fn active_page_selected_drive_idx(&self) -> usize {
        self.selected_drive.min(self.drives.len().saturating_sub(1))
    }

    /// Windows updates are an *installed-build* feature: the release asset the
    /// updater picks is the Inno installer, so applying one on a portable exe
    /// would silently convert it into an installed copy (KI-23). The Linux
    /// build has no installer at all — its portable binary self-replaces, so
    /// updates are always supported there.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn updates_supported(&self) -> bool {
        #[cfg(windows)]
        {
            crate::install_mode::current().is_installed()
        }
        #[cfg(target_os = "linux")]
        {
            true
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn update_check_button_enabled(&self) -> bool {
        self.updates_supported()
            && !self.update_check_busy
            && !self.update_download_busy
            && self.update_check_rx.is_none()
            && self.update_download_rx.is_none()
            && !self.show_update_alert
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn on_about_check_updates_clicked(&mut self, ctx: &egui::Context) {
        // Covers `updates_supported()` too — a portable build never gets here.
        if !self.update_check_button_enabled() {
            return;
        }
        self.start_update_check(ctx, false);
    }

    /// Kick off the background check. `is_auto` suppresses the "up to date" and
    /// "check failed" boxes so a startup check only ever interrupts the user
    /// when there is genuinely something to install.
    #[cfg(any(windows, target_os = "linux"))]
    fn start_update_check(&mut self, ctx: &egui::Context, is_auto: bool) {
        let (tx, rx) = mpsc::channel();
        self.update_check_rx = Some(rx);
        self.update_check_busy = true;
        self.update_check_is_auto = is_auto;
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let out = crate::update::check_for_update_blocking();
            let _ = tx.send(out);
            ctx2.request_repaint();
        });
    }

    /// Fire the once-per-process startup update check, if everything lines up:
    /// an installed build, the setting on, nothing already in flight, and a
    /// window actually on screen (a `--minimized` tray-only start draws a hidden
    /// window — prompting there would be invisible). When the user later opens
    /// the window from the tray it becomes visible and the check runs then.
    #[cfg(any(windows, target_os = "linux"))]
    fn maybe_start_auto_update_check(&mut self, ctx: &egui::Context) {
        // A capture run must not reach the network, and an update box appearing
        // mid-screenshot would land in the wiki.
        if crate::demo::seeding() {
            return;
        }
        if !self.window_visible
            || !self.updates_supported()
            || !self.shared.settings_snapshot().auto_check_updates
            || !self.update_check_button_enabled()
        {
            return;
        }
        if self.shared.claim_auto_update_check() {
            log::info!(target: "diskoria", "startup update check");
            self.start_update_check(ctx, true);
        }
    }

    /// Download an installer in the background and stage it. Shared by the
    /// automatic path (which downloads without asking) and the manual
    /// "Download" confirm.
    #[cfg(any(windows, target_os = "linux"))]
    fn start_update_download(&mut self, ctx: &egui::Context, url: &str) {
        // Name must keep the `setup` marker for installer assets — the apply
        // step tells installer from portable exe by filename alone (KI-22).
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        #[cfg(windows)]
        let dest = std::env::temp_dir().join(crate::update::update_temp_file_name(url, nonce));
        // Linux: stage next to the running binary — the apply step is an
        // atomic same-filesystem rename over current_exe.
        #[cfg(not(windows))]
        let dest = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir)
            .join(crate::update::update_temp_file_name(url, nonce));
        let url = url.to_string();
        let (tx, rx) = mpsc::channel();
        self.update_download_rx = Some(rx);
        self.update_download_busy = true;
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = crate::update::download_to_path(&url, &dest).map(|_| dest);
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn poll_update_check(&mut self, ctx: &egui::Context) {
        let recv = {
            let Some(rx) = &self.update_check_rx else {
                return;
            };
            rx.try_recv()
        };
        match recv {
            Ok(Ok(crate::update::UpdateCheckResult::UpToDate)) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
                if !self.update_check_is_auto {
                    self.show_update_alert = true;
                    self.update_alert_title = "Up to date".to_string();
                    self.update_alert_body =
                        "You are running the latest release.".to_string();
                }
                ctx.request_repaint();
            }
            Ok(Ok(crate::update::UpdateCheckResult::UpdateAvailable {
                version_display,
                download_url,
            })) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
                self.pending_update_version = version_display;
                // Both paths download straight away. Asking "download it?" here
                // was a prompt for something the user had already asked for —
                // explicitly on the manual path, and by leaving automatic checks
                // on for the startup one. What differs is what happens *after*
                // the download (see `poll_update_download`): a manual check
                // installs, an automatic one asks when to.
                self.start_update_download(ctx, &download_url);
                ctx.request_repaint();
            }
            Ok(Err(e)) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
                if self.update_check_is_auto {
                    // Offline, rate-limited, DNS down — none of that is worth a
                    // modal on launch.
                    log::warn!(target: "diskoria", "startup update check failed: {e}");
                } else {
                    self.show_update_alert = true;
                    self.update_alert_title = "Update check failed".to_string();
                    self.update_alert_body = e;
                }
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
            }
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn poll_update_download(&mut self, ctx: &egui::Context) {
        let recv = {
            let Some(rx) = &self.update_download_rx else {
                return;
            };
            rx.try_recv()
        };
        match recv {
            Ok(Ok(path)) => {
                self.update_download_rx = None;
                self.update_download_busy = false;
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                #[cfg(windows)]
                if name.contains("setup") && name.ends_with(".exe") {
                    self.shared.stage_update(path);
                    self.staged_update_version =
                        std::mem::take(&mut self.pending_update_version);
                    if !self.update_check_is_auto && !self.any_test_running() {
                        // Manual check: the user clicked "Check for updates" and
                        // there is one, so install it. Asking again is asking
                        // twice for one decision.
                        if let Some(installer) = self.shared.take_staged_update() {
                            self.cancel_all_tests();
                            crate::update::spawn_run_installer_and_exit(&installer);
                        }
                    } else {
                        // Automatic check, or a test is running: stage it and let
                        // the modal decide when. Applying it under the user
                        // mid-session would kill a running test without warning.
                        self.show_update_staged_modal = true;
                        ctx.request_repaint();
                    }
                } else if let Ok(exe) = std::env::current_exe() {
                    crate::update::spawn_apply_update_and_exit(&path, &exe);
                } else {
                    self.show_update_alert = true;
                    self.update_alert_title = "Update".to_string();
                    self.update_alert_body =
                        "Download finished but the current executable path could not be read."
                            .to_string();
                    ctx.request_repaint();
                }
                // Linux: always stage and ask "now or on close" — a bare
                // binary swap is cheap either way, and the modal is the
                // decision point the user opted into.
                #[cfg(not(windows))]
                {
                    let _ = name;
                    self.shared.stage_update(path);
                    self.staged_update_version =
                        std::mem::take(&mut self.pending_update_version);
                    self.show_update_staged_modal = true;
                    ctx.request_repaint();
                }
            }
            Ok(Err(e)) => {
                self.update_download_rx = None;
                self.update_download_busy = false;
                self.show_update_alert = true;
                self.update_alert_title = "Download failed".to_string();
                self.update_alert_body = e;
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.update_download_rx = None;
                self.update_download_busy = false;
            }
        }
    }

    /// "Update ready" modal shown once an installer has been downloaded and
    /// staged. Applying it now would tear down the process, so while a test is
    /// running the only option is to let it install on exit — interrupting a
    /// destructive write+verify mid-pass leaves the drive in an unknown state.
    #[cfg(any(windows, target_os = "linux"))]
    fn draw_update_staged_modal(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());
        let version = self.staged_update_version.clone();

        if self.any_test_running() {
            let body = format!(
                "Diskoria {version} has been downloaded and will be installed when you \
                 close the app.\n\nA test is currently running, so the update can't be \
                 applied right now. Finish or stop the test to install it sooner."
            );
            if one_button_modal(
                ctx,
                &t,
                OneButtonModalParams {
                    overlay_id: Id::new("diskoria_update_staged_overlay"),
                    dialog_id: Id::new("diskoria_update_staged_dialog"),
                    width: 420.0,
                    height: 240.0,
                    title: "Update ready",
                    body: &body,
                    ok_id: Id::new("diskoria_update_staged_ok"),
                    ok_label: "OK",
                },
                &mut self.update_staged_focus,
            )
            .is_some()
            {
                self.show_update_staged_modal = false;
            }
            return;
        }

        let body = format!(
            "Diskoria {version} has been downloaded and is ready to install.\n\nInstall \
             it now, or leave it to install automatically when you close Diskoria."
        );
        match two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_update_staged_overlay"),
                dialog_id: Id::new("diskoria_update_staged_dialog"),
                width: 420.0,
                height: 220.0,
                title: "Update ready",
                body: &body,
                cancel_id: Id::new("diskoria_update_staged_later"),
                cancel_label: "Update on close",
                confirm_id: Id::new("diskoria_update_staged_now"),
                confirm: ModalConfirmPrimary::AccentIcon {
                    label: "Update now",
                    icon: '\u{f295}',
                },
            },
            &mut self.update_staged_focus,
        ) {
            Some(ModalConfirmResult::Cancel) => {
                // Stays staged; `App::exiting` runs it.
                self.show_update_staged_modal = false;
            }
            Some(ModalConfirmResult::Confirm) => {
                self.show_update_staged_modal = false;
                // Take it so the exit hook doesn't apply a second copy.
                if let Some(staged) = self.shared.take_staged_update() {
                    self.cancel_all_tests();
                    #[cfg(windows)]
                    crate::update::spawn_run_installer_and_exit(&staged);
                    #[cfg(target_os = "linux")]
                    if let Ok(exe) = std::env::current_exe() {
                        if let Err(e) =
                            crate::update::apply_update_now_and_restart(&staged, &exe)
                        {
                            self.show_update_alert = true;
                            self.update_alert_title = "Update failed".to_string();
                            self.update_alert_body = e;
                        }
                    }
                }
            }
            None => {}
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn draw_update_alert(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());
        let title = self.update_alert_title.clone();
        let body = self.update_alert_body.clone();
        if one_button_modal(
            ctx,
            &t,
            OneButtonModalParams {
                overlay_id: Id::new("diskoria_update_alert_overlay"),
                dialog_id: Id::new("diskoria_update_alert_dialog"),
                width: 400.0,
                height: 200.0,
                title: &title,
                body: &body,
                ok_id: Id::new("diskoria_update_alert_ok"),
                ok_label: "OK",
            },
            &mut self.update_alert_focus,
        )
        .is_some()
        {
            self.show_update_alert = false;
            self.update_alert_title.clear();
            self.update_alert_body.clear();
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn draw_update_busy_overlay(&self, ctx: &egui::Context, dark: bool) {
        if !self.update_check_busy && !self.update_download_busy {
            return;
        }
        let t = Theme::new(dark, self.shared.accent_color());
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(LayerId::new(
            Order::Foreground,
            Id::new("diskoria_update_busy_overlay"),
        ));
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
        let msg = if self.update_download_busy {
            "Downloading update…"
        } else {
            "Checking for updates…"
        };
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            msg,
            FontId::proportional(15.0),
            t.txt_pri,
        );
    }

    fn draw_about_page(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        let subtitle = "Version information and updates";
        crate::about::draw_about_header_row(self, ui, t, margin, content_x, content_w, "About");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let pad = (content_x + margin) - ui.min_rect().left();
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.label(
                egui::RichText::new(subtitle)
                    .size(14.0)
                    .color(t.txt_sec),
            );
        });
        ui.add_space(16.0);
        crate::about::draw_about(self, ui, t, margin, content_x, content_w);
    }

    fn speed_volume_pairs(drives: &[DetectedDrive]) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for (di, d) in drives.iter().enumerate() {
            for pi in crate::partition_info::benchmarkable_partitions(&d.partitions) {
                v.push((di, pi));
            }
        }
        v
    }

    /// The `(drive, partition)` pair the Benchmark page will test, or `None` when
    /// the shared drive selection names a disk with no mounted volume.
    ///
    /// KI-15: `selected_drive` is shared with Drive Health / Sector / Sector
    /// Write, so the Benchmark page **derives** its target rather than repointing
    /// the shared selection at the first drive that happens to have a volume. The
    /// partition — a Benchmark-only field — is clamped to what the drive has; the
    /// drive itself is never changed behind the user's back. When the selection
    /// has no volume the page says so and Start stays disabled (which is what
    /// `can_start_speed_test` always intended — the old repointing just made that
    /// branch unreachable).
    pub(crate) fn speed_target_pair(&self) -> Option<(usize, usize)> {
        speed_target(
            self.drives.len(),
            |i| crate::partition_info::benchmarkable_partitions(&self.drives[i].partitions),
            self.selected_drive,
            self.selected_speed_partition,
        )
    }

    fn pick_best_speed_partition_for_selected_drive(&mut self) {
        if self.drives.is_empty() {
            return;
        }
        let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
        if let Some(best) = self.drives[sel].best_speed_test_partition_index() {
            self.selected_speed_partition = best;
        }
    }

    fn reset_speed_results_display(&mut self) {
        self.speed_seq_read_mbps = -1.0;
        self.speed_seq_write_mbps = -1.0;
        self.speed_r4_read_mbps = -1.0;
        self.speed_r4_write_mbps = -1.0;
        self.speed_progress_pct = 0.0;
        self.speed_current_mbps = 0.0;
        self.speed_progress_op = "Ready".to_string();
    }

    /// `None` when the volume has no usable path (not mounted) — the caller
    /// must not start a benchmark it cannot place a file for (KI-38).
    fn speed_test_temp_path(mount_point: &str) -> Option<String> {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        #[cfg(windows)]
        {
            crate::speed_test::temp_path_windows(mount_point, id)
        }
        #[cfg(not(windows))]
        {
            crate::speed_test::temp_path_unix(mount_point, id)
        }
    }

    fn can_start_speed_test(&self) -> bool {
        if self.surface_test_running {
            return false;
        }
        if self.drives.is_empty() || self.drives_loading {
            return false;
        }
        // `None` = the shared selection names a disk with no mounted volume.
        let Some((di, pi)) = self.speed_target_pair() else {
            return false;
        };
        !self.drives[di].partitions[pi].is_encryption_locked()
    }

    fn start_speed_test(&mut self, ctx: &egui::Context) {
        {
            if !self.can_start_speed_test() {
                return;
            }
            // `can_start_speed_test` already rejected a target-less selection.
            let Some((sel, pi)) = self.speed_target_pair() else {
                return;
            };
            let d = &self.drives[sel];
            let disk_number = d.disk_number;
            let letter = d.partitions[pi].mount_point.clone();
            // Last line of defence: the target selection already requires a
            // mounted volume, so a missing path means the mount vanished
            // between the click and here.
            let Some(path) = Self::speed_test_temp_path(&letter) else {
                self.speed_error_msg = Some(
                    "That volume is no longer mounted — nothing to benchmark.".to_string(),
                );
                return;
            };
            let profile = speed_test::speed_profile_for_drive(d.media, d.bus);
            self.reset_speed_results_display();
            self.speed_progress_op = "Starting…".to_string();
            let cancel = Arc::new(AtomicBool::new(false));
            self.speed_test_cancel = Some(cancel.clone());
            let (tx, rx) = mpsc::channel();
            self.speed_test_rx = Some(rx);
            self.speed_test_running = true;
            self.speed_test_target = Some((disk_number, letter.clone()));
            self.speed_focus = Some(0);
            let _jh = speed_test::spawn_speed_test(path, profile, cancel, tx, ctx.clone());
            drop(_jh);
        }
    }

    fn stop_speed_test(&mut self) {
        if let Some(c) = &self.speed_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        self.speed_test_running = false;
        self.speed_test_rx = None;
        self.speed_test_cancel = None;
        self.speed_test_target = None;
        self.speed_progress_op = "Stopped".to_string();
        self.speed_focus = Some(2);
    }

    // ── Pro-Monitoring ────────────────────────────────────────────────────────

    #[cfg(any(windows, target_os = "linux"))]
    fn start_monitor_if_not_running(&mut self, ctx: &egui::Context) {
        // The demo drives do not exist; polling them would read the host's real
        // disks under invented serials and write that into the history DB.
        if crate::demo::seeding() {
            return;
        }
        if !self.shared.pro_edition {
            return;
        }
        if self.shared.monitor_is_running() {
            return;
        }
        if self.drives.is_empty() {
            return;
        }
        if !self.monitoring_enabled {
            return;
        }
        // Load historical temperature data from the SQLite DB before starting the thread.
        self.load_history_from_db();

        let poll_secs = (self.poll_interval_mins as u64) * 60;
        let (tx, rx) = mpsc::channel();
        let cancel = crate::monitor::spawn_monitor_thread(
            self.drives.clone(),
            tx,
            ctx.clone(),
            std::time::Duration::from_secs(poll_secs),
            self.alert_temp_warn,
            self.alert_temp_critical,
            self.alert_wear_threshold,
        );
        // Only internal drives are polled (see monitor::spawn_monitor_thread);
        // logging the full list overstated it on machines with USB drives.
        let drive_count = self
            .drives
            .iter()
            .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
            .count();
        self.shared.set_monitor_running(rx, cancel);
        log::info!(
            target: "diskoria::monitor",
            "Monitor thread started for {} drive(s), poll interval {}min",
            drive_count,
            self.poll_interval_mins
        );
    }

    /// Load up to 7 days of temperature history from SQLite into `temp_history`.
    #[cfg(any(windows, target_os = "linux"))]
    fn load_history_from_db(&mut self) {
        match crate::history_db::open_or_create() {
            Ok(conn) => {
                for drive in &self.drives {
                    match crate::history_db::query_temperature_history(&conn, &drive.serial, 7 * 24) {
                        Ok(rows) => {
                            let points: Vec<[f64; 2]> = rows
                                .into_iter()
                                .map(|(ts, t)| [ts as f64, t as f64])
                                .collect();
                            log::info!(
                                target: "diskoria::monitor",
                                "Loaded {} history points from DB for {}",
                                points.len(),
                                drive.serial
                            );
                            if !points.is_empty() {
                                self.temp_history
                                    .entry(drive.serial.clone())
                                    .or_default()
                                    .extend(points);
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                target: "diskoria::monitor",
                                "Failed to query history for {}: {e}",
                                drive.serial
                            );
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!(target: "diskoria::monitor", "Failed to open history DB for load: {e}");
            }
        }
    }

    /// Suppress all alerts for `serial` for the given duration.
    /// Also clears any stale entries for other serials while we're here.
    #[cfg(any(windows, target_os = "linux"))]
    pub fn suppress_drive_alerts(&mut self, serial: &str, secs: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        self.alert_suppressions.insert(serial.to_string(), until);
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn is_alert_suppressed(&self, serial: &str) -> bool {
        self.alert_suppressions
            .get(serial)
            .map(|&until| std::time::Instant::now() < until)
            .unwrap_or(false)
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn poll_monitor(&mut self, _ctx: &egui::Context) {
        let (msgs, still_connected) = self.shared.drain_monitor_rx();
        if !still_connected && !msgs.is_empty() {
            log::warn!(target: "diskoria::monitor", "Monitor channel disconnected");
        } else if !still_connected {
            // Connection was already closed and nothing was drained — nothing to do.
            return;
        }
        let now_f64 = chrono::Utc::now().timestamp() as f64;

        for msg in msgs {
            match msg {
                crate::monitor::MonitorMsg::Snapshots(snaps) => {
                    for snap in snaps {
                        log::info!(
                            target: "diskoria::monitor",
                            "poll_monitor: received snapshot serial={:?} temp={:?}",
                            snap.serial,
                            snap.temp_c
                        );
                        // Send tray icon temperature update.
                        if let Some(proxy) = &self.event_proxy {
                            let _ = proxy.send_event(crate::UserEvent::TrayIconUpdate {
                                serial: snap.serial.clone(),
                                temp_c: snap.temp_c,
                            });
                        }
                        // Append temperature history point to master map.
                        if let Some(t) = snap.temp_c {
                            let point = [snap.timestamp_unix as f64, t as f64];
                            self.temp_history
                                .entry(snap.serial.clone())
                                .or_default()
                                .push(point);
                        }
                        self.shared.insert_snapshot(snap);
                    }
                    // Trim to 7-day window (keeps memory bounded).
                    for pts in self.temp_history.values_mut() {
                        pts.retain(|p| p[0] >= now_f64 - 604_800.0);
                    }
                }
                crate::monitor::MonitorMsg::AlertFired(alert) => {
                    log::warn!(
                        target: "diskoria::monitor",
                        "Alert [{:?}] on {}: {}",
                        alert.level,
                        alert.model,
                        alert.detail
                    );
                    if self.is_alert_suppressed(&alert.serial) {
                        log::info!(
                            target: "diskoria::monitor",
                            "Alert suppressed for drive {}", alert.serial
                        );
                    } else {
                        // Flash the drive's tray icon to signal the alert.
                        if let Some(proxy) = &self.event_proxy {
                            let is_critical = matches!(
                                alert.level,
                                crate::alert_engine::AlertLevel::Critical
                            );
                            let _ = proxy.send_event(crate::UserEvent::DriveAlert {
                                serial: alert.serial.clone(),
                                is_critical,
                            });
                        }
                        self.pending_alerts.push(alert);
                    }
                }
            }
        }

        // Fire toasts for pending alerts (must run on a separate thread — WinRT is MTA).
        let alerts = std::mem::take(&mut self.pending_alerts);
        for alert in alerts {
            std::thread::spawn(move || {
                let title = format!(
                    "Diskoria — Drive {} ({})",
                    alert.model,
                    match alert.level {
                        crate::alert_engine::AlertLevel::Critical => "Critical",
                        crate::alert_engine::AlertLevel::Warning => "Warning",
                    }
                );
                crate::toast::send_toast(&title, &alert.detail);
            });
        }
    }

    fn poll_speed_test(&mut self, ctx: &egui::Context) {
        if self.speed_test_rx.is_none() {
            return;
        }
        loop {
            let msg = match self
                .speed_test_rx
                .as_ref()
                .unwrap()
                .try_recv()
            {
                Ok(m) => m,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.speed_test_rx = None;
                    break;
                }
            };
            match msg {
                SpeedTestMsg::Progress(p) => {
                    self.speed_progress_op = p.current_operation.clone();
                    self.speed_progress_pct = p.progress_percent;
                    self.speed_current_mbps = p.current_speed_mbps;
                    ctx.request_repaint();
                }
                SpeedTestMsg::Done(r) => {
                    self.speed_test_rx = None;
                    self.speed_test_cancel = None;
                    self.speed_test_running = false;
                    self.speed_test_target = None;
                    self.speed_seq_read_mbps = r.sequential_read_mbps;
                    self.speed_seq_write_mbps = r.sequential_write_mbps;
                    self.speed_r4_read_mbps = r.random_4k_read_mbps;
                    self.speed_r4_write_mbps = r.random_4k_write_mbps;
                    if r.completed {
                        self.speed_progress_op = "Complete".to_string();
                        self.speed_progress_pct = 100.0;
                    } else if let Some(e) = r.error_message {
                        if e != "Cancelled" {
                            self.speed_error_msg = Some(format!("Speed test failed:\n\n{e}"));
                        }
                        self.speed_progress_op = "Error".to_string();
                    } else {
                        self.speed_progress_op = "Stopped".to_string();
                    }
                    ctx.request_repaint();
                    break;
                }
            }
        }
    }

    fn check_speed_test_after_enum(&mut self, ctx: &egui::Context) {
        if !self.speed_test_running {
            return;
        }
        let Some((num, letter)) = self.speed_test_target.clone() else {
            return;
        };
        let still_disk = self.drives.iter().any(|d| d.disk_number == num);
        if !still_disk {
            if let Some(c) = &self.speed_test_cancel {
                c.store(true, Ordering::SeqCst);
            }
            self.speed_test_running = false;
            self.speed_test_rx = None;
            self.speed_test_cancel = None;
            self.speed_test_target = None;
            self.speed_drive_removed_msg = Some(
                "The drive was removed during the speed test. Test has been stopped.".to_string(),
            );
            ctx.request_repaint();
            return;
        }
        let Some(d) = self.drives.iter().find(|d| d.disk_number == num) else {
            return;
        };
        let still_vol = d
            .partitions
            .iter()
            .any(|p| p.mount_point.eq_ignore_ascii_case(&letter));
        if !still_vol {
            if let Some(c) = &self.speed_test_cancel {
                c.store(true, Ordering::SeqCst);
            }
            self.speed_test_running = false;
            self.speed_test_rx = None;
            self.speed_test_cancel = None;
            self.speed_test_target = None;
            self.speed_drive_removed_msg = Some(
                "The volume was no longer available during the speed test. Test has been stopped."
                    .to_string(),
            );
            ctx.request_repaint();
        }
    }

    /// Performance-chart export prompt: "Include health report?" Confirm → HTML
    /// (chart embedded in a fresh Drive Health report), Cancel ("Chart only") →
    /// PNG, Esc → abort the whole export. Only drawn while a request is staged.
        fn draw_chart_export_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        // The modal maps Esc to Cancel ("Chart only"); intercept it first so Esc
        // means a full abort instead.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.pending_chart_export = None;
            self.chart_export_focus = None;
            return;
        }
        let t = Theme::new(dark, self.shared.accent_color());
        let result = two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_chart_export_overlay"),
                dialog_id: Id::new("diskoria_chart_export_dialog"),
                width: 430.0,
                height: 190.0,
                title: "Include health report?",
                body: "Embed this chart in a full Drive Health report (HTML), or save just the chart image (PNG).",
                cancel_id: Id::new("diskoria_chart_export_chartonly"),
                cancel_label: "Chart only",
                confirm_id: Id::new("diskoria_chart_export_include"),
                confirm: ModalConfirmPrimary::AccentIcon {
                    label: "Include report",
                    icon: '\u{f3e9}', // Bootstrap Icons: filetype-html
                },
            },
            &mut self.chart_export_focus,
        );
        match result {
            Some(ModalConfirmResult::Confirm) => {
                self.chart_export_focus = None;
                if let Some(req) = self.pending_chart_export.take() {
                    req.spawn(true);
                }
            }
            Some(ModalConfirmResult::Cancel) => {
                self.chart_export_focus = None;
                if let Some(req) = self.pending_chart_export.take() {
                    req.spawn(false);
                }
            }
            None => {}
        }
    }

    fn update_modal_confirm_tab_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.show_stop_test_confirm {
            if self.modal_two_button_focus.is_none() {
                self.modal_two_button_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.modal_two_button_focus, 2);
            self.destructive_start_confirm_focus = None;
            self.destructive_stop_confirm_focus = None;
            #[cfg(any(windows, target_os = "linux"))]
            {
                self.update_staged_focus = None;
                self.update_alert_focus = None;
            }
            return;
        }
        self.modal_two_button_focus = None;

        if self.show_destructive_start_confirm {
            if self.destructive_start_confirm_focus.is_none() {
                self.destructive_start_confirm_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.destructive_start_confirm_focus, 2);
            self.destructive_stop_confirm_focus = None;
            #[cfg(any(windows, target_os = "linux"))]
            {
                self.update_staged_focus = None;
                self.update_alert_focus = None;
            }
            return;
        }
        self.destructive_start_confirm_focus = None;

        if self.show_destructive_stop_confirm {
            if self.destructive_stop_confirm_focus.is_none() {
                self.destructive_stop_confirm_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.destructive_stop_confirm_focus, 2);
            #[cfg(any(windows, target_os = "linux"))]
            {
                self.update_staged_focus = None;
                self.update_alert_focus = None;
            }
            return;
        }
        self.destructive_stop_confirm_focus = None;

        #[cfg(any(windows, target_os = "linux"))]
        {
            if self.pending_chart_export.is_some() {
                if self.chart_export_focus.is_none() {
                    self.chart_export_focus = Some(0);
                }
                tab_cycle_slots(ctx, &mut self.chart_export_focus, 2);
                self.update_staged_focus = None;
                self.update_alert_focus = None;
                return;
            }
            self.chart_export_focus = None;

            if self.show_update_staged_modal {
                if self.update_staged_focus.is_none() {
                    self.update_staged_focus = Some(0);
                }
                // One button while a test is running, two otherwise.
                let slots = if self.any_test_running() { 1 } else { 2 };
                tab_cycle_slots(ctx, &mut self.update_staged_focus, slots);
                self.update_alert_focus = None;
                return;
            }
            self.update_staged_focus = None;

            if self.show_update_alert {
                if self.update_alert_focus.is_none() {
                    self.update_alert_focus = Some(0);
                }
                tab_cycle_slots(ctx, &mut self.update_alert_focus, 1);
                return;
            }
            self.update_alert_focus = None;
        }
    }

    fn draw_stop_test_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());
        match two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_stop_test_overlay"),
                dialog_id: Id::new("diskoria_stop_test_dialog"),
                width: 360.0,
                height: 160.0,
                title: "Stop sector test?",
                body: "The scan will be aborted. Progress on the map reflects reads so far.",
                cancel_id: Id::new("diskoria_stop_test_no"),
                cancel_label: "No",
                confirm_id: Id::new("diskoria_stop_test_yes"),
                confirm: ModalConfirmPrimary::Danger {
                    label: "Yes, Stop",
                },
            },
            &mut self.modal_two_button_focus,
        ) {
            Some(ModalConfirmResult::Cancel) => {
                self.show_stop_test_confirm = false;
            }
            Some(ModalConfirmResult::Confirm) => {
                self.show_stop_test_confirm = false;
                self.stop_surface_test_user();
            }
            None => {}
        }
    }

    fn sector_primary_id() -> Id {
        Id::new("diskoria_sector_primary")
    }

    /// Healthy / Failing / Disabled — vivid on both themes (status words only).
    fn smart_health_label_colors(dark: bool) -> (Color32, Color32, Color32) {
        if dark {
            (
                Color32::from_rgb(105, 240, 174), // bright mint
                Color32::from_rgb(255, 82, 82),   // vivid red
                Color32::from_rgb(224, 224, 224), // light gray on dark bg
            )
        } else {
            (
                Color32::from_rgb(0, 200, 83),   // strong green
                Color32::from_rgb(213, 0, 0),   // strong red
                Color32::from_rgb(120, 120, 120), // readable gray
            )
        }
    }

    fn draw_smart_health_card(
        &self,
        ui: &mut Ui,
        t: &Theme,
        dark: bool,
        content_x: f32,
        margin: f32,
        section_w: f32,
    ) {
        if self.drives_loading && self.drives.is_empty() {
            return;
        }

        ui.add_space(16.0);

        let (c_ok, c_fail, c_dis) = Self::smart_health_label_colors(dark);
        let pad = 16.0_f32;
        let inner_w = section_w - pad * 2.0;

        // NVMe/UFS drives don't expose ATA SMART predict-fail; for them we show a
        // health % (100 − wear) under a "Drive Health" label instead of
        // "S.M.A.R.T. Status". SATA/USB keep the SMART status as before.
        let sel_bus = if self.drives.is_empty() {
            None
        } else {
            Some(self.drives[self.active_page_selected_drive_idx()].bus)
        };
        let nvme_like = matches!(sel_bus, Some(BusKind::Nvme) | Some(BusKind::Ufs));

        let (label_text, status_str, status_col, reason_lines) = if self.drives.is_empty() {
            ("S.M.A.R.T. Status:", "—".to_string(), t.txt_sec, None)
        } else if self.smart_health_inflight && self.smart_health.is_none() {
            let lbl = if nvme_like { "Drive Health:" } else { "S.M.A.R.T. Status:" };
            (lbl, "Loading…".to_string(), t.txt_sec, None)
        } else if nvme_like {
            if let Some(pct) = self.smart_health_pct {
                let col = if pct >= 50 {
                    c_ok
                } else if pct >= 20 {
                    Color32::from_rgb(241, 196, 15)
                } else {
                    c_fail
                };
                // Surface any WMI predictive-failure reasons alongside the %.
                let reasons = match &self.smart_health {
                    Some(SmartHealth::Failing { reasons }) => Some(reasons),
                    _ => None,
                };
                ("Drive Health:", format!("{pct}% remaining"), col, reasons)
            } else {
                ("Drive Health:", "Unavailable".to_string(), c_dis, None)
            }
        } else {
            match &self.smart_health {
                Some(SmartHealth::Healthy) => {
                    ("S.M.A.R.T. Status:", "Healthy".to_string(), c_ok, None)
                }
                Some(SmartHealth::Failing { reasons }) => {
                    ("S.M.A.R.T. Status:", "Failing".to_string(), c_fail, Some(reasons))
                }
                Some(SmartHealth::Disabled) => {
                    ("S.M.A.R.T. Status:", "Unavailable".to_string(), c_dis, None)
                }
                None => ("S.M.A.R.T. Status:", "…".to_string(), t.txt_sec, None),
            }
        };

        const SMART_LABEL_DRIVE_LETTERS: &str = "Drive Letters: ";
        const SMART_LABEL_PARTITION_STYLE: &str = "Partition Style: ";

        let smart_drive_lines: Option<(String, String)> = if self.drives.is_empty() {
            None
        } else {
            let sel = self.active_page_selected_drive_idx();
            let d = &self.drives[sel];
            Some((
                d.mounts_display(),
                d.partition_style.as_str().to_string(),
            ))
        };
        let header_row_h = 22.0_f32;

        // Every line here is font-measured, so the card is grown as each galley
        // is painted rather than laid out twice to pre-compute a height (KI-18).
        let mut card = CardLayout::builder(content_x + margin, section_w)
            .gap_before(0.0)
            .begin(ui, t);
        let left_x = card.inner_x();

        if let Some((letters, style)) = smart_drive_lines {
            let j1 = smart_health_kv_line_job(
                SMART_LABEL_DRIVE_LETTERS,
                &letters,
                inner_w,
                t.txt_pri,
            );
            let g1 = ui.ctx().fonts(|f| f.layout_job(j1));
            let h1 = g1.rect.height();
            ui.painter().add(egui::Shape::galley(
                Pos2::new(left_x, card.row(h1).top()),
                g1,
                t.txt_pri,
            ));
            card.add_gap(6.0);
            let j2 = smart_health_kv_line_job(
                SMART_LABEL_PARTITION_STYLE,
                &style,
                inner_w,
                t.txt_pri,
            );
            let g2 = ui.ctx().fonts(|f| f.layout_job(j2));
            let h2 = g2.rect.height();
            ui.painter().add(egui::Shape::galley(
                Pos2::new(left_x, card.row(h2).top()),
                g2,
                t.txt_pri,
            ));
            // The status row below is centered in a 22px header row, which adds
            // ~2px of padding above its text; use a smaller gap here so the
            // Partition-Style → status spacing matches the 6px line spacing above.
            card.add_gap(4.0);
        }

        let label_font = FontId::new(14.0, FontFamily::Name("InterBold".into()));
        let row_cy = card.row(header_row_h).center().y;
        ui.painter().text(
            Pos2::new(left_x, row_cy),
            Align2::LEFT_CENTER,
            label_text,
            label_font.clone(),
            t.txt_pri,
        );

        let label_w = ui.ctx().fonts(|f| {
            f.layout_no_wrap(label_text.to_string(), label_font.clone(), t.txt_pri)
                .rect
                .width()
        });
        let gap = 8.0_f32;
        ui.painter().text(
            Pos2::new(left_x + label_w + gap, row_cy),
            Align2::LEFT_CENTER,
            status_str,
            FontId::proportional(15.0),
            status_col,
        );

        if let Some(rs) = reason_lines {
            if !rs.is_empty() {
                card.add_gap(8.0);
            }
            for s in rs {
                let line = format!("• {}", s);
                let galley = ui.ctx().fonts(|f| {
                    f.layout(line, FontId::proportional(13.0), t.txt_sec, inner_w)
                });
                let gh = galley.rect.height();
                ui.painter().add(egui::Shape::galley(
                    Pos2::new(left_x, card.row(gh).top()),
                    galley,
                    t.txt_sec,
                ));
                card.add_gap(2.0);
            }
        }

        if let Some(ref e) = self.smart_health_err {
            let galley = ui.ctx().fonts(|f| {
                f.layout(e.clone(), FontId::proportional(12.0), t.txt_sec, inner_w)
            });
            let gh = galley.rect.height();
            card.add_gap(4.0);
            ui.painter().add(egui::Shape::galley(
                Pos2::new(left_x, card.row(gh).top()),
                galley,
                t.txt_sec,
            ));
        }

        card.end(ui);
    }

    /// Tab order + slot clamping (runs before UI so the same frame’s paint matches focus).
    fn prepare_sector_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 1 {
            self.sector_focus = None;
            return;
        }

        let slots = if self.surface_test_running { 1 } else { 3 };
        if self.surface_test_running {
            if self.sector_focus.is_some_and(|s| s > 0) {
                self.sector_focus = Some(0);
            }
        } else if self.sector_focus.is_some_and(|s| s > 2) {
            self.sector_focus = Some(2);
        }

        tab_cycle_slots(ctx, &mut self.sector_focus, slots);
    }

    /// `request_focus` / popup close / event filters — must run **after** central panel so widget ids
    /// match the ComboBox buttons drawn this frame.
    fn apply_sector_page_focus_bindings(&mut self, ctx: &egui::Context) {
        use crate::focus::{bind_combo_focus_slot, bind_text_focus_slot};

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 1 {
            return;
        }

        if self.surface_test_running {
            bind_text_focus_slot(ctx, self.sector_focus, 0, Some(Self::sector_primary_id()));
        } else {
            bind_text_focus_slot(ctx, self.sector_focus, 0, self.sector_refresh_id);
            bind_combo_focus_slot(ctx, self.sector_focus, 1, self.sector_combo_id);
            bind_text_focus_slot(ctx, self.sector_focus, 2, Some(Self::sector_primary_id()));
        }

        let (manual_id, is_combo_slot) = if self.surface_test_running {
            match self.sector_focus {
                Some(0) => (Some(Self::sector_primary_id()), false),
                _ => (None, false),
            }
        } else {
            match self.sector_focus {
                Some(0) => (self.sector_refresh_id, false),
                Some(1) => (self.sector_combo_id, true),
                Some(2) => (Some(Self::sector_primary_id()), false),
                _ => (None, false),
            }
        };
        apply_manual_focus_event_filter(ctx, manual_id, !is_combo_slot);
    }

    fn prepare_speed_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 3 {
            self.speed_focus = None;
            return;
        }

        let last_idle_slot = 2usize;

        let slots = if self.speed_test_running {
            1
        } else {
            last_idle_slot + 1
        };

        if self.speed_test_running {
            if self.speed_focus.is_some_and(|s| s > 0) {
                self.speed_focus = Some(0);
            }
        } else if self
            .speed_focus
            .is_some_and(|s| s > last_idle_slot)
        {
            self.speed_focus = Some(last_idle_slot);
        }

        tab_cycle_slots(ctx, &mut self.speed_focus, slots);
    }

    fn apply_speed_page_focus_bindings(&mut self, ctx: &egui::Context) {
        use crate::focus::{bind_combo_focus_slot, bind_text_focus_slot};

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 3 {
            return;
        }

        if self.speed_test_running {
            bind_text_focus_slot(ctx, self.speed_focus, 0, Some(Self::speed_primary_id()));
            let (manual_id, is_combo_slot) = match self.speed_focus {
                Some(0) => (Some(Self::speed_primary_id()), false),
                _ => (None, false),
            };
            apply_manual_focus_event_filter(ctx, manual_id, !is_combo_slot);
        } else {
            bind_text_focus_slot(ctx, self.speed_focus, 0, self.speed_refresh_id);
            bind_combo_focus_slot(ctx, self.speed_focus, 1, self.speed_volume_combo_id);
            bind_text_focus_slot(ctx, self.speed_focus, 2, Some(Self::speed_primary_id()));

            let (manual_id, is_combo_slot) = match self.speed_focus {
                Some(0) => (self.speed_refresh_id, false),
                Some(1) => (self.speed_volume_combo_id, true),
                Some(2) => (Some(Self::speed_primary_id()), false),
                _ => (None, false),
            };
            apply_manual_focus_event_filter(ctx, manual_id, !is_combo_slot);
        }
    }

    fn prepare_destructive_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 2 {
            self.destructive_focus = None;
            return;
        }

        if !self.destructive_unlocked {
            // Gate page: single slot for the unlock button.
            tab_cycle_slots(ctx, &mut self.destructive_focus, 1);
        } else if self.destructive_test_running {
            // Running: single slot for Stop.
            if self.destructive_focus.is_some_and(|s| s > 0) {
                self.destructive_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.destructive_focus, 1);
        } else {
            // Idle: 0=Refresh, 1=Combo, 2=Start.
            if self.destructive_focus.is_some_and(|s| s > 2) {
                self.destructive_focus = Some(2);
            }
            tab_cycle_slots(ctx, &mut self.destructive_focus, 3);
        }
    }

    fn apply_destructive_page_focus_bindings(&mut self, ctx: &egui::Context) {
        use crate::focus::{bind_combo_focus_slot, bind_text_focus_slot};

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 2 {
            return;
        }

        if !self.destructive_unlocked {
            bind_text_focus_slot(
                ctx,
                self.destructive_focus,
                0,
                Some(Self::destructive_unlock_id()),
            );
            let (manual_id, is_combo) = match self.destructive_focus {
                Some(0) => (Some(Self::destructive_unlock_id()), false),
                _ => (None, false),
            };
            apply_manual_focus_event_filter(ctx, manual_id, !is_combo);
        } else if self.destructive_test_running {
            bind_text_focus_slot(
                ctx,
                self.destructive_focus,
                0,
                Some(Self::destructive_primary_id()),
            );
            let (manual_id, is_combo) = match self.destructive_focus {
                Some(0) => (Some(Self::destructive_primary_id()), false),
                _ => (None, false),
            };
            apply_manual_focus_event_filter(ctx, manual_id, !is_combo);
        } else {
            bind_text_focus_slot(ctx, self.destructive_focus, 0, self.destructive_refresh_id);
            bind_combo_focus_slot(ctx, self.destructive_focus, 1, self.destructive_combo_id);
            bind_text_focus_slot(
                ctx,
                self.destructive_focus,
                2,
                Some(Self::destructive_primary_id()),
            );
            let (manual_id, is_combo) = match self.destructive_focus {
                Some(0) => (self.destructive_refresh_id, false),
                Some(1) => (self.destructive_combo_id, true),
                Some(2) => (Some(Self::destructive_primary_id()), false),
                _ => (None, false),
            };
            apply_manual_focus_event_filter(ctx, manual_id, !is_combo);
        }
    }

    pub(crate) fn spawn_drive_enumeration(&mut self, ctx: &egui::Context) {
        log::debug!(target: "diskoria", "spawn_drive_enumeration: starting WMI thread");
        self.surface_drive_removed_msg = None;
        self.speed_drive_removed_msg = None;
        self.drives_loading = true;
        self.refresh_min_until = Some(Instant::now() + std::time::Duration::from_millis(750));
        self.drives_error = None;
        self.smart_health_disk = None;
        self.shared.start_drive_enumeration(ctx);
    }

    /// Whether the refresh button should show its disabled/working state: either a
    /// scan is in flight, or the post-refresh minimum-visible window hasn't elapsed.
    pub(crate) fn refresh_busy(&self) -> bool {
        self.drives_loading
            || self
                .refresh_min_until
                .is_some_and(|t| Instant::now() < t)
    }

    fn poll_drive_enumeration(&mut self, ctx: &egui::Context) {
        // Watchdog: a WMI enumeration can occasionally hang (e.g. the BitLocker
        // query). Without this the loading spinner would spin forever. Only the
        // window showing the spinner gives up; a late result is discarded.
        if self.drives_loading && self.shared.drive_poll_timed_out(DRIVE_ENUM_TIMEOUT) {
            log::warn!(target: "diskoria", "poll_drive_enumeration: scan timed out; abandoning");
            self.shared.cancel_drive_poll();
            self.drives_loading = false;
            let msg = "Drive scan timed out. Click Refresh to try again.".to_string();
            self.drives_error = Some(msg.clone());
            self.shared.drives_write(|d| {
                d.loading = false;
                d.error = Some(msg);
            });
            ctx.request_repaint();
            return;
        }

        // Drain the shared one-shot receiver. Whichever window reaches it first
        // writes the result into the shared list and bumps its generation; every
        // window then converges below. This means a second window draining the
        // receiver can no longer strand the window that asked for the refresh.
        if let Some(result) = self.shared.try_recv_drive_poll() {
            match result {
                Ok(drives) => {
                    let nums: Vec<u32> = drives.iter().map(|d| d.disk_number).collect();
                    log::info!(
                        target: "diskoria",
                        "poll_drive_enumeration: ok count={} disk_numbers={:?}",
                        drives.len(),
                        nums
                    );
                    // Only flag the tray for an icon rebuild when the drive set
                    // actually changes — a no-op rebuild would wipe the
                    // temperature rendering the monitor thread filled in.
                    let serials_new: Vec<String> =
                        drives.iter().map(|d| d.serial.clone()).collect();
                    let serials_changed = self.shared.drives_read(|d| {
                        let old: Vec<String> =
                            d.list.iter().map(|dd| dd.serial.clone()).collect();
                        old != serials_new
                    });
                    self.shared.drives_write(|d| {
                        d.list = drives;
                        d.loading = false;
                        d.error = None;
                        d.generation = d.generation.wrapping_add(1);
                    });
                    if serials_changed {
                        self.shared.mark_drive_icons_dirty();
                    }
                }
                Err(e) => {
                    log::warn!(target: "diskoria", "poll_drive_enumeration: error {e}");
                    self.shared.cancel_drive_poll();
                    self.drives_error = Some(e.clone());
                    self.drives_loading = false;
                    self.shared.drives_write(|d| {
                        d.error = Some(e);
                        d.loading = false;
                    });
                    ctx.request_repaint();
                    return;
                }
            }
        }

        // Converge this window's local copy whenever the shared list is newer
        // than what we've applied — covers our own Refresh, another window's
        // Refresh, and the background auto-refresh from device-change events.
        let shared_gen = self.shared.drives_generation();
        if shared_gen == self.drives_generation {
            return;
        }
        let had_no_drives = self.drives.is_empty();
        self.drives = self.shared.drives_read(|d| d.list.clone());
        self.drives_generation = shared_gen;
        self.selected_drive = self.selected_drive.min(self.drives.len().saturating_sub(1));
        // No speed-partition fixup here: `speed_target_pair` clamps on read, so a
        // refresh that shrinks or unmounts a volume can't silently rewrite the
        // Benchmark selection (KI-15).
        if had_no_drives {
            self.pick_best_speed_partition_for_selected_drive();
        }
        self.check_surface_test_drive_after_enum(ctx);
        self.check_destructive_test_drive_after_enum(ctx);
        self.check_speed_test_after_enum(ctx);
        self.drives_error = None;
        self.drives_loading = false;
        self.smart_health_disk = None;
        #[cfg(any(windows, target_os = "linux"))]
        self.start_monitor_if_not_running(ctx);
        ctx.request_repaint();
    }

    fn poll_smart_health(&mut self, ctx: &egui::Context) {
        {
            if let Some(rx) = self.smart_health_rx.take() {
                match rx.try_recv() {
                    Ok((disk, result, health_pct)) => {
                        self.smart_health_inflight = false;
                        let current = self
                            .drives
                            .get(self.active_page_selected_drive_idx())
                            .map(|d| d.disk_number);
                        if current != Some(disk) {
                            self.smart_health_rx = None;
                            return;
                        }
                        match result {
                            Ok(h) => {
                                self.smart_health = Some(h);
                                self.smart_health_err = None;
                            }
                            Err(e) => {
                                self.smart_health = Some(SmartHealth::Disabled);
                                self.smart_health_err = Some(e);
                            }
                        }
                        self.smart_health_pct = health_pct;
                        self.smart_health_disk = Some(disk);
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        self.smart_health_rx = Some(rx);
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.smart_health_inflight = false;
                    }
                }
            }

            if self.drives_loading || self.drives.is_empty() {
                return;
            }

            let idx = self.active_page_selected_drive_idx();
            let drive = &self.drives[idx];
            let disk = drive.disk_number;

            // `--demo-health` (implied by any demo run): canned verdict rather
            // than a real health query against an invented device path.
            if crate::demo::config().health {
                if self.smart_health_disk == Some(disk) {
                    return;
                }
                let (health, pct) = crate::demo::wmi_health(idx);
                self.smart_health = Some(health);
                self.smart_health_err = None;
                self.smart_health_pct = pct;
                self.smart_health_disk = Some(disk);
                return;
            }

            if self.smart_health_inflight {
                return;
            }

            if self.smart_health_disk == Some(disk) {
                return;
            }

            self.smart_health_inflight = true;
            self.smart_health = None;
            self.smart_health_pct = None;
            self.smart_health_err = None;
            let pnp_id = drive.pnp_device_id.clone();
            let device_id = drive.device_id.clone();
            let bus = drive.bus;
            let (tx, rx) = mpsc::channel();
            self.smart_health_rx = Some(rx);
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let r = crate::smart_health::query_smart_health(disk, &pnp_id);
                // NVMe/UFS don't expose ATA SMART predict-fail; derive a health %
                // (100 − wear) from the device's health log instead.
                let health_pct = if matches!(bus, BusKind::Nvme | BusKind::Ufs) {
                    let report = crate::smart_reader::query_smart_detail(&device_id, bus);
                    crate::monitor::health_pct_from_report(&report)
                } else {
                    None
                };
                let _ = tx.send((disk, r, health_pct));
                ctx2.request_repaint();
            });
        }
    }

    fn check_surface_test_drive_after_enum(&mut self, ctx: &egui::Context) {
        if !self.surface_test_running {
            return;
        }
        let Some((num, _)) = self.surface_test_target else {
            log::debug!(
                target: "diskoria",
                "check_surface_test_drive_after_enum: running but no surface_test_target (unexpected)"
            );
            return;
        };
        // Match by disk number only; device_id strings are normalized in enumeration but this
        // avoids any WMI/format drift cancelling an in-flight test.
        let still = self.drives.iter().any(|d| d.disk_number == num);
        log::debug!(
            target: "diskoria",
            "check_surface_test_drive_after_enum: target_disk_number={} still_in_list={} current_disk_numbers={:?}",
            num,
            still,
            self.drives.iter().map(|d| d.disk_number).collect::<Vec<_>>()
        );
        if !still {
            log::warn!(
                target: "diskoria",
                "check_surface_test_drive_after_enum: drive {} missing after enum — cancelling test",
                num
            );
            if let Some(c) = &self.surface_test_cancel {
                c.store(true, Ordering::SeqCst);
            }
            self.surface_test_running = false;
            self.show_stop_test_confirm = false;
            self.surface_test_target = None;
            self.surface_test_started = None;
            self.surface_remaining_label = "--:--:--".to_string();
            self.surface_drive_removed_msg = Some(
                "The drive was removed during the test. Test has been stopped.".to_string(),
            );
            ctx.request_repaint();
            return;
        }
        if let Some(idx) = self.drives.iter().position(|d| d.disk_number == num) {
            self.selected_drive = idx;
        }
    }

    fn reset_sector_cells_for_new_test(&mut self) {
        self.test_result_overlay = None;
        self.fail_overlay_shown = false;
        for c in &mut self.sector_cells {
            *c = SectorCell::Pending;
        }
        self.heat_min_ms = f64::MAX;
        self.heat_max_ms = f64::MIN;
        self.surface_progress_pct = 0.0;
        self.surface_avg_speed_mbps = 0.0;
        self.surface_bad_sectors = 0;
        self.surface_slow_blocks = 0;
        self.surface_elapsed_label = "00:00:00".to_string();
        self.surface_remaining_label = "--:--:--".to_string();
        self.surface_last_error = None;
        self.surface_chart_points.clear();
        self.surface_chart_raw_points.clear();
        self.surface_chart_max_speed = 0.0;
        self.surface_chart_total_gb = 0.0;
        self.surface_chart_bucket_sum = 0.0;
        self.surface_chart_bucket_count = 0;
        self.surface_chart_bucket_idx = 0;
    }

    fn start_surface_test(&mut self, ctx: &egui::Context) {
        if self.drives.is_empty() {
            log::debug!(target: "diskoria", "start_surface_test: skipped (no drives)");
            return;
        }
        if self.surface_test_running {
            log::debug!(target: "diskoria", "start_surface_test: skipped (already running)");
            return;
        }
        let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
        let (disk_number, device_id) = {
            let d = &self.drives[sel];
            (d.disk_number, d.device_id.clone())
        };
        log::info!(
            target: "diskoria",
            "start_surface_test: selected_idx={} disk_number={} device_id={}",
            sel,
            disk_number,
            device_id
        );
        self.reset_sector_cells_for_new_test();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        self.surface_test_cancel = Some(cancel.clone());
        self.surface_test_rx = Some(rx);
        self.surface_test_running = true;
        self.surface_test_target = Some((disk_number, device_id.clone()));
        self.surface_test_started = Some(Instant::now());
        surface_test::spawn_surface_test(
            device_id,
            TOTAL_UI_BLOCKS as i32,
            cancel,
            tx,
        );
        self.surface_logged_decile = u8::MAX;
        log::debug!(target: "diskoria", "start_surface_test: worker spawned, running=true");
        self.sector_focus = Some(0);
        ctx.request_repaint();
    }

    fn stop_surface_test_user(&mut self) {
        log::info!(target: "diskoria", "stop_surface_test_user: user Stop / cancel");
        if let Some(c) = &self.surface_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        self.surface_test_running = false;
        self.surface_test_target = None;
        self.surface_test_started = None;
        self.sector_focus = Some(2);
        self.surface_remaining_label = "--:--:--".to_string();
    }

    // =========================================================================
    // Destructive test — start / stop / poll / finalize / drive removal check
    // =========================================================================

    fn reset_destructive_cells_for_new_test(&mut self) {
        self.test_result_overlay = None;
        self.fail_overlay_shown = false;
        for c in &mut self.destructive_cells {
            *c = SectorCell::Pending;
        }
        self.destructive_heat_min_ms = f64::MAX;
        self.destructive_heat_max_ms = f64::MIN;
        self.destructive_progress_pct = 0.0;
        self.destructive_avg_speed_mbps = 0.0;
        self.destructive_bad_sectors = 0;
        self.destructive_slow_blocks = 0;
        self.destructive_elapsed_label = "00:00:00".to_string();
        self.destructive_remaining_label = "--:--:--".to_string();
        self.destructive_last_error = None;
        self.destructive_chart_points.clear();
        self.destructive_chart_raw_points.clear();
        self.destructive_chart_max_speed = 0.0;
        self.destructive_chart_total_gb = 0.0;
        self.destructive_chart_bucket_sum = 0.0;
        self.destructive_chart_bucket_count = 0;
        self.destructive_chart_bucket_idx = 0;
    }

    fn start_destructive_test(&mut self, ctx: &egui::Context) {
        if self.drives.is_empty() {
            return;
        }
        if self.destructive_test_running {
            return;
        }
        let sel = self
            .selected_drive
            .min(self.drives.len().saturating_sub(1));
        let (disk_number, device_id, drive_letters) = {
            let d = &self.drives[sel];
            let letters: Vec<String> = d.partitions.iter().map(|p| p.mount_point.clone()).collect();
            (d.disk_number, d.device_id.clone(), letters)
        };
        log::info!(
            target: "diskoria",
            "start_destructive_test: idx={} disk_number={} device_id={} volumes={:?}",
            sel, disk_number, device_id, drive_letters
        );
        self.reset_destructive_cells_for_new_test();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        self.destructive_test_cancel = Some(cancel.clone());
        self.destructive_test_rx = Some(rx);
        self.destructive_test_running = true;
        self.destructive_test_target = Some((disk_number, device_id.clone()));
        self.destructive_test_started = Some(Instant::now());
        self.destructive_drive_removed_msg = None;
        crate::destructive_test::spawn_destructive_test(
            device_id,
            drive_letters,
            crate::destructive_test::TOTAL_UI_BLOCKS as i32,
            cancel,
            tx,
        );
        log::debug!(target: "diskoria", "start_destructive_test: worker spawned");
        self.destructive_focus = Some(0);
        ctx.request_repaint();
    }

    fn stop_destructive_test_user(&mut self) {
        log::info!(target: "diskoria", "stop_destructive_test_user: user cancelled");
        if let Some(c) = &self.destructive_test_cancel {
            c.store(true, Ordering::SeqCst);
        }
        self.destructive_test_running = false;
        self.destructive_test_target = None;
        self.destructive_test_started = None;
        self.destructive_focus = Some(2);
        self.destructive_remaining_label = "--:--:--".to_string();
    }

    fn recalibrate_destructive_heat_colors(&mut self) {
        let mut min_t = f64::MAX;
        let mut max_t = f64::MIN;
        for c in &self.destructive_cells {
            if let SectorCell::Heat(t) = *c {
                if t < crate::destructive_test::SLOW_THRESHOLD_MS && t > 0.0 {
                    min_t = min_t.min(t);
                    max_t = max_t.max(t);
                }
            }
        }
        if min_t < max_t && min_t < f64::MAX {
            self.destructive_heat_min_ms = min_t;
            self.destructive_heat_max_ms = max_t;
        }
    }

    fn apply_destructive_progress(&mut self, p: crate::destructive_test::DestructiveTestProgress) {
        self.destructive_progress_pct = p.progress_percent;
        self.destructive_avg_speed_mbps = p.average_speed_mbps;
        self.destructive_bad_sectors = p.bad_sectors;
        self.maybe_raise_fail_overlay(self.destructive_bad_sectors);

        let bi = p.block_index as usize;
        if bi < self.destructive_cells.len() {
            if !p.block_is_good {
                self.destructive_cells[bi] = SectorCell::Bad;
            } else if p.block_read_time_ms >= crate::destructive_test::SLOW_THRESHOLD_MS {
                // Count yellow blocks as they first appear so the "Slow blocks"
                // stat stays authoritative against the map (each block reports
                // once, monotonically — re-counting is guarded just in case).
                if !matches!(self.destructive_cells[bi], SectorCell::Slow) {
                    self.destructive_slow_blocks += 1;
                }
                self.destructive_cells[bi] = SectorCell::Slow;
            } else {
                self.destructive_cells[bi] = SectorCell::Heat(p.block_read_time_ms);
                if p.block_read_time_ms > 0.0
                    && p.block_read_time_ms < crate::destructive_test::SLOW_THRESHOLD_MS
                {
                    let mut range_changed = false;
                    if p.block_read_time_ms < self.destructive_heat_min_ms {
                        self.destructive_heat_min_ms = p.block_read_time_ms;
                        range_changed = true;
                    }
                    if p.block_read_time_ms > self.destructive_heat_max_ms {
                        self.destructive_heat_max_ms = p.block_read_time_ms;
                        range_changed = true;
                    }
                    if range_changed && p.block_index > 50 && p.block_index % 50 == 0 {
                        self.recalibrate_destructive_heat_colors();
                    }
                }
            }
        }

        if p.current_speed_mbps > 0.0 && p.total_bytes > 0 {
            let total_gb = p.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            if total_gb > self.destructive_chart_total_gb {
                self.destructive_chart_total_gb = total_gb;
            }
            let pos_gb = p.bytes_scanned as f64 / (1024.0 * 1024.0 * 1024.0);
            self.destructive_chart_raw_points.push([pos_gb, p.current_speed_mbps]);
            if p.current_speed_mbps > self.destructive_chart_max_speed {
                self.destructive_chart_max_speed = p.current_speed_mbps;
            }
            chart_bucket_accumulate(
                p.bytes_scanned, p.total_bytes, p.current_speed_mbps,
                &mut self.destructive_chart_points,
                &mut self.destructive_chart_bucket_sum,
                &mut self.destructive_chart_bucket_count,
                &mut self.destructive_chart_bucket_idx,
            );
        }

        if let Some(start) = self.destructive_test_started {
            let e = start.elapsed().as_secs();
            self.destructive_elapsed_label =
                format!("{:02}:{:02}:{:02}", e / 3600, (e % 3600) / 60, e % 60);
        }

        if p.progress_percent > 0.0 {
            if let Some(start) = self.destructive_test_started {
                let elapsed_s = start.elapsed().as_secs_f64();
                let pct = (p.progress_percent / 100.0).max(0.0001);
                let est_total_s = elapsed_s / pct;
                let rem_s = (est_total_s - elapsed_s).max(0.0);
                let h = (rem_s / 3600.0) as u64;
                let m = ((rem_s % 3600.0) / 60.0) as u64;
                let s = (rem_s % 60.0) as u64;
                self.destructive_remaining_label = format!("{h:02}:{m:02}:{s:02}");
            }
        }
    }

    fn finalize_destructive_test_on_completed(&mut self) {
        log::info!(
            target: "diskoria",
            "finalize_destructive_test_on_completed: progress_pct={:.2} bad={} slow_blocks={}",
            self.destructive_progress_pct,
            self.destructive_bad_sectors,
            self.destructive_slow_blocks
        );
        self.recalibrate_destructive_heat_colors();
        self.destructive_remaining_label = "00:00:00".to_string();
        let total_bytes = (self.destructive_chart_total_gb * 1024.0 * 1024.0 * 1024.0) as i64;
        chart_bucket_flush(
            total_bytes,
            &mut self.destructive_chart_points,
            &mut self.destructive_chart_bucket_sum,
            &mut self.destructive_chart_bucket_count,
            &mut self.destructive_chart_bucket_idx,
        );
        if self.destructive_progress_pct >= 99.9 {
            self.destructive_progress_pct = 100.0;
        }
        let cancelled = self
            .destructive_test_cancel
            .as_ref()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(false);
        if !cancelled {
            self.raise_completion_overlay(
                self.destructive_bad_sectors,
                self.destructive_slow_blocks,
            );
        }
    }

    fn poll_destructive_test(&mut self, ctx: &egui::Context) {
        use crate::destructive_test::DestructiveTestMsg;
        if self.destructive_test_rx.is_none() {
            return;
        }
        let mut batch = Vec::<DestructiveTestMsg>::new();
        {
            let rx = self.destructive_test_rx.as_ref().unwrap();
            loop {
                match rx.try_recv() {
                    Ok(m) => batch.push(m),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if batch.is_empty() {
                            log::warn!(
                                target: "diskoria",
                                "poll_destructive_test: channel disconnected with no messages"
                            );
                            self.destructive_test_rx = None;
                            self.destructive_test_running = false;
                            self.show_destructive_stop_confirm = false;
                            return;
                        }
                        break;
                    }
                }
            }
        }
        for msg in batch {
            match msg {
                DestructiveTestMsg::Progress(p) => {
                    self.apply_destructive_progress(p);
                    ctx.request_repaint();
                }
                DestructiveTestMsg::Error(e) => {
                    log::warn!(target: "diskoria", "poll_destructive_test: Error {e}");
                    self.destructive_last_error = Some(e);
                    self.destructive_test_running = false;
                    self.show_destructive_stop_confirm = false;
                    self.destructive_test_target = None;
                    self.destructive_test_started = None;
                    self.destructive_remaining_label = "--:--:--".to_string();
                    ctx.request_repaint();
                }
                DestructiveTestMsg::Completed => {
                    log::info!(
                        target: "diskoria",
                        "poll_destructive_test: Completed (had_error={})",
                        self.destructive_last_error.is_some()
                    );
                    if self.destructive_last_error.is_none() {
                        self.finalize_destructive_test_on_completed();
                    }
                    self.destructive_test_rx = None;
                    self.destructive_test_cancel = None;
                    self.destructive_test_running = false;
                    self.show_destructive_stop_confirm = false;
                    self.destructive_test_target = None;
                    self.destructive_test_started = None;
                    ctx.request_repaint();
                }
            }
        }
    }

    fn check_destructive_test_drive_after_enum(&mut self, ctx: &egui::Context) {
        if !self.destructive_test_running {
            return;
        }
        let Some((num, _)) = self.destructive_test_target else {
            return;
        };
        let still = self.drives.iter().any(|d| d.disk_number == num);
        if !still {
            log::warn!(
                target: "diskoria",
                "check_destructive_test_drive_after_enum: drive {} missing — cancelling",
                num
            );
            if let Some(c) = &self.destructive_test_cancel {
                c.store(true, Ordering::SeqCst);
            }
            self.destructive_test_running = false;
            self.show_destructive_stop_confirm = false;
            self.destructive_test_target = None;
            self.destructive_test_started = None;
            self.destructive_remaining_label = "--:--:--".to_string();
            self.destructive_drive_removed_msg = Some(
                "The drive was removed during the test. Test has been stopped.".to_string(),
            );
            ctx.request_repaint();
            return;
        }
        if let Some(idx) = self.drives.iter().position(|d| d.disk_number == num) {
            self.selected_drive = idx;
        }
    }

    fn draw_destructive_start_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());
        let sel = self
            .selected_drive
            .min(self.drives.len().saturating_sub(1));
        let drive_summary = self
            .drives
            .get(sel)
            .map(|d| d.summary.clone())
            .unwrap_or_default();
        let body = format!(
            "This test will overwrite every sector on:\n\n{drive_summary}\n\nAll data will be permanently lost. This cannot be undone."
        );
        match two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_dest_start_overlay"),
                dialog_id: Id::new("diskoria_dest_start_dialog"),
                width: 420.0,
                height: 200.0,
                title: "Destroy all data on this drive?",
                body: &body,
                cancel_id: Id::new("diskoria_dest_start_cancel"),
                cancel_label: "Cancel",
                confirm_id: Id::new("diskoria_dest_start_confirm"),
                confirm: ModalConfirmPrimary::Danger {
                    label: "Continue",
                },
            },
            &mut self.destructive_start_confirm_focus,
        ) {
            Some(ModalConfirmResult::Cancel) => {
                self.show_destructive_start_confirm = false;
            }
            Some(ModalConfirmResult::Confirm) => {
                self.show_destructive_start_confirm = false;
                self.start_destructive_test(ctx);
            }
            None => {}
        }
    }

    fn draw_destructive_stop_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());
        match two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_dest_stop_overlay"),
                dialog_id: Id::new("diskoria_dest_stop_dialog"),
                width: 380.0,
                height: 160.0,
                title: "Stop destructive test?",
                body: "The test will be aborted. Partially-overwritten sectors will not be verified.",
                cancel_id: Id::new("diskoria_dest_stop_cancel"),
                cancel_label: "No",
                confirm_id: Id::new("diskoria_dest_stop_confirm"),
                confirm: ModalConfirmPrimary::Danger {
                    label: "Yes, Stop",
                },
            },
            &mut self.destructive_stop_confirm_focus,
        ) {
            Some(ModalConfirmResult::Cancel) => {
                self.show_destructive_stop_confirm = false;
            }
            Some(ModalConfirmResult::Confirm) => {
                self.show_destructive_stop_confirm = false;
                self.stop_destructive_test_user();
            }
            None => {}
        }
    }

    fn recalibrate_sector_heat_colors(&mut self) {
        let mut min_t = f64::MAX;
        let mut max_t = f64::MIN;
        for c in &self.sector_cells {
            if let SectorCell::Heat(t) = *c {
                if t < surface_test::SLOW_THRESHOLD_MS && t > 0.0 {
                    min_t = min_t.min(t);
                    max_t = max_t.max(t);
                }
            }
        }
        if min_t < max_t && min_t < f64::MAX {
            self.heat_min_ms = min_t;
            self.heat_max_ms = max_t;
        }
    }

    /// Whether the PASS / WARN / FAIL result overlay is enabled in settings.
    fn test_overlays_enabled(&self) -> bool {
        self.shared.settings_snapshot().show_test_result_overlays
    }

    /// Dismiss the result overlay on any key press, early in the frame, and
    /// swallow the key/text events so the same press can't also activate a
    /// focused control (Start/Stop/etc.) underneath. Mouse dismissal is handled
    /// by the overlay's foreground area in [`Self::draw`].
    fn poll_test_result_overlay_input(&mut self, ctx: &egui::Context) {
        if self.test_result_overlay.is_none() {
            return;
        }
        let dismissed = ctx.input_mut(|i| {
            let pressed = i
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }));
            if pressed {
                i.events
                    .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
            }
            pressed
        });
        if dismissed {
            self.test_result_overlay = None;
            ctx.request_repaint();
        }
    }

    /// Raise the FAIL overlay once per run when the first bad block appears.
    fn maybe_raise_fail_overlay(&mut self, bad_sectors: i64) {
        if bad_sectors > 0 && !self.fail_overlay_shown && self.test_overlays_enabled() {
            self.test_result_overlay = Some(crate::test_result_overlay::TestResult::Fail);
            self.fail_overlay_shown = true;
        }
    }

    /// Raise PASS / WARN / FAIL when a *complete* scan finishes (not user-stopped).
    /// PASS = no bad and no slow blocks; WARN = slow but no bad; FAIL = any bad
    /// (only if a mid-scan FAIL was not already shown this run).
    fn raise_completion_overlay(&mut self, bad_sectors: i64, slow_blocks: i64) {
        use crate::test_result_overlay::TestResult;
        if !self.test_overlays_enabled() {
            return;
        }
        if bad_sectors > 0 {
            if !self.fail_overlay_shown {
                self.test_result_overlay = Some(TestResult::Fail);
                self.fail_overlay_shown = true;
            }
        } else if slow_blocks > 0 {
            self.test_result_overlay = Some(TestResult::Warn);
        } else {
            self.test_result_overlay = Some(TestResult::Pass);
        }
    }

    fn apply_surface_progress(&mut self, p: SurfaceTestProgress) {
        self.surface_progress_pct = p.progress_percent;
        self.surface_avg_speed_mbps = p.average_speed_mbps;
        self.surface_bad_sectors = p.bad_sectors;
        self.maybe_raise_fail_overlay(self.surface_bad_sectors);

        let bi = p.block_index as usize;
        if bi < self.sector_cells.len() {
            if !p.block_is_good {
                self.sector_cells[bi] = SectorCell::Bad;
            } else if p.block_read_time_ms >= surface_test::SLOW_THRESHOLD_MS {
                // Count yellow blocks as they first appear so the "Slow blocks"
                // stat stays authoritative against the map (each block reports
                // once, monotonically — re-counting is guarded just in case).
                if !matches!(self.sector_cells[bi], SectorCell::Slow) {
                    self.surface_slow_blocks += 1;
                }
                self.sector_cells[bi] = SectorCell::Slow;
            } else {
                self.sector_cells[bi] = SectorCell::Heat(p.block_read_time_ms);
                if p.block_is_good
                    && p.block_read_time_ms > 0.0
                    && p.block_read_time_ms < surface_test::SLOW_THRESHOLD_MS
                {
                    let mut range_changed = false;
                    if p.block_read_time_ms < self.heat_min_ms {
                        self.heat_min_ms = p.block_read_time_ms;
                        range_changed = true;
                    }
                    if p.block_read_time_ms > self.heat_max_ms {
                        self.heat_max_ms = p.block_read_time_ms;
                        range_changed = true;
                    }
                    if range_changed && p.block_index > 50 && p.block_index % 50 == 0 {
                        self.recalibrate_sector_heat_colors();
                    }
                }
            }
        }

        if p.current_speed_mbps > 0.0 && p.total_bytes > 0 {
            let total_gb = p.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            if total_gb > self.surface_chart_total_gb {
                self.surface_chart_total_gb = total_gb;
            }
            let pos_gb = p.bytes_scanned as f64 / (1024.0 * 1024.0 * 1024.0);
            self.surface_chart_raw_points.push([pos_gb, p.current_speed_mbps]);
            if p.current_speed_mbps > self.surface_chart_max_speed {
                self.surface_chart_max_speed = p.current_speed_mbps;
            }
            chart_bucket_accumulate(
                p.bytes_scanned, p.total_bytes, p.current_speed_mbps,
                &mut self.surface_chart_points,
                &mut self.surface_chart_bucket_sum,
                &mut self.surface_chart_bucket_count,
                &mut self.surface_chart_bucket_idx,
            );
        }

        if let Some(start) = self.surface_test_started {
            let e = start.elapsed().as_secs();
            self.surface_elapsed_label =
                format!("{:02}:{:02}:{:02}", e / 3600, (e % 3600) / 60, e % 60);
        }

        if p.progress_percent > 0.0 {
            if let Some(start) = self.surface_test_started {
                let elapsed_s = start.elapsed().as_secs_f64();
                let pct = (p.progress_percent / 100.0).max(0.0001);
                let est_total_s = elapsed_s / pct;
                let rem_s = (est_total_s - elapsed_s).max(0.0);
                let h = (rem_s / 3600.0) as u64;
                let m = ((rem_s % 3600.0) / 60.0) as u64;
                let s = (rem_s % 60.0) as u64;
                self.surface_remaining_label = format!("{h:02}:{m:02}:{s:02}");
            }
        }
    }

    fn finalize_surface_test_on_completed(&mut self) {
        log::info!(
            target: "diskoria",
            "finalize_surface_test_on_completed: progress_pct={:.2} bad_sectors={} slow_blocks={}",
            self.surface_progress_pct,
            self.surface_bad_sectors,
            self.surface_slow_blocks
        );
        self.recalibrate_sector_heat_colors();
        self.surface_remaining_label = "00:00:00".to_string();
        let total_bytes = (self.surface_chart_total_gb * 1024.0 * 1024.0 * 1024.0) as i64;
        chart_bucket_flush(
            total_bytes,
            &mut self.surface_chart_points,
            &mut self.surface_chart_bucket_sum,
            &mut self.surface_chart_bucket_count,
            &mut self.surface_chart_bucket_idx,
        );
        if self.surface_progress_pct >= 99.9 {
            self.surface_progress_pct = 100.0;
        }
        // A user-stopped scan is not "completely done", so only PASS/WARN on a
        // natural finish. (Mid-scan FAIL is already raised in apply_*_progress.)
        let cancelled = self
            .surface_test_cancel
            .as_ref()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(false);
        if !cancelled {
            self.raise_completion_overlay(self.surface_bad_sectors, self.surface_slow_blocks);
        }
    }

    fn poll_surface_test(&mut self, ctx: &egui::Context) {
        if self.surface_test_rx.is_none() {
            return;
        }
        let mut batch = Vec::<SurfaceTestMsg>::new();
        {
            let rx = self.surface_test_rx.as_ref().unwrap();
            loop {
                match rx.try_recv() {
                    Ok(m) => batch.push(m),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // After the last message, the queue is empty and the sender is dropped;
                        // the next try_recv is Disconnected — not "no messages ever".
                        if batch.is_empty() {
                            log::warn!(
                                target: "diskoria",
                                "poll_surface_test: channel disconnected with no messages (worker dropped sender early?)"
                            );
                            self.surface_test_rx = None;
                            self.surface_test_running = false;
                            self.show_stop_test_confirm = false;
                            return;
                        }
                        break;
                    }
                }
            }
        }
        for msg in batch {
            match msg {
                SurfaceTestMsg::Progress(p) => {
                    // A scan emits a progress message per UI block (1000 of
                    // them) and is polled every frame on top of that — logging
                    // either would bury everything else. One line per 10% is
                    // enough to see a run advancing; `trace` has the detail.
                    let decile = (p.progress_percent / 10.0) as u8;
                    if decile != self.surface_logged_decile {
                        self.surface_logged_decile = decile;
                        log::debug!(
                            target: "diskoria",
                            "surface scan: {:.0}% ({} good, {} bad, {} slow sectors, {:.0} MB/s)",
                            p.progress_percent,
                            p.good_sectors,
                            p.bad_sectors,
                            p.slow_sectors,
                            p.average_speed_mbps
                        );
                    }
                    log::trace!(
                        target: "diskoria",
                        "poll_surface_test: Progress pct={:.3} block={} bytes_scanned={}",
                        p.progress_percent,
                        p.block_index,
                        p.bytes_scanned
                    );
                    self.apply_surface_progress(p);
                    ctx.request_repaint();
                }
                SurfaceTestMsg::Error(e) => {
                    log::warn!(target: "diskoria", "poll_surface_test: Error {e}");
                    self.surface_last_error = Some(e);
                    self.surface_test_running = false;
                    self.show_stop_test_confirm = false;
                    self.surface_test_target = None;
                    self.surface_test_started = None;
                    self.surface_remaining_label = "--:--:--".to_string();
                    ctx.request_repaint();
                }
                SurfaceTestMsg::Completed => {
                    log::info!(
                        target: "diskoria",
                        "poll_surface_test: Completed (had_error={})",
                        self.surface_last_error.is_some()
                    );
                    if self.surface_last_error.is_none() {
                        self.finalize_surface_test_on_completed();
                    }
                    self.surface_test_rx = None;
                    self.surface_test_cancel = None;
                    self.surface_test_running = false;
                    self.show_stop_test_confirm = false;
                    self.surface_test_target = None;
                    self.surface_test_started = None;
                    ctx.request_repaint();
                }
            }
        }
    }

    fn blocks_content_interaction(&self) -> bool {
        let base = self.test_result_overlay.is_some()
            || self.show_stop_test_confirm
            || self.show_destructive_start_confirm
            || self.show_destructive_stop_confirm;
        let base = base || self.pending_chart_export.is_some();
        #[cfg(windows)]
        {
            base
                || self.show_update_alert
                || self.update_check_busy
                || self.update_download_busy
        }
        #[cfg(not(windows))]
        {
            base
        }
    }

    fn save_app_settings(&self) {
        // Sync per-window monitor-settings drafts into shared and persist.
        // Accent/theme fields already live in `shared.settings` and are saved
        // by every `update_settings` call; this covers the still-local monitor
        // fields (step 3 will migrate them too).
        self.shared.update_settings(|s| {
            #[cfg(not(windows))]
            let _ = s;
            #[cfg(windows)]
            {
                s.monitoring_enabled = self.monitoring_enabled;
                s.poll_interval_mins = self.poll_interval_mins;
                s.alert_temp_warn = self.alert_temp_warn;
                s.alert_temp_critical = self.alert_temp_critical;
                s.alert_wear_threshold = self.alert_wear_threshold;
            }
        });
    }

    fn settings_theme_slot_count(&self) -> usize {
        3 + 2
            + if self.shared.settings_snapshot().accent_source == AccentSourcePref::Palette {
                8
            } else {
                0
            }
            + 1
    }

    fn settings_monitoring_slot_start(&self) -> usize {
        self.settings_theme_slot_count()
    }

    fn settings_monitoring_slot_count(&self) -> usize {
        #[cfg(any(windows, target_os = "linux"))]
        {
            if self.shared.pro_edition {
                // toggle(1) + poll segments(4) + (if enabled: warn+crit+wear sliders(3) + test buttons(2))
                if self.monitoring_enabled { 10 } else { 5 }
            } else {
                0
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            0
        }
    }

    /// First tab slot of the Test Results card — appended after the Theme and
    /// (Pro) Monitoring sections. Layout: +0 toggle, +1 Pass, +2 Warn, +3 Fail.
    fn settings_test_overlay_slot(&self) -> usize {
        self.settings_theme_slot_count() + self.settings_monitoring_slot_count()
    }

    /// Slot of the single "Close to system tray" toggle — appended after the
    /// four Test Results slots. Windows-only (there is no tray elsewhere), but
    /// the slot index math is shared.
    fn settings_window_slot(&self) -> usize {
        self.settings_test_overlay_slot() + 4
    }

    fn settings_window_slot_count(&self) -> usize {
        #[cfg(windows)]
        {
            1
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// Slot of the single "Check for updates automatically" toggle.
    fn settings_updates_slot(&self) -> usize {
        self.settings_window_slot() + self.settings_window_slot_count()
    }

    fn settings_updates_slot_count(&self) -> usize {
        #[cfg(any(windows, target_os = "linux"))]
        {
            // A Windows portable build has no update path, so the row is shown
            // disabled and skipped in the tab order rather than trapping focus
            // on a control that cannot do anything. Everywhere else the
            // support check itself decides.
            if self.updates_supported() {
                1
            } else {
                0
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            0
        }
    }

    /// Slot of the single "Launch at startup" toggle — appended after the
    /// Updates card. Windows-only card, but the slot index math is shared.
    fn settings_startup_slot(&self) -> usize {
        self.settings_updates_slot() + self.settings_updates_slot_count()
    }

    fn settings_startup_slot_count(&self) -> usize {
        #[cfg(any(windows, target_os = "linux"))]
        {
            1
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            0
        }
    }

    fn settings_tab_slot_count(&self) -> usize {
        self.settings_startup_slot() + self.settings_startup_slot_count()
    }

    fn settings_hex_slot(&self) -> usize {
        if self.shared.settings_snapshot().accent_source == AccentSourcePref::Palette {
            13
        } else {
            5
        }
    }

    fn health_page_slot_count(&self) -> usize {
        #[cfg(windows)]
        let pro_slots = if self.shared.pro_edition { 5 } else { 0 };
        #[cfg(not(windows))]
        let pro_slots = 0_usize;
        3 + pro_slots
    }

    fn prepare_health_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;
        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 0 {
            self.health_focus = None;
            return;
        }
        let old = self.health_focus;
        let slots = self.health_page_slot_count();
        tab_cycle_slots(ctx, &mut self.health_focus, slots);
        if self.health_focus != old {
            self.scroll_focus_frames = 2;
        }
    }

    fn apply_health_page_focus_bindings(&mut self, ctx: &egui::Context) {
        use crate::focus::{apply_manual_focus_event_filter, bind_combo_focus_slot, bind_text_focus_slot};
        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 0 {
            return;
        }
        bind_text_focus_slot(ctx, self.health_focus, 0, self.health_refresh_id);
        bind_combo_focus_slot(ctx, self.health_focus, 1, self.health_combo_id);
        bind_text_focus_slot(ctx, self.health_focus, 2, Some(Id::new("diskoria_health_export")));
        let (manual_id, is_combo) = match self.health_focus {
            Some(0) => (self.health_refresh_id, false),
            Some(1) => (self.health_combo_id, true),
            Some(2) => (Some(Id::new("diskoria_health_export")), false),
            _ => (None, false),
        };
        apply_manual_focus_event_filter(ctx, manual_id, !is_combo);
    }

    fn update_about_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::manage_page_focus;
        if self.active_nav != 4 {
            self.about_focus = None;
            return;
        }
        if self.blocks_content_interaction() {
            return;
        }
        if manage_page_focus(ctx, &mut self.about_focus, 3, &[]) {
            self.scroll_focus_frames = 2;
        }
    }

    /// Re-resolve the accent after the source changed. The Windows arm records
    /// whether the OS actually had an accent to give and falls back explicitly
    /// rather than leaving the previous source's color in place under a
    /// "Windows accent" label (known-issues KI-30).
    fn reapply_accent_source(&mut self) {
        let snap = self.shared.settings_snapshot();
        match snap.accent_source {
            AccentSourcePref::Windows => {
                let os_accent = os_accent_color();
                self.shared.set_accent_os_available(os_accent.is_some());
                self.shared
                    .set_accent_color(os_accent.unwrap_or(ACCENT_PALETTE[0]));
            }
            AccentSourcePref::Palette => {
                self.shared.set_accent_color(accent_from_palette(
                    snap.accent_palette_idx,
                    snap.accent_use_custom,
                    &snap.accent_custom_hex,
                ));
            }
        }
        // Let the next poll run immediately instead of waiting out the interval.
        self.shared.set_accent_last_poll(None);
    }

    // The early-outs must stay ahead of the `#[cfg(windows)]` DWM poll, which
    // makes the last of them a "needless" return on the Linux compile only.
    #[allow(clippy::needless_return)]
    fn update_accent_color(&mut self, ctx: &egui::Context) {
        #[cfg(not(windows))]
        let _ = ctx;
        // `--demo-accent` pins the accent; polling DWM would overwrite it on the
        // next frame and put the host's colour into the capture.
        if crate::demo::config().accent.is_some() {
            return;
        }
        if self.shared.settings_snapshot().accent_source != AccentSourcePref::Windows {
            return;
        }
        #[cfg(windows)]
        {
            let now = std::time::Instant::now();
            let due = self.shared.accent_last_poll().is_none_or(|t| {
                now.duration_since(t) >= std::time::Duration::from_millis(250)
            });
            if !due {
                return;
            }
            self.shared.set_accent_last_poll(Some(now));
            let os_accent = os_accent_color();
            self.shared.set_accent_os_available(os_accent.is_some());
            if let Some(c) = os_accent {
                if c != self.shared.accent_color() {
                    self.shared.set_accent_color(c);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn update_settings_tab_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::manage_page_focus;

        if self.active_nav != 5 {
            self.settings_focus = None;
            return;
        }
        if self.blocks_content_interaction() {
            return;
        }
        let slots = self.settings_tab_slot_count();
        let hex_slot = self.settings_hex_slot();
        if manage_page_focus(
            ctx,
            &mut self.settings_focus,
            slots,
            &[(hex_slot, self.accent_custom_te_id)],
        ) {
            self.scroll_focus_frames = 2;
        }
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());

        egui::SidePanel::left("diskoria_sidebar")
            .resizable(false)
            .exact_width(SIDE_PANEL_W)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(t.sb_bg))
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.add_space(TITLEBAR_H + 12.0);

                let avail_w = ui.available_width();
                let logo_handle = if dark { &self.logo } else { &self.logo_light };
                if let Some(handle) = logo_handle {
                    let pad = 10.0_f32;
                    let img_w = avail_w - pad * 2.0;
                    let [iw, ih] = self.logo_size;
                    let img_h = if iw > 0 {
                        img_w * ih as f32 / iw as f32
                    } else {
                        img_w
                    };
                    let top_left = Pos2::new(full.left() + pad, ui.cursor().min.y);
                    let rect = Rect::from_min_size(top_left, Vec2::new(img_w, img_h));
                    ui.allocate_space(Vec2::new(avail_w, img_h));
                    ui.painter().image(
                        handle.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                let target_w = avail_w - 20.0;
                let bold_family = FontFamily::Name("InterBold".into());
                let ref_size = 28.0_f32;
                let ref_w = ctx.fonts(|f| {
                    f.layout_no_wrap(
                        "Diskoria".into(),
                        FontId::new(ref_size, bold_family.clone()),
                        Color32::WHITE,
                    )
                    .rect
                    .width()
                });
                let font_size = if ref_w > 0.0 {
                    ref_size * target_w / ref_w
                } else {
                    ref_size
                };
                let text_h = font_size * 1.2;
                ui.painter().text(
                    Pos2::new(
                        full.left() + avail_w / 2.0,
                        ui.cursor().min.y + text_h / 2.0,
                    ),
                    Align2::CENTER_CENTER,
                    "Diskoria",
                    FontId::new(font_size, bold_family),
                    t.txt_pri,
                );
                ui.add_space(text_h + 8.0);

                for (i, (icon, label)) in NAV_TOP.iter().enumerate() {
                    self.draw_nav_row(ctx, ui, &t, dark, i, icon, label);
                }

                let bottom_h = NAV_BOTTOM.len() as f32 * 40.0 + 8.0;
                let avail_h = ui.available_height();
                ui.add_space((avail_h - bottom_h).max(0.0));

                for (i, (icon, label)) in NAV_BOTTOM.iter().enumerate() {
                    let idx = NAV_TOP.len() + i;
                    self.draw_nav_row(ctx, ui, &t, dark, idx, icon, label);
                }
                ui.add_space(8.0);
            });
    }

    fn draw_nav_row(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        t: &Theme,
        dark: bool,
        index: usize,
        icon: &str,
        label: &str,
    ) {
        let is_active = self.active_nav == index;
        #[cfg(any(windows, target_os = "linux"))]
        let update_nav_block = self.pending_chart_export.is_some()
            || self.show_update_staged_modal
            || self.show_update_alert
            || self.update_check_busy
            || self.update_download_busy;
        #[cfg(not(any(windows, target_os = "linux")))]
        let update_nav_block = false;
        let can_select = !self.show_stop_test_confirm
            && !update_nav_block
            && (!self.any_test_running() || is_active);
        let sense = if can_select {
            INTERACT_MANUAL_FOCUS
        } else {
            Sense::empty()
        };
        let item_r = ui.allocate_response(Vec2::new(ui.available_width(), 40.0), sense);

        if is_active {
            let fill = Color32::from_rgba_premultiplied(
                t.accent.r(),
                t.accent.g(),
                t.accent.b(),
                if dark { 38 } else { 30 },
            );
            ui.painter().rect_filled(item_r.rect, 0.0, fill);
            ui.painter().rect_filled(
                Rect::from_min_max(
                    item_r.rect.left_top(),
                    Pos2::new(item_r.rect.left() + 6.0, item_r.rect.bottom()),
                ),
                0.0,
                t.accent,
            );
        } else if can_select && item_r.hovered() {
            ui.painter().rect_filled(item_r.rect, 0.0, t.hover);
        }

        let icon_col = if !can_select {
            t.txt_pri.linear_multiply(0.35)
        } else if is_active {
            if dark {
                Color32::WHITE
            } else {
                t.txt_pri
            }
        } else {
            t.txt_pri
        };
        let cy = item_r.rect.center().y;
        ui.painter().text(
            Pos2::new(item_r.rect.left() + 24.0, cy),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(18.0),
            icon_col,
        );
        ui.painter().text(
            Pos2::new(item_r.rect.left() + 52.0, cy),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(15.0),
            icon_col,
        );

        if can_select && self.alt_pressed && !label.is_empty() {
            let font = FontId::proportional(15.0);
            if let Some(m) = nav_mnemonic_char(index) {
                if let Some((w_before, w_ch)) =
                    nav_mnemonic_prefix_and_char_width(label, m, ctx, &font)
                {
                    let left = item_r.rect.left() + 52.0;
                    let underline_y = cy + 10.0;
                    ui.painter().line_segment(
                        [
                            Pos2::new(left + w_before, underline_y),
                            Pos2::new(left + w_before + w_ch, underline_y),
                        ],
                        Stroke::new(1.0_f32, icon_col),
                    );
                }
            }
        }

        if item_r.clicked() && can_select {
            self.active_nav = index;
        }
    }

    fn draw_central(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.shared.accent_color());

        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(t.bg_sec))
            .show(ctx, |ui| {
                ui.add_space(TITLEBAR_H);

                // One ScrollArea for all tabs (example app); Settings uses pending scroll for focus.
                let scroll_out = ScrollArea::vertical()
                    .id_salt("diskoria_main_scroll")
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add_space(20.0);

                        let margin = CONTENT_MARGIN;
                        let full_w = ui.available_width();
                        let content_w = full_w.min(MAX_CONTENT_W);
                        let content_x = ui.max_rect().left() + (full_w - content_w) * 0.5;

                        match self.active_nav {
                            0 => self.draw_health_status_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            1 => self.draw_sector_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            2 => self.draw_destructive_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            3 => self.draw_speed_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            4 => self.draw_about_page(ui, ctx, &t, margin, content_x, content_w),
                            5 => {
                                self.draw_settings_theme(ui, ctx, &t, margin, content_x, content_w);
                                #[cfg(any(windows, target_os = "linux"))]
                                if self.shared.pro_edition {
                                    self.draw_settings_monitoring(ui, ctx, &t, margin, content_x, content_w);
                                }
                                self.draw_settings_test_overlay(ui, &t, margin, content_x, content_w);
                                #[cfg(windows)]
                                self.draw_settings_window(ui, &t, margin, content_x, content_w);
                                #[cfg(any(windows, target_os = "linux"))]
                                self.draw_settings_updates(ui, &t, margin, content_x, content_w);
                                #[cfg(any(windows, target_os = "linux"))]
                                self.draw_settings_startup(ui, &t, margin, content_x, content_w);
                            }
                            _ => {}
                        }

                        // Trailing breathing room so the last card doesn't butt
                        // against the window bottom — mirrors the 20px top margin
                        // above and gives every page consistent below-card space.
                        ui.add_space(20.0);
                    });

                if let Some(target) = self.pending_scroll_rect.take() {
                    crate::focus::apply_pending_scroll(
                        ctx,
                        scroll_out.id,
                        scroll_out.inner_rect,
                        &scroll_out.state,
                        target,
                    );
                }

            });
    }

    fn destructive_primary_id() -> Id {
        Id::new("diskoria_destructive_primary")
    }

    fn destructive_unlock_id() -> Id {
        Id::new("diskoria_destructive_unlock")
    }

    /// Sector heatmap + progress card for the destructive test.
        fn draw_destructive_test_panel(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        content_x: f32,
        margin: f32,
        section_w: f32,
    ) {
        let left = content_x + margin;
        let (drive_stem, drive_label) = {
            let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
            if self.drives.is_empty() {
                ("drive".to_string(), String::new())
            } else {
                let d = &self.drives[sel];
                (d.safe_filename_stem(), format!("{} \u{2014} {}", d.model, d.serial))
            }
        };
        self.draw_tabbed_map_card(ui, t, left, section_w, false, drive_stem, drive_label);

        ui.add_space(16.0);

        let section_rect = Rect::from_min_size(
            Pos2::new(left, ui.cursor().min.y),
            Vec2::new(section_w, f32::INFINITY),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(section_rect), |ui| {
            Frame::new()
                .fill(t.bg_pri)
                .inner_margin(egui::Margin::same(SECTOR_MAP_PAD as i8))
                .corner_radius(CornerRadius::same(8))
                .stroke(Stroke::new(1.5_f32, t.border))
                .show(ui, |ui| {
                    let w = ui.available_width();
                    ui.label(egui::RichText::new("PROGRESS").size(11.0).color(t.txt_sec));
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.style_mut().visuals.extreme_bg_color = t.bg_sec;
                        ui.add(
                            egui::ProgressBar::new(
                                (self.destructive_progress_pct / 100.0).clamp(0.0, 1.0) as f32,
                            )
                            .desired_width(w)
                            .fill(t.accent),
                        );
                    });
                    ui.label(
                        egui::RichText::new(format!("{:.1}%", self.destructive_progress_pct))
                            .size(14.0)
                            .color(t.txt_pri),
                    );
                    ui.add_space(12.0);

                    let stat = |ui: &mut Ui, k: &str, v: &str, vcol: Color32| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(k).size(13.0).color(t.txt_sec));
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(v).size(13.0).color(vcol));
                        });
                    };

                    let elapsed_str = self.destructive_elapsed_label.clone();

                    ui.columns(2, |cols| {
                        cols[0].vertical(|ui| {
                            ui.label(egui::RichText::new("STATISTICS").size(11.0).color(t.txt_sec));
                            ui.add_space(6.0);
                            stat(
                                ui,
                                "Average speed",
                                &format!("{:.1} MB/s", self.destructive_avg_speed_mbps),
                                t.txt_pri,
                            );
                            stat(
                                ui,
                                "Bad sectors",
                                &format!("{}", self.destructive_bad_sectors),
                                Color32::from_rgb(231, 76, 60),
                            );
                            stat(
                                ui,
                                "Slow blocks",
                                &format!("{}", self.destructive_slow_blocks),
                                Color32::from_rgb(255, 193, 7),
                            );
                        });
                        cols[1].vertical(|ui| {
                            ui.label(egui::RichText::new("TIME").size(11.0).color(t.txt_sec));
                            ui.add_space(6.0);
                            stat(ui, "Elapsed", &elapsed_str, t.txt_pri);
                            stat(ui, "Remaining", &self.destructive_remaining_label, t.txt_pri);
                        });
                    });
                });
        });
        ui.add_space(12.0);
    }

    /// Sector Test page — read-only sector scan UI.
    /// Shared tabbed card: "Sector Map" tab (heat grid) + "Performance Chart" tab (speed vs position).
    /// Used by both `draw_sector_test_panel` and `draw_destructive_test_panel`.
    #[allow(clippy::too_many_lines)]
    fn draw_tabbed_map_card(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        left: f32,
        section_w: f32,
        is_surface: bool,
        drive_filename_stem: String,
        drive_label: String,
    ) {
        const TAB_H: f32 = 36.0;
        const SEP_H: f32 = 1.0;
        let pad = SECTOR_MAP_PAD;
        let cols = SECTOR_GRID_COLS;
        let gap = SECTOR_CELL_GAP;
        let grid_inner_w = section_w - pad * 2.0;
        let cell = ((grid_inner_w - gap * (cols as f32 - 1.0)) / cols as f32).max(2.0);
        let grid_rows = TOTAL_UI_BLOCKS.div_ceil(cols);
        let grid_h = grid_rows as f32 * (cell + gap) - gap;
        // Still needed to size the chart tab's plot; the card's own height is
        // accumulated from the rows below rather than predicted here.
        let content_h = pad + grid_h + SECTOR_LEGEND_GAP + SECTOR_LEGEND_ROW_H + pad;

        // `CardLayout` never reserves space up front — the frame is painted into
        // placeholder shapes at the measured height and `end()` is the only
        // cursor advance. That matters here: allocating `total_h` first and
        // *then* calling `allocate_new_ui` for the chart tab made egui advance
        // the cursor backward into already-consumed space, corrupting layout
        // state and panicking on the next frame. The cursor still sits at the
        // card top when the plot allocates, exactly as before.
        //
        // Padding is zero because the tab strip runs flush to the card edge; the
        // content area applies `pad` itself below.
        let mut card = CardLayout::builder(left, section_w)
            .pad(0.0)
            .gap_before(0.0)
            .begin(ui, t);

        let tab_w = section_w / 2.0;
        let tab_row = card.row(TAB_H);
        let card_top = tab_row.top();
        let left_tab_rect = Rect::from_min_size(Pos2::new(left, card_top), Vec2::new(tab_w, TAB_H));
        let right_tab_rect = Rect::from_min_size(Pos2::new(left + tab_w, card_top), Vec2::new(tab_w, TAB_H));

        let active_tab = if is_surface { self.surface_chart_tab } else { self.destructive_chart_tab };

        if active_tab == 0 {
            ui.painter().rect_filled(
                right_tab_rect,
                CornerRadius { nw: 0, ne: 8, sw: 0, se: 0 },
                t.bg_sec,
            );
        } else {
            ui.painter().rect_filled(
                left_tab_rect,
                CornerRadius { nw: 8, ne: 0, sw: 0, se: 0 },
                t.bg_sec,
            );
        }

        ui.painter().line_segment(
            [Pos2::new(left + tab_w, card_top + 8.0), Pos2::new(left + tab_w, card_top + TAB_H)],
            Stroke::new(1.0_f32, t.border),
        );

        let active_rect = if active_tab == 0 { left_tab_rect } else { right_tab_rect };
        ui.painter().rect_filled(
            Rect::from_min_max(
                Pos2::new(active_rect.left() + 8.0, card_top + TAB_H - 2.5),
                Pos2::new(active_rect.right() - 8.0, card_top + TAB_H),
            ),
            1.0,
            t.accent,
        );

        let tab_font = FontId::new(13.0, FontFamily::Proportional);
        ui.painter().text(
            left_tab_rect.center(),
            Align2::CENTER_CENTER,
            "Sector Map",
            tab_font.clone(),
            if active_tab == 0 { t.txt_pri } else { t.txt_sec },
        );
        ui.painter().text(
            right_tab_rect.center(),
            Align2::CENTER_CENTER,
            "Performance Chart",
            tab_font,
            if active_tab == 1 { t.txt_pri } else { t.txt_sec },
        );

        let tab_id_base = if is_surface { "surface" } else { "destructive" };
        let left_resp = ui.interact(
            left_tab_rect,
            Id::new(("tab_map", tab_id_base)),
            Sense::click(),
        );
        if left_resp.clicked() {
            if is_surface { self.surface_chart_tab = 0; } else { self.destructive_chart_tab = 0; }
        }
        let right_resp = ui.interact(
            right_tab_rect,
            Id::new(("tab_chart", tab_id_base)),
            Sense::click(),
        );
        if right_resp.clicked() {
            if is_surface { self.surface_chart_tab = 1; } else { self.destructive_chart_tab = 1; }
        }

        if active_tab == 0 && right_resp.hovered() {
            ui.painter().rect_filled(right_tab_rect, CornerRadius { nw: 0, ne: 8, sw: 0, se: 0 }, t.hover);
        } else if active_tab == 1 && left_resp.hovered() {
            ui.painter().rect_filled(left_tab_rect, CornerRadius { nw: 8, ne: 0, sw: 0, se: 0 }, t.hover);
        }

        let sep_y = card.row(SEP_H).top();
        ui.painter().line_segment(
            [Pos2::new(left + 1.5, sep_y), Pos2::new(left + section_w - 1.5, sep_y)],
            Stroke::new(SEP_H, t.border),
        );

        let active_tab = if is_surface { self.surface_chart_tab } else { self.destructive_chart_tab };
        let content_top = sep_y + SEP_H;

        if active_tab == 0 {
            card.add_gap(pad);
            let grid_top = card.row(grid_h).top();
            let grid_left = left + pad;
            let (heat_min, heat_max) = if is_surface {
                (self.heat_min_ms, self.heat_max_ms)
            } else {
                (self.destructive_heat_min_ms, self.destructive_heat_max_ms)
            };
            for idx in 0..TOTAL_UI_BLOCKS {
                let c = idx / cols;
                let r = idx % cols;
                let x = grid_left + r as f32 * (cell + gap);
                let y = grid_top + c as f32 * (cell + gap);
                let cell_data = if is_surface {
                    self.sector_cells[idx]
                } else {
                    self.destructive_cells[idx]
                };
                let col = sector_cell_color(cell_data, heat_min, heat_max);
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell)),
                    1.0,
                    col,
                );
            }

            card.add_gap(SECTOR_LEGEND_GAP);
            let legend_cy = card.row(SECTOR_LEGEND_ROW_H).center().y;
            card.add_gap(pad);
            let sw = 11.0_f32;
            let gap_label = 6.0_f32;
            let gap_group = 16.0_f32;
            let legend_font = FontId::new(12.0, FontFamily::Proportional);
            let slow_threshold = if is_surface {
                surface_test::SLOW_THRESHOLD_MS
            } else {
                crate::destructive_test::SLOW_THRESHOLD_MS
            };
            let slow_label = format!("Slow (\u{2265}{:.0} ms)", slow_threshold);
            let not_tested_label = if is_surface { "Not scanned" } else { "Not tested" };
            let bad_label = if is_surface { "Bad / error" } else { "Bad / mismatch" };
            let legend_items: [(Color32, &str); 4] = [
                (Color32::from_rgb(60, 60, 60), not_tested_label),
                (Color32::from_rgb(76, 175, 80), "Good (heat)"),
                (Color32::from_rgb(255, 193, 7), slow_label.as_str()),
                (Color32::from_rgb(244, 67, 54), bad_label),
            ];
            let mut lx = left + pad;
            for (col, label) in legend_items.iter() {
                let sw_rect = Rect::from_center_size(Pos2::new(lx + sw * 0.5, legend_cy), Vec2::splat(sw));
                ui.painter().rect_filled(sw_rect, 2.0, *col);
                ui.painter().text(
                    Pos2::new(lx + sw + gap_label, legend_cy),
                    Align2::LEFT_CENTER,
                    *label,
                    legend_font.clone(),
                    t.txt_sec,
                );
                let tw = ui.ctx().fonts(|f| {
                    f.layout_no_wrap(label.to_string(), legend_font.clone(), t.txt_sec)
                        .rect
                        .width()
                });
                lx += sw + gap_label + tw + gap_group;
            }
        } else {
            card.row(content_h);
            let chart_rect = Rect::from_min_max(
                Pos2::new(left + 1.5, content_top + 1.0),
                Pos2::new(left + section_w - 1.5, content_top + content_h - 1.0),
            );

            let chart_points_clone: Vec<[f64; 2]> = if is_surface {
                self.surface_chart_points.clone()
            } else {
                self.destructive_chart_points.clone()
            };
            let raw_points_clone: Vec<[f64; 2]> = if is_surface {
                self.surface_chart_raw_points.clone()
            } else {
                self.destructive_chart_raw_points.clone()
            };
            let max_speed = if is_surface { self.surface_chart_max_speed } else { self.destructive_chart_max_speed };
            let total_gb = if is_surface { self.surface_chart_total_gb } else { self.destructive_chart_total_gb };
            let nice_max = nice_y_max(max_speed);
            let x_max = total_gb.max(1.0);
            let plot_id = if is_surface { "perf_chart_surface" } else { "perf_chart_destructive" };
            let accent = t.accent;
            let dot_color = t.txt_sec.gamma_multiply(0.75);

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chart_rect), |ui| {
                Plot::new(plot_id)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .include_x(0.0)
                    .include_x(x_max)
                    .include_y(0.0)
                    .include_y(nice_max)
                    .x_axis_label("Position (GB)")
                    .y_axis_label("MB/s")
                    .show(ui, |plot_ui| {
                        if !raw_points_clone.is_empty() {
                            plot_ui.points(
                                Points::new(PlotPoints::from(raw_points_clone.clone()))
                                    .color(dot_color)
                                    .radius(1.0_f32)
                                    .shape(MarkerShape::Circle),
                            );
                        }
                        if !chart_points_clone.is_empty() {
                            plot_ui.line(
                                Line::new(PlotPoints::from(chart_points_clone.clone()))
                                    .color(accent)
                                    .width(4.0_f32),
                            );
                        }
                    });
            });

            const BTN_SIZE: f32 = 26.0;
            const BTN_PAD: f32 = 8.0;
            let btn_rect = Rect::from_min_size(
                Pos2::new(left + section_w - BTN_SIZE - BTN_PAD - 1.5, content_top + BTN_PAD),
                Vec2::splat(BTN_SIZE),
            );
            let btn_resp = ui.interact(
                btn_rect,
                Id::new(("dl_btn", tab_id_base)),
                Sense::click(),
            );
            // Override the plot's crosshair with the normal clickable pointer while
            // hovering the export button (drawn after the plot, so this wins).
            if btn_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let btn_layer = LayerId::new(Order::Foreground, Id::new(("dl_btn_layer", tab_id_base)));
            let btn_painter = ui.ctx().layer_painter(btn_layer);
            let btn_bg = if btn_resp.hovered() { t.accent } else { t.bg_sec };
            let btn_fg = if btn_resp.hovered() { t.txt_on_accent } else { t.txt_sec };
            btn_painter.rect_filled(btn_rect, 6.0, btn_bg);
            btn_painter.rect_stroke(btn_rect, 6.0, Stroke::new(1.0_f32, t.border), StrokeKind::Middle);
            btn_painter.text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                "\u{f30a}",
                FontId::new(16.0, FontFamily::Proportional),
                btn_fg,
            );

            if btn_resp.clicked() {
                let test_type = if is_surface { "Read-only sector test" } else { "Read and write sector test" };
                let now = chrono::Local::now();
                let test_label = format!("{} - {}", now.format("%m/%d/%Y"), test_type);
                // Stage the export; a themed modal then asks whether to embed a
                // health report (HTML) or save the chart alone (PNG). The worker
                // (with the chosen save dialog) is spawned from there.
                if let Some(drive) = self
                    .drives
                    .get(self.selected_drive.min(self.drives.len().saturating_sub(1)))
                    .cloned()
                {
                    self.pending_chart_export = Some(ChartExportRequest {
                        drive,
                        drive_label,
                        drive_filename_stem,
                        test_label,
                        raw_points: raw_points_clone,
                        chart_points: chart_points_clone,
                        max_speed,
                        total_gb,
                    });
                    self.chart_export_focus = None;
                }
            }
        }

        // Single authoritative advance for both tabs. On the chart tab this runs
        // after `allocate_new_ui`, so it re-asserts the card's real bottom rather
        // than leaving the cursor wherever the plot finished.
        card.end(ui);
    }

    /// Sector map + progress/stats/time cards.
        fn draw_sector_test_panel(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        content_x: f32,
        margin: f32,
        section_w: f32,
    ) {
        let left = content_x + margin;
        let (drive_stem, drive_label) = {
            let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
            if self.drives.is_empty() {
                ("drive".to_string(), String::new())
            } else {
                let d = &self.drives[sel];
                (d.safe_filename_stem(), format!("{} \u{2014} {}", d.model, d.serial))
            }
        };
        self.draw_tabbed_map_card(ui, t, left, section_w, true, drive_stem, drive_label);

        ui.add_space(16.0);

        let section_rect = Rect::from_min_size(
            Pos2::new(left, ui.cursor().min.y),
            Vec2::new(section_w, f32::INFINITY),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(section_rect), |ui| {
            Frame::new()
                .fill(t.bg_pri)
                .inner_margin(egui::Margin::same(SECTOR_MAP_PAD as i8))
                .corner_radius(CornerRadius::same(8))
                .stroke(Stroke::new(1.5_f32, t.border))
                .show(ui, |ui| {
                    let w = ui.available_width();
                    ui.label(egui::RichText::new("PROGRESS").size(11.0).color(t.txt_sec));
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.style_mut().visuals.extreme_bg_color = t.bg_sec;
                        ui.add(
                            egui::ProgressBar::new(
                                (self.surface_progress_pct / 100.0).clamp(0.0, 1.0) as f32,
                            )
                            .desired_width(w)
                            .fill(t.accent),
                        );
                    });
                    ui.label(
                        egui::RichText::new(format!("{:.1}%", self.surface_progress_pct))
                            .size(14.0)
                            .color(t.txt_pri),
                    );
                    ui.add_space(12.0);

                    let stat = |ui: &mut Ui, k: &str, v: &str, vcol: Color32| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(k).size(13.0).color(t.txt_sec));
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(v).size(13.0).color(vcol));
                        });
                    };

                    let elapsed_str = self.surface_elapsed_label.clone();

                    ui.columns(2, |cols| {
                        cols[0].vertical(|ui| {
                            ui.label(egui::RichText::new("STATISTICS").size(11.0).color(t.txt_sec));
                            ui.add_space(6.0);
                            stat(ui, "Average speed", &format!("{:.1} MB/s", self.surface_avg_speed_mbps), t.txt_pri);
                            stat(ui, "Bad sectors", &format!("{}", self.surface_bad_sectors), Color32::from_rgb(231, 76, 60));
                            stat(ui, "Slow blocks", &format!("{}", self.surface_slow_blocks), Color32::from_rgb(255, 193, 7));
                        });
                        cols[1].vertical(|ui| {
                            ui.label(egui::RichText::new("TIME").size(11.0).color(t.txt_sec));
                            ui.add_space(6.0);
                            stat(ui, "Elapsed", &elapsed_str, t.txt_pri);
                            stat(ui, "Remaining", &self.surface_remaining_label, t.txt_pri);
                        });
                    });
                });
        });
        ui.add_space(12.0);
    }

    /// Theme + Accent cards (rust-egui-winui-example `draw_settings_theme`).
    fn draw_settings_theme(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let pad = crate::theme::CARD_PAD;
        let seg_h = 34.0_f32;

        let swatch_gap = 8.0_f32;
        let row_available_w = section_w - pad * 2.0;
        let swatch_size = ((row_available_w - swatch_gap * 7.0) / 8.0).clamp(16.0, 26.0);
        let accent_grid_h = swatch_size;

        // One frame, two titled sections. First card on the page, so no lead gap
        // — `draw_central` already added the 20px top margin.
        let mut card = CardLayout::builder(content_x + margin, section_w)
            .gap_before(0.0)
            .title("Theme")
            .begin(ui, t);
        let inner_x = card.inner_x();

        let seg_rect = card.row(seg_h);
        ui.painter().rect_filled(seg_rect, 6.0, t.bg_sec);
        ui.painter().rect_stroke(
            seg_rect,
            6.0,
            Stroke::new(1.5_f32, t.border),
            StrokeKind::Middle,
        );

        let options = [
            ("Auto", ThemePref::Auto),
            ("Dark", ThemePref::Dark),
            ("Light", ThemePref::Light),
        ];
        let seg_w = seg_rect.width() / 3.0;
        for (i, (label, pref)) in options.iter().enumerate() {
            let seg = Rect::from_min_size(
                Pos2::new(seg_rect.left() + i as f32 * seg_w, seg_rect.top()),
                Vec2::new(seg_w, seg_h),
            );
            let selected = self.shared.settings_snapshot().theme == *pref;
            let focused = self.settings_focus == Some(i);
            let resp = ui.interact(seg, Id::new(("diskoria_theme_seg", i)), INTERACT_MANUAL_FOCUS);

            if selected {
                ui.painter().rect_filled(seg, 6.0, t.accent);
            } else if resp.hovered() {
                ui.painter().rect_filled(seg, 6.0, t.hover);
            }
            if focused {
                ui.painter().rect_stroke(
                    seg.expand(3.0),
                    6.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            let txt_col = if selected { t.txt_on_accent } else { t.txt_pri };
            ui.painter().text(
                seg.center(),
                Align2::CENTER_CENTER,
                *label,
                FontId::proportional(13.0),
                txt_col,
            );

            scroll_to_focused(
                &mut self.pending_scroll_rect,
                seg,
                focused,
                self.scroll_focus_frames > 0,
            );
            if resp.clicked() {
                self.shared.update_settings(|s| s.theme = *pref);
                self.settings_focus = Some(i);
            }
            if page_keys && keyboard_activate(ui, focused) {
                self.shared.update_settings(|s| s.theme = *pref);
            }
        }

        // Bottom padding of the Theme section, the inter-section gap, then the
        // Accent section's own top padding — the two used to be laid out as
        // separate cards inside one frame.
        card.add_gap(pad + crate::theme::CARD_GAP + pad);
        card.section_title(ui, t, "Accent");

        let accent_seg_rect = card.row(seg_h);
        ui.painter().rect_filled(accent_seg_rect, 6.0, t.bg_sec);
        ui.painter().rect_stroke(
            accent_seg_rect,
            6.0,
            Stroke::new(1.5_f32, t.border),
            StrokeKind::Middle,
        );

        let accent_options = [
            (
                if cfg!(windows) { "Windows accent" } else { "System accent" },
                AccentSourcePref::Windows,
            ),
            ("Palette", AccentSourcePref::Palette),
        ];
        let accent_seg_w = accent_seg_rect.width() / 2.0;
        for (i, (label, pref)) in accent_options.iter().enumerate() {
            let seg = Rect::from_min_size(
                Pos2::new(
                    accent_seg_rect.left() + i as f32 * accent_seg_w,
                    accent_seg_rect.top(),
                ),
                Vec2::new(accent_seg_w, seg_h),
            );
            let slot = 3 + i;
            let selected = self.shared.settings_snapshot().accent_source == *pref;
            let focused = self.settings_focus == Some(slot);
            let resp = ui.interact(seg, Id::new(("diskoria_accent_seg", i)), INTERACT_MANUAL_FOCUS);

            if selected {
                ui.painter().rect_filled(seg, 6.0, t.accent);
            } else if resp.hovered() {
                ui.painter().rect_filled(seg, 6.0, t.hover);
            }
            if focused {
                ui.painter().rect_stroke(
                    seg.expand(3.0),
                    6.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }

            let txt_col = if selected { t.txt_on_accent } else { t.txt_pri };
            ui.painter().text(
                seg.center(),
                Align2::CENTER_CENTER,
                *label,
                FontId::proportional(13.0),
                txt_col,
            );

            scroll_to_focused(
                &mut self.pending_scroll_rect,
                seg,
                focused,
                self.scroll_focus_frames > 0,
            );
            if resp.clicked() {
                let old = self.shared.settings_snapshot().accent_source;
                self.shared.update_settings(|s| s.accent_source = *pref);
                if old != *pref {
                    self.settings_focus = None;
                } else {
                    self.settings_focus = Some(slot);
                }
                self.reapply_accent_source();
            }
            if page_keys && keyboard_activate(ui, focused) {
                let old = self.shared.settings_snapshot().accent_source;
                self.shared.update_settings(|s| s.accent_source = *pref);
                if old != *pref {
                    self.settings_focus = None;
                }
                self.reapply_accent_source();
            }
        }

        card.add_gap(pad);
        let accent_grid_top = card.row(accent_grid_h).top();
        let start_x = inner_x;
        let start_y = accent_grid_top;
        let palette_snap = self.shared.settings_snapshot();
        if palette_snap.accent_source == AccentSourcePref::Palette {
            for (idx, col) in ACCENT_PALETTE.iter().enumerate() {
                let x = start_x + idx as f32 * (swatch_size + swatch_gap);
                let y = start_y;
                let sw_rect =
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(swatch_size, swatch_size));

                let selected = !palette_snap.accent_use_custom
                    && palette_snap.accent_palette_idx == idx
                    && palette_snap.accent_source == AccentSourcePref::Palette;

                let stroke_col = if selected { t.txt_pri } else { t.border };
                let sw_slot = 5 + idx;
                let sw_focused = self.settings_focus == Some(sw_slot);

                ui.painter().rect_filled(sw_rect, 4.0, *col);
                ui.painter().rect_stroke(
                    sw_rect,
                    4.0,
                    Stroke::new(if selected { 2.0_f32 } else { 1.5_f32 }, stroke_col),
                    StrokeKind::Middle,
                );
                if sw_focused {
                    ui.painter().rect_stroke(
                        sw_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0_f32, t.accent),
                        StrokeKind::Outside,
                    );
                }

                let sw_resp =
                    ui.interact(sw_rect, Id::new(("diskoria_accent_swatch", idx)), INTERACT_MANUAL_FOCUS);
                scroll_to_focused(
                    &mut self.pending_scroll_rect,
                    sw_rect,
                    sw_focused,
                    self.scroll_focus_frames > 0,
                );
                if sw_resp.clicked() {
                    self.settings_focus = Some(sw_slot);
                    self.shared.update_settings(|s| {
                        s.accent_source = AccentSourcePref::Palette;
                        s.accent_palette_idx = idx;
                        s.accent_use_custom = false;
                    });
                    self.shared.set_accent_color(*col);
                }
                if page_keys && keyboard_activate(ui, sw_focused) {
                    self.shared.update_settings(|s| {
                        s.accent_source = AccentSourcePref::Palette;
                        s.accent_palette_idx = idx;
                        s.accent_use_custom = false;
                    });
                    self.shared.set_accent_color(*col);
                }

                if sw_resp.hovered() {
                    ui.painter().rect_stroke(
                        sw_rect.expand(2.0),
                        4.0,
                        Stroke::new(1.0_f32, t.accent),
                        StrokeKind::Middle,
                    );

                    let mouse_pos = ui
                        .ctx()
                        .input(|i| i.pointer.hover_pos())
                        .unwrap_or(sw_rect.right_top());
                    let hex = color_to_hex_6(*col);
                    let tt = format!("{} ({})", ACCENT_PALETTE_LABELS[idx], hex);
                    show_tooltip_text(ctx, Id::new(("diskoria_accent_swatch_tt", idx)), mouse_pos, t, &tt);
                }
            }
        } else {
            // When the OS has no accent to report (Windows PE, or a profile with
            // no accent value) the "Windows accent" option is silently painting
            // the palette fallback — say so instead of looking broken (KI-30).
            let msg = if self.shared.accent_os_available() {
                "Switch to Palette to choose colors."
            } else {
                "Windows reports no accent color — using the fallback. Switch to Palette to choose one."
            };
            ui.painter().text(
                Pos2::new(inner_x, accent_grid_top + 10.0),
                Align2::LEFT_CENTER,
                msg,
                FontId::proportional(13.0),
                t.txt_sec,
            );
        }

        card.add_gap(pad);
        // The label is drawn centered on the row's top edge, so it overhangs
        // upward into the padding above — the row only reserves the 10px down
        // to the input field, which is what the old hand-counted height did too.
        let custom_label_y = card.row(10.0).top();
        ui.painter().text(
            Pos2::new(inner_x, custom_label_y),
            Align2::LEFT_CENTER,
            "Custom hex (#RRGGBB)",
            FontId::proportional(13.0),
            t.txt_pri,
        );

        let input_rect = card.row(34.0);
        ui.painter().rect_filled(input_rect, 0.0, t.bg_sec);

        let hex_slot = self.settings_hex_slot();
        let field_focused = self.settings_focus == Some(hex_slot);
        let line_w = if field_focused { 2.5_f32 } else { 1.5_f32 };
        ui.painter().line_segment(
            [input_rect.left_bottom(), input_rect.right_bottom()],
            Stroke::new(line_w, t.accent),
        );

        let te = egui::TextEdit::singleline(&mut self.accent_custom_hex)
            .font(FontId::proportional(13.0))
            .text_color(t.txt_pri)
            .frame(false)
            .hint_text("#RRGGBB");
        let te_h = 18.0_f32;
        let inner = Rect::from_min_size(
            Pos2::new(input_rect.left() + 8.0, input_rect.center().y - te_h / 2.0),
            Vec2::new(input_rect.width() - 16.0, te_h),
        );
        let te_resp = ui.put(inner, te);
        self.accent_custom_te_id = Some(te_resp.id);
        scroll_to_focused(
            &mut self.pending_scroll_rect,
            input_rect,
            field_focused,
            self.scroll_focus_frames > 0,
        );
        if te_resp.clicked() {
            self.settings_focus = Some(hex_slot);
        }
        if te_resp.changed() {
            self.accent_hex_edited = true;
        }

        // Focus alone must not commit: the manual tab order binds egui focus to
        // this field (`focus.rs::bind_text_focus_slot`), so without this guard
        // Tab-ing past it committed the seeded placeholder and overwrote the
        // chosen palette swatch with purple (known-issues KI-29).
        if te_resp.lost_focus() && !self.accent_hex_edited {
            // Drop any stale draft so the field re-seeds from the real setting.
            self.accent_custom_hex = self.shared.settings_snapshot().accent_custom_hex.clone();
        } else if te_resp.lost_focus() {
            self.accent_hex_edited = false;
            let trimmed = self.accent_custom_hex.trim().to_string();
            if trimmed.is_empty() {
                let draft = self.accent_custom_hex.clone();
                self.shared.update_settings(|s| {
                    s.accent_use_custom = false;
                    s.accent_custom_hex = draft.clone();
                });
                let snap = self.shared.settings_snapshot();
                if snap.accent_source == AccentSourcePref::Palette {
                    self.shared.set_accent_color(accent_from_palette(
                        snap.accent_palette_idx,
                        snap.accent_use_custom,
                        &snap.accent_custom_hex,
                    ));
                }
            } else if let Some(c) = parse_hex_color_6(&self.accent_custom_hex) {
                let normalized = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                self.accent_custom_hex = normalized.clone();
                self.shared.update_settings(|s| {
                    s.accent_use_custom = true;
                    s.accent_custom_hex = normalized;
                });
                if self.shared.settings_snapshot().accent_source == AccentSourcePref::Palette {
                    self.shared.set_accent_color(c);
                }
            }
        }

        // The hex field's `ui.put` rewinds the cursor the same way the Monitoring
        // sliders do; `end()` runs last and re-asserts the card's real bottom (KI-18).
        card.end(ui);
    }

    fn speed_primary_id() -> Id {
        Id::new("diskoria_speed_primary")
    }

}

/// Number of position buckets for chart smoothing.  Each bucket averages all
/// speed samples that fall within it and emits a single chart point at its
/// midpoint.  200 buckets ≈ 0.5% of the disk each — smooth enough to read
/// while still showing real dips/spikes.
const CHART_BUCKETS: usize = 100;

/// Accumulate a speed sample into a positional bucket.  When the position
/// crosses into the next bucket, the average of the old bucket is emitted as a
/// chart point.  This keeps the chart smooth regardless of how many raw I/O
/// samples arrive per bucket.
fn chart_bucket_accumulate(
    bytes_scanned: i64,
    total_bytes: i64,
    speed_mbps: f64,
    points: &mut Vec<[f64; 2]>,
    bucket_sum: &mut f64,
    bucket_count: &mut u32,
    bucket_idx: &mut usize,
) {
    let frac = bytes_scanned as f64 / total_bytes.max(1) as f64;
    let idx = ((frac * CHART_BUCKETS as f64) as usize).min(CHART_BUCKETS.saturating_sub(1));

    if idx != *bucket_idx && *bucket_count > 0 {
        let avg = *bucket_sum / *bucket_count as f64;
        let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mid_gb = (*bucket_idx as f64 + 0.5) / CHART_BUCKETS as f64 * total_gb;
        points.push([mid_gb, avg]);
        *bucket_sum = 0.0;
        *bucket_count = 0;
    }

    *bucket_idx = idx;
    *bucket_sum += speed_mbps;
    *bucket_count += 1;
}

/// Flush any remaining samples in the current bucket (call at test completion).
fn chart_bucket_flush(
    total_bytes: i64,
    points: &mut Vec<[f64; 2]>,
    bucket_sum: &mut f64,
    bucket_count: &mut u32,
    bucket_idx: &mut usize,
) {
    if *bucket_count > 0 {
        let avg = *bucket_sum / *bucket_count as f64;
        let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mid_gb = (*bucket_idx as f64 + 0.5) / CHART_BUCKETS as f64 * total_gb;
        points.push([mid_gb, avg]);
        *bucket_sum = 0.0;
        *bucket_count = 0;
    }
}

/// Compute a "nice" Y-axis ceiling for the performance chart.
fn nice_y_max(max_speed: f64) -> f64 {
    if max_speed <= 0.0 {
        return 500.0;
    }
    let headroom = max_speed * 1.1;
    for &step in &[5.0_f64, 10.0, 15.0, 20.0, 30.0, 50.0, 75.0, 100.0, 150.0, 200.0, 300.0, 500.0, 750.0, 1000.0, 1250.0, 1500.0, 1750.0, 2000.0, 2500.0, 3000.0, 4000.0, 5000.0, 7000.0, 10000.0] {
        if step >= headroom {
            return step;
        }
    }
    (headroom / 1000.0).ceil() * 1000.0
}

/// A staged performance-chart export, awaiting the user's "include health report?"
/// choice in the modal. Carries everything the off-thread save+render needs so no
/// app state is touched from the worker.
struct ChartExportRequest {
    drive: crate::detected_drive::DetectedDrive,
    drive_label: String,
    drive_filename_stem: String,
    test_label: String,
    raw_points: Vec<[f64; 2]>,
    chart_points: Vec<[f64; 2]>,
    max_speed: f64,
    total_gb: f64,
}

impl ChartExportRequest {
    /// Spawn the save-dialog + write worker. `include_report` chooses the HTML
    /// (chart embedded in a fresh Drive Health report) vs. plain PNG path.
    fn spawn(self, include_report: bool) {
        std::thread::spawn(move || {
            if include_report {
                let filename = format!("{}.html", self.drive_filename_stem);
                let handle = pollster::block_on(
                    rfd::AsyncFileDialog::new()
                        .set_title("Save Performance Chart Report")
                        .set_file_name(&filename)
                        .add_filter("HTML file", &["html"])
                        .save_file(),
                );
                if let Some(h) = handle {
                    // Render at 2x so the lightbox has real detail to zoom into.
                    let png = match render_performance_chart_png_bytes(
                        1920,
                        920,
                        &self.drive_label,
                        &self.test_label,
                        &self.raw_points,
                        &self.chart_points,
                        self.max_speed,
                        self.total_gb,
                        true, // dark theme to match the report CSS
                    ) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            log::warn!("Performance chart render failed: {}", e);
                            return;
                        }
                    };
                    let report = crate::smart_reader::query_smart_detail(
                        &self.drive.device_id,
                        self.drive.bus,
                    );
                    let html = crate::smart_health_page::build_chart_report_html(
                        &self.drive,
                        &report,
                        &png,
                    );
                    if let Err(e) = std::fs::write(h.path(), html.as_bytes()) {
                        log::warn!("Performance chart report write failed: {}", e);
                    }
                }
            } else {
                let filename = format!("{}.png", self.drive_filename_stem);
                let handle = pollster::block_on(
                    rfd::AsyncFileDialog::new()
                        .set_title("Save Performance Chart")
                        .set_file_name(&filename)
                        .add_filter("PNG Image", &["png"])
                        .save_file(),
                );
                if let Some(h) = handle {
                    if let Err(e) = export_performance_chart_png(
                        h.path(),
                        &self.drive_label,
                        &self.test_label,
                        &self.raw_points,
                        &self.chart_points,
                        self.max_speed,
                        self.total_gb,
                    ) {
                        log::warn!("Performance chart export failed: {}", e);
                    }
                }
            }
        });
    }
}

/// Render the performance chart into an in-memory PNG byte buffer using plotters.
///
/// Pixel sizes (margins, fonts, marker radii) scale with `height` relative to the
/// baseline 460 px so a 2x render (used for the HTML-embed lightbox) stays crisp
/// and proportioned. The on-disk export and the embedded copy share this path.
/// `pub(crate)` so `demo::write_export_reports` can build the same combined
/// report the chart-export button produces, without a file dialog.
pub(crate) fn render_performance_chart_png_bytes(
    width: u32,
    height: u32,
    drive_label: &str,
    test_label: &str,
    raw_points: &[[f64; 2]],
    chart_points: &[[f64; 2]],
    max_speed: f64,
    total_gb: f64,
    dark: bool,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    use plotters::prelude::*;

    // Theme palette: light is the standalone PNG default; dark matches the report
    // CSS (#1e1e1e body) so the embedded chart blends into the HTML report.
    let bg = if dark { RGBColor(30, 30, 30) } else { WHITE };
    let caption_col = if dark { RGBColor(224, 224, 224) } else { BLACK };
    let label_col = if dark { RGBColor(170, 170, 170) } else { RGBColor(60, 60, 60) };
    let grid_light = if dark { RGBColor(58, 58, 58) } else { RGBColor(220, 220, 220) };
    let grid_bold = if dark { RGBColor(74, 74, 74) } else { RGBColor(200, 200, 200) };
    let axis_col = if dark { RGBColor(110, 110, 110) } else { RGBColor(140, 140, 140) };
    let line_col = if dark { RGBColor(90, 160, 240) } else { BLUE };
    let dot_color = if dark { RGBColor(150, 150, 150) } else { RGBColor(160, 160, 160) };
    let footer_col = if dark { RGBColor(150, 150, 150) } else { RGBColor(100, 100, 100) };

    let mut buf = vec![0u8; width as usize * height as usize * 3];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (width, height)).into_drawing_area();
        root.fill(&bg).map_err(|e| e.to_string())?;

        let scale = f64::from(height) / 460.0;
        let px = |v: f64| (v * scale).round() as i32;
        let fs = |v: f64| (v * scale).round() as i32;

        let nice_max = nice_y_max(max_speed);
        let x_max = total_gb.max(1.0);

        let mut chart = ChartBuilder::on(&root)
            .margin(px(20.0))
            .x_label_area_size(px(45.0))
            .y_label_area_size(px(70.0))
            .caption(drive_label, ("sans-serif", fs(18.0)).into_font().color(&caption_col))
            .build_cartesian_2d(0.0_f64..x_max, 0.0_f64..nice_max)
            .map_err(|e| e.to_string())?;

        chart
            .configure_mesh()
            .x_desc("Position (GB)")
            .y_desc("Disk Performance (MB/s)")
            .axis_desc_style(("sans-serif", fs(16.0)).into_font().color(&label_col))
            .label_style(("sans-serif", fs(14.0)).into_font().color(&label_col))
            .axis_style(ShapeStyle::from(&axis_col).stroke_width(1))
            .light_line_style(grid_light)
            .bold_line_style(grid_bold)
            .draw()
            .map_err(|e| e.to_string())?;

        if !raw_points.is_empty() {
            let r = px(2.0).max(1);
            chart
                .draw_series(raw_points.iter().map(|p| {
                    Circle::new((p[0], p[1]), r, ShapeStyle::from(&dot_color).filled())
                }))
                .map_err(|e| e.to_string())?;
        }

        if !chart_points.is_empty() {
            chart
                .draw_series(LineSeries::new(
                    chart_points.iter().map(|p| (p[0], p[1])),
                    ShapeStyle::from(&line_col).stroke_width(px(2.0).max(1) as u32),
                ))
                .map_err(|e| e.to_string())?;
        }

        root.draw(&Text::new(
            test_label,
            (width as i32 / 2, height as i32 - px(16.0)),
            ("sans-serif", fs(16.0)).into_font().color(&footer_col)
                .pos(plotters::style::text_anchor::Pos::new(
                    plotters::style::text_anchor::HPos::Center,
                    plotters::style::text_anchor::VPos::Top,
                )),
        )).map_err(|e| e.to_string())?;

        root.present().map_err(|e| e.to_string())?;
    }

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&buf, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// Render the performance chart to a PNG file (960x460) using plotters.
fn export_performance_chart_png(
    path: &std::path::Path,
    drive_label: &str,
    test_label: &str,
    raw_points: &[[f64; 2]],
    chart_points: &[[f64; 2]],
    max_speed: f64,
    total_gb: f64,
) -> Result<(), String> {
    let bytes = render_performance_chart_png_bytes(
        960, 460, drive_label, test_label, raw_points, chart_points, max_speed, total_gb, false,
    )?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn sector_cell_color(cell: SectorCell, heat_min_ms: f64, heat_max_ms: f64) -> Color32 {
    match cell {
        SectorCell::Pending => Color32::from_rgb(60, 60, 60),
        SectorCell::Bad => Color32::from_rgb(244, 67, 54),
        SectorCell::Slow => Color32::from_rgb(255, 193, 7),
        SectorCell::Heat(t) => {
            if t >= surface_test::SLOW_THRESHOLD_MS {
                return Color32::from_rgb(255, 193, 7);
            }
            let min_t = heat_min_ms;
            let max_t = heat_max_ms;
            if min_t >= max_t || min_t == f64::MAX {
                return Color32::from_rgb(76, 175, 80);
            }
            let range = (max_t - min_t).max(0.1);
            let factor = ((t - min_t) / range).clamp(0.0, 1.0);
            let r = (76.0 - 61.0 * factor) as u8;
            let g = (175.0 - 125.0 * factor) as u8;
            let b = (80.0 - 62.0 * factor) as u8;
            Color32::from_rgb(r, g, b)
        }
    }
}

fn paint_speed_metric_cell(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: Rect,
    t: &Theme,
    title: &str,
    value: &str,
    foot: &str,
) {
    painter.rect_filled(rect, 8.0, t.bg_pri);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.5_f32, t.border), StrokeKind::Middle);

    let pad = 12.0_f32;
    let title_font = FontId::new(10.0, FontFamily::Proportional);
    let value_font = FontId::new(28.0, FontFamily::Monospace);
    let unit_font = FontId::proportional(13.0);
    let foot_font = FontId::proportional(11.0);

    let title_galley = ctx.fonts(|f| f.layout_no_wrap(title.to_string(), title_font, t.txt_sec));
    let title_w = title_galley.rect.width();
    let title_h = title_galley.rect.height();
    painter.galley(
        Pos2::new(rect.center().x - title_w * 0.5, rect.top() + pad),
        title_galley,
        t.txt_sec,
    );
    let title_bottom = rect.top() + pad + title_h;

    let foot_galley = ctx.fonts(|f| f.layout_no_wrap(foot.to_string(), foot_font, t.txt_sec));
    let foot_h = foot_galley.rect.height();
    let foot_w = foot_galley.rect.width();
    let foot_y = rect.bottom() - pad - foot_h;
    painter.galley(
        Pos2::new(rect.center().x - foot_w * 0.5, foot_y),
        foot_galley,
        t.txt_sec,
    );

    // Center value + unit vertically between title and footnote, horizontally in the card.
    let val_galley = ctx.fonts(|f| f.layout_no_wrap(value.to_string(), value_font, t.txt_pri));
    let unit_galley = ctx.fonts(|f| f.layout_no_wrap("MB/s".to_string(), unit_font, t.txt_sec));
    let gap = 8.0_f32;
    let val_w = val_galley.rect.width();
    let val_h = val_galley.rect.height();
    let unit_h = unit_galley.rect.height();
    let row_h = val_h.max(unit_h);
    let mid_y = (title_bottom + foot_y) * 0.5;
    let x0 = rect.center().x - (val_w + gap + unit_galley.rect.width()) * 0.5;
    let y_val = mid_y - row_h * 0.5;
    let y_unit = mid_y - unit_h * 0.5;
    painter.galley(Pos2::new(x0, y_val), val_galley, t.txt_pri);
    painter.galley(
        Pos2::new(x0 + val_w + gap, y_unit),
        unit_galley,
        t.txt_sec,
    );
}

impl DiskoriaApp {
    pub fn draw(&mut self, ctx: &egui::Context) {
        self.scroll_focus_frames = self.scroll_focus_frames.saturating_sub(1);

        // Frameless-window resize borders. Windows handles this in the
        // WM_NCHITTEST subclass (chrome::install_win32_resize); everywhere
        // else the hit-test is egui-side and starts a compositor resize.
        #[cfg(not(windows))]
        crate::chrome::handle_edge_resize(ctx);

        // Sync monitor-settings editing drafts from shared BEFORE any code
        // that reads them (notably start_monitor_if_not_running via
        // poll_drive_enumeration).  This ensures another window's settings
        // change — broadcast via SettingsChanged — actually takes effect
        // on the very next frame of each renderer.
        #[cfg(any(windows, target_os = "linux"))]
        {
            let s = self.shared.settings_snapshot();
            self.monitoring_enabled = s.monitoring_enabled;
            self.poll_interval_mins = s.poll_interval_mins;
            self.alert_temp_warn = s.alert_temp_warn;
            self.alert_temp_critical = s.alert_temp_critical;
            self.alert_wear_threshold = s.alert_wear_threshold;
        }

        self.poll_drive_enumeration(ctx);
        self.poll_smart_health(ctx);
        self.poll_surface_test(ctx);
        self.poll_destructive_test(ctx);
        self.poll_speed_test(ctx);
        self.poll_test_result_overlay_input(ctx);

        // Publish (or clear) this window's "drive under test" so other windows
        // can gray it out. Tests pin `selected_drive`, so it names the target.
        self.publish_test_lock();
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.maybe_start_auto_update_check(ctx);
            self.poll_update_check(ctx);
            self.poll_update_download(ctx);
        }
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.poll_monitor(ctx);
            self.poll_demo_alert();
        }
        self.update_modal_confirm_tab_focus(ctx);
        self.prepare_health_page_focus(ctx);
        self.prepare_sector_page_focus(ctx);
        self.prepare_destructive_page_focus(ctx);
        self.prepare_speed_page_focus(ctx);
        self.update_settings_tab_focus(ctx);
        self.update_about_page_focus(ctx);
        if self.any_test_running() {
            ctx.request_repaint();
        }
        self.alt_pressed = alt_pressed(ctx);
        #[cfg(any(windows, target_os = "linux"))]
        let update_blocks_shortcuts = self.pending_chart_export.is_some()
            || self.show_update_staged_modal
            || self.show_update_alert
            || self.update_check_busy
            || self.update_download_busy;
        #[cfg(not(any(windows, target_os = "linux")))]
        let update_blocks_shortcuts = false;
        if !self.any_test_running()
            && self.test_result_overlay.is_none()
            && !self.show_stop_test_confirm
            && !self.show_destructive_start_confirm
            && !self.show_destructive_stop_confirm
            && !update_blocks_shortcuts
        {
            if let Some(nav) = handle_alt_shortcuts(
                ctx,
                &[
                    ShortcutBinding {
                        key: Key::H,
                        action: 0usize,
                    },
                    ShortcutBinding {
                        key: Key::R,
                        action: 1,
                    },
                    ShortcutBinding {
                        key: Key::W,
                        action: 2,
                    },
                    ShortcutBinding {
                        key: Key::B,
                        action: 3,
                    },
                    ShortcutBinding {
                        key: Key::A,
                        action: 4,
                    },
                    ShortcutBinding {
                        key: Key::S,
                        action: 5,
                    },
                    ShortcutBinding {
                        key: Key::Num1,
                        action: 0,
                    },
                    ShortcutBinding {
                        key: Key::Num2,
                        action: 1,
                    },
                    ShortcutBinding {
                        key: Key::Num3,
                        action: 2,
                    },
                    ShortcutBinding {
                        key: Key::Num4,
                        action: 3,
                    },
                    ShortcutBinding {
                        key: Key::Num5,
                        action: 4,
                    },
                ],
            ) {
                self.active_nav = nav;
            }

        }

        // Ctrl+N → open a new Diskoria window in the same process.
        // Intentionally outside the shortcut guard above so it still
        // works while a test is running or a modal is open; opening a
        // new window has no interaction with in-flight test state.
        if ctx.input(|i| {
            i.modifiers.ctrl
                && !i.modifiers.shift
                && !i.modifiers.alt
                && i.key_pressed(Key::N)
        }) {
            let _ = self
                .shared
                .event_proxy
                .send_event(crate::UserEvent::OpenNewWindow);
        }

        let system_dark = match ctx.system_theme() {
            Some(egui::Theme::Dark) => true,
            Some(egui::Theme::Light) => false,
            // winit reports no system theme on X11/Wayland — ask the XDG
            // settings portal instead (cached; see theme::os_prefers_dark).
            #[cfg(target_os = "linux")]
            None => crate::theme::os_prefers_dark().unwrap_or(false),
            #[cfg(not(target_os = "linux"))]
            None => false,
        };
        // `--demo-theme` overrides both the OS and the saved preference, so a
        // reference capture of the light theme does not depend on the host.
        let dark = match crate::demo::config().dark {
            Some(d) => d,
            None => match self.shared.settings_snapshot().theme {
                ThemePref::Auto => system_dark,
                ThemePref::Dark => true,
                ThemePref::Light => false,
            },
        };
        self.dark = dark;

        self.update_accent_color(ctx);
        let accent = self.shared.accent_color();
        apply_visuals(ctx, dark, accent);

        let t = Theme::new(dark, accent);
        // Same paint order as copynaut: titlebar → sidebar → content (content last in default layer).
        if draw_titlebar(ctx, &t, self.hwnd) {
            let _ = self
                .shared
                .event_proxy
                .send_event(crate::UserEvent::OpenNewWindow);
        }
        self.draw_sidebar(ctx, dark);
        self.draw_central(ctx, dark);
        self.apply_health_page_focus_bindings(ctx);
        self.apply_sector_page_focus_bindings(ctx);
        self.apply_speed_page_focus_bindings(ctx);
        self.apply_destructive_page_focus_bindings(ctx);
        if self.show_stop_test_confirm {
            self.draw_stop_test_confirm(ctx, dark);
        }
        if self.show_destructive_start_confirm {
            self.draw_destructive_start_confirm(ctx, dark);
        }
        if self.show_destructive_stop_confirm {
            self.draw_destructive_stop_confirm(ctx, dark);
        }
        if self.pending_chart_export.is_some() {
            self.draw_chart_export_confirm(ctx, dark);
        }
        #[cfg(any(windows, target_os = "linux"))]
        {
            if self.show_update_staged_modal {
                self.draw_update_staged_modal(ctx, dark);
            }
            if self.show_update_alert {
                self.draw_update_alert(ctx, dark);
            }
            self.draw_update_busy_overlay(ctx, dark);
        }

        // PASS / WARN / FAIL result overlay sits above everything; any key or
        // click dismisses it. A FAIL raised mid-scan leaves the test running.
        if let Some(result) = self.test_result_overlay {
            if crate::test_result_overlay::draw_test_result_overlay(ctx, result) {
                self.test_result_overlay = None;
                ctx.request_repaint();
            }
        }
    }

    /// "Test Results" card — a single toggle for the PASS / WARN / FAIL overlay.
    /// Cross-platform: the overlay only fires on Windows (where tests run), but
    /// the preference is shown everywhere for parity.
    fn draw_settings_test_overlay(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        use crate::test_result_overlay::TestResult;

        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let row_h = 34.0_f32;

        let mut card = CardLayout::builder(content_x + margin, section_w)
            .title("Test Results")
            .begin(ui, t);
        let inner_x = card.inner_x();
        let slot = self.settings_test_overlay_slot();

        // ── Enable toggle (slot +0) ─────────────────────────────────────────
        {
            let row_rect = card.row(row_h);
            let toggle_rect = Rect::from_min_size(
                Pos2::new(card.right() - card.pad() - 44.0, row_rect.top() + (row_h - 24.0) / 2.0),
                Vec2::new(44.0, 24.0),
            );
            let focused = self.settings_focus == Some(slot);

            ui.painter().text(
                Pos2::new(inner_x, row_rect.center().y),
                Align2::LEFT_CENTER,
                "Show PASS / WARN / FAIL result overlay",
                FontId::new(13.0, egui::FontFamily::Proportional),
                t.txt_pri,
            );

            let toggle_resp = ui.interact(toggle_rect, Id::new("test_overlay_toggle"), Sense::click());
            let enabled = self.shared.settings_snapshot().show_test_result_overlays;
            crate::widgets::paint_toggle(ui, t, toggle_rect, enabled);

            let kb = page_keys && keyboard_activate(ui, focused);
            if toggle_resp.clicked() || kb {
                self.shared
                    .update_settings(|s| s.show_test_result_overlays = !s.show_test_result_overlays);
            }
            if focused {
                ui.painter().rect_stroke(
                    toggle_rect.expand(3.0),
                    14.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        }

        // ── Preview buttons (slots +1 Pass, +2 Warn, +3 Fail) ───────────────
        {
            let row_rect = card.row(row_h);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.center().y),
                Align2::LEFT_CENTER,
                "Preview overlay",
                FontId::new(12.0, egui::FontFamily::Proportional),
                t.txt_sec,
            );

            let btn_w = 80.0_f32;
            let btn_h = 24.0_f32;
            let btn_gap = 8.0_f32;
            let btn_y = row_rect.top() + (row_h - btn_h) / 2.0;
            // Right-aligned trio so it lines up with the toggle above.
            let fail_rect = Rect::from_min_size(Pos2::new(card.right() - card.pad() - btn_w, btn_y), Vec2::new(btn_w, btn_h));
            let warn_rect = Rect::from_min_size(Pos2::new(fail_rect.left() - btn_gap - btn_w, btn_y), Vec2::new(btn_w, btn_h));
            let pass_rect = Rect::from_min_size(Pos2::new(warn_rect.left() - btn_gap - btn_w, btn_y), Vec2::new(btn_w, btn_h));

            let buttons = [
                (pass_rect, "PASS", TestResult::Pass, slot + 1, Color32::from_rgb(46, 204, 113), Color32::BLACK, "test_overlay_pass"),
                (warn_rect, "WARN", TestResult::Warn, slot + 2, Color32::from_rgb(241, 196, 15), Color32::BLACK, "test_overlay_warn"),
                (fail_rect, "FAIL", TestResult::Fail, slot + 3, Color32::from_rgb(231, 53, 43), Color32::WHITE, "test_overlay_fail"),
            ];

            for (rect, label, result, btn_slot, col, fg, id) in buttons {
                let focused = self.settings_focus == Some(btn_slot);
                let resp = ui.interact(rect, Id::new(id), Sense::click());
                let fill = if resp.hovered() || focused { col.gamma_multiply(1.15) } else { col.gamma_multiply(0.9) };
                ui.painter().rect_filled(rect, 4.0, fill);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::new(12.0, FontFamily::Name("InterBold".into())),
                    fg,
                );
                if focused {
                    ui.painter().rect_stroke(rect.expand(3.0), 4.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
                }
                let kb = page_keys && keyboard_activate(ui, focused);
                if resp.clicked() || kb {
                    self.test_result_overlay = Some(result);
                }
                scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
            }
        }

        card.end(ui);
    }

    // ── Window-behaviour settings section ─────────────────────────────────────

    /// Single-row "Window" card holding the "Close to system tray" toggle.
    ///
    /// Unlike the Startup card below, this *is* a persisted setting — but its
    /// initial value is derived from `install_mode` (installed → on, portable →
    /// off), so a bare `diskoria.exe` quits on close the way users expect while
    /// an installed copy keeps monitoring from the tray. Windows-only: the tray
    /// subsystem doesn't exist on the non-Windows shell.
    #[cfg(windows)]
    fn draw_settings_window(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let row_h = 40.0_f32;

        let mut card = CardLayout::builder(content_x + margin, section_w)
            .title("Window")
            .begin(ui, t);
        let inner_x = card.inner_x();
        let slot = self.settings_window_slot();

        let enabled = self.shared.settings_snapshot().close_to_tray;
        let row_rect = card.row(row_h);
        let toggle_rect = Rect::from_min_size(
            Pos2::new(card.right() - card.pad() - 44.0, row_rect.top() + (row_h - 24.0) / 2.0),
            Vec2::new(44.0, 24.0),
        );
        let focused = self.settings_focus == Some(slot);

        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            "Close to system tray",
            FontId::new(13.0, egui::FontFamily::Proportional),
            t.txt_pri,
        );
        // Spell out the consequence of turning it off — quitting stops the
        // background drive monitoring, which isn't obvious from the label.
        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y + 9.0),
            Align2::LEFT_CENTER,
            if enabled {
                "Closing the last window keeps Diskoria monitoring from the tray"
            } else {
                "Closing the last window quits Diskoria and stops monitoring"
            },
            FontId::new(11.0, egui::FontFamily::Proportional),
            t.txt_sec,
        );

        let toggle_resp = ui.interact(toggle_rect, Id::new("close_to_tray_toggle"), Sense::click());
        crate::widgets::paint_toggle(ui, t, toggle_rect, enabled);

        let kb = page_keys && keyboard_activate(ui, focused);
        if toggle_resp.clicked() || kb {
            self.shared.update_settings(|s| s.close_to_tray = !s.close_to_tray);
        }
        if focused {
            ui.painter().rect_stroke(
                toggle_rect.expand(3.0),
                14.0,
                Stroke::new(2.0, t.accent),
                StrokeKind::Outside,
            );
        }
        scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);

        card.end(ui);
    }

    // ── Updates settings section ──────────────────────────────────────────────

    /// Single-row "Updates" card. On a Windows portable build the toggle is
    /// inert and greyed with an explanation — an update applies the Inno
    /// installer, which a portable exe has no business running (KI-23). The
    /// Linux portable binary self-replaces, so it is live there.
    #[cfg(any(windows, target_os = "linux"))]
    fn draw_settings_updates(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        let installed = crate::install_mode::current().is_installed();
        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let row_h = 40.0_f32;

        let mut card = CardLayout::builder(content_x + margin, section_w)
            .title("Updates")
            .begin(ui, t);
        let inner_x = card.inner_x();
        let slot = self.settings_updates_slot();

        // Portable builds never check, whatever the stored value says.
        let enabled = installed && self.shared.settings_snapshot().auto_check_updates;
        let row_rect = card.row(row_h);
        let toggle_rect = Rect::from_min_size(
            Pos2::new(card.right() - card.pad() - 44.0, row_rect.top() + (row_h - 24.0) / 2.0),
            Vec2::new(44.0, 24.0),
        );
        let focused = installed && self.settings_focus == Some(slot);

        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            "Check for updates automatically",
            FontId::new(13.0, egui::FontFamily::Proportional),
            if installed { t.txt_pri } else { t.txt_sec },
        );
        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y + 9.0),
            Align2::LEFT_CENTER,
            if !installed {
                "Unavailable — updates are handled by the installer"
            } else if enabled {
                "On launch. Updates install when you close Diskoria"
            } else {
                "Check manually from the About page"
            },
            FontId::new(11.0, egui::FontFamily::Proportional),
            t.txt_sec,
        );

        if installed {
            let toggle_resp =
                ui.interact(toggle_rect, Id::new("auto_check_updates_toggle"), Sense::click());
            crate::widgets::paint_toggle(ui, t, toggle_rect, enabled);
            let kb = page_keys && keyboard_activate(ui, focused);
            if toggle_resp.clicked() || kb {
                self.shared
                    .update_settings(|s| s.auto_check_updates = !s.auto_check_updates);
            }
            if focused {
                ui.painter().rect_stroke(
                    toggle_rect.expand(3.0),
                    14.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        } else {
            // Inert, dimmed track — reads as "off and not yours to change".
            ui.painter().rect_filled(toggle_rect, 12.0, t.border.gamma_multiply(0.5));
            ui.painter().circle_filled(
                Pos2::new(toggle_rect.left() + 12.0, toggle_rect.center().y),
                9.0,
                Color32::WHITE.gamma_multiply(0.5),
            );
        }

        card.end(ui);
    }

    // ── Launch-at-startup settings section ────────────────────────────────────

    /// Single-row "Launch at startup" card. State is the Windows scheduled task
    /// (`crate::autostart`), not a persisted setting — so an installed build
    /// (installer created the task) reads ON and a portable exe reads OFF with no
    /// mode-detection. The queried state is cached in `self.startup_enabled`.
    #[cfg(any(windows, target_os = "linux"))]
    fn draw_settings_startup(
        &mut self,
        ui: &mut egui::Ui,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let row_h = 40.0_f32;

        let mut card = CardLayout::builder(content_x + margin, section_w)
            .title("Startup")
            .begin(ui, t);
        let inner_x = card.inner_x();
        let slot = self.settings_startup_slot();

        // Query the scheduled task once (subprocess), then reuse the cache.
        let enabled = *self
            .startup_enabled
            .get_or_insert_with(crate::autostart::is_enabled);

        let row_rect = card.row(row_h);
        let toggle_rect = Rect::from_min_size(
            Pos2::new(card.right() - card.pad() - 44.0, row_rect.top() + (row_h - 24.0) / 2.0),
            Vec2::new(44.0, 24.0),
        );
        let focused = self.settings_focus == Some(slot);

        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y - 8.0),
            Align2::LEFT_CENTER,
            "Launch at startup",
            FontId::new(13.0, egui::FontFamily::Proportional),
            t.txt_pri,
        );
        ui.painter().text(
            Pos2::new(inner_x, row_rect.center().y + 9.0),
            Align2::LEFT_CENTER,
            "Start minimized to the system tray at logon",
            FontId::new(11.0, egui::FontFamily::Proportional),
            t.txt_sec,
        );

        let toggle_resp = ui.interact(toggle_rect, Id::new("startup_toggle"), Sense::click());
        crate::widgets::paint_toggle(ui, t, toggle_rect, enabled);

        let kb = page_keys && keyboard_activate(ui, focused);
        if toggle_resp.clicked() || kb {
            let want = !enabled;
            match crate::autostart::set_enabled(want) {
                Ok(()) => self.startup_enabled = Some(want),
                Err(e) => {
                    log::warn!(target: "diskoria", "failed to update launch-at-startup task: {e}");
                    // Re-sync from the OS so the toggle reflects reality.
                    self.startup_enabled = Some(crate::autostart::is_enabled());
                }
            }
        }
        if focused {
            ui.painter().rect_stroke(
                toggle_rect.expand(3.0),
                14.0,
                Stroke::new(2.0_f32, t.accent),
                StrokeKind::Outside,
            );
        }
        scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);

        card.end(ui);
    }

    // ── Pro-Monitoring settings section ───────────────────────────────────────

    #[cfg(any(windows, target_os = "linux"))]
    fn draw_settings_monitoring(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        t: &Theme,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        use crate::focus::{keyboard_activate, scroll_to_focused};

        let page_keys = !self.blocks_content_interaction();
        let section_w = content_w - margin * 2.0;
        let row_h = 34.0_f32;

        // The card sizes itself to the rows actually added, so the early return
        // below (monitoring off) genuinely shrinks it instead of leaving four
        // rows of empty frame. See `card.rs` / known-issues KI-18.
        let mut card = CardLayout::builder(content_x + margin, section_w)
            .title("Monitoring")
            .begin(ui, t);
        let inner_x = card.inner_x();

        // Monitoring slot numbering: M+0=toggle, M+1..4=poll segs, M+5=warn, M+6=crit, M+7=wear, M+8=test_warn, M+9=test_crit
        let m = self.settings_monitoring_slot_start();

        // ── Monitoring enabled toggle ───────────────────────────────────────
        {
            let row_rect = card.row(row_h);
            let toggle_rect = Rect::from_min_size(
                Pos2::new(card.right() - card.pad() - 44.0, row_rect.top() + (row_h - 24.0) / 2.0),
                Vec2::new(44.0, 24.0),
            );
            let focused = self.settings_focus == Some(m);

            ui.painter().text(
                Pos2::new(inner_x, row_rect.center().y),
                Align2::LEFT_CENTER,
                "Enable background monitoring",
                FontId::new(13.0, egui::FontFamily::Proportional),
                t.txt_pri,
            );

            let toggle_resp = ui.interact(toggle_rect, Id::new("mon_enabled_toggle"), Sense::click());
            crate::widgets::paint_toggle(ui, t, toggle_rect, self.monitoring_enabled);

            let kb = page_keys && keyboard_activate(ui, focused);
            if toggle_resp.clicked() || kb {
                self.monitoring_enabled = !self.monitoring_enabled;
                self.save_app_settings();
                if !self.monitoring_enabled {
                    self.shared.cancel_monitor();
                }
            }
            if focused {
                ui.painter().rect_stroke(
                    toggle_rect.expand(3.0),
                    14.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        }

        // ── Poll interval ───────────────────────────────────────────────────
        {
            const OPTS: [(&str, u8); 4] = [("1 min", 1), ("3 min", 3), ("5 min", 5), ("10 min", 10)];
            let row_rect = card.row(row_h);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.top() + row_h / 2.0 - 7.0),
                Align2::LEFT_TOP,
                "Poll interval",
                FontId::new(12.0, egui::FontFamily::Proportional),
                t.txt_sec,
            );
            let seg_total_w = card.inner_w() - 130.0;
            let seg_x = inner_x + 130.0;
            let seg_h = 24.0_f32;
            let seg_w = seg_total_w / OPTS.len() as f32;
            let seg_top = row_rect.top() + (row_h - seg_h) / 2.0;
            let seg_rect = Rect::from_min_size(Pos2::new(seg_x, seg_top), Vec2::new(seg_total_w, seg_h));
            ui.painter().rect_filled(seg_rect, 6.0, t.bg_sec);
            ui.painter().rect_stroke(seg_rect, 6.0, Stroke::new(1.0_f32, t.border), StrokeKind::Middle);
            for (i, (label, mins)) in OPTS.iter().enumerate() {
                let seg = Rect::from_min_size(
                    Pos2::new(seg_x + i as f32 * seg_w, seg_top),
                    Vec2::new(seg_w, seg_h),
                );
                let selected = self.poll_interval_mins == *mins;
                let focused = self.settings_focus == Some(m + 1 + i);
                let resp = ui.interact(seg, Id::new(("poll_interval_seg", i)), INTERACT_MANUAL_FOCUS);
                if selected {
                    ui.painter().rect_filled(seg, 6.0, t.accent);
                } else if resp.hovered() {
                    ui.painter().rect_filled(seg, 6.0, t.hover);
                }
                let txt_col = if selected { t.txt_on_accent } else { t.txt_pri };
                ui.painter().text(seg.center(), Align2::CENTER_CENTER, *label, FontId::proportional(12.0), txt_col);
                if focused {
                    ui.painter().rect_stroke(seg.expand(3.0), 6.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
                }
                let kb = page_keys && keyboard_activate(ui, focused);
                if resp.clicked() || kb {
                    self.poll_interval_mins = *mins;
                    self.save_app_settings();
                    self.shared.cancel_monitor();
                }
                scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
            }
        }

        if !self.monitoring_enabled {
            card.end(ui);
            return;
        }

        // ── Temp warn threshold ─────────────────────────────────────────────
        {
            let focused = self.settings_focus == Some(m + 5);
            let row_rect = card.row(row_h);
            let label = format!("Temperature warning  ({}°C)", self.alert_temp_warn);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.top() + row_h / 2.0 - 7.0),
                Align2::LEFT_TOP,
                label,
                FontId::new(12.0, egui::FontFamily::Proportional),
                if focused { t.txt_pri } else { t.txt_sec },
            );
            let slider_rect = Rect::from_min_size(
                Pos2::new(inner_x + 240.0, row_rect.top() + (row_h - 20.0) / 2.0),
                Vec2::new(card.inner_w() - 250.0, 20.0),
            );
            let resp = ui.allocate_rect(slider_rect, Sense::click_and_drag());
            if resp.dragged() || resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let frac = ((pos.x - slider_rect.left()) / slider_rect.width()).clamp(0.0, 1.0);
                    self.alert_temp_warn = (30.0 + frac * 50.0) as i32;
                    self.alert_temp_warn = self.alert_temp_warn.min(self.alert_temp_critical - 1);
                    self.save_app_settings();
                }
            }
            if focused && page_keys {
                let delta = ui.input_mut(|i| {
                    let r = i.consume_key(Modifiers::NONE, Key::ArrowRight);
                    let l = i.consume_key(Modifiers::NONE, Key::ArrowLeft);
                    if r { 1i32 } else if l { -1 } else { 0 }
                });
                if delta != 0 {
                    self.alert_temp_warn = (self.alert_temp_warn + delta).clamp(30, 80);
                    self.alert_temp_warn = self.alert_temp_warn.min(self.alert_temp_critical - 1);
                    self.save_app_settings();
                }
            }
            let frac = ((self.alert_temp_warn - 30) as f32 / 50.0).clamp(0.0, 1.0);
            ui.painter().rect_filled(slider_rect, 2.0, t.border);
            ui.painter().rect_filled(
                Rect::from_min_size(slider_rect.min, Vec2::new(slider_rect.width() * frac, slider_rect.height())),
                2.0, t.accent,
            );
            ui.painter().circle_filled(
                Pos2::new(slider_rect.left() + slider_rect.width() * frac, slider_rect.center().y),
                6.0, t.accent,
            );
            if focused {
                ui.painter().rect_stroke(slider_rect.expand(3.0), 3.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        }

        // ── Temp critical threshold ─────────────────────────────────────────
        {
            let focused = self.settings_focus == Some(m + 6);
            let row_rect = card.row(row_h);
            let label = format!("Temperature critical  ({}°C)", self.alert_temp_critical);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.top() + row_h / 2.0 - 7.0),
                Align2::LEFT_TOP,
                label,
                FontId::new(12.0, egui::FontFamily::Proportional),
                if focused { t.txt_pri } else { t.txt_sec },
            );
            let slider_rect = Rect::from_min_size(
                Pos2::new(inner_x + 240.0, row_rect.top() + (row_h - 20.0) / 2.0),
                Vec2::new(card.inner_w() - 250.0, 20.0),
            );
            let resp = ui.allocate_rect(slider_rect, Sense::click_and_drag());
            if resp.dragged() || resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let frac = ((pos.x - slider_rect.left()) / slider_rect.width()).clamp(0.0, 1.0);
                    self.alert_temp_critical = (40.0 + frac * 55.0) as i32;
                    self.alert_temp_critical = self.alert_temp_critical.max(self.alert_temp_warn + 1);
                    self.save_app_settings();
                }
            }
            if focused && page_keys {
                let delta = ui.input_mut(|i| {
                    let r = i.consume_key(Modifiers::NONE, Key::ArrowRight);
                    let l = i.consume_key(Modifiers::NONE, Key::ArrowLeft);
                    if r { 1i32 } else if l { -1 } else { 0 }
                });
                if delta != 0 {
                    self.alert_temp_critical = (self.alert_temp_critical + delta).clamp(40, 95);
                    self.alert_temp_critical = self.alert_temp_critical.max(self.alert_temp_warn + 1);
                    self.save_app_settings();
                }
            }
            let crit_col = Color32::from_rgb(231, 76, 60);
            let frac = ((self.alert_temp_critical - 40) as f32 / 55.0).clamp(0.0, 1.0);
            ui.painter().rect_filled(slider_rect, 2.0, t.border);
            ui.painter().rect_filled(
                Rect::from_min_size(slider_rect.min, Vec2::new(slider_rect.width() * frac, slider_rect.height())),
                2.0, crit_col,
            );
            ui.painter().circle_filled(
                Pos2::new(slider_rect.left() + slider_rect.width() * frac, slider_rect.center().y),
                6.0, crit_col,
            );
            if focused {
                ui.painter().rect_stroke(slider_rect.expand(3.0), 3.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        }

        // ── Wear threshold ──────────────────────────────────────────────────
        {
            let focused = self.settings_focus == Some(m + 7);
            let row_rect = card.row(row_h);
            let label = format!("Wear level alert  ({}%)", self.alert_wear_threshold);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.top() + row_h / 2.0 - 7.0),
                Align2::LEFT_TOP,
                label,
                FontId::new(12.0, egui::FontFamily::Proportional),
                if focused { t.txt_pri } else { t.txt_sec },
            );
            let slider_rect = Rect::from_min_size(
                Pos2::new(inner_x + 240.0, row_rect.top() + (row_h - 20.0) / 2.0),
                Vec2::new(card.inner_w() - 250.0, 20.0),
            );
            let resp = ui.allocate_rect(slider_rect, Sense::click_and_drag());
            if resp.dragged() || resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let frac = ((pos.x - slider_rect.left()) / slider_rect.width()).clamp(0.0, 1.0);
                    self.alert_wear_threshold = (50.0 + frac * 50.0) as u8;
                    self.save_app_settings();
                }
            }
            if focused && page_keys {
                let delta = ui.input_mut(|i| {
                    let r = i.consume_key(Modifiers::NONE, Key::ArrowRight);
                    let l = i.consume_key(Modifiers::NONE, Key::ArrowLeft);
                    if r { 1i32 } else if l { -1 } else { 0 }
                });
                if delta != 0 {
                    self.alert_wear_threshold = ((self.alert_wear_threshold as i32 + delta).clamp(50, 100)) as u8;
                    self.save_app_settings();
                }
            }
            let frac = ((self.alert_wear_threshold.saturating_sub(50)) as f32 / 50.0).clamp(0.0, 1.0);
            ui.painter().rect_filled(slider_rect, 2.0, t.border);
            ui.painter().rect_filled(
                Rect::from_min_size(slider_rect.min, Vec2::new(slider_rect.width() * frac, slider_rect.height())),
                2.0, t.accent,
            );
            ui.painter().circle_filled(
                Pos2::new(slider_rect.left() + slider_rect.width() * frac, slider_rect.center().y),
                6.0, t.accent,
            );
            if focused {
                ui.painter().rect_stroke(slider_rect.expand(3.0), 3.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, focused, self.scroll_focus_frames > 0);
        }

        // ── Test notifications ──────────────────────────────────────────────
        {
            let warn_focused = self.settings_focus == Some(m + 8);
            let crit_focused = self.settings_focus == Some(m + 9);
            let row_rect = card.row(row_h);
            ui.painter().text(
                Pos2::new(inner_x, row_rect.top() + row_h / 2.0 - 7.0),
                Align2::LEFT_TOP,
                "Test notifications",
                FontId::new(12.0, egui::FontFamily::Proportional),
                t.txt_sec,
            );

            let btn_w = 90.0_f32;
            let btn_h = 24.0_f32;
            let btn_y = row_rect.top() + (row_h - btn_h) / 2.0;
            let btn_gap = 8.0_f32;
            let warn_rect = Rect::from_min_size(Pos2::new(inner_x + 160.0, btn_y), Vec2::new(btn_w, btn_h));
            let crit_rect = Rect::from_min_size(Pos2::new(warn_rect.right() + btn_gap, btn_y), Vec2::new(btn_w, btn_h));

            let warn_resp = ui.interact(warn_rect, Id::new("test_warn_btn"), Sense::click());
            let crit_resp = ui.interact(crit_rect, Id::new("test_crit_btn"), Sense::click());

            let warn_col = Color32::from_rgb(241, 196, 15);
            let crit_col = Color32::from_rgb(231, 76, 60);

            ui.painter().rect_filled(warn_rect, 4.0, if warn_resp.hovered() || warn_focused { warn_col.gamma_multiply(1.15) } else { warn_col.gamma_multiply(0.85) });
            ui.painter().text(warn_rect.center(), Align2::CENTER_CENTER, "Warning", FontId::new(12.0, egui::FontFamily::Proportional), Color32::BLACK);

            ui.painter().rect_filled(crit_rect, 4.0, if crit_resp.hovered() || crit_focused { crit_col.gamma_multiply(1.15) } else { crit_col });
            ui.painter().text(crit_rect.center(), Align2::CENTER_CENTER, "Critical", FontId::new(12.0, egui::FontFamily::Proportional), Color32::WHITE);

            if warn_focused {
                ui.painter().rect_stroke(warn_rect.expand(3.0), 4.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
            }
            if crit_focused {
                ui.painter().rect_stroke(crit_rect.expand(3.0), 4.0, Stroke::new(2.0_f32, t.accent), StrokeKind::Outside);
            }

            let (drive_serial, drive_model) = self.drives.first()
                .map(|d| (d.serial.clone(), d.model.clone()))
                .unwrap_or_else(|| ("TEST-0001".to_string(), "Test Drive".to_string()));

            let warn_kb = page_keys && keyboard_activate(ui, warn_focused);
            if (warn_resp.clicked() || warn_kb) && !self.is_alert_suppressed(&drive_serial) {
                if let Some(proxy) = &self.event_proxy {
                    let _ = proxy.send_event(crate::UserEvent::DriveAlert {
                        serial: drive_serial.clone(),
                        is_critical: false,
                    });
                }
                self.pending_alerts.push(crate::alert_engine::AlertEvent {
                    serial: drive_serial.clone(),
                    model: drive_model.clone(),
                    condition: crate::alert_engine::AlertCondition::TemperatureWarning,
                    level: crate::alert_engine::AlertLevel::Warning,
                    detail: format!("{} — temperature reached 62°C (threshold: {}°C)", drive_model, self.alert_temp_warn),
                });
            }
            let crit_kb = page_keys && keyboard_activate(ui, crit_focused);
            if (crit_resp.clicked() || crit_kb) && !self.is_alert_suppressed(&drive_serial) {
                if let Some(proxy) = &self.event_proxy {
                    let _ = proxy.send_event(crate::UserEvent::DriveAlert {
                        serial: drive_serial.clone(),
                        is_critical: true,
                    });
                }
                self.pending_alerts.push(crate::alert_engine::AlertEvent {
                    serial: drive_serial,
                    model: drive_model.clone(),
                    condition: crate::alert_engine::AlertCondition::NewUncorrectableSectors,
                    level: crate::alert_engine::AlertLevel::Critical,
                    detail: format!("{} — uncorrectable sector count increased (now 3)", drive_model),
                });
            }
            scroll_to_focused(&mut self.pending_scroll_rect, row_rect, warn_focused || crit_focused, self.scroll_focus_frames > 0);
        }

        // The sliders' `allocate_rect` calls above still rewind the layout
        // cursor; `end()` is the last advance, so it wins and the next card
        // lands below this one regardless (KI-18).
        card.end(ui);
    }
}

/// Pure core of [`DiskoriaApp::speed_target_pair`] — kept free of `DiskoriaApp`
/// so the KI-15 rule ("clamp the partition, never move the drive") is unit
/// testable. `volumes_on(i)` is the mounted-volume count of drive `i`.
/// `eligible_on(drive)` yields the partition indices that can actually host
/// the benchmark file — mounted, unlocked volumes (see
/// `partition_info::benchmarkable_partitions`). Anything else has nowhere to
/// write: an unmounted partition has no path, and using its empty mount point
/// silently benchmarked whatever filesystem the resulting relative path landed
/// on (KI-38).
fn speed_target(
    drive_count: usize,
    eligible_on: impl Fn(usize) -> Vec<usize>,
    selected_drive: usize,
    partition: usize,
) -> Option<(usize, usize)> {
    let di = selected_drive.min(drive_count.checked_sub(1)?);
    let eligible = eligible_on(di);
    if eligible.is_empty() {
        return None;
    }
    // Keep the user's pick when it is still usable; otherwise fall back to the
    // drive's first eligible volume. The *drive* is never changed (KI-15).
    let pi = if eligible.contains(&partition) {
        partition
    } else {
        eligible[0]
    };
    Some((di, pi))
}

#[cfg(test)]
mod tests {
    use super::speed_target;

    /// `target(&[&[0, 1], &[], &[2]], sel, part)` — three drives whose
    /// benchmarkable (mounted, unlocked) partition indices are listed.
    fn target(
        eligible: &[&[usize]],
        selected_drive: usize,
        partition: usize,
    ) -> Option<(usize, usize)> {
        speed_target(
            eligible.len(),
            |i| eligible[i].to_vec(),
            selected_drive,
            partition,
        )
    }

    #[test]
    fn keeps_the_selected_drive_when_it_has_a_volume() {
        assert_eq!(target(&[&[0, 1], &[], &[0]], 0, 1), Some((0, 1)));
        assert_eq!(target(&[&[0, 1], &[], &[0]], 2, 0), Some((2, 0)));
    }

    /// KI-38: an unmounted partition is not a benchmark target — the page must
    /// report "no mounted volume" instead of writing to whatever path an empty
    /// mount point produces.
    #[test]
    fn unmounted_partitions_are_never_targets() {
        // Drive 0 has partitions 0..3 but only #2 is mounted.
        assert_eq!(target(&[&[2]], 0, 0), Some((0, 2)));
        assert_eq!(target(&[&[2]], 0, 1), Some((0, 2)));
        assert_eq!(target(&[&[2]], 0, 2), Some((0, 2)));
        // Nothing mounted at all → no target.
        assert_eq!(target(&[&[]], 0, 0), None);
    }

    /// The KI-15 regression: a partition-less selection must *not* be silently
    /// repointed at another drive that happens to have a volume.
    #[test]
    fn partitionless_selection_yields_no_target_instead_of_jumping() {
        assert_eq!(target(&[&[0, 1], &[], &[0]], 1, 0), None);
    }

    #[test]
    fn stale_partition_falls_back_to_the_first_eligible_one() {
        // Stale index from a drive that had more volumes.
        assert_eq!(target(&[&[0, 1], &[], &[0]], 0, 7), Some((0, 0)));
        assert_eq!(target(&[&[0, 1], &[], &[0]], 2, 7), Some((2, 0)));
    }

    #[test]
    fn drive_index_past_the_end_is_clamped_not_wrapped() {
        assert_eq!(target(&[&[0, 1], &[], &[0]], 9, 0), Some((2, 0)));
        // …and clamping onto a volume-less last drive still yields no target.
        assert_eq!(target(&[&[0], &[]], 9, 0), None);
    }

    #[test]
    fn no_drives_and_no_volumes_anywhere_yield_no_target() {
        assert_eq!(target(&[], 0, 0), None);
        assert_eq!(target(&[&[], &[]], 0, 0), None);
        assert_eq!(target(&[&[], &[]], 1, 3), None);
    }
}
