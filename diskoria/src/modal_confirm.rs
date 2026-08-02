//! Two-button confirm dialog (overlay + card + Tab between buttons) — aligned with Copynaut `ui/composites.rs`.

use egui::{
    Align2, Color32, Context, FontFamily, FontId, Id, Key, Modifiers, Pos2, Rect, Stroke, StrokeKind,
    Vec2,
};

use crate::chrome::INTERACT_MANUAL_FOCUS;
use crate::theme::Theme;

fn paint_modal_title_body(
    painter: &egui::Painter,
    dlg: Rect,
    title: &str,
    body: &str,
    t: &Theme,
    btn_y: f32,
) {
    const H_PAD: f32 = 20.0;
    const TOP_PAD: f32 = 20.0;
    const TITLE_BODY_GAP: f32 = 10.0;
    const BODY_BTN_GAP: f32 = 12.0;

    let text_w = (dlg.width() - 2.0 * H_PAD).max(1.0);
    let content_top = dlg.top() + TOP_PAD;
    let content_bottom = (btn_y - BODY_BTN_GAP).max(content_top);
    let clip_rect = Rect::from_min_max(
        Pos2::new(dlg.left() + H_PAD, content_top),
        Pos2::new(dlg.right() - H_PAD, content_bottom),
    )
    .intersect(dlg);

    let p = painter.with_clip_rect(clip_rect);

    let title_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));
    let title_galley = p.layout(title.to_string(), title_font, t.txt_pri, text_w);
    let title_h = title_galley.size().y;
    let title_pos = Pos2::new(dlg.left() + H_PAD, content_top);
    p.galley(title_pos, title_galley, t.txt_pri);

    let body_top = content_top + title_h + TITLE_BODY_GAP;
    let body_font = FontId::proportional(13.0);
    let body_galley = p.layout(body.to_string(), body_font, t.txt_sec, text_w);
    p.galley(
        Pos2::new(dlg.left() + H_PAD, body_top),
        body_galley,
        t.txt_sec,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModalConfirmResult {
    Cancel,
    Confirm,
}

pub struct TwoButtonModalParams<'a> {
    pub overlay_id: Id,
    pub dialog_id: Id,
    pub width: f32,
    pub height: f32,
    pub title: &'a str,
    pub body: &'a str,
    pub cancel_id: Id,
    pub cancel_label: &'a str,
    pub confirm_id: Id,
    pub confirm: ModalConfirmPrimary<'a>,
}

pub enum ModalConfirmPrimary<'a> {
    /// Reserved for future accent-styled confirms (Copynaut parity).
    #[allow(dead_code)]
    AccentIcon { label: &'a str, icon: char },
    Danger { label: &'a str },
}

/// `button_focus`: `Some(0)` = Cancel, `Some(1)` = Confirm. Tab order from [`crate::focus::tab_cycle_slots`] in `App::update`.
pub fn two_button_modal(
    ctx: &Context,
    t: &Theme,
    p: TwoButtonModalParams<'_>,
    button_focus: &mut Option<usize>,
) -> Option<ModalConfirmResult> {
    let screen = ctx.screen_rect();

    egui::Area::new(p.overlay_id)
        .fixed_pos(screen.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
        });

    let dlg = Rect::from_center_size(screen.center(), Vec2::new(p.width, p.height));

    let inner = egui::Area::new(p.dialog_id)
        .fixed_pos(dlg.min)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            let painter = ui.painter();
            painter.rect_filled(dlg, 8.0, t.bg_pri);
            painter.rect_stroke(dlg, 8.0, Stroke::new(1.5_f32, t.accent), StrokeKind::Middle);

            let btn_h = 30.0_f32;
            let btn_y = dlg.bottom() - 16.0 - btn_h;
            const BTN_MIN_W: f32 = 90.0;
            const BTN_H_PAD: f32 = 18.0;
            let measure_font = FontId::proportional(13.0);

            // Size each button to its label (icon + text for the accent confirm) so
            // longer captions like "Include report" aren't cramped; short labels
            // keep the BTN_MIN_W baseline.
            let text_w = |s: &str| -> f32 {
                ctx.fonts(|f| s.chars().map(|c| f.glyph_width(&measure_font, c)).sum::<f32>())
            };
            let conf_content = match &p.confirm {
                ModalConfirmPrimary::AccentIcon { label, icon } => {
                    ctx.fonts(|f| f.glyph_width(&measure_font, *icon)) + 5.0 + text_w(label)
                }
                ModalConfirmPrimary::Danger { label } => text_w(label),
            };
            let canc_w = (text_w(p.cancel_label) + 2.0 * BTN_H_PAD).max(BTN_MIN_W);
            let conf_w = (conf_content + 2.0 * BTN_H_PAD).max(BTN_MIN_W);

            paint_modal_title_body(painter, dlg, p.title, p.body, t, btn_y);

            let confirm_rect = Rect::from_min_size(
                Pos2::new(dlg.right() - 20.0 - conf_w, btn_y),
                Vec2::new(conf_w, btn_h),
            );
            let cancel_rect = Rect::from_min_size(
                Pos2::new(confirm_rect.left() - 8.0 - canc_w, btn_y),
                Vec2::new(canc_w, btn_h),
            );

            if button_focus.is_none() {
                *button_focus = Some(0);
            }
            let focus_idx = button_focus.unwrap_or(0).min(1);
            let cancel_focused = focus_idx == 0;
            let confirm_focused = focus_idx == 1;

            ctx.memory_mut(|m| {
                if cancel_focused {
                    m.request_focus(p.cancel_id);
                } else {
                    m.request_focus(p.confirm_id);
                }
            });

            let cancel_r = ui.interact(cancel_rect, p.cancel_id, INTERACT_MANUAL_FOCUS);
            let confirm_r = ui.interact(confirm_rect, p.confirm_id, INTERACT_MANUAL_FOCUS);

            let cancel_bg = if cancel_r.hovered() {
                t.hover
            } else {
                t.bg_sec
            };
            painter.rect_filled(cancel_rect, 4.0, cancel_bg);
            painter.rect_stroke(
                cancel_rect,
                4.0,
                Stroke::new(1.5_f32, t.border),
                StrokeKind::Middle,
            );
            if cancel_focused {
                painter.rect_stroke(
                    cancel_rect.expand(3.0),
                    4.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            painter.text(
                cancel_rect.center(),
                Align2::CENTER_CENTER,
                p.cancel_label,
                FontId::proportional(13.0),
                t.txt_pri,
            );

            match &p.confirm {
                ModalConfirmPrimary::AccentIcon { label, icon } => {
                    let confirm_bg = if confirm_r.hovered() {
                        Color32::from_rgb(
                            t.accent.r().saturating_add(20),
                            t.accent.g().saturating_add(20),
                            t.accent.b().saturating_add(20),
                        )
                    } else {
                        t.accent
                    };
                    painter.rect_filled(confirm_rect, 4.0, confirm_bg);
                    let icon_font = FontId::proportional(13.0);
                    let text_font = FontId::proportional(13.0);
                    let gap = 5.0_f32;
                    let (iw, tw) = ctx.fonts(|f| {
                        let iw = f.glyph_width(&icon_font, *icon);
                        let tw = label
                            .chars()
                            .map(|c| f.glyph_width(&text_font, c))
                            .sum::<f32>();
                        (iw, tw)
                    });
                    let total = iw + gap + tw;
                    let cx = confirm_rect.center().x;
                    let cy = confirm_rect.center().y;
                    let left = cx - total * 0.5;
                    painter.text(
                        Pos2::new(left, cy),
                        Align2::LEFT_CENTER,
                        icon.to_string(),
                        icon_font,
                        t.txt_on_accent,
                    );
                    painter.text(
                        Pos2::new(left + iw + gap, cy),
                        Align2::LEFT_CENTER,
                        *label,
                        text_font,
                        t.txt_on_accent,
                    );
                    if confirm_focused {
                        painter.rect_stroke(
                            confirm_rect.expand(3.0),
                            4.0,
                            Stroke::new(2.0_f32, t.accent),
                            StrokeKind::Outside,
                        );
                    }
                }
                ModalConfirmPrimary::Danger { label } => {
                    let confirm_bg = if confirm_r.hovered() {
                        Color32::from_rgb(180, 35, 20)
                    } else {
                        Color32::from_rgb(196, 43, 28)
                    };
                    painter.rect_filled(confirm_rect, 4.0, confirm_bg);
                    painter.text(
                        confirm_rect.center(),
                        Align2::CENTER_CENTER,
                        *label,
                        FontId::proportional(13.0),
                        Color32::WHITE,
                    );
                    if confirm_focused {
                        painter.rect_stroke(
                            confirm_rect.expand(3.0),
                            4.0,
                            Stroke::new(2.0_f32, t.accent),
                            StrokeKind::Outside,
                        );
                    }
                }
            }

            let kb_cancel = cancel_focused
                && ui.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, Key::Enter)
                        || i.consume_key(Modifiers::NONE, Key::Space)
                });
            let kb_confirm = confirm_focused
                && ui.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, Key::Enter)
                        || i.consume_key(Modifiers::NONE, Key::Space)
                });

            if cancel_r.clicked() {
                *button_focus = Some(0);
                return Some(ModalConfirmResult::Cancel);
            }
            if kb_cancel {
                return Some(ModalConfirmResult::Cancel);
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                return Some(ModalConfirmResult::Cancel);
            }
            if confirm_r.clicked() {
                *button_focus = Some(1);
                return Some(ModalConfirmResult::Confirm);
            }
            if kb_confirm {
                return Some(ModalConfirmResult::Confirm);
            }
            None
        });

    inner.inner
}

pub struct OneButtonModalParams<'a> {
    pub overlay_id: Id,
    pub dialog_id: Id,
    pub width: f32,
    pub height: f32,
    pub title: &'a str,
    pub body: &'a str,
    pub ok_id: Id,
    pub ok_label: &'a str,
}

/// Single OK button — Escape or OK dismisses. `button_focus` is `Some(0)` when OK is focused.
pub fn one_button_modal(
    ctx: &Context,
    t: &Theme,
    p: OneButtonModalParams<'_>,
    button_focus: &mut Option<usize>,
) -> Option<()> {
    let screen = ctx.screen_rect();

    egui::Area::new(p.overlay_id)
        .fixed_pos(screen.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
        });

    let dlg = Rect::from_center_size(screen.center(), Vec2::new(p.width, p.height));

    let inner = egui::Area::new(p.dialog_id)
        .fixed_pos(dlg.min)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            let painter = ui.painter();
            painter.rect_filled(dlg, 8.0, t.bg_pri);
            painter.rect_stroke(dlg, 8.0, Stroke::new(1.5_f32, t.accent), StrokeKind::Middle);

            let btn_h = 30.0_f32;
            let btn_w = 90.0_f32;
            let btn_y = dlg.bottom() - 16.0 - btn_h;

            paint_modal_title_body(painter, dlg, p.title, p.body, t, btn_y);

            let ok_rect = Rect::from_min_size(
                Pos2::new(dlg.right() - 20.0 - btn_w, btn_y),
                Vec2::new(btn_w, btn_h),
            );

            if button_focus.is_none() {
                *button_focus = Some(0);
            }
            let ok_focused = button_focus.unwrap_or(0) == 0;

            ctx.memory_mut(|m| {
                m.request_focus(p.ok_id);
            });

            let ok_r = ui.interact(ok_rect, p.ok_id, INTERACT_MANUAL_FOCUS);

            let ok_bg = if ok_r.hovered() {
                Color32::from_rgb(
                    t.accent.r().saturating_add(20),
                    t.accent.g().saturating_add(20),
                    t.accent.b().saturating_add(20),
                )
            } else {
                t.accent
            };
            painter.rect_filled(ok_rect, 4.0, ok_bg);
            if ok_focused {
                painter.rect_stroke(
                    ok_rect.expand(3.0),
                    4.0,
                    Stroke::new(2.0_f32, t.accent),
                    StrokeKind::Outside,
                );
            }
            painter.text(
                ok_rect.center(),
                Align2::CENTER_CENTER,
                p.ok_label,
                FontId::proportional(13.0),
                t.txt_on_accent,
            );

            let kb_ok = ok_focused
                && ui.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, Key::Enter)
                        || i.consume_key(Modifiers::NONE, Key::Space)
                });

            if ok_r.clicked() || kb_ok {
                return Some(());
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                return Some(());
            }
            None
        });

    inner.inner
}
