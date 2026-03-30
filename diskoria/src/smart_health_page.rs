//! Health Status page — raw SMART / NVMe diagnostics for any drive the user selects.
//!
//! This module owns the per-page state fields on `DiskoriaApp` that carry the selected
//! drive index and the polled `SmartReport`.  Page drawing is done by free functions
//! called from `DiskoriaApp::draw_health_status_page`.

use egui::{
    Align2, Color32, FontId, Id, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, Vec2,
};

use crate::detected_drive::{BusKind, DetectedDrive, MediaKind};
use crate::smart_reader::{AtaAttribute, AtaSmartData, AttrStatus, NvmeHealthData, SmartReport};
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
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

fn chip_width(ctx: &egui::Context, label: &str) -> f32 {
    let galley = ctx.fonts(|f| f.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), Color32::WHITE));
    galley.rect.width() + 20.0
}

fn chip_colors_media(m: MediaKind) -> (Color32, Color32) {
    match m {
        MediaKind::Hdd    => (Color32::from_rgb(52, 73, 94),   Color32::WHITE),
        MediaKind::Ssd    => (Color32::from_rgb(41, 128, 185), Color32::WHITE),
        MediaKind::SdCard => (Color32::from_rgb(39, 174, 96),  Color32::WHITE),
        MediaKind::Flash  => (Color32::from_rgb(155, 89, 182), Color32::WHITE),
        MediaKind::EMmc   => (Color32::from_rgb(26, 188, 156), Color32::WHITE),
        MediaKind::Unknown => (Color32::from_rgb(127, 140, 141), Color32::WHITE),
    }
}

fn chip_colors_bus(b: BusKind) -> (Color32, Color32) {
    match b {
        BusKind::Nvme => (Color32::from_rgb(142, 68, 173),  Color32::WHITE),
        BusKind::Sata => (Color32::from_rgb(22, 160, 133),  Color32::WHITE),
        BusKind::Usb  => (Color32::from_rgb(230, 126, 34),  Color32::WHITE),
        BusKind::Ufs  => (Color32::from_rgb(52, 152, 219),  Color32::WHITE),
    }
}

fn chip_pill(painter: &egui::Painter, rect: Rect, label: &str, bg: Color32, fg: Color32) {
    painter.rect_filled(rect, 6.0, bg);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), fg);
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
    content_x: f32,
    margin: f32,
    section_w: f32,
) {
    let row_h = 34.0_f32;
    let chip_h = 26.0_f32;
    let gap_chips = 8.0_f32;
    let gap_combo_chips = 12.0_f32;

    let drives = &app.drives;
    let sel = app.health_selected_drive.min(drives.len().saturating_sub(1));
    let options: Vec<String> = drives.iter().map(|d| d.summary.clone()).collect();

    let y_row = ui.cursor().min.y;

    let combo_w = if !drives.is_empty() {
        let d = &drives[sel];
        let mw = chip_width(ui.ctx(), d.media.label());
        let bw = chip_width(ui.ctx(), d.bus.label());
        (section_w - gap_combo_chips - mw - gap_chips - bw).max(120.0)
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

    let prev_sel = app.health_selected_drive;

    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(combo_inner)
            .style(std::sync::Arc::new(combo_style)),
        |ui| {
            egui::ComboBox::from_id_salt("diskoria_health_drive_combo")
                .selected_text(options.get(sel).map(|s: &String| s.as_str()).unwrap_or("—"))
                .width(combo_inner.width())
                .truncate()
                .show_ui(ui, |ui| {
                    ui.style_mut().visuals.override_text_color = Some(t.txt_pri);
                    for (idx, label) in options.iter().enumerate() {
                        ui.selectable_value(&mut app.health_selected_drive, idx, label);
                    }
                });
        },
    );

    // If selection changed, clear the old report and timer so we re-poll immediately
    if app.health_selected_drive != prev_sel {
        app.health_report = None;
        app.health_poll_running = false;
        app.health_last_poll = None;
    }

    if !app.drives.is_empty() {
        let d = &app.drives[sel];
        let chip_y = y_row + (row_h - chip_h) * 0.5;
        let mut x = combo_rect.right() + gap_combo_chips;

        let (m_bg, m_fg) = chip_colors_media(d.media);
        let mw = chip_width(ui.ctx(), d.media.label());
        let mr = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(mw, chip_h));
        chip_pill(ui.painter(), mr, d.media.label(), m_bg, m_fg);
        x += mw + gap_chips;

        let (b_bg, b_fg) = chip_colors_bus(d.bus);
        let bw = chip_width(ui.ctx(), d.bus.label());
        let br = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(bw, chip_h));
        chip_pill(ui.painter(), br, d.bus.label(), b_bg, b_fg);
    }

    let row_rect = Rect::from_min_max(
        Pos2::new(content_x + margin, y_row),
        Pos2::new(content_x + margin + section_w, y_row + row_h),
    );
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

    // Attribute ID + name
    let id_str = format!("{:02X}", attr.id);
    ui.painter().text(
        Pos2::new(inner_x, rect.min.y + 10.0),
        Align2::LEFT_TOP,
        &id_str,
        FontId::proportional(11.0),
        t.txt_sec,
    );

    // Name (may be truncated) — right side of same row
    let name_galley = ui.ctx().fonts(|f| {
        f.layout(
            attr.name.to_owned(),
            FontId::proportional(13.0),
            t.txt_pri,
            inner_w - 30.0,
        )
    });
    ui.painter().add(egui::Shape::galley(
        Pos2::new(inner_x + 30.0, rect.min.y + 8.0),
        name_galley,
        t.txt_pri,
    ));

    // C7 cable warning icon — use interact() so cursor doesn't advance
    if attr.id == 0xC7 && attr.raw > 0 {
        let icon_id = Id::new("health_c7_warn_icon");
        let icon_rect = Rect::from_min_size(
            Pos2::new(rect.max.x - 20.0, rect.min.y + 6.0),
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
        let total_rows = (attrs.len() + 1) / 2;
        if row_idx + 1 < total_rows {
            ui.add_space(card_gap);
        }
    }
}

// ── Self-test progress modal ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfTestModalAction {
    Abort,
    Dismiss,
}

/// Draws a centered modal overlay showing self-test progress.
/// Returns `Some(action)` when the user clicks Abort or Dismiss.
fn draw_self_test_modal(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    t: &Theme,
    kind: Option<crate::smart_reader::SelfTestKind>,
    data: &AtaSmartData,
) -> Option<SelfTestModalAction> {
    use crate::smart_reader::SelfTestStatus;

    let kind_label = kind.map(|k| k.label()).unwrap_or("Unknown");
    let title = format!("{kind_label} Self-Test");

    // Determine progress and status from the most recent poll
    let (status_text, status_color, progress_pct, is_running, is_done) = match data.self_test.as_ref() {
        None => ("Waiting for first update…".to_string(), t.txt_sec, 0.0_f32, true, false),
        Some(r) => match &r.status {
            SelfTestStatus::InProgress { pct_remaining } => {
                let done = (100u8.saturating_sub(*pct_remaining)) as f32 / 100.0;
                (
                    format!("{}% complete ({pct_remaining}% remaining)", 100 - pct_remaining),
                    t.txt_sec,
                    done,
                    true,
                    false,
                )
            }
            SelfTestStatus::Passed => (
                "Test passed successfully.".to_string(),
                Color32::from_rgb(39, 174, 96),
                1.0,
                false,
                true,
            ),
            SelfTestStatus::Failed { reason } => (
                format!("Test FAILED: {reason}"),
                Color32::from_rgb(231, 76, 60),
                1.0,
                false,
                true,
            ),
            SelfTestStatus::Aborted => (
                "Test was aborted.".to_string(),
                t.txt_sec,
                0.0,
                false,
                true,
            ),
            SelfTestStatus::NeverRun => (
                "Waiting for drive to start test…".to_string(),
                t.txt_sec,
                0.0,
                true,
                false,
            ),
            SelfTestStatus::Unknown => (
                "Waiting for drive to start test…".to_string(),
                t.txt_sec,
                0.0,
                true,
                false,
            ),
        },
    };

    let bar_color = if is_done && !matches!(data.self_test.as_ref().map(|r| &r.status), Some(SelfTestStatus::Failed { .. })) {
        Color32::from_rgb(39, 174, 96)
    } else if is_done {
        Color32::from_rgb(231, 76, 60)
    } else {
        Color32::from_rgb(39, 174, 96) // green while running
    };

    // Modal card dimensions
    let modal_w = 380.0_f32;
    let pad = 20.0_f32;
    let title_h = 28.0_f32;
    let status_h = 22.0_f32;
    let bar_h = 10.0_f32;
    let gap = 10.0_f32;
    let btn_h = 32.0_f32;
    let modal_h = pad + title_h + gap + status_h + gap + bar_h + gap + btn_h + pad;

    // Draw scrim (dimmed overlay behind the modal)
    let screen = ctx.screen_rect();
    ui.painter().rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 140));

    // Center the modal on screen
    let modal_pos = Pos2::new(
        (screen.center().x - modal_w * 0.5).round(),
        (screen.center().y - modal_h * 0.5).round(),
    );
    let modal_rect = Rect::from_min_size(modal_pos, Vec2::new(modal_w, modal_h));

    // Draw modal card background via Area so it sits above scroll content
    let area_resp = egui::Area::new(Id::new("health_self_test_modal"))
        .order(egui::Order::Foreground)
        .fixed_pos(modal_pos)
        .show(ctx, |ui| {
            ui.set_min_size(Vec2::new(modal_w, modal_h));
            let painter = ui.painter();
            painter.rect_filled(modal_rect, 12.0, t.bg_sec);
            painter.rect_stroke(modal_rect, 12.0, Stroke::new(1.5, t.border), StrokeKind::Middle);

            let mut y = modal_rect.min.y + pad;

            // Title
            painter.text(
                Pos2::new(modal_rect.min.x + pad, y + title_h * 0.5),
                Align2::LEFT_CENTER,
                &title,
                FontId::new(16.0, egui::FontFamily::Name("InterBold".into())),
                t.txt_pri,
            );
            y += title_h + gap;

            // Status text
            painter.text(
                Pos2::new(modal_rect.min.x + pad, y + status_h * 0.5),
                Align2::LEFT_CENTER,
                &status_text,
                FontId::proportional(13.0),
                status_color,
            );
            y += status_h + gap;

            // Progress bar track
            let bar_rect = Rect::from_min_size(
                Pos2::new(modal_rect.min.x + pad, y + (bar_h * 0.5 - bar_h * 0.5).max(0.0)),
                Vec2::new(modal_w - pad * 2.0, bar_h),
            );
            painter.rect_filled(bar_rect, 5.0, t.border);
            if progress_pct > 0.0 {
                let fill = Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * progress_pct, bar_h));
                painter.rect_filled(fill, 5.0, bar_color);
            }
            // Animated indeterminate "pulse" while waiting for first InProgress status
            if is_running && progress_pct == 0.0 {
                let t_anim = ctx.input(|i| i.time) as f32;
                let pulse_x = (t_anim * 0.8).sin() * 0.5 + 0.5;
                let pw = bar_rect.width() * 0.3;
                let px = bar_rect.min.x + (bar_rect.width() - pw) * pulse_x;
                let pulse_rect = Rect::from_min_size(Pos2::new(px, bar_rect.min.y), Vec2::new(pw, bar_h));
                painter.rect_filled(pulse_rect, 5.0, t.accent);
                ctx.request_repaint();
            }
            y += bar_h + gap;

            // Buttons
            let btn_w = (modal_w - pad * 2.0 - gap) / 2.0;

            let mut action: Option<SelfTestModalAction> = None;

            // Abort — only while running
            if is_running {
                let abort_rect = Rect::from_min_size(Pos2::new(modal_rect.min.x + pad, y), Vec2::new(btn_w, btn_h));
                let abort_r = ui.interact(abort_rect, Id::new("health_modal_abort"), Sense::click());
                let abort_bg = if abort_r.hovered() { Color32::from_rgba_unmultiplied(231, 76, 60, 40) } else { t.bg_sec };
                painter.rect_filled(abort_rect, 4.0, abort_bg);
                painter.rect_stroke(abort_rect, 4.0, Stroke::new(1.5, Color32::from_rgb(231, 76, 60)), StrokeKind::Middle);
                painter.text(abort_rect.center(), Align2::CENTER_CENTER, "Abort Test", FontId::proportional(13.0), Color32::from_rgb(231, 76, 60));
                if abort_r.clicked() {
                    action = Some(SelfTestModalAction::Abort);
                }
            }

            // Dismiss — only when done
            if is_done {
                let dismiss_rect = Rect::from_min_size(Pos2::new(modal_rect.min.x + pad, y), Vec2::new(modal_w - pad * 2.0, btn_h));
                let dismiss_r = ui.interact(dismiss_rect, Id::new("health_modal_dismiss"), Sense::click());
                let dismiss_bg = if dismiss_r.hovered() { t.hover } else { t.accent };
                painter.rect_filled(dismiss_rect, 4.0, dismiss_bg);
                painter.text(dismiss_rect.center(), Align2::CENTER_CENTER, "Dismiss", FontId::proportional(13.0), Color32::WHITE);
                if dismiss_r.clicked() {
                    action = Some(SelfTestModalAction::Dismiss);
                }
            }

            action
        });

    area_resp.inner
}

// ── Self-test card ────────────────────────────────────────────────────────────

/// Returns Some((kind,)) if a test button was clicked.
fn draw_self_test(
    ui: &mut egui::Ui,
    t: &Theme,
    content_x: f32,
    margin: f32,
    section_w: f32,
    data: &AtaSmartData,
    test_active: bool,
    test_error: Option<&str>,
) -> Option<(crate::smart_reader::SelfTestKind, )> {
    use crate::smart_reader::{SelfTestKind, SelfTestStatus};

    // Extra row for error text when a test trigger fails
    let error_row_h = if test_error.is_some() { 22.0_f32 } else { 0.0_f32 };
    let pad = 16.0_f32;
    let row_h = 22.0_f32;
    let btn_h = 32.0_f32;
    let card_h = pad + row_h + 8.0 + btn_h + error_row_h + pad;

    draw_section_label(ui, t, content_x, margin, section_w, "Self-Test", false);
    ui.add_space(6.0);

    let card = card_rect(ui, t, content_x, margin, section_w, card_h);
    let inner_w = section_w - pad * 2.0;

    // Last result row
    let result_text = match &data.self_test {
        None => "No self-test log available.".to_string(),
        Some(r) => {
            let kind_str = r.kind.label();
            match &r.status {
                SelfTestStatus::Passed => format!("{kind_str} test passed."),
                SelfTestStatus::Failed { reason } => format!("{kind_str} test FAILED: {reason}"),
                SelfTestStatus::InProgress { pct_remaining } =>
                    format!("{kind_str} test in progress ({pct_remaining}% remaining)"),
                SelfTestStatus::Aborted => format!("{kind_str} test was aborted."),
                SelfTestStatus::NeverRun => "No self-test has been run.".to_string(),
                SelfTestStatus::Unknown => "Unknown self-test status.".to_string(),
            }
        }
    };

    let result_color = match &data.self_test {
        Some(r) => match &r.status {
            SelfTestStatus::Passed => Color32::from_rgb(39, 174, 96),
            SelfTestStatus::Failed { .. } => Color32::from_rgb(231, 76, 60),
            _ => t.txt_sec,
        },
        None => t.txt_sec,
    };

    ui.painter().text(
        Pos2::new(card.min.x + pad, card.min.y + pad + row_h * 0.5),
        Align2::LEFT_CENTER,
        &result_text,
        FontId::proportional(13.0),
        result_color,
    );

    // Short + Long test buttons (grayed out while a test is running)
    let btn_y = card.min.y + pad + row_h + 8.0;
    let btn_w = (inner_w - 8.0) * 0.5;

    let short_rect = Rect::from_min_size(Pos2::new(card.min.x + pad, btn_y), Vec2::new(btn_w, btn_h));
    let long_rect = Rect::from_min_size(Pos2::new(card.min.x + pad + btn_w + 8.0, btn_y), Vec2::new(btn_w, btn_h));

    let mut clicked_kind: Option<SelfTestKind> = None;

    for (rect, label, kind) in [
        (short_rect, "Run Short Test", SelfTestKind::Short),
        (long_rect, "Run Extended Test", SelfTestKind::Long),
    ] {
        let sense = if test_active { Sense::hover() } else { Sense::click() };
        let r = ui.interact(rect, Id::new(format!("health_test_{}", label)), sense);
        let label_color = if test_active { t.txt_sec } else { t.txt_pri };
        let bg = if !test_active && r.hovered() { t.hover } else { t.bg_sec };
        ui.painter().rect_filled(rect, 4.0, bg);
        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.5, t.border), StrokeKind::Middle);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), label_color);
        if !test_active && r.clicked() {
            clicked_kind = Some(kind);
        }
    }

    // Inline error text below buttons when trigger failed
    if let Some(err) = test_error {
        let err_y = btn_y + btn_h + 6.0;
        ui.painter().text(
            Pos2::new(card.min.x + pad, err_y + error_row_h * 0.5),
            Align2::LEFT_CENTER,
            err,
            FontId::proportional(12.0),
            Color32::from_rgb(231, 76, 60),
        );
    }

    let cursor_y = ui.cursor().min.y;
    let card_bot = card.max.y;
    if cursor_y < card_bot {
        ui.add_space(card_bot - cursor_y);
    }

    clicked_kind.map(|k| (k,))
}

// ── HTML report ───────────────────────────────────────────────────────────────

fn build_report_html(drive: &DetectedDrive, report: &SmartReport) -> String {
    let now = chrono::Local::now();
    let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let capacity = crate::detected_drive::DetectedDrive::format_size(drive.size_bytes);

    let body = match report {
        SmartReport::Ata(data) => {
            let temp = data.temperature_c.map(|c| format!("{c}°C")).unwrap_or_else(|| "N/A".to_string());
            let poh  = data.power_on_hours.map(|h| fmt_thousands(h)).unwrap_or_else(|| "N/A".to_string());
            let pc   = data.power_cycles.map(|c| fmt_thousands(c)).unwrap_or_else(|| "N/A".to_string());

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
<table class="attrs">
<thead><tr>
<th>ID</th><th>Name</th><th>Status</th>
<th>Current</th><th>Worst</th><th>Threshold</th><th>Raw</th>
</tr></thead><tbody>{attrs}</tbody></table>"#
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
table {{ border-collapse: collapse; width: 100%; margin-bottom: 20px; font-size: 0.9rem; }}
th, td {{ text-align: left; padding: 7px 12px; border: 1px solid #333; }}
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
<h1>Health Data Report — {model}</h1>
<p class="meta">Generated {ts}</p>
<table class="info">
<tr><th>Model</th><td>{model}</td></tr>
<tr><th>Serial</th><td>{serial}</td></tr>
<tr><th>Bus</th><td>{bus}</td></tr>
<tr><th>Media</th><td>{media}</td></tr>
<tr><th>Capacity</th><td>{capacity}</td></tr>
</table>
{body}
</body>
</html>"#,
        model    = drive.model,
        serial   = drive.serial,
        bus      = drive.bus.label(),
        media    = drive.media.label(),
        capacity = capacity,
    )
}

pub fn save_smart_report(drive: &DetectedDrive, report: &SmartReport) {
    let html = build_report_html(drive, report);
    let filename = format!("SMART-{}.html", drive.safe_filename_stem());

    std::thread::spawn(move || {
        let path = rfd::FileDialog::new()
            .set_file_name(&filename)
            .add_filter("HTML file", &["html"])
            .save_file();
        if let Some(p) = path {
            let _ = std::fs::write(p, html.as_bytes());
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
            use crate::about::ABOUT_HEADER_ROW_H;
            let row_top = ui.cursor().min.y;
            let row_rect = Rect::from_min_size(
                Pos2::new(content_x + margin, row_top),
                Vec2::new(section_w, ABOUT_HEADER_ROW_H),
            );

            // Title
            ui.painter().text(
                Pos2::new(row_rect.min.x, row_rect.center().y),
                Align2::LEFT_CENTER,
                "Health Status",
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
                let sel = self.health_selected_drive.min(self.drives.len().saturating_sub(1));
                Some(self.drives[sel].clone())
            } else {
                None
            };
            let export_report = if has_report { self.health_report.clone() } else { None };

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
                    if btn.clicked() {
                        if let (Some(drv), Some(rep)) = (export_drive, export_report) {
                            save_smart_report(&drv, &rep);
                        }
                    }
                },
            );

            ui.advance_cursor_after_rect(row_rect);
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
            ui.add_space(16.0);

            ui.push_id("diskoria_health_refresh", |ui| {
                let refresh = ui.add_enabled(
                    !self.drives_loading,
                    egui::Button::new(RichText::new("⟳ Refresh").color(t.txt_pri)),
                );
                if refresh.clicked() {
                    self.health_report = None;
                    self.health_poll_running = false;
                    self.health_last_poll = None;
                    self.spawn_drive_enumeration(ctx);
                }
            });

            if self.drives_loading {
                ui.add_space(10.0);
                ui.spinner();
                ui.label(RichText::new("Loading drives…").color(t.txt_sec));
            }
        });
        ui.add_space(20.0);

        // Error / empty states
        if let Some(ref err) = self.drives_error {
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(RichText::new(format!("Could not enumerate drives: {err}")).size(13.0).color(Color32::from_rgb(231, 76, 60)));
            });
            ui.add_space(12.0);
        }

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

        // ── Drive picker ──────────────────────────────────────────────────────
        draw_drive_picker(self, ui, t, content_x, margin, section_w);
        ui.add_space(16.0);

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

        let sel = self.health_selected_drive.min(self.drives.len().saturating_sub(1));
        let drive = self.drives[sel].clone();

        // ── Report display ────────────────────────────────────────────────────
        let test_active = self.health_test_active;
        let test_error = self.health_test_error.clone();
        let device_id = drive.device_id.clone();

        match &report {
            SmartReport::Ata(data) => {
                let data = data.clone();
                draw_ata_vitals(ui, t, content_x, margin, section_w, &data);
                ui.add_space(16.0);

                if !data.attributes.is_empty() {
                    draw_attributes(ui, t, dark, content_x, margin, section_w, &data.attributes);
                    ui.add_space(16.0);
                }

                if let Some((kind,)) = draw_self_test(
                    ui, t, content_x, margin, section_w, &data,
                    test_active, test_error.as_deref(),
                ) {
                    let ok = crate::smart_reader::trigger_self_test(&device_id, kind == crate::smart_reader::SelfTestKind::Long);
                    if ok {
                        self.health_test_active = true;
                        self.health_test_kind = Some(kind);
                        self.health_test_error = None;
                        // Force an immediate re-poll so the modal sees InProgress quickly
                        self.health_last_poll = None;
                    } else {
                        self.health_test_error = Some("Failed to start self-test. Try running as Administrator.".to_string());
                    }
                }
                ui.add_space(16.0);

                // Self-test progress modal overlay
                if self.health_test_active {
                    let test_kind = self.health_test_kind;
                    let ata_data = data.clone();
                    let device_id2 = device_id.clone();
                    if let Some(dismiss) = draw_self_test_modal(ui, ctx, t, test_kind, &ata_data) {
                        if dismiss == SelfTestModalAction::Abort {
                            crate::smart_reader::abort_self_test(&device_id2);
                            self.health_last_poll = None;
                        }
                        if dismiss == SelfTestModalAction::Dismiss {
                            self.health_test_active = false;
                            self.health_test_kind = None;
                            self.health_test_error = None;
                        }
                    }
                }
            }
            SmartReport::Nvme(data) => {
                let data = data.clone();
                draw_nvme_vitals(ui, t, content_x, margin, section_w, &data);
                ui.add_space(16.0);
            }
            SmartReport::Unavailable { reason } => {
                let reason = reason.clone();
                draw_unavailable(ui, t, content_x, margin, section_w, &reason);
                ui.add_space(16.0);
            }
        }

    }

    // ── Poll lifecycle ────────────────────────────────────────────────────────

    fn spawn_health_poll_if_needed(&mut self, ctx: &egui::Context) {
        // Poll faster while a self-test is running so progress updates promptly.
        let poll_interval = if self.health_test_active {
            std::time::Duration::from_secs(3)
        } else {
            std::time::Duration::from_secs(5)
        };

        if self.health_poll_running {
            return;
        }
        if self.drives.is_empty() {
            return;
        }
        // If we already have a report, wait for the interval before re-polling.
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

        let sel = self.health_selected_drive.min(self.drives.len().saturating_sub(1));
        let drive = &self.drives[sel];
        let device_path = drive.device_id.clone();
        let bus = drive.bus;

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
                    SmartReport::Unavailable { reason } => log::warn!(
                        target: "diskoria",
                        "health: SMART unavailable — {reason}",
                    ),
                }
                // If a self-test was active, check whether it has finished so
                // the modal can switch to its "result" view automatically.
                if self.health_test_active {
                    use crate::smart_reader::{SmartReport, SelfTestStatus};
                    if let SmartReport::Ata(ref d) = report {
                        let finished = matches!(
                            d.self_test.as_ref().map(|r| &r.status),
                            Some(SelfTestStatus::Passed)
                            | Some(SelfTestStatus::Failed { .. })
                            | Some(SelfTestStatus::Aborted)
                        );
                        if finished {
                            log::info!(target: "diskoria", "health: self-test finished");
                        }
                        // Keep health_test_active = true so the modal stays open
                        // showing the result; the user dismisses it manually.
                    }
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
