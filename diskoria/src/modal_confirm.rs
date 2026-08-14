//! Two-button confirm dialog (overlay + card + Tab between buttons) — aligned with Copynaut `ui/composites.rs`.

use egui::{
    Align2, Color32, Context, FontFamily, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};

use crate::chrome::INTERACT_MANUAL_FOCUS;
use crate::theme::Theme;

const H_PAD: f32 = 20.0;
const TOP_PAD: f32 = 20.0;
const TITLE_BODY_GAP: f32 = 10.0;
const BODY_BTN_GAP: f32 = 12.0;
/// Height of a modal's button row, and its inset from the dialog's bottom.
const BTN_H: f32 = 30.0;
const BTN_BOTTOM_PAD: f32 = 16.0;
/// Gap kept between the dialog and the window edge when the dialog has to be
/// shrunk to fit.
const SCREEN_MARGIN: f32 = 12.0;

/// Fit a caller's preferred dialog size to the window.
///
/// Callers pass a size chosen for a desktop window. Once the window can be
/// 380px wide, a 420px dialog hangs off both edges and its buttons fall outside
/// the screen (known-issues KI-52). Width is capped at the window; the text
/// then wraps to more lines, so the height *grows* to fit it — capped at the
/// window in turn, since a clipped dialog is still better than one whose OK
/// button is off-screen.
fn fit_dialog(ctx: &Context, screen: Rect, title: &str, body: &str, want: Vec2) -> Rect {
    let w = fit_dialog_w(screen.width(), want.x);
    let text_h = modal_text_h(ctx, title, body, w);
    let h = fit_dialog_h(screen.height(), want.y, text_h);
    Rect::from_center_size(screen.center(), Vec2::new(w, h))
}

/// Dialog width for a window `screen_w` wide, given the caller's preference.
fn fit_dialog_w(screen_w: f32, want_w: f32) -> f32 {
    want_w.min((screen_w - 2.0 * SCREEN_MARGIN).max(1.0))
}

/// Dialog height, given the height its wrapped text needs. Split from
/// [`fit_dialog`] so the sizing rule can be tested without a font atlas.
fn fit_dialog_h(screen_h: f32, want_h: f32, text_h: f32) -> f32 {
    let needed = TOP_PAD + text_h + BODY_BTN_GAP + BTN_H + BTN_BOTTOM_PAD;
    want_h
        .max(needed)
        .min((screen_h - 2.0 * SCREEN_MARGIN).max(1.0))
}

/// Height the title and body take when wrapped to a dialog `dlg_w` wide.
fn modal_text_h(ctx: &Context, title: &str, body: &str, dlg_w: f32) -> f32 {
    let text_w = (dlg_w - 2.0 * H_PAD).max(1.0);
    let title_font = FontId::new(15.0, FontFamily::Name("InterBold".into()));
    let body_font = FontId::proportional(13.0);
    ctx.fonts(|f| {
        let th = f
            .layout(title.to_owned(), title_font, Color32::WHITE, text_w)
            .size()
            .y;
        let bh = f
            .layout(body.to_owned(), body_font, Color32::WHITE, text_w)
            .size()
            .y;
        th + TITLE_BODY_GAP + bh
    })
}

fn paint_modal_title_body(
    painter: &egui::Painter,
    dlg: Rect,
    title: &str,
    body: &str,
    t: &Theme,
    btn_y: f32,
) {
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

/// Dim the window and swallow every pointer event aimed at what's underneath.
///
/// The dim alone was not enough: `rect_filled` on an `Area`'s painter allocates
/// nothing, so egui's hit-test saw a zero-sized layer and kept handing hovers
/// and clicks to the page behind the dialog. Tooltips were the visible symptom —
/// they kept popping up over a modal, from controls the user could not reach
/// (known-issues KI-61). One full-window widget in the overlay's own layer is
/// what blocks them: egui drops every layer behind a hit that covers the whole
/// hit-area.
///
/// `Order::Middle` is load-bearing. The title bar is an `Order::Foreground`
/// area, so it stays live above this — a modal must never trap the window with
/// no way to move, minimize or close it.
fn modal_scrim(ctx: &Context, screen: Rect, overlay_id: Id) {
    egui::Area::new(overlay_id)
        .fixed_pos(screen.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            // click_and_drag, not click: a drag that starts on the scrim must
            // not reach a plot or a slider underneath either.
            ui.interact(screen, overlay_id.with("eat"), Sense::click_and_drag());
        });
}

/// `button_focus`: `Some(0)` = Cancel, `Some(1)` = Confirm. Tab order from [`crate::focus::tab_cycle_slots`] in `App::update`.
pub fn two_button_modal(
    ctx: &Context,
    t: &Theme,
    p: TwoButtonModalParams<'_>,
    button_focus: &mut Option<usize>,
) -> Option<ModalConfirmResult> {
    let screen = ctx.screen_rect();

    modal_scrim(ctx, screen, p.overlay_id);

    let dlg = fit_dialog(ctx, screen, p.title, p.body, Vec2::new(p.width, p.height));

    let inner = egui::Area::new(p.dialog_id)
        .fixed_pos(dlg.min)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            let painter = ui.painter();
            painter.rect_filled(dlg, 8.0, t.bg_pri);
            painter.rect_stroke(dlg, 8.0, Stroke::new(1.5_f32, t.accent), StrokeKind::Middle);

            let btn_h = BTN_H;
            let btn_y = dlg.bottom() - BTN_BOTTOM_PAD - btn_h;
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

    modal_scrim(ctx, screen, p.overlay_id);

    let dlg = fit_dialog(ctx, screen, p.title, p.body, Vec2::new(p.width, p.height));

    let inner = egui::Area::new(p.dialog_id)
        .fixed_pos(dlg.min)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            let painter = ui.painter();
            painter.rect_filled(dlg, 8.0, t.bg_pri);
            painter.rect_stroke(dlg, 8.0, Stroke::new(1.5_f32, t.accent), StrokeKind::Middle);

            let btn_h = BTN_H;
            let btn_w = 90.0_f32;
            let btn_y = dlg.bottom() - BTN_BOTTOM_PAD - btn_h;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest window the app allows (`lib.rs` `with_min_inner_size`).
    const MIN_WIN_W: f32 = 380.0;
    const MIN_WIN_H: f32 = 480.0;
    /// Room the button row needs: two buttons at up to ~150px, the 8px between
    /// them and the 20px inset each side.
    const BUTTON_ROW_BUDGET: f32 = 300.0;

    #[test]
    fn a_desktop_window_gets_exactly_what_the_caller_asked_for() {
        // 420x200 is the shape the confirm dialogs pass. Nothing about the
        // fitting rule may move them on a normal window.
        assert_eq!(fit_dialog_w(1100.0, 420.0), 420.0);
        assert_eq!(fit_dialog_h(700.0, 200.0, 60.0), 200.0);
    }

    #[test]
    fn the_narrowest_window_still_fits_the_dialog_and_its_buttons() {
        let w = fit_dialog_w(MIN_WIN_W, 420.0);
        assert!(w <= MIN_WIN_W - 2.0 * SCREEN_MARGIN, "dialog {w}px overhangs");
        assert!(
            w >= BUTTON_ROW_BUDGET,
            "only {w}px for a button row needing {BUTTON_ROW_BUDGET}px"
        );
    }

    #[test]
    fn a_dialog_grows_when_its_text_wraps_to_more_lines() {
        // Narrowing wraps the body, so the caller's height stops being enough.
        // Same content, three times the lines: the dialog has to grow, or the
        // clip rect eats the sentence that says the data is unrecoverable.
        let one_line = fit_dialog_h(MIN_WIN_H, 200.0, 60.0);
        let many_lines = fit_dialog_h(MIN_WIN_H, 200.0, 180.0);
        assert_eq!(one_line, 200.0);
        assert!(many_lines > one_line);
        assert!(many_lines >= TOP_PAD + 180.0 + BODY_BTN_GAP + BTN_H + BTN_BOTTOM_PAD);
    }

    #[test]
    fn growth_stops_at_the_window_edge() {
        // A body long enough to want more than the window gets capped rather
        // than hanging its OK button off the bottom of the screen.
        let h = fit_dialog_h(MIN_WIN_H, 200.0, 2000.0);
        assert!(h <= MIN_WIN_H - 2.0 * SCREEN_MARGIN);
    }

    /// Runs the pointer parked at `pointer` through a headless egui context and
    /// reports whether a widget in the central panel and one in an
    /// `Order::Tooltip` area (standing in for the dialog) were hovered.
    ///
    /// Three frames, not one: egui resolves hover against the *previous* pass's
    /// widget rects, and an `Area` only gets a real size after its first layout,
    /// so a two-frame probe reports no hover for anything in an area.
    fn hover_probe(pointer: Pos2, with_scrim: bool) -> (bool, bool) {
        let ctx = Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        // The page widget spans the dialog, so "hovered the page" and "hovered
        // the dialog" can both be asked at the same pointer position.
        let behind_rect = Rect::from_min_size(Pos2::new(0.0, 120.0), Vec2::new(800.0, 400.0));
        let dialog_rect = Rect::from_min_size(Pos2::new(300.0, 240.0), Vec2::new(200.0, 80.0));
        let (mut behind, mut dialog) = (false, false);
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    behind = ui
                        .interact(behind_rect, Id::new("probe_behind"), Sense::click())
                        .hovered();
                });
                if with_scrim {
                    modal_scrim(ctx, screen, Id::new("probe_scrim"));
                }
                egui::Area::new(Id::new("probe_dialog"))
                    .fixed_pos(dialog_rect.min)
                    .order(egui::Order::Tooltip)
                    .show(ctx, |ui| {
                        dialog = ui
                            .interact(dialog_rect, Id::new("probe_dialog_btn"), Sense::click())
                            .hovered();
                    });
            });
        }
        (behind, dialog)
    }

    #[test]
    fn the_scrim_takes_the_hover_away_from_the_page_behind_it() {
        // KI-61: dimming the window is not blocking it. Without a widget in the
        // overlay's layer, egui kept hovering the controls underneath and their
        // tooltips kept popping up over the dialog.
        let (behind, _) = hover_probe(Pos2::new(120.0, 280.0), false);
        assert!(behind, "probe is wrong: the page should hover with no scrim");

        let (behind, _) = hover_probe(Pos2::new(120.0, 280.0), true);
        assert!(!behind, "the page behind the scrim is still hovering");
    }

    #[test]
    fn the_scrim_does_not_block_the_dialog_it_sits_under() {
        // The other direction: a scrim that ate the dialog's own buttons would
        // make every modal unclickable.
        let (behind, dialog) = hover_probe(Pos2::new(400.0, 280.0), true);
        assert!(dialog, "the dialog's own button stopped hovering");
        assert!(!behind, "the page under the dialog is still hovering");
    }

    #[test]
    fn degenerate_window_sizes_stay_positive() {
        // A zero-sized surface (minimized, or mid-resize) must not produce a
        // negative rect — `Rect::from_center_size` would silently invert it.
        for (sw, sh) in [(0.0_f32, 0.0_f32), (1.0, 1.0), (20.0, 20.0)] {
            assert!(fit_dialog_w(sw, 420.0) > 0.0);
            assert!(fit_dialog_h(sh, 200.0, 60.0) > 0.0);
        }
    }
}
