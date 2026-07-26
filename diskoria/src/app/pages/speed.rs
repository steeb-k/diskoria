//! Speed (benchmark) Test page. Moved verbatim from `app.rs` in the Phase 3c split;
//! see `docs/refactor-roadmap.md`. No behavior change.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Id, Key, Modifiers, Pos2, Rect,
    Sense, Stroke, StrokeKind, Vec2,
};

use crate::drive_selector::{self, ChipSpec, DriveEntry};
use crate::theme::{Theme, CLOSE_HOVER_BG};

use super::super::{paint_speed_metric_cell, SPEED_PAGE_BOTTOM_PAD, SPEED_PAGE_SECTION_GAP};

impl crate::app::DiskoriaApp {
    /// Speed Test page — volume combo, 2×2 metrics, progress, Start/Stop.
    pub(crate) fn draw_speed_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &Theme,
        _dark: bool,
        margin: f32,
        content_x: f32,
        content_w: f32,
    ) {
        let title = "Benchmark";
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

        let section_w = content_w - margin * 2.0;

        // Refresh icon button (left) and the two-row volume card (right) share a row.
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
            Id::new("diskoria_speed_refresh"),
            btn_rect,
            !self.any_test_running(),
            self.refresh_busy(),
            self.speed_focus == Some(0),
        );
        if refresh.clicked() {
            self.speed_focus = Some(0);
            self.spawn_drive_enumeration(ctx);
        }
        self.speed_refresh_id = Some(refresh.id);

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

        // One entry per mounted volume; row 1 = volume + model, free-space is the
        // neutral chip (the per-partition equivalent of the whole-drive size chip).
        let busy = self.drives_busy_elsewhere();
        let pairs = Self::speed_volume_pairs(&self.drives);
        let mut targets: Vec<Option<(usize, usize)>> = pairs.iter().copied().map(Some).collect();
        let mut entries: Vec<DriveEntry> = pairs
            .iter()
            .map(|&(di, pi)| {
                let d = &self.drives[di];
                let p = &d.partitions[pi];
                let letter = p
                    .drive_letter
                    .trim()
                    .trim_end_matches('\\')
                    .trim_end_matches(':');
                let vol = if letter.is_empty() {
                    "(no letter)".to_string()
                } else {
                    format!("{}:\\", letter)
                };
                DriveEntry {
                    title: format!("{} — {}", vol, d.model.trim()),
                    chips: vec![
                        ChipSpec::neutral(t, format!("{} free", p.free_space_display())),
                        ChipSpec::media(d.media),
                        ChipSpec::bus(d.bus),
                    ],
                    // A whole physical drive is locked, so every volume on it.
                    disabled: busy
                        .contains(&d.lock_key())
                        .then_some(drive_selector::BUSY_ELSEWHERE),
                }
            })
            .collect();

        // KI-15: the drive selection is shared with Drive Health / Sector /
        // Sector Write, so it can name a disk with no mounted volume. Rather than
        // repointing it at whichever drive happens to have one — which would
        // change what those pages show — list the selection here, in drive order
        // and unselectable, so the page displays what it was actually given and
        // the user picks a volume deliberately.
        let target = self.speed_target_pair();
        if target.is_none() {
            let sel = self.selected_drive.min(self.drives.len().saturating_sub(1));
            if let Some(d) = self.drives.get(sel) {
                let at = pairs
                    .iter()
                    .position(|&(di, _)| di > sel)
                    .unwrap_or(pairs.len());
                entries.insert(
                    at,
                    DriveEntry {
                        title: format!("Drive {} — {}", d.disk_number, d.model.trim()),
                        chips: vec![
                            ChipSpec::neutral(t, "No mounted volume"),
                            ChipSpec::media(d.media),
                            ChipSpec::bus(d.bus),
                        ],
                        disabled: Some("No mounted volume to benchmark"),
                    },
                );
                targets.insert(at, None);
            }
        }
        let flat_current = targets.iter().position(|&x| x == target).unwrap_or(0);

        ui.add_enabled_ui(!self.any_test_running(), |ui| {
            let out = drive_selector::two_row_combo(
                ui,
                t,
                Id::new("diskoria_speed_volume_combo"),
                &entries,
                flat_current,
                !self.any_test_running() && self.speed_focus == Some(1),
                !self.refresh_busy(),
                card_left,
                card_w,
                y_row,
            );
            // Picking a volume here *is* an explicit choice, so this is the one
            // place the Benchmark page may move the shared drive selection.
            if out.selected != flat_current {
                if let Some(Some((di, pi))) = targets.get(out.selected).copied() {
                    self.selected_drive = di;
                    self.selected_speed_partition = pi;
                }
            }
            self.speed_volume_combo_id = Some(out.id);
            if out.clicked {
                self.speed_focus = Some(1);
            }
            ui.advance_cursor_after_rect(row_rect);
        });

        if self.speed_target_busy_elsewhere() {
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

        match target {
            Some((di, pi)) if self.drives[di].partitions[pi].is_bitlocker_locked() => {
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
            }
            // Reachable now that the page no longer repoints the selection away
            // from a partition-less drive (KI-15); Start stays disabled until the
            // user picks a volume above.
            None if !self.drives.is_empty() => {
                ui.horizontal(|ui| {
                    let pad = (content_x + margin) - ui.min_rect().left();
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.label(
                        egui::RichText::new(
                            "No mounted volume on this disk. Choose a volume above to benchmark.",
                        )
                        .size(13.0)
                        .color(t.txt_sec),
                    );
                });
                ui.add_space(10.0);
            }
            _ => {}
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
                        Stroke::new(2.0, t.accent),
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
                let can_start = self.can_start_speed_test() && !self.speed_target_busy_elsewhere();
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
                        Stroke::new(2.0, t.accent),
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
            painter,
            r00,
            t,
            "SEQUENTIAL READ",
            &fmt(self.speed_seq_read_mbps),
            "1 MiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            painter,
            r01,
            t,
            "SEQUENTIAL WRITE",
            &fmt(self.speed_seq_write_mbps),
            "1 MiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            painter,
            r10,
            t,
            "RANDOM 4K READ",
            &fmt(self.speed_r4_read_mbps),
            "4 KiB blocks",
        );
        paint_speed_metric_cell(
            ctx,
            painter,
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
