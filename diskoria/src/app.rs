//! Main shell: sidebar + central pages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Id, Key, LayerId, Modifiers, Order,
    Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use egui::text::{LayoutJob, TextFormat};
use egui_plot::{Line, MarkerShape, Plot, PlotPoints, Points};

use crate::app_settings::{
    self, accent_from_palette, color_to_hex_6, initial_accent_color, parse_hex_color_6,
    AccentSourcePref, ThemePref, ACCENT_PALETTE, ACCENT_PALETTE_LABELS,
};
use crate::chrome::{
    apply_win11_rounded_corners, draw_titlebar, load_logo_textures, setup_fonts,
    INTERACT_MANUAL_FOCUS,
};
use crate::detected_drive::{BusKind, DetectedDrive, MediaKind};
use crate::drive_enumeration;
use crate::partition_info::PartitionInfo;
use crate::shortcuts::{handle_alt_shortcuts, alt_pressed, ShortcutBinding};
use crate::surface_test::{
    self, SurfaceTestMsg, SurfaceTestProgress, TOTAL_UI_BLOCKS,
};
use crate::modal_confirm::{
    one_button_modal, two_button_modal, ModalConfirmPrimary, ModalConfirmResult,
    OneButtonModalParams, TwoButtonModalParams,
};
use crate::smart_health::SmartHealth;
use crate::speed_test::{self, SpeedTestMsg};
use crate::theme::{
    apply_visuals, windows_accent_color, Theme, CLOSE_HOVER_BG, CONTENT_MARGIN, MAX_CONTENT_W,
    SIDE_PANEL_W, TITLEBAR_H,
};
use crate::focus::{apply_manual_focus_event_filter, close_popup_if_selectable_clicked};
use crate::widgets::show_tooltip_text;

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

// Bootstrap Icons PUA (v1.11).
const NAV_TOP: &[(&str, &str)] = &[
    ("\u{f3f8}", "Sector Test"),
    ("\u{f33b}", "Destructive Test"),
    ("\u{f57f}", "Speed Test"),
    ("\u{f473}", "Health Status"),
];
const NAV_BOTTOM: &[(&str, &str)] = &[
    ("\u{f431}", "About"),
    ("\u{f3e5}", "Settings"),
];

/// Sidebar nav index 0..4. Mnemonics: **e** Sector, **d** Destructive, **p** Speed, **a** About, **s** Settings.
fn nav_mnemonic_char(nav_index: usize) -> Option<char> {
    match nav_index {
        0 => Some('e'),
        1 => Some('d'),
        2 => Some('p'),
        3 => Some('a'),
        4 => Some('s'),
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
    let m = mnemonic.to_ascii_lowercase();
    let mut n_before = 0usize;
    let mut matched_ch = None;
    for ch in label.chars() {
        if ch.to_ascii_lowercase() == m {
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

pub struct DiskoriaApp {
    /// Current resolved dark state — updated at start of each `draw()` call.
    /// Exposed so the softbuffer renderer can fill the background the right colour.
    pub dark: bool,
    /// Win32 HWND — used to query `IsZoomed` for reliable maximized state.
    #[cfg(windows)]
    hwnd: isize,
    theme_pref: ThemePref,
    accent_source: AccentSourcePref,
    accent_palette_idx: usize,
    accent_use_custom: bool,
    accent_custom_hex: String,
    pub accent_color: Color32,
    #[cfg(windows)]
    accent_last_poll: Option<std::time::Instant>,
    accent_custom_te_id: Option<Id>,
    /// Settings page (`active_nav == 4`): manual keyboard focus slots (see example app).
    settings_focus: Option<usize>,
    scroll_focus_frames: u8,
    pending_scroll_rect: Option<Rect>,
    active_nav: usize,
    alt_pressed: bool,
    logo: Option<egui::TextureHandle>,
    logo_light: Option<egui::TextureHandle>,
    logo_size: [usize; 2],
    pub(crate) drives: Vec<DetectedDrive>,
    pub(crate) drives_loading: bool,
    pub(crate) drives_error: Option<String>,
    drive_poll_rx: Option<mpsc::Receiver<Result<Vec<DetectedDrive>, String>>>,
    selected_drive: usize,

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
    surface_total_sectors: i64,
    surface_good_sectors: i64,
    surface_bad_sectors: i64,
    surface_slow_sectors: i64,
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
    smart_health_err: Option<String>,
    smart_health_disk: Option<u32>,
    smart_health_rx: Option<mpsc::Receiver<(u32, Result<SmartHealth, String>)>>,
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
    destructive_total_sectors: i64,
    destructive_good_sectors: i64,
    destructive_bad_sectors: i64,
    destructive_slow_sectors: i64,
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
    /// Drive selection for destructive test (independent of Sector Test selection).
    selected_destructive_drive: usize,
    /// Confirm before starting (data destruction warning).
    show_destructive_start_confirm: bool,
    destructive_start_confirm_focus: Option<usize>,
    /// Confirm before stopping an in-progress destructive test.
    show_destructive_stop_confirm: bool,
    destructive_stop_confirm_focus: Option<usize>,

    /// Health Status page state.
    pub(crate) health_selected_drive: usize,
    pub(crate) health_report: Option<crate::smart_reader::SmartReport>,
    pub(crate) health_poll_rx: Option<std::sync::mpsc::Receiver<crate::smart_reader::SmartReport>>,
    pub(crate) health_poll_running: bool,
    /// When the last completed SMART poll finished (used for live refresh).
    pub(crate) health_last_poll: Option<std::time::Instant>,

    pub(crate) about_appicon: Option<egui::TextureHandle>,
    #[cfg(windows)]
    update_check_rx: Option<mpsc::Receiver<Result<crate::update::UpdateCheckResult, String>>>,
    #[cfg(windows)]
    update_download_rx: Option<mpsc::Receiver<Result<std::path::PathBuf, String>>>,
    #[cfg(windows)]
    show_update_download_confirm: bool,
    #[cfg(windows)]
    pending_update_version: String,
    #[cfg(windows)]
    pending_update_url: String,
    #[cfg(windows)]
    show_update_alert: bool,
    #[cfg(windows)]
    update_alert_title: String,
    #[cfg(windows)]
    update_alert_body: String,
    #[cfg(windows)]
    update_check_busy: bool,
    #[cfg(windows)]
    update_download_busy: bool,
    #[cfg(windows)]
    update_download_confirm_focus: Option<usize>,
    #[cfg(windows)]
    update_alert_focus: Option<usize>,
}

impl DiskoriaApp {
    pub fn new(ctx: &egui::Context, system_dark: bool, hwnd: isize) -> Self {
        setup_fonts(ctx);
        apply_win11_rounded_corners(hwnd);
        #[cfg(windows)]
        crate::chrome::install_win32_resize(hwnd);

        static LOGO_PNG: &[u8] = include_bytes!("../../applogo.png");
        let (logo, logo_light, logo_size) = load_logo_textures(ctx, LOGO_PNG);

        static ABOUT_PNG: &[u8] = include_bytes!("../../appicon.png");
        let about_appicon = crate::chrome::load_appicon_texture(ctx, ABOUT_PNG);

        let s = app_settings::load_settings();
        let accent_color = initial_accent_color(&s);
        let theme_pref = s.theme;
        let accent_source = s.accent_source;
        let accent_palette_idx = s.accent_palette_idx;
        let accent_use_custom = s.accent_use_custom;
        let accent_custom_hex = s.accent_custom_hex;

        let dark = match theme_pref {
            ThemePref::Auto => system_dark,
            ThemePref::Dark => true,
            ThemePref::Light => false,
        };
        apply_visuals(ctx, dark, accent_color);

        let (tx, rx) = mpsc::channel();
        #[cfg(windows)]
        {
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let result = drive_enumeration::enumerate_physical_disks();
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
        }
        #[cfg(not(windows))]
        {
            let _ = tx.send(Err(
                "Drive enumeration runs on Windows only.".to_string(),
            ));
            ctx.request_repaint();
        }

        Self {
            dark,
            #[cfg(windows)]
            hwnd,
            theme_pref,
            accent_source,
            accent_palette_idx,
            accent_use_custom,
            accent_custom_hex,
            accent_color,
            #[cfg(windows)]
            accent_last_poll: None,
            accent_custom_te_id: None,
            settings_focus: None,
            scroll_focus_frames: 0,
            pending_scroll_rect: None,
            active_nav: 0,
            alt_pressed: false,
            logo,
            logo_light,
            logo_size,
            drives: Vec::new(),
            drives_loading: true,
            drives_error: None,
            drive_poll_rx: Some(rx),
            selected_drive: 0,
            surface_test_running: false,
            surface_test_rx: None,
            surface_test_cancel: None,
            surface_test_target: None,
            sector_cells: (0..TOTAL_UI_BLOCKS).map(|_| SectorCell::Pending).collect(),
            heat_min_ms: f64::MAX,
            heat_max_ms: f64::MIN,
            surface_progress_pct: 0.0,
            surface_avg_speed_mbps: 0.0,
            surface_total_sectors: 0,
            surface_good_sectors: 0,
            surface_bad_sectors: 0,
            surface_slow_sectors: 0,
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
            destructive_total_sectors: 0,
            destructive_good_sectors: 0,
            destructive_bad_sectors: 0,
            destructive_slow_sectors: 0,
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
            selected_destructive_drive: 0,
            show_destructive_start_confirm: false,
            destructive_start_confirm_focus: None,
            show_destructive_stop_confirm: false,
            destructive_stop_confirm_focus: None,
            health_selected_drive: 0,
            health_report: None,
            health_poll_rx: None,
            health_poll_running: false,
            health_last_poll: None,
            about_appicon,
            #[cfg(windows)]
            update_check_rx: None,
            #[cfg(windows)]
            update_download_rx: None,
            #[cfg(windows)]
            show_update_download_confirm: false,
            #[cfg(windows)]
            pending_update_version: String::new(),
            #[cfg(windows)]
            pending_update_url: String::new(),
            #[cfg(windows)]
            show_update_alert: false,
            #[cfg(windows)]
            update_alert_title: String::new(),
            #[cfg(windows)]
            update_alert_body: String::new(),
            #[cfg(windows)]
            update_check_busy: false,
            #[cfg(windows)]
            update_download_busy: false,
            #[cfg(windows)]
            update_download_confirm_focus: None,
            #[cfg(windows)]
            update_alert_focus: None,
        }
    }

    fn any_test_running(&self) -> bool {
        self.surface_test_running || self.destructive_test_running || self.speed_test_running
    }

    /// Returns the drive index that the currently-active page has selected.
    /// Used to drive SMART health polling and the shared health card.
    fn active_page_selected_drive_idx(&self) -> usize {
        let idx = if self.active_nav == 1 {
            self.selected_destructive_drive
        } else {
            self.selected_drive
        };
        idx.min(self.drives.len().saturating_sub(1))
    }

    #[cfg(windows)]
    pub(crate) fn update_check_button_enabled(&self) -> bool {
        !self.update_check_busy
            && !self.update_download_busy
            && self.update_check_rx.is_none()
            && self.update_download_rx.is_none()
            && !self.show_update_download_confirm
            && !self.show_update_alert
    }

    #[cfg(windows)]
    pub(crate) fn on_about_check_updates_clicked(&mut self, ctx: &egui::Context) {
        if !self.update_check_button_enabled() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.update_check_rx = Some(rx);
        self.update_check_busy = true;
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let out = crate::update::check_for_update_blocking();
            let _ = tx.send(out);
            ctx2.request_repaint();
        });
    }

    #[cfg(windows)]
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
                self.show_update_alert = true;
                self.update_alert_title = "Up to date".to_string();
                self.update_alert_body =
                    "You are running the latest release.".to_string();
                ctx.request_repaint();
            }
            Ok(Ok(crate::update::UpdateCheckResult::UpdateAvailable {
                version_display,
                download_url,
            })) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
                self.show_update_download_confirm = true;
                self.pending_update_version = version_display;
                self.pending_update_url = download_url;
                ctx.request_repaint();
            }
            Ok(Err(e)) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
                self.show_update_alert = true;
                self.update_alert_title = "Update check failed".to_string();
                self.update_alert_body = e;
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.update_check_rx = None;
                self.update_check_busy = false;
            }
        }
    }

    #[cfg(windows)]
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
                if name.contains("setup") && name.ends_with(".exe") {
                    crate::update::spawn_run_installer_and_exit(&path);
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

    #[cfg(windows)]
    fn draw_update_download_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.accent_color);
        let body = format!(
            "Version {} is available on GitHub. Download and install now?",
            self.pending_update_version
        );
        match two_button_modal(
            ctx,
            &t,
            TwoButtonModalParams {
                overlay_id: Id::new("diskoria_update_dl_overlay"),
                dialog_id: Id::new("diskoria_update_dl_dialog"),
                width: 400.0,
                height: 200.0,
                title: "Update available",
                body: &body,
                cancel_id: Id::new("diskoria_update_dl_cancel"),
                cancel_label: "Cancel",
                confirm_id: Id::new("diskoria_update_dl_ok"),
                confirm: ModalConfirmPrimary::AccentIcon {
                    label: "Download",
                    icon: '\u{f295}',
                },
            },
            &mut self.update_download_confirm_focus,
        ) {
            Some(ModalConfirmResult::Cancel) => {
                self.show_update_download_confirm = false;
                self.pending_update_version.clear();
                self.pending_update_url.clear();
            }
            Some(ModalConfirmResult::Confirm) => {
                self.show_update_download_confirm = false;
                let url = self.pending_update_url.clone();
                self.pending_update_version.clear();
                self.pending_update_url.clear();
                let dest = std::env::temp_dir().join(format!(
                    "Diskoria_update_{}.exe",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
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
            None => {}
        }
    }

    #[cfg(windows)]
    fn draw_update_alert(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.accent_color);
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

    #[cfg(windows)]
    fn draw_update_busy_overlay(&self, ctx: &egui::Context, dark: bool) {
        if !self.update_check_busy && !self.update_download_busy {
            return;
        }
        let t = Theme::new(dark, self.accent_color);
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

    fn sync_speed_partition_after_drives_refresh(&mut self) {
        if self.drives.is_empty() {
            return;
        }
        let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
        let parts = &self.drives[sel].partitions;
        if parts.is_empty() {
            self.selected_speed_partition = 0;
        } else if self.selected_speed_partition >= parts.len() {
            self.selected_speed_partition = parts.len() - 1;
        }
        self.ensure_speed_volume_selection_valid();
    }

    fn speed_volume_pairs(drives: &[DetectedDrive]) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for (di, d) in drives.iter().enumerate() {
            for pi in 0..d.partitions.len() {
                v.push((di, pi));
            }
        }
        v
    }

    fn format_speed_volume_row(d: &DetectedDrive, p: &PartitionInfo) -> String {
        let letter = p.drive_letter.trim().trim_end_matches('\\');
        let letter = letter.trim_end_matches(':');
        let vol = if letter.is_empty() {
            "(no letter)".to_string()
        } else {
            format!("{}:\\", letter)
        };
        let free = p.free_space_display();
        let model = d.model.trim();
        format!("{} - {} free - {}", vol, free, model)
    }

    /// Keep `(selected_drive, selected_speed_partition)` pointing at a real mounted volume for speed test.
    fn ensure_speed_volume_selection_valid(&mut self) {
        let pairs = Self::speed_volume_pairs(&self.drives);
        if pairs.is_empty() {
            return;
        }
        let di = self.selected_drive.min(self.drives.len().saturating_sub(1));
        let pi = self.selected_speed_partition;
        if pairs.iter().any(|&(a, b)| a == di && b == pi) {
            return;
        }
        let preferred_disk = self.drives.get(di).map(|d| d.disk_number);
        if let Some(n) = preferred_disk {
            if let Some(&(ndi, npi)) = pairs.iter().find(|&&(didx, _)| {
                self.drives.get(didx).map(|d| d.disk_number) == Some(n)
            }) {
                self.selected_drive = ndi;
                self.selected_speed_partition = npi;
                return;
            }
        }
        let (ndi, npi) = pairs[0];
        self.selected_drive = ndi;
        self.selected_speed_partition = npi;
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

    fn speed_test_temp_path(drive_letter: &str) -> String {
        let clean = drive_letter.trim_end_matches('\\');
        let root = if clean.ends_with(':') {
            format!("{}\\", clean)
        } else {
            format!("{}:\\", clean.trim_end_matches(':'))
        };
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "{}Diskoria_SpeedTest_{}.tmp",
            root, id
        )
    }

    fn can_start_speed_test(&self) -> bool {
        if self.surface_test_running {
            return false;
        }
        if self.drives.is_empty() || self.drives_loading {
            return false;
        }
        let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
        let parts = &self.drives[sel].partitions;
        if parts.is_empty() {
            return false;
        }
        let pi = self.selected_speed_partition.min(parts.len().saturating_sub(1));
        !parts[pi].is_bitlocker_locked()
    }

    fn start_speed_test(&mut self, ctx: &egui::Context) {
        #[cfg(not(windows))]
        {
            self.speed_error_msg = Some("Speed test requires Windows.".to_string());
            return;
        }
        #[cfg(windows)]
        {
            if !self.can_start_speed_test() {
                return;
            }
            let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
            let d = &self.drives[sel];
            let pi = self
                .selected_speed_partition
                .min(d.partitions.len().saturating_sub(1));
            let disk_number = d.disk_number;
            let letter = d.partitions[pi].drive_letter.clone();
            let path = Self::speed_test_temp_path(&letter);
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
        #[cfg(windows)]
        {
            self.speed_focus = Some(2);
        }
        #[cfg(not(windows))]
        {
            self.speed_focus = Some(1);
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
            .any(|p| p.drive_letter.eq_ignore_ascii_case(&letter));
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

    fn update_modal_confirm_tab_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.show_stop_test_confirm {
            if self.modal_two_button_focus.is_none() {
                self.modal_two_button_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.modal_two_button_focus, 2);
            self.destructive_start_confirm_focus = None;
            self.destructive_stop_confirm_focus = None;
            #[cfg(windows)]
            {
                self.update_download_confirm_focus = None;
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
            #[cfg(windows)]
            {
                self.update_download_confirm_focus = None;
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
            #[cfg(windows)]
            {
                self.update_download_confirm_focus = None;
                self.update_alert_focus = None;
            }
            return;
        }
        self.destructive_stop_confirm_focus = None;

        #[cfg(windows)]
        {
            if self.show_update_download_confirm {
                if self.update_download_confirm_focus.is_none() {
                    self.update_download_confirm_focus = Some(0);
                }
                tab_cycle_slots(ctx, &mut self.update_download_confirm_focus, 2);
                self.update_alert_focus = None;
                return;
            }
            self.update_download_confirm_focus = None;

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
        let t = Theme::new(dark, self.accent_color);
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

        let (status_str, status_col, reason_lines): (&str, Color32, Option<&Vec<String>>) =
            if self.drives.is_empty() {
                ("—", t.txt_sec, None)
            } else if self.smart_health_inflight && self.smart_health.is_none() {
                ("Loading…", t.txt_sec, None)
            } else {
                match &self.smart_health {
                    Some(SmartHealth::Healthy) => ("Healthy", c_ok, None),
                    Some(SmartHealth::Failing { reasons }) => ("Failing", c_fail, Some(reasons)),
                    Some(SmartHealth::Disabled) => ("Unavailable", c_dis, None),
                    None => ("…", t.txt_sec, None),
                }
            };

        let mut body_h = 0.0_f32;
        if let Some(rs) = reason_lines {
            if !rs.is_empty() {
                body_h += 8.0;
                for s in rs {
                    let line = format!("• {}", s);
                    let galley = ui.ctx().fonts(|f| {
                        f.layout(line, FontId::proportional(13.0), t.txt_sec, inner_w)
                    });
                    body_h += galley.rect.height() + 2.0;
                }
            }
        }

        let err_h = self.smart_health_err.as_ref().map_or(0.0, |e| {
            6.0
                + ui.ctx().fonts(|f| {
                    f.layout(e.clone(), FontId::proportional(12.0), t.txt_sec, inner_w)
                        .rect
                        .height()
                })
        });

        const SMART_LABEL_DRIVE_LETTERS: &str = "Drive Letters: ";
        const SMART_LABEL_PARTITION_STYLE: &str = "Partition Style: ";

        let smart_drive_lines: Option<(String, String)> = if self.drives.is_empty() {
            None
        } else {
            let sel = self.active_page_selected_drive_idx();
            let d = &self.drives[sel];
            Some((
                d.drive_letters_display(),
                d.partition_style.as_str().to_string(),
            ))
        };
        let (drive_lines_h, gap_drive_to_status) =
            if let Some((ref letters, ref style)) = smart_drive_lines {
                let j1 = smart_health_kv_line_job(
                    SMART_LABEL_DRIVE_LETTERS,
                    letters,
                    inner_w,
                    t.txt_pri,
                );
                let j2 = smart_health_kv_line_job(
                    SMART_LABEL_PARTITION_STYLE,
                    style,
                    inner_w,
                    t.txt_pri,
                );
                let h1 = ui
                    .ctx()
                    .fonts(|f| f.layout_job(j1.clone()))
                    .rect
                    .height();
                let h2 = ui.ctx().fonts(|f| f.layout_job(j2.clone())).rect.height();
                (h1 + 6.0 + h2, 8.0_f32)
            } else {
                (0.0_f32, 0.0_f32)
            };

        let header_row_h = 22.0_f32;
        let card_h =
            pad + drive_lines_h + gap_drive_to_status + header_row_h + body_h + err_h + pad;

        let (_, alloc) = ui.allocate_space(Vec2::new(ui.available_width(), card_h));
        let card = Rect::from_min_size(
            Pos2::new(content_x + margin, alloc.top()),
            Vec2::new(section_w, card_h),
        );

        ui.painter().rect_filled(card, 8.0, t.bg_pri);
        ui.painter()
            .rect_stroke(card, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

        let left_x = card.left() + pad;
        let mut y = card.top() + pad;

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
                Pos2::new(left_x, y),
                g1,
                t.txt_pri,
            ));
            y += h1 + 6.0;
            let j2 = smart_health_kv_line_job(
                SMART_LABEL_PARTITION_STYLE,
                &style,
                inner_w,
                t.txt_pri,
            );
            let g2 = ui.ctx().fonts(|f| f.layout_job(j2));
            let h2 = g2.rect.height();
            ui.painter().add(egui::Shape::galley(
                Pos2::new(left_x, y),
                g2,
                t.txt_pri,
            ));
            y += h2 + gap_drive_to_status;
        }

        let label_text = "S.M.A.R.T. Status:";
        let label_font = FontId::new(14.0, FontFamily::Name("InterBold".into()));
        let row_cy = y + header_row_h * 0.5;
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

        y += header_row_h;
        if let Some(rs) = reason_lines {
            if !rs.is_empty() {
                y += 8.0;
            }
            for s in rs {
                let line = format!("• {}", s);
                let galley = ui.ctx().fonts(|f| {
                    f.layout(line, FontId::proportional(13.0), t.txt_sec, inner_w)
                });
                let gh = galley.rect.height();
                ui.painter().add(egui::Shape::galley(
                    Pos2::new(left_x, y),
                    galley,
                    t.txt_sec,
                ));
                y += gh + 2.0;
            }
        }

        if let Some(ref e) = self.smart_health_err {
            let galley = ui.ctx().fonts(|f| {
                f.layout(e.clone(), FontId::proportional(12.0), t.txt_sec, inner_w)
            });
            ui.painter().add(egui::Shape::galley(
                Pos2::new(left_x, y + 4.0),
                galley,
                t.txt_sec,
            ));
        }

        ui.advance_cursor_after_rect(alloc);
    }

    /// Tab order + slot clamping (runs before UI so the same frame’s paint matches focus).
    fn prepare_sector_page_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::tab_cycle_slots;

        if self.blocks_content_interaction() {
            return;
        }
        if self.active_nav != 0 {
            self.sector_focus = None;
            return;
        }

        let slots = if self.surface_test_running { 1 } else { 3 };
        if self.surface_test_running {
            if self.sector_focus.map_or(false, |s| s > 0) {
                self.sector_focus = Some(0);
            }
        } else if self.sector_focus.map_or(false, |s| s > 2) {
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
        if self.active_nav != 0 {
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
        if self.active_nav != 2 {
            self.speed_focus = None;
            return;
        }

        #[cfg(windows)]
        let last_idle_slot = 2usize;
        #[cfg(not(windows))]
        let last_idle_slot = 1usize;

        let slots = if self.speed_test_running {
            1
        } else {
            last_idle_slot + 1
        };

        if self.speed_test_running {
            if self.speed_focus.map_or(false, |s| s > 0) {
                self.speed_focus = Some(0);
            }
        } else if self
            .speed_focus
            .map_or(false, |s| s > last_idle_slot)
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
        if self.active_nav != 2 {
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
            #[cfg(windows)]
            bind_text_focus_slot(ctx, self.speed_focus, 2, Some(Self::speed_primary_id()));

            let (manual_id, is_combo_slot) = match self.speed_focus {
                Some(0) => (self.speed_refresh_id, false),
                Some(1) => (self.speed_volume_combo_id, true),
                #[cfg(windows)]
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
        if self.active_nav != 1 {
            self.destructive_focus = None;
            return;
        }

        if !self.destructive_unlocked {
            // Gate page: single slot for the unlock button.
            tab_cycle_slots(ctx, &mut self.destructive_focus, 1);
        } else if self.destructive_test_running {
            // Running: single slot for Stop.
            if self.destructive_focus.map_or(false, |s| s > 0) {
                self.destructive_focus = Some(0);
            }
            tab_cycle_slots(ctx, &mut self.destructive_focus, 1);
        } else {
            // Idle: 0=Refresh, 1=Combo, 2=Start.
            if self.destructive_focus.map_or(false, |s| s > 2) {
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
        if self.active_nav != 1 {
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
        self.drives_error = None;
        self.smart_health_disk = None;
        let (tx, rx) = mpsc::channel();
        #[cfg(windows)]
        {
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let result = drive_enumeration::enumerate_physical_disks();
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
        }
        #[cfg(not(windows))]
        {
            let _ = tx.send(Err(
                "Drive enumeration runs on Windows only.".to_string(),
            ));
            ctx.request_repaint();
        }
        self.drive_poll_rx = Some(rx);
    }

    fn poll_drive_enumeration(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.drive_poll_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(drives)) => {
                let had_no_drives = self.drives.is_empty();
                let nums: Vec<u32> = drives.iter().map(|d| d.disk_number).collect();
                log::info!(
                    target: "diskoria",
                    "poll_drive_enumeration: ok count={} disk_numbers={:?} selected_drive_idx={}",
                    drives.len(),
                    nums,
                    self.selected_drive
                );
                self.drives = drives;
                self.selected_drive = self
                    .selected_drive
                    .min(self.drives.len().saturating_sub(1));
                self.sync_speed_partition_after_drives_refresh();
                if had_no_drives {
                    self.pick_best_speed_partition_for_selected_drive();
                }
                self.check_surface_test_drive_after_enum(ctx);
                self.check_destructive_test_drive_after_enum(ctx);
                self.check_speed_test_after_enum(ctx);
                self.drives_error = None;
                self.drives_loading = false;
                self.drive_poll_rx = None;
                self.smart_health_disk = None;
                ctx.request_repaint();
            }
            Ok(Err(e)) => {
                log::warn!(target: "diskoria", "poll_drive_enumeration: error {e}");
                self.drives_error = Some(e);
                self.drives_loading = false;
                self.drive_poll_rx = None;
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                log::warn!(target: "diskoria", "poll_drive_enumeration: channel disconnected");
                self.drives_loading = false;
                self.drive_poll_rx = None;
            }
        }
    }

    fn poll_smart_health(&mut self, ctx: &egui::Context) {
        #[cfg(not(windows))]
        {
            if self.drives_loading || self.drives.is_empty() {
                return;
            }
            let disk = self.drives[self.active_page_selected_drive_idx()].disk_number;
            if self.smart_health_disk == Some(disk) {
                return;
            }
            self.smart_health = Some(SmartHealth::Disabled);
            self.smart_health_err = Some(
                "Storage health reporting is only available on Windows.".to_string(),
            );
            self.smart_health_disk = Some(disk);
            return;
        }

        #[cfg(windows)]
        {
            if let Some(rx) = self.smart_health_rx.take() {
                match rx.try_recv() {
                    Ok((disk, result)) => {
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

            let drive = &self.drives[self.active_page_selected_drive_idx()];
            let disk = drive.disk_number;

            if self.smart_health_inflight {
                return;
            }

            if self.smart_health_disk == Some(disk) {
                return;
            }

            self.smart_health_inflight = true;
            self.smart_health = None;
            self.smart_health_err = None;
            let pnp_id = drive.pnp_device_id.clone();
            let (tx, rx) = mpsc::channel();
            self.smart_health_rx = Some(rx);
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let r = crate::smart_health::query_smart_health(disk, &pnp_id);
                let _ = tx.send((disk, r));
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
        for c in &mut self.sector_cells {
            *c = SectorCell::Pending;
        }
        self.heat_min_ms = f64::MAX;
        self.heat_max_ms = f64::MIN;
        self.surface_progress_pct = 0.0;
        self.surface_avg_speed_mbps = 0.0;
        self.surface_total_sectors = 0;
        self.surface_good_sectors = 0;
        self.surface_bad_sectors = 0;
        self.surface_slow_sectors = 0;
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
        for c in &mut self.destructive_cells {
            *c = SectorCell::Pending;
        }
        self.destructive_heat_min_ms = f64::MAX;
        self.destructive_heat_max_ms = f64::MIN;
        self.destructive_progress_pct = 0.0;
        self.destructive_avg_speed_mbps = 0.0;
        self.destructive_total_sectors = 0;
        self.destructive_good_sectors = 0;
        self.destructive_bad_sectors = 0;
        self.destructive_slow_sectors = 0;
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
            .selected_destructive_drive
            .min(self.drives.len().saturating_sub(1));
        let (disk_number, device_id, drive_letters) = {
            let d = &self.drives[sel];
            let letters: Vec<String> = d.partitions.iter().map(|p| p.drive_letter.clone()).collect();
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
        self.destructive_total_sectors = p.total_sectors;
        self.destructive_good_sectors = p.good_sectors;
        self.destructive_bad_sectors = p.bad_sectors;
        self.destructive_slow_sectors = p.slow_sectors;

        let bi = p.block_index as usize;
        if bi < self.destructive_cells.len() {
            if !p.block_is_good {
                self.destructive_cells[bi] = SectorCell::Bad;
            } else if p.block_read_time_ms >= crate::destructive_test::SLOW_THRESHOLD_MS {
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
            "finalize_destructive_test_on_completed: progress_pct={:.2} bad={} slow={}",
            self.destructive_progress_pct,
            self.destructive_bad_sectors,
            self.destructive_slow_sectors
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
            self.selected_destructive_drive = idx;
        }
    }

    fn draw_destructive_start_confirm(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.accent_color);
        let sel = self
            .selected_destructive_drive
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
        let t = Theme::new(dark, self.accent_color);
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

    fn apply_surface_progress(&mut self, p: SurfaceTestProgress) {
        self.surface_progress_pct = p.progress_percent;
        self.surface_avg_speed_mbps = p.average_speed_mbps;
        self.surface_total_sectors = p.total_sectors;
        self.surface_good_sectors = p.good_sectors;
        self.surface_bad_sectors = p.bad_sectors;
        self.surface_slow_sectors = p.slow_sectors;

        let bi = p.block_index as usize;
        if bi < self.sector_cells.len() {
            if !p.block_is_good {
                self.sector_cells[bi] = SectorCell::Bad;
            } else if p.block_read_time_ms >= surface_test::SLOW_THRESHOLD_MS {
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
            "finalize_surface_test_on_completed: progress_pct={:.2} bad_sectors={} slow_sectors={}",
            self.surface_progress_pct,
            self.surface_bad_sectors,
            self.surface_slow_sectors
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
        if !batch.is_empty() {
            log::debug!(
                target: "diskoria",
                "poll_surface_test: batch len={}",
                batch.len()
            );
        }
        for msg in batch {
            match msg {
                SurfaceTestMsg::Progress(p) => {
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
        let base = self.show_stop_test_confirm
            || self.show_destructive_start_confirm
            || self.show_destructive_stop_confirm;
        #[cfg(windows)]
        {
            return base
                || self.show_update_download_confirm
                || self.show_update_alert
                || self.update_check_busy
                || self.update_download_busy;
        }
        #[cfg(not(windows))]
        {
            base
        }
    }

    fn save_app_settings(&self) {
        app_settings::save_settings(&app_settings::Settings {
            theme: self.theme_pref,
            accent_source: self.accent_source,
            accent_palette_idx: self.accent_palette_idx,
            accent_use_custom: self.accent_use_custom,
            accent_custom_hex: self.accent_custom_hex.clone(),
        });
    }

    fn settings_tab_slot_count(&self) -> usize {
        3 + 2
            + if self.accent_source == AccentSourcePref::Palette {
                8
            } else {
                0
            }
            + 1
    }

    fn settings_hex_slot(&self) -> usize {
        if self.accent_source == AccentSourcePref::Palette {
            13
        } else {
            5
        }
    }

    fn update_accent_color(&mut self, ctx: &egui::Context) {
        if self.accent_source != AccentSourcePref::Windows {
            return;
        }
        #[cfg(windows)]
        {
            let now = std::time::Instant::now();
            let due = self.accent_last_poll.map_or(true, |t| {
                now.duration_since(t) >= std::time::Duration::from_millis(250)
            });
            if !due {
                return;
            }
            self.accent_last_poll = Some(now);
            if let Some(c) = windows_accent_color() {
                if c != self.accent_color {
                    self.accent_color = c;
                    ctx.request_repaint();
                }
            }
        }
    }

    fn update_settings_tab_focus(&mut self, ctx: &egui::Context) {
        use crate::focus::manage_page_focus;

        if self.active_nav != 4 {
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
        let t = Theme::new(dark, self.accent_color);

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
        #[cfg(windows)]
        let update_nav_block = self.show_update_download_confirm
            || self.show_update_alert
            || self.update_check_busy
            || self.update_download_busy;
        #[cfg(not(windows))]
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
                        Stroke::new(1.0, icon_col),
                    );
                }
            }
        }

        if item_r.clicked() && can_select {
            self.active_nav = index;
        }
    }

    fn draw_central(&mut self, ctx: &egui::Context, dark: bool) {
        let t = Theme::new(dark, self.accent_color);

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
                            0 => self.draw_sector_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            1 => self.draw_destructive_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            2 => self.draw_speed_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            3 => self.draw_health_status_page(ui, ctx, &t, dark, margin, content_x, content_w),
                            4 => self.draw_about_page(ui, ctx, &t, margin, content_x, content_w),
                            5 => self.draw_settings_theme(ui, ctx, &t, margin, content_x, content_w),
                            _ => {}
                        }
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

    /// Destructive Test page — gate blocker (first visit) then write+verify UI.
    fn draw_destructive_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        dark: bool,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        // ----------------------------------------------------------------
        // Title (always shown)
        // ----------------------------------------------------------------
        ui.painter().text(
            Pos2::new(content_x + margin, ui.cursor().min.y + 14.0),
            Align2::LEFT_CENTER,
            "Destructive Test",
            FontId::new(28.0, FontFamily::Proportional),
            t.txt_pri,
        );
        ui.add_space(32.0);

        // ----------------------------------------------------------------
        // Gate page — shown until the user explicitly acknowledges the risk.
        // ----------------------------------------------------------------
        if !self.destructive_unlocked {
            let section_w = content_w - margin * 2.0;
            let left = content_x + margin;

            let warning_lines = [
                "This test will write data to every sector on the selected drive",
                "and immediately read it back to verify the write.",
                "",
                "ALL EXISTING DATA ON THE DRIVE WILL BE PERMANENTLY DESTROYED.",
                "This operation cannot be undone.",
                "",
                "Only proceed if you intend to wipe this device completely.",
                "Do not select a drive that contains any data you wish to keep.",
            ];

            let mut y = ui.cursor().min.y;
            for line in &warning_lines {
                if line.is_empty() {
                    y += 8.0;
                    continue;
                }
                let is_key = line.starts_with("ALL") || line.starts_with("This operation");
                let col = if is_key {
                    Color32::from_rgb(231, 76, 60)
                } else {
                    t.txt_sec
                };
                let font = if is_key {
                    FontId::new(14.0, FontFamily::Name("InterBold".into()))
                } else {
                    FontId::proportional(14.0)
                };
                ui.painter().text(
                    Pos2::new(left, y + 10.0),
                    Align2::LEFT_CENTER,
                    *line,
                    font,
                    col,
                );
                y += 22.0;
            }
            // Allocate vertical space for the warning block.
            let text_h = y - ui.cursor().min.y;
            ui.add_space(text_h + 16.0);

            // Large unlock button (same pattern as Start button elsewhere).
            const PRIMARY_BTN_H: f32 = 48.0;
            let (_, btn_alloc) = ui.allocate_space(Vec2::new(ui.available_width(), PRIMARY_BTN_H));
            let screen = ctx.screen_rect();
            let btn_x = left.max(screen.left() + 12.0);
            let btn_w = section_w.min((screen.right() - 12.0 - btn_x).max(0.0));
            let btn_rect = Rect::from_min_size(
                Pos2::new(btn_x, btn_alloc.top()),
                Vec2::new(btn_w, PRIMARY_BTN_H),
            );
            let btn_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));
            let unlock_focused = self.destructive_focus == Some(0);
            let btn_r = ui.interact(btn_rect, Self::destructive_unlock_id(), INTERACT_MANUAL_FOCUS);
            let bg = if btn_r.hovered() || unlock_focused {
                Color32::from_rgb(
                    t.accent.r().saturating_add(15),
                    t.accent.g().saturating_add(15),
                    t.accent.b().saturating_add(15),
                )
            } else {
                t.accent
            };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            if unlock_focused {
                ui.painter().rect_stroke(
                    btn_rect.expand(3.0),
                    4.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
            ui.painter().text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                "\u{f337}  I Understand, Continue",
                btn_font,
                t.txt_on_accent,
            );
            let kb = crate::focus::keyboard_activate(ui, unlock_focused);
            if btn_r.clicked() || kb {
                self.destructive_unlocked = true;
                self.destructive_focus = None;
            }
            ui.advance_cursor_after_rect(btn_alloc);
            return;
        }

        // ----------------------------------------------------------------
        // Unlocked — full write+verify UI (mirrors draw_sector_page).
        // ----------------------------------------------------------------
        let subtitle = "Read + write test — destroys all data on disk";
        let section_w = content_w - margin * 2.0;
        let row_h = 34.0_f32;
        let chip_h = 26.0_f32;
        let gap_chips = 8.0_f32;
        let gap_combo_chips = 12.0_f32;

        ui.horizontal(|ui| {
            let pad = (content_x + margin) - ui.min_rect().left();
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.label(egui::RichText::new(subtitle).size(14.0).color(t.txt_sec));
            ui.add_space(16.0);
            ui.push_id("diskoria_destructive_refresh", |ui| {
                let refresh = ui.add_enabled(
                    !self.any_test_running(),
                    egui::Button::new(egui::RichText::new("⟳ Refresh").color(t.txt_pri)),
                );
                if refresh.clicked() {
                    self.destructive_focus = Some(0);
                    self.spawn_drive_enumeration(ctx);
                }
                self.destructive_refresh_id = Some(refresh.id);
                if !self.any_test_running() && self.destructive_focus == Some(0) {
                    ui.painter().rect_stroke(
                        refresh.rect.expand(2.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
            });
            if self.drives_loading {
                ui.add_space(10.0);
                ui.spinner();
                ui.label(egui::RichText::new("Loading drives…").color(t.txt_sec));
            }
        });
        ui.add_space(20.0);

        if let Some(ref err) = self.drives_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(format!("Could not enumerate drives: {err}"))
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(12.0);
        }

        if !self.drives_loading && self.drives.is_empty() && self.drives_error.is_none() {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new("No physical disks found.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
            ui.add_space(16.0);
        }

        if let Some(ref msg) = self.destructive_drive_removed_msg {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(msg)
                        .size(13.0)
                        .color(Color32::from_rgb(241, 196, 15)),
                );
            });
            ui.add_space(8.0);
        }

        if let Some(ref err) = self.destructive_last_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(format!("Destructive test error: {err}"))
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(8.0);
        }

        if self.drives_loading && self.drives.is_empty() {
            return;
        }

        // Drive combo
        let combo_id = Id::new("diskoria_destructive_drive_combo");
        let options: Vec<String> = self.drives.iter().map(|d| d.summary.clone()).collect();
        let sel = self
            .selected_destructive_drive
            .min(options.len().saturating_sub(1));

        let y_row = ui.cursor().min.y;

        ui.add_enabled_ui(!self.any_test_running(), |ui| {
            let combo_w = if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let chips_w = mw + gap_chips + bw;
                (section_w - gap_combo_chips - chips_w).max(120.0)
            } else {
                section_w
            };

            let combo_rect = Rect::from_min_size(
                Pos2::new(content_x + margin, y_row),
                Vec2::new(combo_w, row_h),
            );
            ui.painter().rect_filled(combo_rect, 0.0, t.bg_pri);
            ui.painter().line_segment(
                [combo_rect.left_bottom(), combo_rect.right_bottom()],
                Stroke::new(1.5, t.accent),
            );

            let combo_inner = combo_rect.shrink2(Vec2::new(8.0, 4.0));
            let mut combo_style = (**ui.style()).clone();
            combo_style.spacing.button_padding = Vec2::new(10.0, 6.0);
            for w in [
                &mut combo_style.visuals.widgets.inactive,
                &mut combo_style.visuals.widgets.hovered,
                &mut combo_style.visuals.widgets.active,
                &mut combo_style.visuals.widgets.open,
            ] {
                w.weak_bg_fill = Color32::TRANSPARENT;
                w.bg_stroke = Stroke::NONE;
                w.fg_stroke = Stroke::new(1.0, t.txt_pri);
            }

            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(combo_inner)
                    .style(std::sync::Arc::new(combo_style)),
                |ui| {
                    let ir = egui::ComboBox::from_id_salt(combo_id)
                        .selected_text(options.get(sel).map(|s| s.as_str()).unwrap_or("—"))
                        .width(combo_inner.width())
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.style_mut().visuals.override_text_color = Some(t.txt_pri);
                            for (idx, label) in options.iter().enumerate() {
                                let r = ui.selectable_value(
                                    &mut self.selected_destructive_drive,
                                    idx,
                                    label,
                                );
                                close_popup_if_selectable_clicked(ui.ctx(), &r);
                            }
                        });
                    self.destructive_combo_id = Some(ir.response.id);
                    if ir.response.clicked() {
                        self.destructive_focus = Some(1);
                    }
                    ir.response
                },
            );

            if !self.any_test_running() && self.destructive_focus == Some(1) {
                ui.painter().rect_stroke(
                    combo_rect.expand(2.0),
                    4.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }

            if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let chip_y = y_row + (row_h - chip_h) * 0.5;
                let mut x = combo_rect.right() + gap_combo_chips;

                let (m_bg, m_fg) = chip_colors_media(d.media);
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let mr = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(mw, chip_h));
                chip_pill(ui.painter(), mr, d.media.label(), m_bg, m_fg);
                x += mw + gap_chips;

                let (b_bg, b_fg) = chip_colors_bus(d.bus);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let br = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(bw, chip_h));
                chip_pill(ui.painter(), br, d.bus.label(), b_bg, b_fg);
            }

            let row_rect = Rect::from_min_max(
                Pos2::new(content_x + margin, y_row),
                Pos2::new(content_x + margin + section_w, y_row + row_h),
            );
            ui.advance_cursor_after_rect(row_rect);
        });

        self.draw_smart_health_card(ui, t, dark, content_x, margin, section_w);

        // Primary Start / Stop button
        #[cfg(windows)]
        {
            ui.add_space(12.0);
            const PRIMARY_BTN_H: f32 = 48.0;
            let (_, btn_alloc) = ui.allocate_space(Vec2::new(ui.available_width(), PRIMARY_BTN_H));
            let screen = ctx.screen_rect();
            let btn_x = (content_x + margin).max(screen.left() + 12.0);
            let btn_w = section_w.min((screen.right() - 12.0 - btn_x).max(0.0));
            let btn_rect = Rect::from_min_size(
                Pos2::new(btn_x, btn_alloc.top()),
                Vec2::new(btn_w, PRIMARY_BTN_H),
            );
            let btn_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));

            if self.destructive_test_running {
                let stop_focused = self.destructive_focus == Some(0);
                let btn_r =
                    ui.interact(btn_rect, Self::destructive_primary_id(), Sense::click());
                let bg = if btn_r.hovered() || stop_focused {
                    Color32::from_rgb(
                        CLOSE_HOVER_BG.r().saturating_add(12),
                        CLOSE_HOVER_BG.g().saturating_add(8),
                        CLOSE_HOVER_BG.b().saturating_add(8),
                    )
                } else {
                    CLOSE_HOVER_BG
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if stop_focused {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Stop Test",
                    btn_font,
                    Color32::WHITE,
                );
                let kb = btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if btn_r.clicked() || kb {
                    self.show_destructive_stop_confirm = true;
                }
            } else {
                let can_start = !self.drives.is_empty()
                    && !self.drives_loading
                    && !self.surface_test_running
                    && !self.speed_test_running;
                let start_focused = can_start && self.destructive_focus == Some(2);
                let btn_sense = if can_start {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let btn_r = ui.interact(btn_rect, Self::destructive_primary_id(), btn_sense);
                let bg = if !can_start {
                    Color32::from_rgba_premultiplied(
                        t.accent.r() / 2,
                        t.accent.g() / 2,
                        t.accent.b() / 2,
                        180,
                    )
                } else if btn_r.hovered() || start_focused {
                    Color32::from_rgb(
                        t.accent.r().saturating_add(15),
                        t.accent.g().saturating_add(15),
                        t.accent.b().saturating_add(15),
                    )
                } else {
                    t.accent
                };
                let fg = if can_start {
                    t.txt_on_accent
                } else {
                    Color32::from_rgba_premultiplied(255, 255, 255, 120)
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if start_focused && can_start {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Start Test",
                    btn_font,
                    fg,
                );
                let kb = can_start
                    && btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if (btn_r.clicked() || kb) && can_start {
                    self.show_destructive_start_confirm = true;
                }
            }
            ui.advance_cursor_after_rect(btn_alloc);
        }

        ui.add_space(16.0);

        if cfg!(not(windows)) {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new("Destructive testing requires Windows.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
            return;
        }

        #[cfg(windows)]
        self.draw_destructive_test_panel(ui, t, content_x, margin, section_w);
    }

    /// Sector heatmap + progress card for the destructive test.
    #[cfg(windows)]
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
            let sel = self.selected_destructive_drive.min(self.drives.len().saturating_sub(1));
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
                .stroke(Stroke::new(1.5, t.border))
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
                                "Total sectors",
                                &format!("{}", self.destructive_total_sectors),
                                t.txt_pri,
                            );
                            stat(
                                ui,
                                "Good sectors",
                                &format!("{}", self.destructive_good_sectors),
                                Color32::from_rgb(46, 204, 113),
                            );
                            stat(
                                ui,
                                "Bad sectors",
                                &format!("{}", self.destructive_bad_sectors),
                                Color32::from_rgb(231, 76, 60),
                            );
                            stat(
                                ui,
                                "Slow sectors",
                                &format!("{}", self.destructive_slow_sectors),
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
    fn draw_sector_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        dark: bool,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        ui.painter().text(
            Pos2::new(content_x + margin, ui.cursor().min.y + 14.0),
            Align2::LEFT_CENTER,
            "Sector Test",
            FontId::new(28.0, FontFamily::Proportional),
            t.txt_pri,
        );
        ui.add_space(32.0);

        let subtitle = "Read-only scan — checks every sector for errors";
        let section_w = content_w - margin * 2.0;
        let row_h = 34.0_f32;
        let chip_h = 26.0_f32;
        let gap_chips = 8.0_f32;
        let gap_combo_chips = 12.0_f32;

        ui.horizontal(|ui| {
            let pad = (content_x + margin) - ui.min_rect().left();
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.label(egui::RichText::new(subtitle).size(14.0).color(t.txt_sec));
            ui.add_space(16.0);
            ui.push_id("diskoria_sector_refresh", |ui| {
                let refresh = ui.add_enabled(
                    !self.any_test_running(),
                    egui::Button::new(egui::RichText::new("\u{27f3} Refresh").color(t.txt_pri)),
                );
                if refresh.clicked() {
                    self.sector_focus = Some(0);
                    self.spawn_drive_enumeration(ctx);
                }
                self.sector_refresh_id = Some(refresh.id);
                if !self.any_test_running() && self.sector_focus == Some(0) {
                    ui.painter().rect_stroke(
                        refresh.rect.expand(2.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
            });
            if self.drives_loading {
                ui.add_space(10.0);
                ui.spinner();
                ui.label(egui::RichText::new("Loading drives\u{2026}").color(t.txt_sec));
            }
        });
        ui.add_space(20.0);

        if let Some(ref err) = self.drives_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new(format!("Could not enumerate drives: {err}"))
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(12.0);
        }

        if !self.drives_loading && self.drives.is_empty() && self.drives_error.is_none() {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new("No physical disks found.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
            ui.add_space(16.0);
        }

        if let Some(ref msg) = self.surface_drive_removed_msg {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new(msg)
                        .size(13.0)
                        .color(Color32::from_rgb(241, 196, 15)),
                );
            });
            ui.add_space(8.0);
        }

        if let Some(ref err) = self.surface_last_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new(format!("Sector test error: {err}"))
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(8.0);
        }

        if self.drives_loading && self.drives.is_empty() {
            return;
        }

        let combo_id = Id::new("diskoria_sector_drive_combo");
        let options: Vec<String> = self.drives.iter().map(|d| d.summary.clone()).collect();
        let sel = self.selected_drive.min(options.len().saturating_sub(1));

        let y_row = ui.cursor().min.y;

        ui.add_enabled_ui(!self.any_test_running(), |ui| {
            let combo_w = if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let chips_w = mw + gap_chips + bw;
                (section_w - gap_combo_chips - chips_w).max(120.0)
            } else {
                section_w
            };

            let combo_rect = Rect::from_min_size(
                Pos2::new(content_x + margin, y_row),
                Vec2::new(combo_w, row_h),
            );
            ui.painter().rect_filled(combo_rect, 0.0, t.bg_pri);
            ui.painter().line_segment(
                [combo_rect.left_bottom(), combo_rect.right_bottom()],
                Stroke::new(1.5, t.accent),
            );

            let combo_inner = combo_rect.shrink2(Vec2::new(8.0, 4.0));
            let mut combo_style = (**ui.style()).clone();
            combo_style.spacing.button_padding = Vec2::new(10.0, 6.0);
            for w in [
                &mut combo_style.visuals.widgets.inactive,
                &mut combo_style.visuals.widgets.hovered,
                &mut combo_style.visuals.widgets.active,
                &mut combo_style.visuals.widgets.open,
            ] {
                w.weak_bg_fill = Color32::TRANSPARENT;
                w.bg_stroke = Stroke::NONE;
                w.fg_stroke = Stroke::new(1.0, t.txt_pri);
            }

            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(combo_inner)
                    .style(std::sync::Arc::new(combo_style)),
                |ui| {
                    let ir = egui::ComboBox::from_id_salt(combo_id)
                        .selected_text(options.get(sel).map(|s| s.as_str()).unwrap_or("\u{2014}"))
                        .width(combo_inner.width())
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.style_mut().visuals.override_text_color = Some(t.txt_pri);
                            for (idx, label) in options.iter().enumerate() {
                                let r = ui.selectable_value(
                                    &mut self.selected_drive,
                                    idx,
                                    label,
                                );
                                close_popup_if_selectable_clicked(ui.ctx(), &r);
                            }
                        });
                    self.sector_combo_id = Some(ir.response.id);
                    if ir.response.clicked() {
                        self.sector_focus = Some(1);
                    }
                    ir.response
                },
            );

            if !self.any_test_running() && self.sector_focus == Some(1) {
                ui.painter().rect_stroke(
                    combo_rect.expand(2.0),
                    4.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }

            if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let chip_y = y_row + (row_h - chip_h) * 0.5;
                let mut x = combo_rect.right() + gap_combo_chips;

                let (m_bg, m_fg) = chip_colors_media(d.media);
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let mr = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(mw, chip_h));
                chip_pill(ui.painter(), mr, d.media.label(), m_bg, m_fg);
                x += mw + gap_chips;

                let (b_bg, b_fg) = chip_colors_bus(d.bus);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let br = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(bw, chip_h));
                chip_pill(ui.painter(), br, d.bus.label(), b_bg, b_fg);
            }

            let row_rect = Rect::from_min_max(
                Pos2::new(content_x + margin, y_row),
                Pos2::new(content_x + margin + section_w, y_row + row_h),
            );
            ui.advance_cursor_after_rect(row_rect);
        });

        self.draw_smart_health_card(ui, t, dark, content_x, margin, section_w);

        #[cfg(windows)]
        {
            ui.add_space(12.0);
            const PRIMARY_BTN_H: f32 = 48.0;
            let (_, btn_alloc) = ui.allocate_space(Vec2::new(ui.available_width(), PRIMARY_BTN_H));
            let screen = ctx.screen_rect();
            let btn_x = (content_x + margin).max(screen.left() + 12.0);
            let btn_w = section_w.min((screen.right() - 12.0 - btn_x).max(0.0));
            let btn_rect = Rect::from_min_size(
                Pos2::new(btn_x, btn_alloc.top()),
                Vec2::new(btn_w, PRIMARY_BTN_H),
            );
            let btn_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));

            if self.surface_test_running {
                let stop_focused = self.sector_focus == Some(0);
                let btn_r = ui.interact(btn_rect, Id::new("diskoria_sector_primary"), Sense::click());
                let bg = if btn_r.hovered() || stop_focused {
                    Color32::from_rgb(
                        CLOSE_HOVER_BG.r().saturating_add(12),
                        CLOSE_HOVER_BG.g().saturating_add(8),
                        CLOSE_HOVER_BG.b().saturating_add(8),
                    )
                } else {
                    CLOSE_HOVER_BG
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if stop_focused {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Stop Test",
                    btn_font,
                    Color32::WHITE,
                );
                let kb = btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if btn_r.clicked() || kb {
                    self.show_stop_test_confirm = true;
                }
            } else {
                let can_start = !self.drives.is_empty()
                    && !self.drives_loading
                    && !self.destructive_test_running
                    && !self.speed_test_running;
                let start_focused = can_start && self.sector_focus == Some(2);
                let btn_sense = if can_start { Sense::click() } else { Sense::hover() };
                let btn_r = ui.interact(btn_rect, Id::new("diskoria_sector_primary"), btn_sense);
                let bg = if !can_start {
                    Color32::from_rgba_premultiplied(
                        t.accent.r() / 2,
                        t.accent.g() / 2,
                        t.accent.b() / 2,
                        180,
                    )
                } else if btn_r.hovered() || start_focused {
                    Color32::from_rgb(
                        t.accent.r().saturating_add(15),
                        t.accent.g().saturating_add(15),
                        t.accent.b().saturating_add(15),
                    )
                } else {
                    t.accent
                };
                let fg = if can_start {
                    t.txt_on_accent
                } else {
                    Color32::from_rgba_premultiplied(255, 255, 255, 120)
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if start_focused && can_start {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Start Test",
                    btn_font,
                    fg,
                );
                let kb = can_start
                    && btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if (btn_r.clicked() || kb) && can_start {
                    self.start_surface_test(ctx);
                }
            }
            ui.advance_cursor_after_rect(btn_alloc);
        }

        ui.add_space(16.0);

        if cfg!(not(windows)) {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new("Sector testing requires Windows.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
            return;
        }

        #[cfg(windows)]
        self.draw_sector_test_panel(ui, t, content_x, margin, section_w);
    }

    /// Shared tabbed card: "Sector Map" tab (heat grid) + "Performance Chart" tab (speed vs position).
    /// Used by both `draw_sector_test_panel` and `draw_destructive_test_panel`.
    #[cfg(windows)]
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
        let grid_rows = (TOTAL_UI_BLOCKS + cols - 1) / cols;
        let grid_h = grid_rows as f32 * (cell + gap) - gap;
        let content_h = pad + grid_h + SECTOR_LEGEND_GAP + SECTOR_LEGEND_ROW_H + pad;
        let total_h = TAB_H + SEP_H + content_h;

        // Capture card_top before any allocations so the cursor is still here
        // when we call allocate_new_ui for the chart tab.  Allocating total_h
        // upfront first and then calling allocate_new_ui causes egui to try to
        // advance the cursor *backward* (into already-consumed space) which
        // corrupts layout state and panics on the next frame.
        let card_top = ui.cursor().min.y;
        let full_card_rect = Rect::from_min_size(Pos2::new(left, card_top), Vec2::new(section_w, total_h));

        ui.painter().rect_filled(full_card_rect, 8.0, t.bg_pri);
        ui.painter().rect_stroke(full_card_rect, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

        let tab_w = section_w / 2.0;
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
            Stroke::new(1.0, t.border),
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

        let sep_y = card_top + TAB_H;
        ui.painter().line_segment(
            [Pos2::new(left + 1.5, sep_y), Pos2::new(left + section_w - 1.5, sep_y)],
            Stroke::new(SEP_H, t.border),
        );

        let active_tab = if is_surface { self.surface_chart_tab } else { self.destructive_chart_tab };
        let content_top = sep_y + SEP_H;

        if active_tab == 0 {
            // Advance the cursor past the full card for the sector-map tab.
            // For the chart tab, allocate_new_ui on chart_rect (which sits below
            // the current cursor) handles cursor advancement naturally.
            ui.advance_cursor_after_rect(full_card_rect);

            let grid_top = content_top + pad;
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

            let legend_top = content_top + pad + grid_h + SECTOR_LEGEND_GAP;
            let legend_cy = legend_top + SECTOR_LEGEND_ROW_H * 0.5;
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
                                    .radius(1.0)
                                    .shape(MarkerShape::Circle),
                            );
                        }
                        if !chart_points_clone.is_empty() {
                            plot_ui.line(
                                Line::new(PlotPoints::from(chart_points_clone.clone()))
                                    .color(accent)
                                    .width(4.0),
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
            let btn_layer = LayerId::new(Order::Foreground, Id::new(("dl_btn_layer", tab_id_base)));
            let btn_painter = ui.ctx().layer_painter(btn_layer);
            let btn_bg = if btn_resp.hovered() { t.accent } else { t.bg_sec };
            let btn_fg = if btn_resp.hovered() { Color32::WHITE } else { t.txt_sec };
            btn_painter.rect_filled(btn_rect, 6.0, btn_bg);
            btn_painter.rect_stroke(btn_rect, 6.0, Stroke::new(1.0, t.border), StrokeKind::Middle);
            btn_painter.text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                "\u{f30a}",
                FontId::new(16.0, FontFamily::Proportional),
                btn_fg,
            );

            if btn_resp.clicked() {
                let filename = format!("{}.png", drive_filename_stem);
                let test_type = if is_surface { "Read-only sector test" } else { "Read and write sector test" };
                let now = chrono::Local::now();
                let test_label = format!("{} - {}", now.format("%m/%d/%Y"), test_type);
                std::thread::spawn(move || {
                    let handle = pollster::block_on(
                        rfd::AsyncFileDialog::new()
                            .set_title("Save Performance Chart")
                            .set_file_name(&filename)
                            .add_filter("PNG Image", &["png"])
                            .save_file(),
                    );
                    if let Some(h) = handle {
                        if let Err(e) = export_performance_chart_png(h.path(), &drive_label, &test_label, &raw_points_clone, &chart_points_clone, max_speed, total_gb) {
                            log::warn!("Performance chart export failed: {}", e);
                        }
                    }
                });
            }
        }
    }

    /// Sector map + progress/stats/time cards.
    #[cfg(windows)]
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
                .stroke(Stroke::new(1.5, t.border))
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
                            stat(ui, "Total sectors", &format!("{}", self.surface_total_sectors), t.txt_pri);
                            stat(ui, "Good sectors", &format!("{}", self.surface_good_sectors), Color32::from_rgb(46, 204, 113));
                            stat(ui, "Bad sectors", &format!("{}", self.surface_bad_sectors), Color32::from_rgb(231, 76, 60));
                            stat(ui, "Slow sectors", &format!("{}", self.surface_slow_sectors), Color32::from_rgb(255, 193, 7));
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
        let pad = 16.0_f32;
        let seg_h = 34.0_f32;
        let theme_h = pad + 22.0 + 12.0 + seg_h + pad;

        let swatch_gap = 8.0_f32;
        let row_available_w = section_w - pad * 2.0;
        let swatch_size = ((row_available_w - swatch_gap * 7.0) / 8.0).clamp(16.0, 26.0);
        let accent_grid_h = swatch_size;
        let accent_h = pad + 22.0 + 12.0 + seg_h + pad + accent_grid_h + pad + 10.0 + 34.0 + pad;

        let section_h = theme_h + 12.0 + accent_h;

        let (_, section_rect) = ui.allocate_space(Vec2::new(ui.available_width(), section_h));
        let card = Rect::from_min_size(
            Pos2::new(content_x + margin, section_rect.top()),
            Vec2::new(section_w, section_h),
        );

        ui.painter().rect_filled(card, 8.0, t.bg_pri);
        ui.painter()
            .rect_stroke(card, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

        ui.painter().text(
            Pos2::new(card.left() + pad, card.top() + pad + 11.0),
            Align2::LEFT_CENTER,
            "Theme",
            FontId::new(14.0, FontFamily::Name("InterBold".into())),
            t.txt_pri,
        );

        let seg_top = card.top() + pad + 22.0 + 12.0;
        let seg_rect = Rect::from_min_size(
            Pos2::new(card.left() + pad, seg_top),
            Vec2::new(section_w - pad * 2.0, seg_h),
        );
        ui.painter().rect_filled(seg_rect, 6.0, t.bg_sec);
        ui.painter().rect_stroke(
            seg_rect,
            6.0,
            Stroke::new(1.5, t.border),
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
            let selected = self.theme_pref == *pref;
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
                    Stroke::new(2.0, t.accent),
                    StrokeKind::Outside,
                );
            }
            let txt_col = if selected { Color32::WHITE } else { t.txt_pri };
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
                self.theme_pref = *pref;
                self.settings_focus = Some(i);
                self.save_app_settings();
            }
            if page_keys && keyboard_activate(ui, focused) {
                self.theme_pref = *pref;
                self.save_app_settings();
            }
        }

        let accent_card_top = card.top() + theme_h + 12.0;
        let accent_card = Rect::from_min_size(
            Pos2::new(card.left(), accent_card_top),
            Vec2::new(section_w, accent_h),
        );

        ui.painter().text(
            Pos2::new(accent_card.left() + pad, accent_card.top() + pad + 11.0),
            Align2::LEFT_CENTER,
            "Accent",
            FontId::new(14.0, FontFamily::Name("InterBold".into())),
            t.txt_pri,
        );

        let accent_seg_top = accent_card.top() + pad + 22.0 + 12.0;
        let accent_seg_rect = Rect::from_min_size(
            Pos2::new(accent_card.left() + pad, accent_seg_top),
            Vec2::new(section_w - pad * 2.0, seg_h),
        );
        ui.painter().rect_filled(accent_seg_rect, 6.0, t.bg_sec);
        ui.painter().rect_stroke(
            accent_seg_rect,
            6.0,
            Stroke::new(1.5, t.border),
            StrokeKind::Middle,
        );

        let accent_options = [
            ("Windows accent", AccentSourcePref::Windows),
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
            let selected = self.accent_source == *pref;
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
                    Stroke::new(2.0, t.accent),
                    StrokeKind::Outside,
                );
            }

            let txt_col = if selected { Color32::WHITE } else { t.txt_pri };
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
                let old = self.accent_source;
                self.accent_source = *pref;
                if old != self.accent_source {
                    self.settings_focus = None;
                } else {
                    self.settings_focus = Some(slot);
                }
                match self.accent_source {
                    AccentSourcePref::Windows => {
                        #[cfg(windows)]
                        if let Some(c) = windows_accent_color() {
                            self.accent_color = c;
                        }
                    }
                    AccentSourcePref::Palette => {
                        self.accent_color = accent_from_palette(
                            self.accent_palette_idx,
                            self.accent_use_custom,
                            &self.accent_custom_hex,
                        );
                    }
                }
                #[cfg(windows)]
                {
                    self.accent_last_poll = None;
                }
                self.save_app_settings();
            }
            if page_keys && keyboard_activate(ui, focused) {
                let old = self.accent_source;
                self.accent_source = *pref;
                if old != self.accent_source {
                    self.settings_focus = None;
                }
                match self.accent_source {
                    AccentSourcePref::Windows => {
                        #[cfg(windows)]
                        if let Some(c) = windows_accent_color() {
                            self.accent_color = c;
                        }
                    }
                    AccentSourcePref::Palette => {
                        self.accent_color = accent_from_palette(
                            self.accent_palette_idx,
                            self.accent_use_custom,
                            &self.accent_custom_hex,
                        );
                    }
                }
                #[cfg(windows)]
                {
                    self.accent_last_poll = None;
                }
                self.save_app_settings();
            }
        }

        let accent_grid_top = accent_seg_rect.bottom() + pad;
        let start_x = accent_card.left() + pad;
        let start_y = accent_grid_top;
        if self.accent_source == AccentSourcePref::Palette {
            for (idx, col) in ACCENT_PALETTE.iter().enumerate() {
                let x = start_x + idx as f32 * (swatch_size + swatch_gap);
                let y = start_y;
                let sw_rect =
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(swatch_size, swatch_size));

                let selected = !self.accent_use_custom
                    && self.accent_palette_idx == idx
                    && self.accent_source == AccentSourcePref::Palette;

                let stroke_col = if selected { t.txt_pri } else { t.border };
                let sw_slot = 5 + idx;
                let sw_focused = self.settings_focus == Some(sw_slot);

                ui.painter().rect_filled(sw_rect, 4.0, *col);
                ui.painter().rect_stroke(
                    sw_rect,
                    4.0,
                    Stroke::new(if selected { 2.0 } else { 1.5 }, stroke_col),
                    StrokeKind::Middle,
                );
                if sw_focused {
                    ui.painter().rect_stroke(
                        sw_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, t.accent),
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
                    self.accent_source = AccentSourcePref::Palette;
                    self.accent_palette_idx = idx;
                    self.accent_use_custom = false;
                    self.accent_color = *col;
                    self.save_app_settings();
                }
                if page_keys && keyboard_activate(ui, sw_focused) {
                    self.accent_source = AccentSourcePref::Palette;
                    self.accent_palette_idx = idx;
                    self.accent_use_custom = false;
                    self.accent_color = *col;
                    self.save_app_settings();
                }

                if sw_resp.hovered() {
                    ui.painter().rect_stroke(
                        sw_rect.expand(2.0),
                        4.0,
                        Stroke::new(1.0, t.accent),
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
            ui.painter().text(
                Pos2::new(accent_card.left() + pad, accent_grid_top + 10.0),
                Align2::LEFT_CENTER,
                "Switch to Palette to choose colors.",
                FontId::proportional(13.0),
                t.txt_sec,
            );
        }

        let custom_label_y = accent_grid_top + accent_grid_h + pad;
        ui.painter().text(
            Pos2::new(accent_card.left() + pad, custom_label_y),
            Align2::LEFT_CENTER,
            "Custom hex (#RRGGBB)",
            FontId::proportional(13.0),
            t.txt_pri,
        );

        let input_rect_y = custom_label_y + 10.0;
        let input_h = 34.0_f32;
        let input_rect = Rect::from_min_size(
            Pos2::new(accent_card.left() + pad, input_rect_y),
            Vec2::new(section_w - pad * 2.0, input_h),
        );
        ui.painter().rect_filled(input_rect, 0.0, t.bg_sec);

        let hex_slot = self.settings_hex_slot();
        let field_focused = self.settings_focus == Some(hex_slot);
        let line_w = if field_focused { 2.5 } else { 1.5 };
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

        if te_resp.lost_focus() {
            let trimmed = self.accent_custom_hex.trim();
            if trimmed.is_empty() {
                self.accent_use_custom = false;
                if self.accent_source == AccentSourcePref::Palette {
                    self.accent_color = accent_from_palette(
                        self.accent_palette_idx,
                        self.accent_use_custom,
                        &self.accent_custom_hex,
                    );
                }
                self.save_app_settings();
            } else if let Some(c) = parse_hex_color_6(&self.accent_custom_hex) {
                self.accent_use_custom = true;
                self.accent_custom_hex = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                if self.accent_source == AccentSourcePref::Palette {
                    self.accent_color = c;
                }
                self.save_app_settings();
            }
        }

        ui.advance_cursor_after_rect(section_rect);
    }

    fn speed_primary_id() -> Id {
        Id::new("diskoria_speed_primary")
    }

    /// Speed Test page — volume combo, 2×2 metrics, progress, Start/Stop.
    fn draw_speed_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        _dark: bool,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        let title = "Speed Test";
        let subtitle = "Measure sequential and random read/write performance";

        ui.painter().text(
            Pos2::new(content_x + margin, ui.cursor().min.y + 14.0),
            Align2::LEFT_CENTER,
            title,
            FontId::new(28.0, FontFamily::Proportional),
            t.txt_pri,
        );
        ui.add_space(32.0);

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
            ui.add_space(16.0);
            ui.push_id("diskoria_speed_refresh", |ui| {
                let refresh = ui.add_enabled(
                    !self.any_test_running(),
                    egui::Button::new(egui::RichText::new("⟳ Refresh").color(t.txt_pri)),
                );
                if refresh.clicked() {
                    self.speed_focus = Some(0);
                    self.spawn_drive_enumeration(ctx);
                }
                self.speed_refresh_id = Some(refresh.id);
                if !self.any_test_running() && self.speed_focus == Some(0) {
                    ui.painter().rect_stroke(
                        refresh.rect.expand(2.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
            });
            if self.drives_loading {
                ui.add_space(10.0);
                ui.spinner();
                ui.label(egui::RichText::new("Loading drives…").color(t.txt_sec));
            }
        });
        ui.add_space(20.0);

        if let Some(ref err) = self.drives_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(format!("Could not enumerate drives: {err}"))
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(12.0);
        }

        if let Some(ref msg) = self.speed_drive_removed_msg {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(msg)
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                if ui.button("Dismiss").clicked() {
                    self.speed_drive_removed_msg = None;
                }
            });
            ui.add_space(12.0);
        }

        if let Some(ref err) = self.speed_error_msg {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new(err)
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                if ui.button("Dismiss").clicked() {
                    self.speed_error_msg = None;
                }
            });
            ui.add_space(12.0);
        }

        if !self.drives_loading && self.drives.is_empty() && self.drives_error.is_none() {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new("No physical disks found.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
            ui.add_space(16.0);
        }

        if self.drives_loading && self.drives.is_empty() {
            return;
        }

        self.ensure_speed_volume_selection_valid();

        let section_w = content_w - margin * 2.0;
        let row_h = 34.0_f32;
        let chip_h = 26.0_f32;
        let gap_chips = 8.0_f32;
        let gap_combo_chips = 12.0_f32;

        let volume_combo_id = Id::new("diskoria_speed_volume_combo");
        let pairs = Self::speed_volume_pairs(&self.drives);
        let volume_labels: Vec<String> = pairs
            .iter()
            .map(|&(di, pi)| {
                let d = &self.drives[di];
                let p = &d.partitions[pi];
                Self::format_speed_volume_row(d, p)
            })
            .collect();
        let sel = self
            .selected_drive
            .min(self.drives.len().saturating_sub(1));
        let flat_current = pairs
            .iter()
            .position(|&(di, pi)| di == self.selected_drive && pi == self.selected_speed_partition)
            .unwrap_or(0);

        let y_row = ui.cursor().min.y;

        ui.add_enabled_ui(!self.any_test_running(), |ui| {
            let combo_w = if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let chips_w = mw + gap_chips + bw;
                (section_w - gap_combo_chips - chips_w).max(120.0)
            } else {
                section_w
            };

            let combo_rect = Rect::from_min_size(
                Pos2::new(content_x + margin, y_row),
                Vec2::new(combo_w, row_h),
            );
            ui.painter().rect_filled(combo_rect, 0.0, t.bg_pri);
            ui.painter().line_segment(
                [combo_rect.left_bottom(), combo_rect.right_bottom()],
                Stroke::new(1.5, t.accent),
            );

            let combo_inner = combo_rect.shrink2(Vec2::new(8.0, 4.0));
            let mut combo_style = (**ui.style()).clone();
            combo_style.spacing.button_padding = Vec2::new(10.0, 6.0);
            for w in [
                &mut combo_style.visuals.widgets.inactive,
                &mut combo_style.visuals.widgets.hovered,
                &mut combo_style.visuals.widgets.active,
                &mut combo_style.visuals.widgets.open,
            ] {
                w.weak_bg_fill = Color32::TRANSPARENT;
                w.bg_stroke = Stroke::NONE;
                w.fg_stroke = Stroke::new(1.0, t.txt_pri);
            }

            let selected_text = volume_labels
                .get(flat_current)
                .map(|s| s.as_str())
                .unwrap_or("—");

            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(combo_inner)
                    .style(std::sync::Arc::new(combo_style)),
                |ui| {
                    let mut choice = flat_current;
                    let ir = egui::ComboBox::from_id_salt(volume_combo_id)
                        .selected_text(selected_text)
                        .width(combo_inner.width())
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.style_mut().visuals.override_text_color = Some(t.txt_pri);
                            for (idx, label) in volume_labels.iter().enumerate() {
                                let r =
                                    ui.selectable_value(&mut choice, idx, label.as_str());
                                close_popup_if_selectable_clicked(ui.ctx(), &r);
                            }
                        });
                    if choice != flat_current {
                        if let Some(&(di, pi)) = pairs.get(choice) {
                            self.selected_drive = di;
                            self.selected_speed_partition = pi;
                        }
                    }
                    self.speed_volume_combo_id = Some(ir.response.id);
                    if ir.response.clicked() {
                        self.speed_focus = Some(1);
                    }
                    ir.response
                },
            );

            if !self.any_test_running() && self.speed_focus == Some(1) {
                ui.painter().rect_stroke(
                    combo_rect.expand(2.0),
                    4.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }

            if !self.drives.is_empty() {
                let d = &self.drives[sel];
                let chip_y = y_row + (row_h - chip_h) * 0.5;
                let mut x = combo_rect.right() + gap_combo_chips;

                let (m_bg, m_fg) = chip_colors_media(d.media);
                let mw = chip_width(ui.ctx(), d.media.label(), chip_h);
                let mr = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(mw, chip_h));
                chip_pill(ui.painter(), mr, d.media.label(), m_bg, m_fg);
                x += mw + gap_chips;

                let (b_bg, b_fg) = chip_colors_bus(d.bus);
                let bw = chip_width(ui.ctx(), d.bus.label(), chip_h);
                let br = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(bw, chip_h));
                chip_pill(ui.painter(), br, d.bus.label(), b_bg, b_fg);
            }

            let row_rect = Rect::from_min_max(
                Pos2::new(content_x + margin, y_row),
                Pos2::new(content_x + margin + section_w, y_row + row_h),
            );
            ui.advance_cursor_after_rect(row_rect);
        });

        if let Some(d) = self.drives.get(sel) {
            let pi = self
                .selected_speed_partition
                .min(d.partitions.len().saturating_sub(1));
            if !d.partitions.is_empty() && d.partitions[pi].is_bitlocker_locked() {
                ui.horizontal(|ui| {
                    let pad = (content_x + margin) - ui.min_rect().left();
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.label(
                        egui::RichText::new(
                            "This volume is BitLocker-locked. Speed tests cannot run on locked volumes.",
                        )
                        .size(13.0)
                        .color(Color32::from_rgb(231, 76, 60)),
                    );
                });
                ui.add_space(10.0);
            } else if d.partitions.is_empty() {
                ui.horizontal(|ui| {
                    let pad = (content_x + margin) - ui.min_rect().left();
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.label(
                        egui::RichText::new("No mounted volume on this disk for testing.")
                            .size(13.0)
                            .color(t.txt_sec),
                    );
                });
                ui.add_space(10.0);
            }
        }

        ui.add_space(8.0);

        // Start / Stop (above progress — matches workflow: pick volume → run → see results below)
        #[cfg(windows)]
        {
            const PRIMARY_BTN_H: f32 = 48.0;
            let (_, btn_alloc) = ui.allocate_space(Vec2::new(ui.available_width(), PRIMARY_BTN_H));
            let screen = ui.ctx().screen_rect();
            let btn_x = (content_x + margin).max(screen.left() + 12.0);
            let btn_w = section_w.min((screen.right() - 12.0 - btn_x).max(0.0));
            let btn_rect = Rect::from_min_size(
                Pos2::new(btn_x, btn_alloc.top()),
                Vec2::new(btn_w, PRIMARY_BTN_H),
            );
            let btn_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));

            if self.speed_test_running {
                let stop_focused = self.speed_focus == Some(0);
                let btn_r = ui.interact(btn_rect, Self::speed_primary_id(), Sense::click());
                let bg = if btn_r.hovered() || stop_focused {
                    Color32::from_rgb(
                        CLOSE_HOVER_BG.r().saturating_add(12),
                        CLOSE_HOVER_BG.g().saturating_add(8),
                        CLOSE_HOVER_BG.b().saturating_add(8),
                    )
                } else {
                    CLOSE_HOVER_BG
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if stop_focused {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Stop Test",
                    btn_font,
                    Color32::WHITE,
                );
                let kb = btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if btn_r.clicked() || kb {
                    self.stop_speed_test();
                }
            } else {
                let can_start = self.can_start_speed_test();
                let start_focused = can_start && self.speed_focus == Some(2);
                let btn_sense = if can_start {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let btn_r = ui.interact(btn_rect, Self::speed_primary_id(), btn_sense);
                let bg = if !can_start {
                    Color32::from_rgba_premultiplied(
                        t.accent.r() / 2,
                        t.accent.g() / 2,
                        t.accent.b() / 2,
                        180,
                    )
                } else if btn_r.hovered() || start_focused {
                    Color32::from_rgb(
                        t.accent.r().saturating_add(15),
                        t.accent.g().saturating_add(15),
                        t.accent.b().saturating_add(15),
                    )
                } else {
                    t.accent
                };
                let fg = if can_start {
                    t.txt_on_accent
                } else {
                    Color32::from_rgba_premultiplied(255, 255, 255, 120)
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if start_focused && can_start {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    "Start Test",
                    btn_font,
                    fg,
                );
                let kb = can_start
                    && btn_r.has_focus()
                    && ui.input_mut(|inp| {
                        inp.consume_key(Modifiers::NONE, Key::Enter)
                            || inp.consume_key(Modifiers::NONE, Key::Space)
                    });
                if (btn_r.clicked() || kb) && can_start {
                    self.start_speed_test(ctx);
                }
            }
            ui.advance_cursor_after_rect(btn_alloc);
        }

        #[cfg(windows)]
        ui.add_space(SPEED_PAGE_SECTION_GAP);

        // Progress card
        #[cfg(windows)]
        {
            let left = content_x + margin;
            let section_rect = Rect::from_min_size(
                Pos2::new(left, ui.cursor().min.y),
                Vec2::new(section_w, f32::INFINITY),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(section_rect), |ui| {
                Frame::new()
                    .fill(t.bg_pri)
                    .inner_margin(egui::Margin::same(16))
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.5, t.border))
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        ui.label(egui::RichText::new("PROGRESS").size(11.0).color(t.txt_sec));
                        ui.add_space(4.0);
                        ui.scope(|ui| {
                            ui.style_mut().visuals.extreme_bg_color = t.bg_sec;
                            ui.add(
                                egui::ProgressBar::new(
                                    (self.speed_progress_pct / 100.0).clamp(0.0, 1.0) as f32,
                                )
                                .desired_width(w)
                                .fill(t.accent),
                            );
                        });
                        ui.label(
                            egui::RichText::new(format!("{:.1}%", self.speed_progress_pct))
                                .size(14.0)
                                .color(t.txt_pri),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(&self.speed_progress_op)
                                .size(14.0)
                                .color(t.txt_pri),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("{:.1} MB/s", self.speed_current_mbps))
                                .size(13.0)
                                .color(t.txt_sec),
                        );
                    });
            });
        }

        #[cfg(windows)]
        ui.add_space(SPEED_PAGE_SECTION_GAP);

        // Results 2×2
        let gap = 12.0_f32;
        let cell_w = ((section_w - gap) * 0.5).max(80.0);
        let cell_h = 120.0_f32;
        let fmt = |v: f64| {
            if v < 0.0 {
                "—".to_string()
            } else {
                format!("{:.1}", v)
            }
        };

        let grid_top = ui.cursor().min.y;
        let gleft = content_x + margin;
        let r00 = Rect::from_min_size(Pos2::new(gleft, grid_top), Vec2::new(cell_w, cell_h));
        let r01 = Rect::from_min_size(
            Pos2::new(gleft + cell_w + gap, grid_top),
            Vec2::new(cell_w, cell_h),
        );
        let r10 = Rect::from_min_size(
            Pos2::new(gleft, grid_top + cell_h + gap),
            Vec2::new(cell_w, cell_h),
        );
        let r11 = Rect::from_min_size(
            Pos2::new(gleft + cell_w + gap, grid_top + cell_h + gap),
            Vec2::new(cell_w, cell_h),
        );

        let painter = ui.painter();
        paint_speed_metric_cell(
            ctx,
            &painter,
            r00,
            t,
            "SEQUENTIAL READ",
            &fmt(self.speed_seq_read_mbps),
            "1 MiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            &painter,
            r01,
            t,
            "SEQUENTIAL WRITE",
            &fmt(self.speed_seq_write_mbps),
            "1 MiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            &painter,
            r10,
            t,
            "RANDOM 4K READ",
            &fmt(self.speed_r4_read_mbps),
            "4 KiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            &painter,
            r11,
            t,
            "RANDOM 4K WRITE",
            &fmt(self.speed_r4_write_mbps),
            "4 KiB blocks",
        );

        let grid_h = cell_h * 2.0 + gap;
        ui.advance_cursor_after_rect(Rect::from_min_size(
            Pos2::new(gleft, grid_top),
            Vec2::new(section_w, grid_h),
        ));

        ui.add_space(SPEED_PAGE_BOTTOM_PAD);

        #[cfg(not(windows))]
        {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 {
                    ui.add_space(pad);
                }
                ui.label(
                    egui::RichText::new("Speed test requires Windows.")
                        .size(14.0)
                        .color(t.txt_sec),
                );
            });
        }
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

/// Render the performance chart to a PNG file using plotters.
#[cfg(windows)]
fn export_performance_chart_png(
    path: &std::path::Path,
    drive_label: &str,
    test_label: &str,
    raw_points: &[[f64; 2]],
    chart_points: &[[f64; 2]],
    max_speed: f64,
    total_gb: f64,
) -> Result<(), String> {
    use plotters::prelude::*;

    let root = BitMapBackend::new(path, (960, 460))
        .into_drawing_area();
    root.fill(&WHITE).map_err(|e| e.to_string())?;

    let nice_max = nice_y_max(max_speed);
    let x_max = total_gb.max(1.0);

    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .x_label_area_size(45)
        .y_label_area_size(70)
        .caption(drive_label, ("sans-serif", 18).into_font().color(&BLACK))
        .build_cartesian_2d(0.0_f64..x_max, 0.0_f64..nice_max)
        .map_err(|e| e.to_string())?;

    chart
        .configure_mesh()
        .x_desc("Position (GB)")
        .y_desc("Disk Performance (MB/s)")
        .axis_desc_style(("sans-serif", 16).into_font())
        .label_style(("sans-serif", 14).into_font())
        .draw()
        .map_err(|e| e.to_string())?;

    let dot_color = RGBColor(160, 160, 160);
    if !raw_points.is_empty() {
        chart
            .draw_series(raw_points.iter().map(|p| {
                Circle::new((p[0], p[1]), 2, ShapeStyle::from(&dot_color).filled())
            }))
            .map_err(|e| e.to_string())?;
    }

    if !chart_points.is_empty() {
        chart
            .draw_series(LineSeries::new(
                chart_points.iter().map(|p| (p[0], p[1])),
                ShapeStyle::from(&BLUE).stroke_width(2),
            ))
            .map_err(|e| e.to_string())?;
    }

    root.draw(&Text::new(
        test_label,
        (480, 444),
        ("sans-serif", 16).into_font().color(&RGBColor(100, 100, 100))
            .pos(plotters::style::text_anchor::Pos::new(
                plotters::style::text_anchor::HPos::Center,
                plotters::style::text_anchor::VPos::Top,
            )),
    )).map_err(|e| e.to_string())?;

    root.present().map_err(|e| e.to_string())?;
    Ok(())
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

fn chip_width(ctx: &egui::Context, label: &str, _h: f32) -> f32 {
    let galley = ctx.fonts(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            FontId::proportional(12.0),
            Color32::WHITE,
        )
    });
    galley.rect.width() + 20.0
}

fn chip_colors_media(m: MediaKind) -> (Color32, Color32) {
    match m {
        MediaKind::Hdd => (Color32::from_rgb(52, 73, 94), Color32::WHITE),
        MediaKind::Ssd => (Color32::from_rgb(41, 128, 185), Color32::WHITE),
        MediaKind::SdCard => (Color32::from_rgb(39, 174, 96), Color32::WHITE),
        MediaKind::Flash => (Color32::from_rgb(155, 89, 182), Color32::WHITE),
        MediaKind::EMmc => (Color32::from_rgb(26, 188, 156), Color32::WHITE),
        MediaKind::Unknown => (Color32::from_rgb(127, 140, 141), Color32::WHITE),
    }
}

fn chip_colors_bus(b: BusKind) -> (Color32, Color32) {
    match b {
        BusKind::Nvme => (Color32::from_rgb(142, 68, 173), Color32::WHITE),
        BusKind::Sata => (Color32::from_rgb(22, 160, 133), Color32::WHITE),
        BusKind::Usb => (Color32::from_rgb(230, 126, 34), Color32::WHITE),
        BusKind::Ufs => (Color32::from_rgb(52, 152, 219), Color32::WHITE),
    }
}

fn chip_pill(
    painter: &egui::Painter,
    rect: Rect,
    label: &str,
    bg: Color32,
    fg: Color32,
) {
    painter.rect_filled(rect, 6.0, bg);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        fg,
    );
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
    painter.rect_stroke(rect, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

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
        self.poll_drive_enumeration(ctx);
        self.poll_smart_health(ctx);
        self.poll_surface_test(ctx);
        self.poll_destructive_test(ctx);
        self.poll_speed_test(ctx);
        #[cfg(windows)]
        {
            self.poll_update_check(ctx);
            self.poll_update_download(ctx);
        }
        self.update_modal_confirm_tab_focus(ctx);
        self.prepare_sector_page_focus(ctx);
        self.prepare_destructive_page_focus(ctx);
        self.prepare_speed_page_focus(ctx);
        self.update_settings_tab_focus(ctx);
        if self.any_test_running() {
            ctx.request_repaint();
        }
        self.alt_pressed = alt_pressed(ctx);
        #[cfg(windows)]
        let update_blocks_shortcuts = self.show_update_download_confirm
            || self.show_update_alert
            || self.update_check_busy
            || self.update_download_busy;
        #[cfg(not(windows))]
        let update_blocks_shortcuts = false;
        if !self.any_test_running()
            && !self.show_stop_test_confirm
            && !self.show_destructive_start_confirm
            && !self.show_destructive_stop_confirm
            && !update_blocks_shortcuts
        {
            if let Some(nav) = handle_alt_shortcuts(
                ctx,
                &[
                    ShortcutBinding {
                        key: Key::E,
                        action: 0usize,
                    },
                    ShortcutBinding {
                        key: Key::D,
                        action: 1,
                    },
                    ShortcutBinding {
                        key: Key::P,
                        action: 2,
                    },
                    ShortcutBinding {
                        key: Key::A,
                        action: 3,
                    },
                    ShortcutBinding {
                        key: Key::S,
                        action: 4,
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

        let system_dark = match ctx.system_theme() {
            Some(egui::Theme::Dark) => true,
            Some(egui::Theme::Light) => false,
            None => false,
        };
        let dark = match self.theme_pref {
            ThemePref::Auto => system_dark,
            ThemePref::Dark => true,
            ThemePref::Light => false,
        };
        self.dark = dark;

        self.update_accent_color(ctx);
        apply_visuals(ctx, dark, self.accent_color);

        let t = Theme::new(dark, self.accent_color);
        // Same paint order as copynaut: titlebar → sidebar → content (content last in default layer).
        #[cfg(windows)]
        draw_titlebar(ctx, &t, self.hwnd);
        #[cfg(not(windows))]
        draw_titlebar(ctx, &t, 0);
        self.draw_sidebar(ctx, dark);
        self.draw_central(ctx, dark);
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
        #[cfg(windows)]
        {
            if self.show_update_download_confirm {
                self.draw_update_download_confirm(ctx, dark);
            }
            if self.show_update_alert {
                self.draw_update_alert(ctx, dark);
            }
            self.draw_update_busy_overlay(ctx, dark);
        }
    }
}
