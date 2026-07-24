//! About page — version, project URL, copyright (layout aligned with Copynaut `ui/pages/about.rs`).

use egui::{
    Align, Align2, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Pos2, Rect, RichText,
    Sense, Stroke, UiBuilder, Vec2,
};

use crate::chrome::shell_open_uri;
use crate::theme::Theme;
use crate::widgets::small_browse_style_button;
use crate::DiskoriaApp;

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

fn utc_year_now() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut secs = secs;
    const DAY: u64 = 86400;
    let mut y = 1970i32;
    loop {
        let diy = if is_leap_year(y) { 366_u64 } else { 365 };
        let ys = diy * DAY;
        if secs < ys {
            return y;
        }
        secs -= ys;
        y += 1;
    }
}

const ABOUT_CARD_BODY_H: f32 = 122.0;
const ABOUT_APPICON_W: f32 = 128.0;
const ABOUT_APPICON_GAP: f32 = 16.0;
const ABOUT_TEXT_COL_MIN_W: f32 = 100.0;
const INNER_PAD_PX: i8 = 12;

const ICON_CLOUD_ARROW_DOWN: &str = "\u{f295}";

pub fn draw_about_header_row(
    app: &mut DiskoriaApp,
    ui: &mut egui::Ui,
    t: &Theme,
    margin: f32,
    content_x: f32,
    content_w: f32,
    page_title: &str,
) {
    let section_w = content_w - margin * 2.0;
    // Height 28 → title centered at +14, matching the Drive Health and
    // Sector/Write/Benchmark page titles (drawn at cursor+14). The 34px update
    // button overflows this row symmetrically (harmless) and stays aligned with
    // the title. Drawing the title via the painter avoids the galley leading
    // that previously pushed a `ui.label` title ~5px lower than the other pages.
    const HEADER_ROW_H: f32 = 28.0;
    let row_top = ui.cursor().min.y;
    let row_rect = Rect::from_min_size(
        Pos2::new(content_x + margin, row_top),
        Vec2::new(section_w, HEADER_ROW_H),
    );

    ui.painter().text(
        Pos2::new(row_rect.min.x, row_rect.center().y),
        Align2::LEFT_CENTER,
        page_title,
        FontId::new(28.0, FontFamily::Proportional),
        t.txt_pri,
    );

    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(row_rect)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            let icon = ICON_CLOUD_ARROW_DOWN.chars().next().unwrap_or('\u{f295}');
            #[cfg(windows)]
            let enabled = app.update_check_button_enabled();
            #[cfg(not(windows))]
            let enabled = false;
            let check_focused = app.about_focus == Some(0);
            let check_r = small_browse_style_button(
                ui,
                t,
                egui::Id::new("diskoria_about_check_updates"),
                icon,
                "Check for updates",
                enabled,
            );
            // A permanently-greyed button reads as broken, so say why. Only for
            // the portable case — the transient busy/modal states explain
            // themselves through the spinner and dialog that caused them.
            #[cfg(windows)]
            if !app.updates_supported() && check_r.hovered() {
                if let Some(pos) = ui.ctx().pointer_latest_pos() {
                    crate::widgets::show_tooltip_text(
                        ui.ctx(),
                        egui::Id::new("about_updates_portable_tt"),
                        pos,
                        t,
                        "Updates are handled by the installer — this is a portable build.",
                    );
                }
            }
            let kb = check_focused && enabled && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
            });
            if check_r.clicked() || kb {
                #[cfg(windows)]
                app.on_about_check_updates_clicked(ui.ctx());
            }
            if check_focused {
                ui.painter().rect_stroke(
                    check_r.rect.expand(2.0),
                    4.0,
                    egui::Stroke::new(2.0, t.accent),
                    egui::StrokeKind::Outside,
                );
            }
        },
    );
    // Force the header block to ~32px (title at +14, content below at +32) so it
    // lines up with the other pages, regardless of the overflowing button.
    ui.advance_cursor_after_rect(row_rect);
    ui.add_space(4.0);
}

pub fn draw_about(
    app: &DiskoriaApp,
    ui: &mut egui::Ui,
    t: &Theme,
    margin: f32,
    content_x: f32,
    content_w: f32,
) {
    let section_w = content_w - margin * 2.0;
    let card_top = ui.cursor().min.y;
    let inner_pad_f = INNER_PAD_PX as f32;
    let inner_w = (section_w - inner_pad_f * 2.0).max(0.0);
    let max_img_w = if app.about_appicon.is_some() {
        (inner_w - ABOUT_APPICON_GAP - ABOUT_TEXT_COL_MIN_W).clamp(0.0, ABOUT_APPICON_W)
    } else {
        0.0
    };
    let show_appicon = max_img_w > 1.0;
    let body_h = if show_appicon {
        app.about_appicon.as_ref().map_or(ABOUT_CARD_BODY_H, |tex| {
            let [tw, th] = tex.size();
            let img_h = max_img_w * (th as f32) / (tw as f32).max(1.0);
            ABOUT_CARD_BODY_H.max(img_h)
        })
    } else {
        ABOUT_CARD_BODY_H
    };
    let section_h = body_h + inner_pad_f * 2.0;

    let card_rect = Rect::from_min_size(
        Pos2::new(content_x + margin, card_top),
        Vec2::new(section_w, section_h),
    );

    let about_focus = app.about_focus;
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(card_rect)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            Frame::NONE
                .fill(t.bg_pri)
                .stroke(Stroke::new(1.5, t.border))
                .inner_margin(Margin::same(INNER_PAD_PX))
                .corner_radius(CornerRadius::same(8))
                .show(ui, |ui| {
                    ui.set_min_width(inner_w);
                    if let Some(tex) = &app.about_appicon {
                        if show_appicon {
                            ui.horizontal_top(|ui| {
                                ui.add(
                                    egui::Image::from_texture(tex)
                                        .max_width(max_img_w)
                                        .maintain_aspect_ratio(true),
                                );
                                ui.add_space(ABOUT_APPICON_GAP);
                                ui.vertical(|ui| {
                                    about_card_text_column(ui, t, about_focus);
                                });
                            });
                        } else {
                            about_card_text_column(ui, t, about_focus);
                        }
                    } else {
                        about_card_text_column(ui, t, about_focus);
                    }
                });
        },
    );
}

/// "Installed" / "Portable" pill next to the version.
///
/// The two builds behave differently (tray-on-close and launch-at-startup
/// default on when installed, off when portable), so which one is running has
/// to be visible somewhere. Portable takes the accent fill because it's the
/// state worth noticing; installed is a quiet neutral chip.
fn draw_install_mode_chip(ui: &mut egui::Ui, t: &Theme) {
    let mode = crate::install_mode::current();
    let label = mode.label();
    let font = FontId::proportional(11.0);
    let text_w = ui
        .ctx()
        .fonts(|f| label.chars().map(|c| f.glyph_width(&font, c)).sum::<f32>());

    let (_, rect) = ui.allocate_space(Vec2::new(text_w + 14.0, 18.0));
    let (bg, fg) = if mode.is_installed() {
        (t.bg_sec, t.txt_sec)
    } else {
        (t.accent, t.txt_on_accent)
    };
    ui.painter().rect_filled(rect, 9.0, bg);
    ui.painter()
        .rect_stroke(rect, 9.0, Stroke::new(1.0, t.border), egui::StrokeKind::Middle);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
}

fn about_card_text_column(ui: &mut egui::Ui, t: &Theme, about_focus: Option<usize>) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Version:")
                .strong()
                .size(13.0)
                .color(t.txt_pri),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(env!("CARGO_PKG_VERSION"))
                .size(13.0)
                .color(t.txt_sec),
        );
        ui.add_space(8.0);
        draw_install_mode_chip(ui, t);
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("URL:")
                .strong()
                .size(13.0)
                .color(t.txt_pri),
        );
        ui.add_space(6.0);
        let releases_url = "https://kznjk.com/".to_string();
        let url_focused = about_focus == Some(1);
        let link = ui.add(
            egui::Label::new(
                RichText::new(&releases_url)
                    .size(12.0)
                    .color(t.accent)
                    .underline(),
            )
            .sense(Sense::click()),
        );
        let kb_link = url_focused && ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
        });
        if link.clicked() || kb_link {
            shell_open_uri(&releases_url);
        }
        if link.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if url_focused {
            ui.painter().rect_stroke(
                link.rect.expand(2.0),
                3.0,
                egui::Stroke::new(2.0, t.accent),
                egui::StrokeKind::Outside,
            );
        }
    });
    ui.add_space(10.0);
    let kofi_focused = about_focus == Some(2);
    let kofi_btn = small_browse_style_button(
        ui,
        t,
        egui::Id::new("about_kofi_btn"),
        '\u{f415}',
        "Support me on Ko-fi",
        true,
    );
    let kb_kofi = kofi_focused && ui.input_mut(|i| {
        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
    });
    if kofi_btn.clicked() || kb_kofi {
        shell_open_uri("https://ko-fi.com/kznjk");
    }
    if kofi_btn.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if kofi_focused {
        ui.painter().rect_stroke(
            kofi_btn.rect.expand(2.0),
            4.0,
            egui::Stroke::new(2.0, t.accent),
            egui::StrokeKind::Outside,
        );
    }
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(
        RichText::new(format!("© {} Steve Kzenjak", utc_year_now()))
            .size(11.0)
            .color(t.txt_sec),
    );
}
