//! Small egui helpers (from rust-egui-winui-example `ui::widgets`).

use egui::{Align2, FontId, Id, Pos2, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::Theme;

/// Compact bordered button (Browse / Check for updates style), from Copynaut `small_browse_style_button`.
pub fn small_browse_style_button(
    ui: &mut Ui,
    t: &Theme,
    id: Id,
    icon: char,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let row_h = 34.0_f32;
    let pad_x = 10.0_f32;
    let gap = 5.0_f32;
    let icon_font = FontId::proportional(13.0);
    let text_font = FontId::proportional(13.0);
    let (iw, tw) = ui.ctx().fonts(|f| {
        (
            f.glyph_width(&icon_font, icon),
            label
                .chars()
                .map(|c| f.glyph_width(&text_font, c))
                .sum::<f32>(),
        )
    });
    let btn_w = (pad_x * 2.0 + iw + gap + tw).ceil().max(72.0);

    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };

    let (_, rect) = ui.allocate_space(Vec2::new(btn_w, row_h));
    let response = ui.interact(rect, id, sense);

    let btn_bg = if enabled {
        if response.hovered() {
            t.hover
        } else {
            t.bg_sec
        }
    } else {
        t.bg_sec
    };
    let btn_fg = if enabled { t.txt_pri } else { t.txt_sec };
    let btn_border = t.border;

    ui.painter().rect_filled(rect, 4.0, btn_bg);
    ui.painter().rect_stroke(
        rect,
        4.0,
        Stroke::new(1.5, btn_border),
        StrokeKind::Middle,
    );

    let cx = rect.center().x;
    let cy = rect.center().y;
    let total = iw + gap + tw;
    let left = cx - total * 0.5;
    ui.painter().text(
        Pos2::new(left, cy),
        Align2::LEFT_CENTER,
        icon.to_string(),
        icon_font,
        btn_fg,
    );
    ui.painter().text(
        Pos2::new(left + iw + gap, cy),
        Align2::LEFT_CENTER,
        label,
        text_font,
        btn_fg,
    );

    if !enabled && response.hovered() {
        ui.ctx()
            .set_cursor_icon(egui::CursorIcon::NotAllowed);
    }

    response
}

pub fn show_tooltip_text(ctx: &egui::Context, id: Id, anchor_pos: Pos2, t: &Theme, text: &str) {
    let screen = ctx.screen_rect();
    let pad = 8.0_f32;
    let font = FontId::proportional(13.0);
    let galley = ctx.fonts(|f| f.layout_no_wrap(text.to_owned(), font, t.txt_pri));
    let body = galley.rect.size();
    let size = Vec2::new(body.x + pad * 2.0, body.y + pad * 2.0);
    let offset = Vec2::new(15.0, 15.0);
    let mut pos = anchor_pos + offset;
    if pos.x + size.x > screen.right() {
        pos.x = anchor_pos.x - offset.x - size.x;
    }
    if pos.y + size.y > screen.bottom() {
        pos.y = anchor_pos.y - offset.y - size.y;
    }
    pos.x = pos
        .x
        .min(screen.right() - 2.0 - size.x)
        .max(screen.left() + 2.0);
    pos.y = pos
        .y
        .min(screen.bottom() - 2.0 - size.y)
        .max(screen.top() + 2.0);

    egui::Area::new(id)
        .fixed_pos(pos)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            let (rect, _resp) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(rect, 4.0, t.bg_sec);
            ui.painter()
                .rect_stroke(rect, 4.0, Stroke::new(1.0, t.accent), StrokeKind::Middle);
            let text_pos = rect.min + Vec2::splat(pad);
            ui.painter()
                .add(egui::Shape::galley(text_pos, galley, t.txt_pri));
        });
}
