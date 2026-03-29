//! About page — version, project URL, copyright (layout aligned with Copynaut `ui/pages/about.rs`).

use egui::{
    Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Pos2, Rect, RichText, Sense,
    Stroke, UiBuilder, Vec2,
};

use crate::chrome::shell_open_uri;
use crate::theme::Theme;
use crate::widgets::small_browse_style_button;
use crate::JfStorageTesterApp;

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
pub const ABOUT_HEADER_ROW_H: f32 = 38.0;

pub const JF_GITHUB_REPO_URL: &str = "https://github.com/jjfbno/JF-Storage-Tester";

pub fn draw_about_header_row(
    app: &mut JfStorageTesterApp,
    ui: &mut egui::Ui,
    t: &Theme,
    margin: f32,
    content_x: f32,
    content_w: f32,
    page_title: &str,
) {
    let section_w = content_w - margin * 2.0;
    let row_top = ui.cursor().min.y;
    let row_rect = Rect::from_min_size(
        Pos2::new(content_x + margin, row_top),
        Vec2::new(section_w, ABOUT_HEADER_ROW_H),
    );
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(row_rect)
            .layout(Layout::left_to_right(Align::TOP)),
        |ui| {
            ui.label(
                RichText::new(page_title)
                    .font(FontId::new(28.0, FontFamily::Proportional))
                    .color(t.txt_pri),
            );
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                let icon = ICON_CLOUD_ARROW_DOWN.chars().next().unwrap_or('\u{f295}');
                #[cfg(windows)]
                let enabled = app.update_check_button_enabled();
                #[cfg(not(windows))]
                let enabled = false;
                let check_r = small_browse_style_button(
                    ui,
                    t,
                    egui::Id::new("jf_about_check_updates"),
                    icon,
                    "Check for updates",
                    enabled,
                );
                if check_r.clicked() {
                    #[cfg(windows)]
                    app.on_about_check_updates_clicked(ui.ctx());
                }
            });
        },
    );
}

pub fn draw_about(
    app: &JfStorageTesterApp,
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
        (inner_w - ABOUT_APPICON_GAP - ABOUT_TEXT_COL_MIN_W)
            .min(ABOUT_APPICON_W)
            .max(0.0)
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
                                    about_card_text_column(ui, t);
                                });
                            });
                        } else {
                            about_card_text_column(ui, t);
                        }
                    } else {
                        about_card_text_column(ui, t);
                    }
                });
        },
    );
}

fn about_card_text_column(ui: &mut egui::Ui, t: &Theme) {
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
        let link = ui.add(
            egui::Label::new(
                RichText::new(JF_GITHUB_REPO_URL)
                    .size(12.0)
                    .color(t.accent)
                    .underline(),
            )
            .sense(Sense::click()),
        );
        if link.clicked() {
            shell_open_uri(JF_GITHUB_REPO_URL);
        }
        if link.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    });
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(
        RichText::new(format!("© {} JF Storage Tester", utc_year_now()))
            .size(11.0)
            .color(t.txt_sec),
    );
}
