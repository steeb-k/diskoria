//! Sector Read Test page. Moved verbatim from `app.rs` in the Phase 3c split;
//! see `docs/refactor-roadmap.md`. No behavior change.

use egui::{
    Align2, Color32, FontFamily, FontId, Id, Pos2, Rect, Vec2,
};
use egui::{Key, Modifiers, Sense, Stroke, StrokeKind};

use crate::detected_drive::DetectedDrive;
use crate::drive_selector::{self, ChipSpec, DriveEntry};
use crate::theme::Theme;
use crate::theme::CLOSE_HOVER_BG;

impl crate::app::DiskoriaApp {
    pub(crate) fn draw_sector_page(
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
            "Sector Read Test",
            FontId::new(28.0, FontFamily::Proportional),
            t.txt_pri,
        );
        ui.add_space(32.0);

        let subtitle = "Read-only scan — checks every sector for errors";
        let section_w = content_w - margin * 2.0;

        ui.horizontal(|ui| {
            let pad = (content_x + margin) - ui.min_rect().left();
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.label(egui::RichText::new(subtitle).size(14.0).color(t.txt_sec));
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

        // Refresh icon button (left) and the two-row drive card (right) share a row.
        let left = content_x + margin;
        let y_row = ui.cursor().min.y;
        let btn_rect = Rect::from_min_size(
            Pos2::new(left, y_row + (drive_selector::ROW_H - drive_selector::REFRESH_W) * 0.5),
            Vec2::splat(drive_selector::REFRESH_W),
        );
        let refresh = drive_selector::refresh_button(
            ui,
            ctx,
            t,
            Id::new("diskoria_sector_refresh"),
            btn_rect,
            !self.any_test_running(),
            self.refresh_busy(),
            self.sector_focus == Some(0),
        );
        if refresh.clicked() {
            self.sector_focus = Some(0);
            self.spawn_drive_enumeration(ctx);
        }
        self.sector_refresh_id = Some(refresh.id);

        let card_left = left + drive_selector::REFRESH_W + drive_selector::REFRESH_GAP;
        let card_w = section_w - drive_selector::REFRESH_W - drive_selector::REFRESH_GAP;
        let row_rect = Rect::from_min_size(
            Pos2::new(left, y_row),
            Vec2::new(section_w, drive_selector::ROW_H),
        );

        if self.drives_loading && self.drives.is_empty() {
            ui.advance_cursor_after_rect(row_rect);
            return;
        }

        let busy = self.drives_busy_elsewhere();
        let entries: Vec<DriveEntry> = self
            .drives
            .iter()
            .map(|d| DriveEntry {
                title: format!("Drive {} — {}", d.disk_number, d.model.trim()),
                chips: vec![
                    ChipSpec::neutral(t, DetectedDrive::format_size(d.size_bytes)),
                    ChipSpec::media(d.media),
                    ChipSpec::bus(d.bus),
                ],
                disabled: busy
                    .contains(&d.lock_key())
                    .then_some(drive_selector::BUSY_ELSEWHERE),
            })
            .collect();
        let sel = self.selected_drive.min(entries.len().saturating_sub(1));

        ui.add_enabled_ui(!self.any_test_running(), |ui| {
            let out = drive_selector::two_row_combo(
                ui,
                t,
                Id::new("diskoria_sector_drive_combo"),
                &entries,
                sel,
                !self.any_test_running() && self.sector_focus == Some(1),
                !self.refresh_busy(),
                card_left,
                card_w,
                y_row,
            );
            self.sector_combo_id = Some(out.id);
            if out.clicked {
                self.sector_focus = Some(1);
            }
            self.selected_drive = out.selected;
            ui.advance_cursor_after_rect(row_rect);
        });

        if self.selected_drive_busy_elsewhere() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let pad = (content_x + margin) - ui.min_rect().left();
                if pad > 0.0 { ui.add_space(pad); }
                ui.label(
                    egui::RichText::new("This drive is being tested in another window.")
                        .size(13.0)
                        .color(Color32::from_rgb(241, 196, 15)),
                );
            });
        }

        self.draw_smart_health_card(ui, t, dark, content_x, margin, section_w);

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
                        Stroke::new(2.0_f32, t.accent),
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
                    && !self.speed_test_running
                    && !self.selected_drive_busy_elsewhere();
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
                    // Dim the *legible* on-accent color, not a hard-coded white:
                    // on a pale accent white-on-white was invisible even at full
                    // opacity.
                    t.txt_on_accent.gamma_multiply(0.47)
                };
                ui.painter().rect_filled(btn_rect, 4.0, bg);
                if start_focused && can_start {
                    ui.painter().rect_stroke(
                        btn_rect.expand(3.0),
                        4.0,
                        Stroke::new(2.0_f32, t.accent),
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

        self.draw_sector_test_panel(ui, t, content_x, margin, section_w);
    }
}
