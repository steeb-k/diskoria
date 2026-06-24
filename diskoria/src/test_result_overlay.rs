//! Full-window PASS / WARN / FAIL result overlay shown over a test window.
//!
//! When the sector or destructive test finds a bad block (FAIL) or finishes a
//! complete scan (PASS / WARN), the window is dimmed and a large status word is
//! printed across it. Any key press or a click anywhere dismisses it; for a
//! FAIL raised mid-scan the underlying test keeps running.

use egui::{Align2, Color32, Context, FontFamily, FontId, Pos2, Sense, Vec2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestResult {
    Pass,
    Warn,
    Fail,
}

impl TestResult {
    fn word(self) -> &'static str {
        match self {
            TestResult::Pass => "PASS",
            TestResult::Warn => "WARN",
            TestResult::Fail => "FAIL",
        }
    }

    fn color(self) -> Color32 {
        match self {
            TestResult::Pass => Color32::from_rgb(46, 204, 113),
            TestResult::Warn => Color32::from_rgb(241, 196, 15),
            TestResult::Fail => Color32::from_rgb(231, 53, 43),
        }
    }
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("InterBold".into()))
}

/// Draws the dim + status word + hint for one frame. Returns `true` when a click
/// anywhere dismissed it. Keyboard dismissal is handled earlier in the frame (so
/// the dismiss key can't also activate a focused control underneath).
///
/// A FAIL is raised mid-scan, so its hint reminds the user the test is still
/// running; a completed PASS/WARN simply says "continue".
pub fn draw_test_result_overlay(ctx: &Context, result: TestResult) -> bool {
    let screen = ctx.screen_rect();
    let id = egui::Id::new("diskoria_test_result_overlay");
    let outline = Color32::from_rgb(20, 20, 22);

    let clicked = egui::Area::new(id)
        .fixed_pos(screen.min)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let painter = ui.painter();

            // Dim the whole window (matches the app's modal overlays).
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));

            // Scale the status word to ~85% of the window width, capped by height.
            let word = result.word();
            let ref_galley = painter.layout_no_wrap(word.to_string(), bold(100.0), result.color());
            let ref_w = ref_galley.size().x.max(1.0);
            let size = (100.0 * (screen.width() * 0.85 / ref_w))
                .min(screen.height() * 0.55)
                .max(24.0);
            let font = bold(size);
            let center = Pos2::new(screen.center().x, screen.center().y - size * 0.15);

            // Faux outline: dark copies offset around a colored fill.
            let o = (size * 0.03).max(1.5);
            for (dx, dy) in [
                (-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o),
                (-o, -o), (o, -o), (-o, o), (o, o),
            ] {
                painter.text(
                    center + Vec2::new(dx, dy),
                    Align2::CENTER_CENTER,
                    word,
                    font.clone(),
                    outline,
                );
            }
            painter.text(center, Align2::CENTER_CENTER, word, font.clone(), result.color());

            // Hint below the word, in the same color with a thin outline. Each
            // line is drawn separately so it is individually centered (a single
            // multi-line string would left-align the lines within the block).
            // FAIL fires mid-scan, so it reassures the user the test continues.
            let hint_lines: &[&str] = match result {
                TestResult::Fail => {
                    &["Press any key to continue.", "(Test has not been stopped.)"]
                }
                TestResult::Warn => &[
                    "Slow blocks were found \u{2014} see the Performance Chart tab.",
                    "Press any key to continue.",
                ],
                TestResult::Pass => &["Press any key to continue."],
            };
            let hint_size = (size * 0.16).clamp(14.0, 28.0);
            let hint_font = bold(hint_size);
            let line_h = hint_size * 1.3;
            let first_y = center.y + size * 0.5 + line_h;
            for (li, line) in hint_lines.iter().enumerate() {
                let pos = Pos2::new(center.x, first_y + li as f32 * line_h);
                for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
                    painter.text(
                        pos + Vec2::new(dx, dy),
                        Align2::CENTER_CENTER,
                        *line,
                        hint_font.clone(),
                        outline,
                    );
                }
                painter.text(pos, Align2::CENTER_CENTER, *line, hint_font.clone(), result.color());
            }

            // Full-window click target consumes the dismissing click so it can't
            // fall through to the controls underneath.
            ui.interact(screen, id.with("eat"), Sense::click()).clicked()
        })
        .inner;

    clicked
}
