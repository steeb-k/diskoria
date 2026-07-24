//! Drive Health page — raw SMART / NVMe diagnostics for any drive the user selects.
//!
//! This module owns the per-page state fields on `DiskoriaApp` that carry the selected
//! drive index and the polled `SmartReport`.  Page drawing is done by free functions
//! called from `DiskoriaApp::draw_health_status_page`.

use egui::{
    Align2, Color32, FontId, Id, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, Vec2,
};
#[cfg(windows)]
use egui_plot::{Line, Plot, PlotPoints};

use crate::detected_drive::DetectedDrive;
use crate::drive_selector::{self, ChipSpec, DriveEntry};
use crate::smart_reader::{AtaAttribute, AtaSmartData, AttrStatus, NvmeHealthData, UfsHealthData, SmartReport};
use crate::theme::Theme;
use crate::widgets::{show_tooltip_text, small_browse_style_button};
use crate::DiskoriaApp;

// ── Icon constants (Bootstrap Icons PUA) ─────────────────────────────────────

const ICON_FILETYPE_HTML: char = '\u{f3e9}';
const ICON_HEART_PULSE: char = '\u{f473}';
const ICON_WARNING: char = '\u{f333}';
const ICON_QUESTION: char = '\u{f505}';

// ── Public state fields (held on DiskoriaApp) ─────────────────────────────────

// ── Attr tooltip table ────────────────────────────────────────────────────────

pub fn attr_description(id: u8) -> &'static str {
    match id {
        0x01 => "Counts low-level errors when reading from the magnetic surface.",
        0x02 => "Drive's throughput relative to its hardware design capability.",
        0x03 => "Time (ms) needed for the disk motor to spin up to full speed.",
        0x04 => "Total spindle start/stop cycles since leaving the factory.",
        0x05 => "Sectors remapped to reserve area due to uncorrectable errors.",
        0x07 => "Rate of seek errors — how often the head fails to reach the right track.",
        0x09 => "Total power-on time in hours.",
        0x0A => "Retries to reach target RPM — elevated count suggests bearing problems.",
        0x0B => "Number of times the drive had to recalibrate its heads.",
        0x0C => "Total power on/off cycles.",
        0xBB => "Sectors that could not be corrected by hardware ECC.",
        0xBC => "Commands that timed out — often a sign of communication issues.",
        0xBD => "Writes made at excessive head-fly height — can corrupt sectors.",
        0xBE => "Drive temperature measured at a secondary sensor.",
        0xC0 => "Emergency head retractions triggered by loss of power.",
        0xC1 => "Total load/unload cycles (head park operations).",
        0xC2 => "Drive temperature in degrees Celsius.",
        0xC3 => "Number of errors corrected by hardware ECC (info only).",
        0xC4 => "Total count of reallocation events (sectors moved to reserve).",
        0xC5 => "Sectors waiting to be remapped — read/write may still succeed.",
        0xC6 => "Sectors that could not be recovered even with error recovery.",
        0xC7 => "CRC errors on the interface cable between drive and controller. A high count usually means a faulty or loose cable.",
        0xE7 => "Drive's self-reported remaining lifespan as a percentage.",
        0xF0 => "Total hours the head assembly has been in loaded position.",
        0xF1 => "Total logical blocks written over the drive's lifetime.",
        0xF2 => "Total logical blocks read over the drive's lifetime.",
        _ => "",
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fmt_thousands(n: u64) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

fn status_color(status: AttrStatus, _dark: bool) -> Color32 {
    match status {
        AttrStatus::Good    => Color32::from_rgb(39, 174, 96),
        AttrStatus::Warning => Color32::from_rgb(241, 196, 15),
        AttrStatus::Failed  => Color32::from_rgb(231, 76, 60),
        AttrStatus::Info    => Color32::from_rgb(52, 152, 219),
    }
}

// ── Card frame helper ─────────────────────────────────────────────────────────

fn card_rect(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    height: f32,
) -> Rect {
    let top = ui.cursor().min.y;
    let rect = Rect::from_min_size(
        Pos2::new(content_x + margin, top),
        Vec2::new(section_w, height),
    );
    ui.painter().rect_filled(rect, 8.0, t.bg_pri);
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);
    rect
}

// ── Drive picker ──────────────────────────────────────────────────────────────

fn draw_drive_picker(
    app: &mut DiskoriaApp,
    ui: &mut egui::Ui,
    t: &Theme,
    ctx: &egui::Context,
    content_x: f32,
    margin: f32,
    section_w: f32,
) {
    let left = content_x + margin;
    let y_row = ui.cursor().min.y;
    let row_rect =
        Rect::from_min_size(Pos2::new(left, y_row), Vec2::new(section_w, drive_selector::ROW_H));

    // Refresh icon button (left) — always shown so the user can retry when no
    // drives are found.
    let btn_rect = Rect::from_min_size(
        Pos2::new(left, y_row + (drive_selector::ROW_H - drive_selector::REFRESH_W) * 0.5),
        Vec2::splat(drive_selector::REFRESH_W),
    );
    let refresh = drive_selector::refresh_button(
        ui,
        ctx,
        t,
        Id::new("diskoria_health_refresh"),
        btn_rect,
        true,
        app.refresh_busy(),
        app.health_focus == Some(0),
    );
    if refresh.clicked() {
        app.health_focus = Some(0);
        app.health_report = None;
        app.health_poll_running = false;
        app.health_last_poll = None;
        app.spawn_drive_enumeration(ctx);
    }
    app.health_refresh_id = Some(refresh.id);
    crate::focus::scroll_to_focused(
        &mut app.pending_scroll_rect,
        btn_rect,
        app.health_focus == Some(0),
        app.scroll_focus_frames > 0,
    );

    if app.drives.is_empty() {
        ui.advance_cursor_after_rect(row_rect);
        return;
    }

    let card_left = left + drive_selector::REFRESH_W + drive_selector::REFRESH_GAP;
    let card_w = section_w - drive_selector::REFRESH_W - drive_selector::REFRESH_GAP;

    let entries: Vec<DriveEntry> = app
        .drives
        .iter()
        .map(|d| DriveEntry {
            title: format!("Drive {} — {}", d.disk_number, d.model.trim()),
            chips: vec![
                ChipSpec::neutral(t, DetectedDrive::format_size(d.size_bytes)),
                ChipSpec::media(d.media),
                ChipSpec::bus(d.bus),
            ],
            // Drive Health is read-only — any drive stays viewable even while
            // it is under test in another window.
            disabled: false,
        })
        .collect();
    let sel = app.selected_drive.min(entries.len().saturating_sub(1));
    let prev_sel = app.selected_drive;

    let out = drive_selector::two_row_combo(
        ui,
        t,
        Id::new("diskoria_health_drive_combo"),
        &entries,
        sel,
        app.health_focus == Some(1),
        !app.refresh_busy(),
        card_left,
        card_w,
        y_row,
    );
    app.health_combo_id = Some(out.id);
    if out.clicked {
        app.health_focus = Some(1);
    }
    app.selected_drive = out.selected;

    let card_rect =
        Rect::from_min_size(Pos2::new(card_left, y_row), Vec2::new(card_w, drive_selector::ROW_H));
    crate::focus::scroll_to_focused(
        &mut app.pending_scroll_rect,
        card_rect,
        app.health_focus == Some(1),
        app.scroll_focus_frames > 0,
    );

    // If selection changed, clear the old report and timer so we re-poll immediately
    if app.selected_drive != prev_sel {
        app.health_report = None;
        app.health_poll_running = false;
        app.health_last_poll = None;
    }

    ui.advance_cursor_after_rect(row_rect);
}

// ── Unavailable card ──────────────────────────────────────────────────────────

fn draw_unavailable(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    reason: &str,
) {
    let pad = 16.0_f32;
    let inner_w = section_w - pad * 2.0;
    let icon_size = 26.0_f32;
    let gap = 10.0_f32;
    let text_w = inner_w - icon_size - gap;

    let galley = ui.ctx().fonts(|f| {
        f.layout(
            reason.to_owned(),
            FontId::proportional(13.0),
            t.txt_sec,
            text_w.max(80.0),
        )
    });
    let text_h = galley.rect.height();
    let content_h = icon_size.max(text_h);
    let card_h = pad + content_h + pad;

    let card = card_rect(ui, t, content_x, margin, section_w, card_h);
    let inner = Rect::from_min_size(Pos2::new(card.min.x + pad, card.min.y + pad), Vec2::new(inner_w, content_h));

    let icon_rect = Rect::from_min_size(
        Pos2::new(inner.min.x, inner.min.y + (content_h - icon_size) * 0.5),
        Vec2::splat(icon_size),
    );
    ui.painter().text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        format!("{}", ICON_HEART_PULSE),
        FontId::proportional(20.0),
        t.txt_sec,
    );

    let text_pos = Pos2::new(inner.min.x + icon_size + gap, inner.min.y + (content_h - text_h) * 0.5);
    ui.painter().add(egui::Shape::galley(text_pos, galley, t.txt_sec));

    ui.advance_cursor_after_rect(card);
}

// ── Section label row ─────────────────────────────────────────────────────────

fn draw_section_label(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    label: &str,
    live_badge: bool,
) {
    let row_top = ui.cursor().min.y;
    let row_h = 24.0_f32;
    let row_rect = Rect::from_min_size(
        Pos2::new(content_x + margin, row_top),
        Vec2::new(section_w, row_h),
    );

    ui.painter().text(
        Pos2::new(row_rect.min.x, row_rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::new(13.0, egui::FontFamily::Name("InterBold".into())),
        t.txt_sec,
    );

    if live_badge {
        let badge_text = "\u{F309} LIVE";
        let badge_galley = ui.ctx().fonts(|f| {
            f.layout_no_wrap(badge_text.to_owned(), FontId::proportional(11.0), Color32::WHITE)
        });
        let bw = badge_galley.rect.width() + 10.0;
        let bh = 16.0_f32;
        let badge_rect = Rect::from_min_size(
            Pos2::new(row_rect.max.x - bw, row_rect.center().y - bh * 0.5),
            Vec2::new(bw, bh),
        );
        ui.painter().rect_filled(badge_rect, 4.0, Color32::from_rgb(39, 174, 96));
        ui.painter().add(egui::Shape::galley(
            Pos2::new(badge_rect.min.x + 5.0, badge_rect.min.y + (bh - badge_galley.rect.height()) * 0.5),
            badge_galley,
            Color32::WHITE,
        ));
    }

    ui.advance_cursor_after_rect(row_rect);
}

// ── Vitals key/value row (absolute-Y version — no cursor side-effects) ────────

/// Paint a label/value row at an explicit Y coordinate.
/// Does NOT advance the cursor; caller controls layout entirely.
fn paint_vitals_kv(
    ui: &mut egui::Ui,
    t: &Theme,
    card_x: f32,
    card_inner_w: f32,
    pad: f32,
    row_y: f32,
    row_h: f32,
    label: &str,
    value: &str,
    value_color: Color32,
    tooltip: Option<&str>,
) {
    // Fixed label column: wide enough for the longest label ("Unsafe Shutdowns")
    // but no wider so the value column has plenty of room.
    let label_col_w = (card_inner_w * 0.30).max(150.0);
    let cy = row_y + row_h * 0.5;

    ui.painter().text(
        Pos2::new(card_x + pad + label_col_w, cy),
        Align2::RIGHT_CENTER,
        label,
        FontId::proportional(13.0),
        t.txt_sec,
    );
    ui.painter().text(
        Pos2::new(card_x + pad + label_col_w + 8.0, cy),
        Align2::LEFT_CENTER,
        value,
        FontId::proportional(13.0),
        value_color,
    );

    if let Some(tip) = tooltip {
        let row_rect = Rect::from_min_size(Pos2::new(card_x + pad, row_y), Vec2::new(card_inner_w, row_h));
        let resp = ui.interact(row_rect, Id::new(format!("health_tip_i_{label}")), Sense::hover());
        if resp.hovered() {
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                show_tooltip_text(ui.ctx(), Id::new(format!("health_tip_tt_{label}")), pos, t, tip);
            }
        }
    }
}

// ── Wear-level bar (absolute-Y version) ──────────────────────────────────────

fn paint_wear_bar(
    ui: &mut egui::Ui,
    t: &Theme,
    card_x: f32,
    card_inner_w: f32,
    pad: f32,
    row_y: f32,
    row_h: f32,
    pct_used: u8,
) {
    let label_col_w = (card_inner_w * 0.30).max(150.0);
    let bar_h = 8.0_f32;
    let range_reserve = 50.0_f32;   // "100%" is short — only needs ~40px
    let bar_x = card_x + pad + label_col_w + 8.0;
    let full_bar_w = (card_inner_w - label_col_w - 8.0 - range_reserve).max(20.0);

    ui.painter().text(
        Pos2::new(card_x + pad + label_col_w, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        "Wear Level",
        FontId::proportional(13.0),
        t.txt_sec,
    );

    let bar_y = row_y + (row_h - bar_h) * 0.5;
    let bar_rect = Rect::from_min_size(Pos2::new(bar_x, bar_y), Vec2::new(full_bar_w, bar_h));
    ui.painter().rect_filled(bar_rect, 4.0, t.border);

    let pct = (pct_used as f32 / 100.0).clamp(0.0, 1.0);
    let fill_color = if pct_used < 75 {
        Color32::from_rgb(39, 174, 96)
    } else if pct_used < 90 {
        Color32::from_rgb(241, 196, 15)
    } else {
        Color32::from_rgb(231, 76, 60)
    };
    let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * pct, bar_h));
    if fill_rect.width() > 0.0 {
        ui.painter().rect_filled(fill_rect, 4.0, fill_color);
    }

    // Percentage value — right-anchored so it never overflows
    let right_inner = card_x + pad + card_inner_w;
    ui.painter().text(
        Pos2::new(right_inner, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        format!("{}%", pct_used),
        FontId::proportional(12.0),
        fill_color,
    );
    // No cursor advance — caller owns layout.
}

// ── Temperature bar (absolute-Y version) ─────────────────────────────────────

fn paint_temp_bar(
    ui: &mut egui::Ui,
    t: &Theme,
    card_x: f32,
    card_inner_w: f32,
    pad: f32,
    row_y: f32,
    row_h: f32,
    temp_c: i32,
) {
    // Match the same label column width as paint_vitals_kv
    let label_col_w = (card_inner_w * 0.30).max(150.0);
    let bar_h = 8.0_f32;
    // Reserve 110px on the right for the "37°C  (0 - 70°C)" range label
    let range_reserve = 110.0_f32;
    let bar_x = card_x + pad + label_col_w + 8.0;
    // Bar spans from bar_x to the start of the range label area
    let full_bar_w = (card_inner_w - label_col_w - 8.0 - range_reserve).max(20.0);

    ui.painter().text(
        Pos2::new(card_x + pad + label_col_w, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        "Temperature",
        FontId::proportional(13.0),
        t.txt_sec,
    );

    let bar_y = row_y + (row_h - bar_h) * 0.5;
    let bar_rect = Rect::from_min_size(Pos2::new(bar_x, bar_y), Vec2::new(full_bar_w, bar_h));

    // Background track
    ui.painter().rect_filled(bar_rect, 4.0, t.border);

    // Fill (clamp 0-70 °C)
    let pct = (temp_c as f32 / 70.0).clamp(0.0, 1.0);
    let fill_color = if temp_c < 40 {
        Color32::from_rgb(39, 174, 96)
    } else if temp_c < 55 {
        Color32::from_rgb(241, 196, 15)
    } else {
        Color32::from_rgb(231, 76, 60)
    };
    let fill_rect = Rect::from_min_size(
        bar_rect.min,
        Vec2::new(bar_rect.width() * pct, bar_h),
    );
    if fill_rect.width() > 0.0 {
        ui.painter().rect_filled(fill_rect, 4.0, fill_color);
    }

    // Range label — right-aligned at the card's inner right edge so it never overflows
    let right_inner = card_x + pad + card_inner_w;
    ui.painter().text(
        Pos2::new(right_inner, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        format!("{}°C  (0 - 70°C)", temp_c),
        FontId::proportional(12.0),
        t.txt_sec,
    );
    // No cursor advance — caller owns layout.
}

// ── ATA vitals card ───────────────────────────────────────────────────────────

fn draw_ata_vitals(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    data: &AtaSmartData,
) {
    let pad = 16.0_f32;
    let inner_w = section_w - pad * 2.0;
    let row_h = 22.0_f32;
    let gap = 4.0_f32;

    let row_count = 1  // temperature or placeholder
        + if data.power_on_hours.is_some() { 1 } else { 0 }
        + if data.power_cycles.is_some() { 1 } else { 0 };

    // Exact height: top pad + rows + (rows-1) gaps + bottom pad
    let card_h = pad + row_count as f32 * row_h + (row_count - 1) as f32 * gap + pad;

    draw_section_label(ui, t, content_x, margin, section_w, "Vitals", true);
    ui.add_space(6.0);

    let card = card_rect(ui, t, content_x, margin, section_w, card_h);
    let card_x = card.min.x;

    // Use absolute Y positions — no cursor tracking inside the card.
    let mut row_y = card.min.y + pad;
    let row_step = row_h + gap;

    if let Some(tc) = data.temperature_c {
        paint_temp_bar(ui, t, card_x, inner_w, pad, row_y, row_h, tc);
    } else {
        paint_vitals_kv(ui, t, card_x, inner_w, pad, row_y, row_h, "Temperature", "N/A", t.txt_pri, None);
    }
    row_y += row_step;

    if let Some(poh) = data.power_on_hours {
        let days = poh / 24;
        let label = if days > 0 {
            format!("{} hours ({} days)", fmt_thousands(poh), fmt_thousands(days))
        } else {
            format!("{} hours", poh)
        };
        paint_vitals_kv(ui, t, card_x, inner_w, pad, row_y, row_h, "Power-On Hours", &label, t.txt_pri, None);
        row_y += row_step;
    }

    if let Some(pc) = data.power_cycles {
        paint_vitals_kv(ui, t, card_x, inner_w, pad, row_y, row_h, "Power Cycles", &fmt_thousands(pc), t.txt_pri, None);
    }

    // Single cursor advance past the whole card.
    ui.advance_cursor_after_rect(card);
}

// ── NVMe vitals card ──────────────────────────────────────────────────────────

fn draw_nvme_vitals(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    data: &NvmeHealthData,
) {
    let pad = 16.0_f32;
    let inner_w = section_w - pad * 2.0;
    let row_h = 22.0_f32;
    let gap = 4.0_f32;

    // Rows: temp, % used, spare, POH, power cycles, data written, unsafe shutdowns, media errors, critical warning
    let rows = 9usize;
    // Exact height: top pad + rows*row_h + (rows-1)*gap + bottom pad
    let card_h = pad + rows as f32 * row_h + (rows - 1) as f32 * gap + pad;

    draw_section_label(ui, t, content_x, margin, section_w, "Vitals", true);
    ui.add_space(6.0);

    let card = card_rect(ui, t, content_x, margin, section_w, card_h);
    let card_x = card.min.x;

    // Absolute Y positions — no cursor tracking inside the card.
    let row_step = row_h + gap;
    let mut row_y = card.min.y + pad;

    paint_temp_bar(ui, t, card_x, inner_w, pad, row_y, row_h, data.temperature_c as i32);
    row_y += row_step;

    paint_wear_bar(ui, t, card_x, inner_w, pad, row_y, row_h, data.percentage_used);
    row_y += row_step;

    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Available Spare",
        &format!("{}% (threshold {}%)", data.available_spare_pct, data.available_spare_threshold),
        t.txt_pri,
        Some("Remaining spare NAND blocks the controller can use for remapping bad cells."),
    );
    row_y += row_step;

    let poh = data.power_on_hours;
    let days = poh / 24;
    let poh_label = if days > 0 {
        format!("{} hours ({} days)", fmt_thousands(poh), fmt_thousands(days))
    } else {
        format!("{} hours", poh)
    };
    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Power-On Hours", &poh_label, t.txt_pri,
        Some("Total time the drive has been powered on since manufacture."),
    );
    row_y += row_step;

    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Power Cycles", &fmt_thousands(data.power_cycles), t.txt_pri,
        Some("Number of times the drive has been powered on and off."),
    );
    row_y += row_step;

    // Data written: NVMe reports in 512 KiB units
    let dw_gib = data.data_units_written as f64 * 512.0 / (1024.0 * 1024.0);
    let dw_label = if dw_gib >= 1024.0 {
        format!("{:.2} TiB", dw_gib / 1024.0)
    } else {
        format!("{:.1} GiB", dw_gib)
    };
    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Data Written", &dw_label, t.txt_pri,
        Some("Cumulative host data written in 512 KiB units (NVMe spec)."),
    );
    row_y += row_step;

    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Unsafe Shutdowns", &fmt_thousands(data.unsafe_shutdowns), t.txt_pri,
        Some("Power loss events that did not allow the drive to flush its cache. High counts can accelerate NAND wear."),
    );
    row_y += row_step;

    // Media Errors — colored value
    let media_color = if data.media_errors > 0 { Color32::from_rgb(241, 196, 15) } else { t.txt_pri };
    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Media Errors", &fmt_thousands(data.media_errors), media_color,
        Some("Errors that occurred directly on the NAND media. Anything above zero warrants attention."),
    );
    row_y += row_step;

    // Critical Warning — colored value, routed through paint_vitals_kv for consistent alignment
    let warn_color = if data.critical_warning != 0 { Color32::from_rgb(231, 76, 60) } else { t.txt_pri };
    let warn_text = if data.critical_warning == 0 {
        "None".to_string()
    } else {
        format!("{:02X}", data.critical_warning)
    };
    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Critical Warning", &warn_text, warn_color,
        Some("Bit field set by the controller for serious health events (spare below threshold, temperature excursion, read-only mode, etc.)."),
    );

    // Single cursor advance past the whole card.
    ui.advance_cursor_after_rect(card);
}

// ── ATA attribute grid ────────────────────────────────────────────────────────

// ── UFS vitals card ───────────────────────────────────────────────────────────

fn ufs_lifetime_label(v: u8) -> &'static str {
    match v {
        0x01 => "0\u{2013}10%",
        0x02 => "10\u{2013}20%",
        0x03 => "20\u{2013}30%",
        0x04 => "30\u{2013}40%",
        0x05 => "40\u{2013}50%",
        0x06 => "50\u{2013}60%",
        0x07 => "60\u{2013}70%",
        0x08 => "70\u{2013}80%",
        0x09 => "80\u{2013}90%",
        0x0A => "90\u{2013}100%",
        0x0B => "Exceeded maximum",
        _ => "Not defined",
    }
}

fn ufs_lifetime_midpoint(v: u8) -> u8 {
    match v {
        0x01..=0x0A => (v - 1) * 10 + 5,
        0x0B => 100,
        _ => 0,
    }
}

fn paint_ufs_lifetime_bar(
    ui: &mut egui::Ui,
    t: &Theme,
    card_x: f32,
    card_inner_w: f32,
    pad: f32,
    row_y: f32,
    row_h: f32,
    label: &str,
    value: u8,
) {
    let label_col_w = (card_inner_w * 0.30).max(150.0);
    let bar_h = 8.0_f32;
    let range_reserve = 120.0_f32;
    let bar_x = card_x + pad + label_col_w + 8.0;
    let full_bar_w = (card_inner_w - label_col_w - 8.0 - range_reserve).max(20.0);

    ui.painter().text(
        Pos2::new(card_x + pad + label_col_w, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        label,
        FontId::proportional(13.0),
        t.txt_sec,
    );

    let bar_y = row_y + (row_h - bar_h) * 0.5;
    let bar_rect = Rect::from_min_size(Pos2::new(bar_x, bar_y), Vec2::new(full_bar_w, bar_h));
    ui.painter().rect_filled(bar_rect, 4.0, t.border);

    let midpoint = ufs_lifetime_midpoint(value);
    let pct = midpoint as f32 / 100.0;
    let fill_color = if midpoint < 75 {
        Color32::from_rgb(39, 174, 96)
    } else if midpoint < 90 {
        Color32::from_rgb(241, 196, 15)
    } else {
        Color32::from_rgb(231, 76, 60)
    };
    let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * pct, bar_h));
    if fill_rect.width() > 0.0 {
        ui.painter().rect_filled(fill_rect, 4.0, fill_color);
    }

    let right_inner = card_x + pad + card_inner_w;
    ui.painter().text(
        Pos2::new(right_inner, row_y + row_h * 0.5),
        Align2::RIGHT_CENTER,
        ufs_lifetime_label(value),
        FontId::proportional(12.0),
        if midpoint > 0 { fill_color } else { t.txt_sec },
    );
}

fn draw_ufs_vitals(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    data: &UfsHealthData,
) {
    let pad = 16.0_f32;
    let inner_w = section_w - pad * 2.0;
    let row_h = 22.0_f32;
    let gap = 4.0_f32;

    let rows = 3usize; // Pre-EOL status, Life Used A, Life Used B
    let card_h = pad + rows as f32 * row_h + (rows - 1) as f32 * gap + pad;

    draw_section_label(ui, t, content_x, margin, section_w, "Vitals", true);
    ui.add_space(6.0);

    let card = card_rect(ui, t, content_x, margin, section_w, card_h);
    let card_x = card.min.x;

    let row_step = row_h + gap;
    let mut row_y = card.min.y + pad;

    let (pre_eol_text, pre_eol_color) = match data.pre_eol_info {
        0x01 => ("Normal", Color32::from_rgb(39, 174, 96)),
        0x02 => ("Warning", Color32::from_rgb(241, 196, 15)),
        0x03 => ("Urgent", Color32::from_rgb(231, 76, 60)),
        _ => ("Not defined", t.txt_sec),
    };
    paint_vitals_kv(
        ui, t, card_x, inner_w, pad, row_y, row_h,
        "Pre-EOL Status", pre_eol_text, pre_eol_color,
        Some("End-of-life indicator from the UFS controller. Warning = 80%+ of reserved blocks consumed. Urgent = replacement recommended."),
    );
    row_y += row_step;

    paint_ufs_lifetime_bar(ui, t, card_x, inner_w, pad, row_y, row_h, "Life Used (Type A)", data.life_time_est_a);
    row_y += row_step;

    paint_ufs_lifetime_bar(ui, t, card_x, inner_w, pad, row_y, row_h, "Life Used (Type B)", data.life_time_est_b);

    ui.advance_cursor_after_rect(card);
}

fn draw_attribute_card(
    ui: &mut egui::Ui,
    t: &Theme,
    dark: bool,
    rect: Rect,
    attr: &AtaAttribute,
) {
    let status_color = status_color(attr.status, dark);
    ui.painter().rect_filled(rect, 8.0, t.bg_pri);
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

    let pad = 10.0_f32;
    let inner_x = rect.min.x + pad;
    let inner_w = rect.width() - pad * 2.0;

    // Status bar on left edge
    let bar_rect = Rect::from_min_size(
        rect.min + Vec2::new(0.0, 0.0),
        Vec2::new(4.0, rect.height()),
    );
    ui.painter().rect_filled(bar_rect, egui::CornerRadius { nw: 8, sw: 8, ne: 0, se: 0 }, status_color);

    // Attribute ID — top-right corner
    let id_str = format!("{:02X}", attr.id);
    ui.painter().text(
        Pos2::new(rect.max.x - pad, rect.min.y + 10.0),
        Align2::RIGHT_TOP,
        &id_str,
        FontId::proportional(11.0),
        t.txt_sec,
    );

    // Name — full width of the card (ID is now out of the way in the corner)
    let name_galley = ui.ctx().fonts(|f| {
        f.layout(
            attr.name.to_owned(),
            FontId::proportional(13.0),
            t.txt_pri,
            inner_w,
        )
    });
    let name_w = name_galley.size().x;
    ui.painter().add(egui::Shape::galley(
        Pos2::new(inner_x, rect.min.y + 8.0),
        name_galley,
        t.txt_pri,
    ));

    // C7 cable warning icon — positioned immediately right of the attribute name
    if attr.id == 0xC7 && attr.raw > 0 {
        let icon_id = Id::new("health_c7_warn_icon");
        let icon_rect = Rect::from_min_size(
            Pos2::new(inner_x + name_w + 5.0, rect.min.y + 8.0),
            Vec2::splat(16.0),
        );
        let warn_r = ui.interact(icon_rect, icon_id, Sense::hover());
        ui.painter().text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            format!("{}", ICON_WARNING),
            FontId::proportional(14.0),
            Color32::from_rgb(231, 76, 60),
        );
        if warn_r.hovered() {
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                show_tooltip_text(
                    ui.ctx(), Id::new("health_c7_warn_tt"), pos, t,
                    "High C7 (UltraDMA CRC Errors) usually means a faulty or loose SATA cable. Try reseating the cable.",
                );
            }
        }
    }

    // Cur / Wst / Thr / Raw columns
    let col_y = rect.min.y + 30.0;
    let col_labels = ["Cur", "Wst", "Thr"];
    let col_vals = [
        attr.current.to_string(),
        attr.worst.to_string(),
        attr.threshold.to_string(),
    ];
    let col_count = 3usize;
    let raw_col_w = inner_w * 0.4;
    let meta_col_w = inner_w - raw_col_w;
    let each_w = meta_col_w / col_count as f32;

    for (i, (lbl, val)) in col_labels.iter().zip(col_vals.iter()).enumerate() {
        let cx = inner_x + i as f32 * each_w + each_w * 0.5;
        ui.painter().text(
            Pos2::new(cx, col_y),
            Align2::CENTER_TOP,
            lbl,
            FontId::proportional(11.0),
            t.txt_sec,
        );
        ui.painter().text(
            Pos2::new(cx, col_y + 14.0),
            Align2::CENTER_TOP,
            val,
            FontId::proportional(13.0),
            t.txt_pri,
        );
    }

    // Raw
    let raw_x = inner_x + meta_col_w;
    ui.painter().text(
        Pos2::new(raw_x + raw_col_w * 0.5, col_y),
        Align2::CENTER_TOP,
        "Raw",
        FontId::proportional(11.0),
        t.txt_sec,
    );
    let raw_str = fmt_thousands(attr.raw);
    ui.painter().text(
        Pos2::new(raw_x + raw_col_w * 0.5, col_y + 14.0),
        Align2::CENTER_TOP,
        &raw_str,
        FontId::proportional(13.0),
        status_color,
    );

    // Tooltip on hover — use interact() not allocate_rect() so the cursor doesn't advance.
    let tip = attr_description(attr.id);
    if !tip.is_empty() {
        let hover_resp = ui.interact(rect, Id::new(format!("health_attr_tip_{}", attr.id)), Sense::hover());
        if hover_resp.hovered() {
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                show_tooltip_text(
                    ui.ctx(),
                    Id::new(format!("health_attr_tip_tt_{}", attr.id)),
                    pos,
                    t,
                    tip,
                );
            }
        }
    }
}

fn draw_attributes(
    ui: &mut egui::Ui,
    t: &Theme,
    dark: bool,
    content_x: f32,
    margin: f32,
    section_w: f32,
    attrs: &[AtaAttribute],
) {
    // ── Section label row ─────────────────────────────────────────────────────
    let label_top = ui.cursor().min.y;
    let label_h = 24.0_f32;
    let label_rect = Rect::from_min_size(
        Pos2::new(content_x + margin, label_top),
        Vec2::new(section_w, label_h),
    );

    // Paint label text
    ui.painter().text(
        Pos2::new(label_rect.min.x, label_rect.center().y),
        Align2::LEFT_CENTER,
        "Attributes",
        FontId::new(13.0, egui::FontFamily::Name("InterBold".into())),
        t.txt_sec,
    );

    // Legend (?) icon — interact() does not advance the cursor
    let attr_lbl_w = ui.ctx().fonts(|f| {
        f.layout_no_wrap(
            "Attributes".to_owned(),
            FontId::new(13.0, egui::FontFamily::Name("InterBold".into())),
            t.txt_sec,
        ).rect.width()
    });
    let icon_rect = Rect::from_min_size(
        Pos2::new(label_rect.min.x + attr_lbl_w + 6.0, label_rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    let icon_resp = ui.interact(icon_rect, Id::new("health_attr_legend_icon"), Sense::hover());
    ui.painter().text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        format!("{}", ICON_QUESTION),
        FontId::proportional(15.0),
        t.accent,
    );
    if icon_resp.hovered() {
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            show_tooltip_text(
                ui.ctx(), Id::new("health_attr_legend_tt"), pos, t,
                "Cur = current normalized value (100 = best)\nWst = worst recorded value\nThr = failure threshold\nRaw = actual raw sensor count",
            );
        }
    }

    // Advance past the label row exactly once (add_space avoids item_spacing appended by advance_cursor_after_rect)
    ui.add_space(label_h + 6.0);

    // ── 2-column card grid ────────────────────────────────────────────────────
    let card_h   = 70.0_f32;
    let card_gap =  8.0_f32;
    let col_gap  =  8.0_f32;
    let card_w   = (section_w - col_gap) / 2.0;

    // Iterate in pairs (chunks of 2) so we control the cursor exactly once per row.
    for (row_idx, chunk) in attrs.chunks(2).enumerate() {
        let row_y = ui.cursor().min.y;

        // Paint all cards in this row without touching the cursor
        for (col, attr) in chunk.iter().enumerate() {
            let x = content_x + margin + col as f32 * (card_w + col_gap);
            let card = Rect::from_min_size(Pos2::new(x, row_y), Vec2::new(card_w, card_h));
            draw_attribute_card(ui, t, dark, card, attr);
        }

        // Advance the cursor past this full row using add_space
        // (avoids item_spacing being appended by advance_cursor_after_rect)
        ui.add_space(card_h);

        // Gap between rows (skip after the last row)
        let total_rows = attrs.len().div_ceil(2);
        if row_idx + 1 < total_rows {
            ui.add_space(card_gap);
        }
    }
}
// ── HTML report ───────────────────────────────────────────────────────────────

/// Standard-alphabet base64 encoder (no padding omitted). Kept local to avoid a
/// new crate dependency; used to inline the performance-chart PNG as a data URI.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Builds the Drive Health report HTML. When `chart_png` is `Some`, the PNG is
/// inlined above the report header at content width (rounded corners) with a
/// click-to-open zoom/pan lightbox; the Drive Health page itself passes `None`.
fn build_report_html(drive: &DetectedDrive, report: &SmartReport, chart_png: Option<&[u8]>) -> String {
    let now = chrono::Local::now();
    let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let capacity = crate::detected_drive::DetectedDrive::format_size(drive.size_bytes);

    // Optional embedded performance chart: a clickable thumbnail above the header
    // plus a full-screen zoom/pan lightbox overlay + script at the end of <body>.
    let (chart_section, lightbox_section) = match chart_png {
        Some(png) => {
            let uri = format!("data:image/png;base64,{}", base64_encode(png));
            let figure = format!(
                r#"<figure class="chart-embed"><img id="chart-thumb" src="{uri}" alt="Performance chart" title="Click to zoom"></figure>"#
            );
            let overlay = r#"<div id="lightbox">
<div class="lb-controls">
<button id="lb-out" title="Zoom out">&minus;</button>
<button id="lb-in" title="Zoom in">&plus;</button>
<button id="lb-close" title="Close">&times;</button>
</div>
<img id="lightbox-img" alt="Performance chart (zoomed)">
<div class="lb-hint">Scroll to zoom · drag to pan · Esc to close</div>
</div>
<script>
(function(){
  var lb=document.getElementById('lightbox'),img=document.getElementById('lightbox-img'),thumb=document.getElementById('chart-thumb');
  if(!lb||!img||!thumb)return;
  var scale=1,tx=0,ty=0,min=0.1,max=8,dragging=false,sx=0,sy=0,stx=0,sty=0;
  function apply(){img.style.transform='translate('+tx+'px,'+ty+'px) scale('+scale+')';}
  function fit(){var iw=img.naturalWidth,ih=img.naturalHeight,vw=window.innerWidth,vh=window.innerHeight;scale=Math.min(vw/iw,vh/ih)*0.95;min=scale*0.5;tx=(vw-iw*scale)/2;ty=(vh-ih*scale)/2;apply();}
  function show(){lb.classList.add('open');fit();}
  function open(){if(!img.src){img.src=thumb.src;img.onload=show;}else{show();}}
  function close(){lb.classList.remove('open');}
  function zoomAt(cx,cy,f){var ns=Math.min(max,Math.max(min,scale*f)),k=ns/scale;tx=cx-(cx-tx)*k;ty=cy-(cy-ty)*k;scale=ns;apply();}
  thumb.addEventListener('click',open);
  document.getElementById('lb-close').addEventListener('click',close);
  document.getElementById('lb-in').addEventListener('click',function(){zoomAt(window.innerWidth/2,window.innerHeight/2,1.25);});
  document.getElementById('lb-out').addEventListener('click',function(){zoomAt(window.innerWidth/2,window.innerHeight/2,0.8);});
  lb.addEventListener('click',function(e){if(e.target===lb)close();});
  document.addEventListener('keydown',function(e){if(e.key==='Escape')close();});
  lb.addEventListener('wheel',function(e){e.preventDefault();zoomAt(e.clientX,e.clientY,e.deltaY<0?1.15:0.87);},{passive:false});
  img.addEventListener('mousedown',function(e){e.preventDefault();dragging=true;sx=e.clientX;sy=e.clientY;stx=tx;sty=ty;lb.style.cursor='grabbing';});
  window.addEventListener('mousemove',function(e){if(!dragging)return;tx=stx+(e.clientX-sx);ty=sty+(e.clientY-sy);apply();});
  window.addEventListener('mouseup',function(){dragging=false;lb.style.cursor='grab';});
  window.addEventListener('resize',function(){if(lb.classList.contains('open'))fit();});
})();
</script>"#.to_string();
            (figure, overlay)
        }
        None => (String::new(), String::new()),
    };

    let body = match report {
        SmartReport::Ata(data) => {
            let temp = data.temperature_c.map(|c| format!("{c}°C")).unwrap_or_else(|| "N/A".to_string());
            let poh  = data.power_on_hours.map(fmt_thousands).unwrap_or_else(|| "N/A".to_string());
            let pc   = data.power_cycles.map(fmt_thousands).unwrap_or_else(|| "N/A".to_string());

            let vitals = format!(
                r#"<div class="vitals"><p><strong>Temperature</strong><span>{temp}</span></p><p><strong>Power-On Hours</strong><span>{poh}</span></p><p><strong>Power Cycles</strong><span>{pc}</span></p></div>"#
            );

            let attrs: String = data.attributes.iter().map(|a| {
                let (cls, status_str) = match a.status {
                    AttrStatus::Good    => ("good", "GOOD"),
                    AttrStatus::Warning => ("warn", "WARN"),
                    AttrStatus::Failed  => ("fail", "FAIL"),
                    AttrStatus::Info    => ("info", "INFO"),
                };
                format!(
                    "<tr class=\"{cls}\">\n<td>{id:02X}</td><td>{name}</td><td>{st}</td>\n<td>{cur}</td><td>{wst}</td><td>{thr}</td><td>{raw}</td>\n</tr>",
                    id = a.id, name = a.name, st = status_str,
                    cur = a.current, wst = a.worst, thr = a.threshold,
                    raw = fmt_thousands(a.raw),
                )
            }).collect::<Vec<_>>().join("\n");

            format!(
                r#"{vitals}<h2>Attributes</h2>
<div class="tbl"><table class="attrs">
<thead><tr>
<th>ID</th><th>Name</th><th>Status</th>
<th>Current</th><th>Worst</th><th>Threshold</th><th>Raw</th>
</tr></thead><tbody>{attrs}</tbody></table></div>"#
            )
        }
        SmartReport::Nvme(d) => {
            let dw_gib = d.data_units_written as f64 * 512.0 / (1024.0 * 1024.0);
            let dw_label = if dw_gib >= 1024.0 { format!("{:.2} TiB", dw_gib / 1024.0) } else { format!("{:.1} GiB", dw_gib) };
            let warn_str = if d.critical_warning == 0 { "None".to_string() } else { format!("0x{:02X}", d.critical_warning) };
            format!(
                r#"<div class="vitals">
<p><strong>Temperature</strong><span>{}°C</span></p>
<p><strong>Percentage Used</strong><span>{}%</span></p>
<p><strong>Available Spare</strong><span>{}% (threshold {}%)</span></p>
<p><strong>Power-On Hours</strong><span>{}</span></p>
<p><strong>Power Cycles</strong><span>{}</span></p>
<p><strong>Data Written</strong><span>{}</span></p>
<p><strong>Unsafe Shutdowns</strong><span>{}</span></p>
<p><strong>Media Errors</strong><span>{}</span></p>
<p><strong>Critical Warning</strong><span>{}</span></p>
</div>"#,
                d.temperature_c, d.percentage_used,
                d.available_spare_pct, d.available_spare_threshold,
                fmt_thousands(d.power_on_hours),
                fmt_thousands(d.power_cycles),
                dw_label,
                fmt_thousands(d.unsafe_shutdowns),
                fmt_thousands(d.media_errors),
                warn_str,
            )
        }
        SmartReport::Ufs(d) => {
            let pre_eol = match d.pre_eol_info {
                0x01 => "Normal",
                0x02 => "Warning",
                0x03 => "Urgent",
                _ => "Not defined",
            };
            format!(
                r#"<div class="vitals">
<p><strong>Pre-EOL Status</strong><span>{}</span></p>
<p><strong>Life Used (Type A)</strong><span>{}</span></p>
<p><strong>Life Used (Type B)</strong><span>{}</span></p>
</div>"#,
                pre_eol,
                ufs_lifetime_label(d.life_time_est_a),
                ufs_lifetime_label(d.life_time_est_b),
            )
        }
        SmartReport::Unavailable { reason } => {
            format!("<p class=\"fail\">SMART data unavailable: {reason}</p>")
        }
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Health Data Report — {model}</title>
<style>
*, *::before, *::after {{ box-sizing: border-box; }}
body {{
  font-family: system-ui, -apple-system, sans-serif;
  max-width: 920px; margin: 40px auto; padding: 0 24px;
  background: #1e1e1e; color: #e0e0e0;
  line-height: 1.5;
}}
h1 {{ font-size: 1.5rem; margin: 0 0 4px; color: #fff; }}
h2 {{ font-size: 1rem; margin: 28px 0 10px; color: #aaa;
      text-transform: uppercase; letter-spacing: .06em; font-weight: 600; }}
.meta {{ color: #777; font-size: 0.85rem; margin-bottom: 28px; }}
.tbl {{ border-radius: 8px; overflow: hidden; margin-bottom: 20px; }}
table {{ border-collapse: collapse; width: 100%; font-size: 0.9rem; }}
.tbl table {{ margin: 0; }}
th, td {{ text-align: left; padding: 7px 12px; border: 1px solid #333; }}
.chart-embed {{ margin: 0 0 28px; }}
.chart-embed img {{ width: 100%; max-width: 100%; height: auto; display: block;
  border: 1px solid #333; border-radius: 8px; cursor: zoom-in; }}
#lightbox {{ position: fixed; inset: 0; background: rgba(0,0,0,.9); display: none;
  z-index: 1000; cursor: grab; overflow: hidden; }}
#lightbox.open {{ display: block; }}
#lightbox img {{ position: absolute; top: 0; left: 0; transform-origin: 0 0;
  user-select: none; -webkit-user-drag: none; will-change: transform; }}
.lb-controls {{ position: fixed; top: 16px; right: 16px; display: flex; gap: 8px; z-index: 1001; }}
.lb-controls button {{ width: 36px; height: 36px; font-size: 18px; line-height: 1;
  border: 1px solid #555; background: #2a2a2a; color: #fff; border-radius: 6px; cursor: pointer; }}
.lb-controls button:hover {{ background: #3a3a3a; }}
.lb-hint {{ position: fixed; bottom: 16px; left: 50%; transform: translateX(-50%);
  color: #aaa; font-size: 0.8rem; z-index: 1001; }}
thead th {{
  background: #2a2a2a; color: #aaa;
  font-size: 0.75rem; text-transform: uppercase; letter-spacing: .05em;
  font-weight: 600;
}}
table.info th {{ width: 180px; background: #252525; color: #888; font-weight: 500; }}
table.info td {{ background: #222; }}
table.attrs tbody tr {{ background: #222; }}
table.attrs tbody tr:hover {{ background: #272727; }}
.good td:nth-child(3) {{ color: #66bb6a; font-weight: 600; }}
.warn td:nth-child(3) {{ color: #ffa726; font-weight: 600; }}
.fail td:nth-child(3) {{ color: #ef5350; font-weight: 600; }}
.info td:nth-child(3) {{ color: #78909c; }}
.fail {{ background: #2a1a1a !important; }}
.warn {{ background: #2a2010 !important; }}
p.fail {{ color: #ef5350; }}
.vitals p {{ margin: 4px 0; }}
.vitals strong {{ color: #bbb; font-weight: 500; }}
.vitals span {{ color: #fff; font-weight: 600; margin-left: 6px; }}
</style>
</head>
<body>
{chart_section}<h1>Health Data Report — {model}</h1>
<p class="meta">Generated {ts}</p>
<div class="tbl"><table class="info">
<tr><th>Model</th><td>{model}</td></tr>
<tr><th>Serial</th><td>{serial}</td></tr>
<tr><th>Bus</th><td>{bus}</td></tr>
<tr><th>Media</th><td>{media}</td></tr>
<tr><th>Capacity</th><td>{capacity}</td></tr>
</table></div>
{body}
{lightbox_section}
</body>
</html>"#,
        model    = drive.model,
        serial   = drive.serial,
        bus      = drive.bus.label(),
        media    = drive.media.label(),
        capacity = capacity,
    )
}

/// Builds the health report HTML with the performance-chart PNG inlined above the
/// header (rounded corners + zoom/pan lightbox). Used by the chart export path.
pub fn build_chart_report_html(
    drive: &DetectedDrive,
    report: &SmartReport,
    chart_png: &[u8],
) -> String {
    build_report_html(drive, report, Some(chart_png))
}

pub fn save_smart_report(drive: &DetectedDrive, report: &SmartReport) {
    let html = build_report_html(drive, report, None);
    let filename = format!("SMART-{}.html", drive.safe_filename_stem());

    std::thread::spawn(move || {
        let path = pollster::block_on(
            rfd::AsyncFileDialog::new()
                .set_file_name(&filename)
                .add_filter("HTML file", &["html"])
                .save_file(),
        );
        if let Some(handle) = path {
            let _ = std::fs::write(handle.path(), html.as_bytes());
        }
    });
}

// ── Main page drawing entry point ─────────────────────────────────────────────

impl DiskoriaApp {
    pub fn draw_health_status_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        dark: bool,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        let section_w = content_w - margin * 2.0;

        // ── Page title row (title left, Export Log right) ─────────────────────
        {
            // Height 28 → title vertically centered at +14, matching the page
            // titles on the Sector/Write/Benchmark pages (drawn at cursor+14).
            // The 34px Export Log button overflows this row symmetrically, which
            // is harmless and keeps it aligned with the title. (About uses its
            // own top-aligned header; this page tracks the bare-title test pages.)
            const HEADER_ROW_H: f32 = 28.0;
            let row_top = ui.cursor().min.y;
            let row_rect = Rect::from_min_size(
                Pos2::new(content_x + margin, row_top),
                Vec2::new(section_w, HEADER_ROW_H),
            );

            // Title
            ui.painter().text(
                Pos2::new(row_rect.min.x, row_rect.center().y),
                Align2::LEFT_CENTER,
                "Drive Health",
                egui::FontId::new(28.0, egui::FontFamily::Proportional),
                t.txt_pri,
            );

            // Export Log button — only enabled when a non-Unavailable report is loaded
            let has_report = self.health_report.as_ref()
                .map(|r| !matches!(r, SmartReport::Unavailable { .. }))
                .unwrap_or(false);

            // We need to allocate the UI so the button sits right-aligned.
            // Grab a copy of what we need before the mutable borrow below.
            let export_drive = if has_report && !self.drives.is_empty() {
                let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
                Some(self.drives[sel].clone())
            } else {
                None
            };
            let export_report = if has_report { self.health_report.clone() } else { None };

            let export_focused = has_report && self.health_focus == Some(2);
            ui.allocate_new_ui(
                UiBuilder::new()
                    .max_rect(row_rect)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| {
                    let btn = small_browse_style_button(
                        ui, t,
                        Id::new("diskoria_health_export"),
                        ICON_FILETYPE_HTML,
                        "Export Log",
                        has_report,
                    );
                    let kb = export_focused && ui.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
                    });
                    if btn.clicked() || kb {
                        if let (Some(drv), Some(rep)) = (export_drive, export_report) {
                            save_smart_report(&drv, &rep);
                        }
                    }
                    if export_focused {
                        ui.painter().rect_stroke(
                            btn.rect.expand(2.0),
                            4.0,
                            Stroke::new(2.0, t.accent),
                            StrokeKind::Outside,
                        );
                    }
                    crate::focus::scroll_to_focused(
                        &mut self.pending_scroll_rect,
                        btn.rect,
                        export_focused,
                        self.scroll_focus_frames > 0,
                    );
                },
            );

            ui.advance_cursor_after_rect(row_rect);
            // Bring the total header block to ~32px (title at +14, subtitle at
            // +32) so it lines up with the Sector/Write/Benchmark pages.
            ui.add_space(4.0);
        }

        // ── Subtitle + refresh button ─────────────────────────────────────────
        ui.horizontal(|ui| {
            let pad = (content_x + margin) - ui.min_rect().left();
            if pad > 0.0 { ui.add_space(pad); }

            ui.label(
                RichText::new("Monitor drive health by viewing self-reported diagnostics.")
                    .size(14.0)
                    .color(t.txt_sec),
            );
        });
        ui.add_space(20.0);

        // Error state (shown above the selector row).
        if let Some(ref err) = self.drives_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(RichText::new(format!("Could not enumerate drives: {err}")).size(13.0).color(Color32::from_rgb(231, 76, 60)));
            });
            ui.add_space(12.0);
        }

        // ── Refresh button + drive picker (one row) ───────────────────────────
        draw_drive_picker(self, ui, t, ctx, content_x, margin, section_w);
        ui.add_space(16.0);

        if !self.drives_loading && self.drives.is_empty() && self.drives_error.is_none() {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(RichText::new("No physical disks found.").size(14.0).color(t.txt_sec));
            });
            return;
        }

        if self.drives.is_empty() {
            return;
        }

        // Trigger poll if we don't have a report yet for this drive
        self.spawn_health_poll_if_needed(ctx);

        // Poll incoming result
        self.poll_health_report(ctx);

        // ── Poll spinner ──────────────────────────────────────────────────────
        if self.health_poll_running && self.health_report.is_none() {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.spinner();
                ui.label(RichText::new("Reading drive health…").color(t.txt_sec));
            });
            ui.add_space(8.0);
            return;
        }

        let report = match &self.health_report {
            Some(r) => r.clone(),
            None => return,
        };

        match &report {
            SmartReport::Ata(data) => {
                let data = data.clone();
                draw_ata_vitals(ui, t, content_x, margin, section_w, &data);
                ui.add_space(16.0);

                if !data.attributes.is_empty() {
                    draw_attributes(ui, t, dark, content_x, margin, section_w, &data.attributes);
                    ui.add_space(16.0);
                }
            }
            SmartReport::Nvme(data) => {
                let data = data.clone();
                draw_nvme_vitals(ui, t, content_x, margin, section_w, &data);
                ui.add_space(16.0);
            }
            SmartReport::Ufs(data) => {
                let data = data.clone();
                draw_ufs_vitals(ui, t, content_x, margin, section_w, &data);
                ui.add_space(16.0);
            }
            SmartReport::Unavailable { reason } => {
                let reason = reason.clone();
                draw_unavailable(ui, t, content_x, margin, section_w, &reason);
                ui.add_space(16.0);
            }
        }

        // ── Pro-Monitoring: temperature history chart ─────────────────────────
        #[cfg(windows)]
        if self.shared.pro_edition {
            let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
            if !self.drives.is_empty() {
                let serial = self.drives[sel].serial.clone();
                draw_temperature_history(
                    ui, t, ctx, content_x, margin, section_w,
                    &serial,
                    &self.temp_history,
                    &mut self.health_chart_range,
                    self.health_focus,
                    &mut self.pending_scroll_rect,
                    self.scroll_focus_frames,
                );
            }
        }
    }

    // ── Poll lifecycle ────────────────────────────────────────────────────────

    fn spawn_health_poll_if_needed(&mut self, ctx: &egui::Context) {
        let poll_interval = std::time::Duration::from_secs(5);

        if self.health_poll_running {
            return;
        }
        if self.drives.is_empty() {
            return;
        }
        let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
        // The current report belongs to a different drive (the shared selection
        // changed, possibly on another page) → drop it and poll fresh.
        if self.health_report_drive != Some(sel) {
            self.health_report = None;
            self.health_last_poll = None;
        }

        // If we already have a report for this drive, wait the interval.
        if self.health_report.is_some() {
            if let Some(last) = self.health_last_poll {
                if last.elapsed() < poll_interval {
                    // Wake ourselves up when the interval expires.
                    let remaining = poll_interval.saturating_sub(last.elapsed());
                    ctx.request_repaint_after(remaining);
                    return;
                }
            }
        }

        let drive = &self.drives[sel];
        let device_path = drive.device_id.clone();
        let bus = drive.bus;

        self.health_report_drive = Some(sel);
        self.health_poll_running = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.health_poll_rx = Some(rx);

        log::info!(target: "diskoria", "health: polling SMART data for {device_path}");

        std::thread::spawn(move || {
            let report = crate::smart_reader::query_smart_detail(&device_path, bus);
            let _ = tx.send(report);
        });

        ctx.request_repaint();
    }

    fn poll_health_report(&mut self, ctx: &egui::Context) {
        let rx = match self.health_poll_rx.take() {
            Some(r) => r,
            None => return,
        };
        match rx.try_recv() {
            Ok(report) => {
                use crate::smart_reader::SmartReport;
                match &report {
                    SmartReport::Ata(d) => log::info!(
                        target: "diskoria",
                        "health: ATA report received — temp={temp}°C poh={poh}h attrs={n}",
                        temp = d.temperature_c.map(|c| c.to_string()).unwrap_or_else(|| "N/A".into()),
                        poh  = d.power_on_hours.unwrap_or(0),
                        n    = d.attributes.len(),
                    ),
                    SmartReport::Nvme(d) => log::info!(
                        target: "diskoria",
                        "health: NVMe report received — temp={}°C wear={}% spare={}%",
                        d.temperature_c, d.percentage_used, d.available_spare_pct,
                    ),
                    SmartReport::Ufs(d) => log::info!(
                        target: "diskoria",
                        "health: UFS report received — pre_eol={:#04x} lt_a={:#04x} lt_b={:#04x}",
                        d.pre_eol_info, d.life_time_est_a, d.life_time_est_b,
                    ),
                    SmartReport::Unavailable { reason } => log::warn!(
                        target: "diskoria",
                        "health: SMART unavailable — {reason}",
                    ),
                }
                self.health_report = Some(report);
                self.health_poll_running = false;
                self.health_last_poll = Some(std::time::Instant::now());
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.health_poll_rx = Some(rx);
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.health_poll_running = false;
            }
        }
    }
}

// ── Pro-Monitoring: temperature history chart ─────────────────────────────────

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn draw_temperature_history(
    ui: &mut egui::Ui,
    t: &Theme,
    _ctx: &egui::Context,
    content_x: f32,
    margin: f32,
    section_w: f32,
    serial: &str,
    history: &std::collections::HashMap<String, Vec<[f64; 2]>>,
    range: &mut usize,
    health_focus: Option<usize>,
    pending_scroll_rect: &mut Option<egui::Rect>,
    scroll_focus_frames: u8,
) {
    // range: 0=1h 1=6h 2=12h 3=24h 4=7d
    const WINDOWS: [f64; 5] = [3_600.0, 21_600.0, 43_200.0, 86_400.0, 604_800.0];
    const TAB_LABELS: [&str; 5] = ["1 h", "6 h", "12 h", "24 h", "7 d"];
    *range = (*range).min(4);

    let left = content_x + margin;
    let now_unix = chrono::Utc::now().timestamp() as f64;
    let window_secs = WINDOWS[*range];
    let cutoff = now_unix - window_secs;

    // Section heading
    ui.painter().text(
        Pos2::new(left, ui.cursor().min.y),
        Align2::LEFT_TOP,
        "Temperature History",
        FontId::new(13.0, egui::FontFamily::Proportional),
        t.txt_sec,
    );
    ui.add_space(20.0);

    // Range tab buttons — slots 3..7 in the health page focus order
    const RANGE_SLOT_START: usize = 3;
    ui.horizontal(|ui| {
        let pad = left - ui.min_rect().left();
        if pad > 0.0 { ui.add_space(pad); }
        for (i, label) in TAB_LABELS.iter().enumerate() {
            let selected = *range == i;
            let focused = health_focus == Some(RANGE_SLOT_START + i);
            let (bg, fg) = if selected {
                (t.accent, t.txt_on_accent)
            } else {
                (t.bg_sec, t.txt_sec)
            };
            let btn_resp = ui.add(
                egui::Button::new(RichText::new(*label).color(fg).size(12.0))
                    .fill(bg)
                    .stroke(Stroke::new(1.0, if selected { t.accent } else { t.border }))
                    .min_size(Vec2::new(52.0, 24.0)),
            );
            let kb = focused && ui.input_mut(|inp| {
                inp.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || inp.consume_key(egui::Modifiers::NONE, egui::Key::Space)
            });
            if btn_resp.clicked() || kb { *range = i; }
            if focused {
                ui.painter().rect_stroke(
                    btn_resp.rect.expand(2.0),
                    4.0,
                    Stroke::new(2.0, t.accent),
                    StrokeKind::Outside,
                );
            }
            crate::focus::scroll_to_focused(
                pending_scroll_rect,
                btn_resp.rect,
                focused,
                scroll_focus_frames > 0,
            );
        }
    });
    ui.add_space(8.0);

    // Filter master map to the selected window.
    let all_pts = history.get(serial);
    let filtered: Vec<[f64; 2]> = all_pts
        .map(|v| v.iter().filter(|p| p[0] >= cutoff).cloned().collect())
        .unwrap_or_default();

    let chart_h = 140.0;
    let chart_rect = Rect::from_min_size(
        Pos2::new(left, ui.cursor().min.y),
        Vec2::new(section_w, chart_h),
    );
    ui.allocate_rect(chart_rect, Sense::hover());

    if filtered.is_empty() {
        let msg = if all_pts.map(|v| !v.is_empty()).unwrap_or(false) {
            "No data in this time window"
        } else {
            "No history data yet — collecting on next monitor cycle"
        };
        ui.painter().text(
            chart_rect.center(),
            Align2::CENTER_CENTER,
            msg,
            FontId::new(12.0, egui::FontFamily::Proportional),
            t.txt_sec,
        );
        ui.add_space(16.0);
        return;
    }

    let temps: Vec<f64> = filtered.iter().map(|p| p[1]).collect();
    let y_min = temps.iter().cloned().fold(f64::INFINITY, f64::min).max(0.0) - 5.0;
    let y_max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max) + 10.0;

    // Split into continuous segments: a gap of >15 min means the app was closed.
    // Each segment is rendered as a separate Line so closed-periods appear as
    // blank space rather than a flatline connecting the last and first points.
    const GAP_SECS: f64 = 900.0;
    let mut segments: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut seg: Vec<[f64; 2]> = Vec::new();
    for &pt in &filtered {
        if let Some(&last) = seg.last() {
            if pt[0] - last[0] > GAP_SECS {
                segments.push(std::mem::take(&mut seg));
            }
        }
        seg.push(pt);
    }
    if !seg.is_empty() { segments.push(seg); }

    let accent = t.accent;
    let now = now_unix;
    let range_idx = *range;

    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chart_rect), |ui| {
        Plot::new(format!("temp_history_{serial}_{range_idx}"))
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .include_x(cutoff)
            .include_x(now_unix)
            .include_y(y_min)
            .include_y(y_max)
            .y_axis_label("°C")
            .x_axis_formatter(move |mark, _range| {
                let diff = (now - mark.value).max(0.0);
                if range_idx <= 3 {
                    let mins = (diff / 60.0).round() as i64;
                    let hrs = mins / 60;
                    let m = mins % 60;
                    if hrs == 0 && m == 0 { "now".into() }
                    else if hrs == 0 { format!("{}m", m) }
                    else if m == 0 { format!("{}h", hrs) }
                    else { format!("{}h{}m", hrs, m) }
                } else {
                    let d = (diff / 86400.0).round() as i64;
                    if d == 0 { "now".into() } else { format!("{}d", d) }
                }
            })
            .label_formatter(move |_name, point| {
                let diff = (now - point.x).max(0.0);
                let time_str = if range_idx <= 3 {
                    let mins = diff as i64 / 60;
                    let hrs = mins / 60;
                    let m = mins % 60;
                    if mins == 0 { "just now".into() }
                    else if hrs == 0 { format!("{}m ago", m) }
                    else if m == 0 { format!("{}h ago", hrs) }
                    else { format!("{}h {}m ago", hrs, m) }
                } else {
                    let d = diff as i64 / 86400;
                    if d == 0 { "today".into() } else { format!("{}d ago", d) }
                };
                format!("{}°C — {}", point.y as i32, time_str)
            })
            .show(ui, |plot_ui| {
                for seg in segments {
                    plot_ui.line(
                        Line::new(PlotPoints::from(seg))
                            .color(accent)
                            .width(2.0),
                    );
                }
            });
    });

    ui.add_space(16.0);
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encodes_high_bytes() {
        assert_eq!(base64_encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }
}
