//! Shared two-row drive/volume selector and icon refresh button.
//!
//! Every drive-bearing page (Sector Read, Sector Write, Benchmark, Drive Health)
//! draws the same control: an icon-only refresh button on the left that glows on
//! hover and disables/grays while drives reload, then a two-row card — name
//! on row 1, media/bus/size (or free-space) chips on row 2 — with a dropdown
//! caret on the right. The open list repeats the two-row layout per option.
//!
//! Pages build a `Vec<DriveEntry>` (title + precomputed chips), lay out the
//! refresh button and the card on one row using the constants below, and read
//! back the (possibly changed) selection from [`ComboOut`].

use egui::popup::{popup_below_widget, PopupCloseBehavior};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, FontFamily, FontId, Id, Key, Modifiers, Pos2, Rect, Response,
    ScrollArea, Sense, Shape, Stroke, StrokeKind, Vec2,
};

use crate::detected_drive::{BusKind, MediaKind};
use crate::theme::Theme;

/// Total height of the selector row (refresh button + card share this height).
pub(crate) const ROW_H: f32 = 56.0;
/// Square side of the refresh icon button.
pub(crate) const REFRESH_W: f32 = 48.0;
/// Gap between the refresh button and the card.
pub(crate) const REFRESH_GAP: f32 = 12.0;

const CHIP_H: f32 = 26.0;
const CHIP_GAP: f32 = 8.0;
/// Bootstrap Icons `arrow-clockwise` (verified present in the bundled font).
const ICON_REFRESH: &str = "\u{F116}";

/// One row-2 pill: a colored label (media/bus) or the neutral size/free chip.
pub(crate) struct ChipSpec {
    pub label: String,
    pub bg: Color32,
    pub fg: Color32,
}

impl ChipSpec {
    pub(crate) fn media(m: MediaKind) -> Self {
        let (bg, fg) = match m {
            MediaKind::Hdd => (Color32::from_rgb(52, 73, 94), Color32::WHITE),
            MediaKind::Ssd => (Color32::from_rgb(41, 128, 185), Color32::WHITE),
            MediaKind::SdCard => (Color32::from_rgb(39, 174, 96), Color32::WHITE),
            MediaKind::Flash => (Color32::from_rgb(155, 89, 182), Color32::WHITE),
            MediaKind::EMmc => (Color32::from_rgb(26, 188, 156), Color32::WHITE),
            MediaKind::Unknown => (Color32::from_rgb(127, 140, 141), Color32::WHITE),
        };
        Self { label: m.label().to_owned(), bg, fg }
    }

    pub(crate) fn bus(b: BusKind) -> Self {
        let (bg, fg) = match b {
            BusKind::Nvme => (Color32::from_rgb(142, 68, 173), Color32::WHITE),
            BusKind::Sata => (Color32::from_rgb(22, 160, 133), Color32::WHITE),
            BusKind::Usb => (Color32::from_rgb(230, 126, 34), Color32::WHITE),
            BusKind::Ufs => (Color32::from_rgb(52, 152, 219), Color32::WHITE),
        };
        Self { label: b.label().to_owned(), bg, fg }
    }

    /// Neutral pill for the size / free-space chip. Inverts the page text/bg
    /// colors so it reads as a light pill on dark themes (as in the mockup) and a
    /// dark pill on light themes.
    pub(crate) fn neutral(t: &Theme, label: impl Into<String>) -> Self {
        Self { label: label.into(), bg: t.txt_pri, fg: t.bg_pri }
    }
}

/// Tooltip for a row grayed out by the cross-window test lock (KI-17).
pub(crate) const BUSY_ELSEWHERE: &str = "Testing in another window";

/// A selectable drive/volume: title on row 1, chips on row 2.
pub(crate) struct DriveEntry {
    pub title: String,
    pub chips: Vec<ChipSpec>,
    /// `Some(reason)` when the row is shown grayed and unselectable; `reason` is
    /// the hover tooltip. Usually [`BUSY_ELSEWHERE`], but the Benchmark page also
    /// grays the shared drive selection when it has no volume to test (KI-15),
    /// so the reason travels with the entry rather than being assumed.
    pub disabled: Option<&'static str>,
}

/// Result of [`two_row_combo`]: the ComboBox button id (for manual focus
/// binding), the current selection (updated if the user picked a new row), and
/// whether the button was clicked this frame.
pub(crate) struct ComboOut {
    pub id: Id,
    pub selected: usize,
    pub clicked: bool,
}

fn chip_text_w(ctx: &egui::Context, label: &str) -> f32 {
    let galley =
        ctx.fonts(|f| f.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), Color32::WHITE));
    galley.rect.width() + 20.0
}

fn chip_pill(painter: &egui::Painter, rect: Rect, label: &str, bg: Color32, fg: Color32) {
    painter.rect_filled(rect, 6.0, bg);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), fg);
}

/// Paint one entry (title row + chip row) into `rect`. `right_reserve` keeps the
/// title clear of the dropdown caret on the collapsed card (0 inside the popup).
fn paint_entry(
    painter: &egui::Painter,
    ctx: &egui::Context,
    rect: Rect,
    entry: &DriveEntry,
    t: &Theme,
    right_reserve: f32,
) {
    let title_max_w = (rect.width() - 16.0 - right_reserve).max(24.0);
    let galley = ctx.fonts(|f| {
        let mut job = LayoutJob::single_section(
            entry.title.clone(),
            TextFormat::simple(FontId::proportional(14.0), t.txt_pri),
        );
        job.wrap.max_width = title_max_w;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        f.layout_job(job)
    });
    let title_h = galley.size().y;
    painter.galley(
        Pos2::new(rect.left() + 8.0, rect.top() + 13.0 - title_h * 0.5),
        galley,
        t.txt_pri,
    );

    let chip_y = rect.bottom() - 8.0 - CHIP_H;
    let chip_limit = rect.right() - 8.0;
    let mut x = rect.left() + 8.0;
    for c in &entry.chips {
        let w = chip_text_w(ctx, &c.label);
        // Drop the rest rather than paint outside the card. Chips are ordered
        // most- to least-important (size, then media, then bus), so what
        // survives on a phone-width card is the part worth keeping.
        if x + w > chip_limit {
            break;
        }
        let r = Rect::from_min_size(Pos2::new(x, chip_y), Vec2::new(w, CHIP_H));
        chip_pill(painter, r, &c.label, c.bg, c.fg);
        x += w + CHIP_GAP;
    }
}

/// Draw the two-row selector card at `(left, y_row)` spanning `width`.
///
/// The card (background, title, chips, caret) is painted by hand and made
/// clickable with a single `ui.interact` keyed on `combo_id`; the dropdown is
/// egui's `popup_below_widget` opened on the popup id that `ComboBox::is_open`
/// expects (`combo_id.with("popup")`), so the existing manual-focus machinery in
/// `focus.rs` keeps working unchanged.
///
/// Keyboard: Enter/Space/↓ on the focused card opens the list and focuses the
/// current row; egui's geometric arrow-key focus navigation then moves between
/// rows (they are focusable `SelectableLabel`s), Enter/Space selects the focused
/// row, and Escape closes. Each row is overpainted with the two-row layout and
/// gets an accent focus ring. Returns the selection (updated if a row was
/// chosen by mouse or keyboard).
#[allow(clippy::too_many_arguments)]
pub(crate) fn two_row_combo(
    ui: &mut egui::Ui,
    t: &Theme,
    combo_id: Id,
    entries: &[DriveEntry],
    current: usize,
    focused: bool,
    enabled: bool,
    left: f32,
    width: f32,
    y_row: f32,
) -> ComboOut {
    let current = current.min(entries.len().saturating_sub(1));
    let card_rect = Rect::from_min_size(Pos2::new(left, y_row), Vec2::new(width, ROW_H));
    let popup_id = combo_id.with("popup");

    // Whole-card click target (focusable, so Tab/Enter from focus.rs reach it).
    // When disabled it only senses hover so it can't be opened or focused.
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(card_rect, combo_id, sense);

    ui.painter().rect_filled(card_rect, 0.0, t.bg_pri);
    ui.painter().line_segment(
        [card_rect.left_bottom(), card_rect.right_bottom()],
        Stroke::new(1.5_f32, t.accent),
    );

    if let Some(entry) = entries.get(current) {
        paint_entry(ui.painter(), ui.ctx(), card_rect, entry, t, 28.0);
    }

    // Downward caret on the right.
    let cx = card_rect.right() - 16.0;
    let cy = card_rect.center().y;
    ui.painter().add(Shape::convex_polygon(
        vec![
            Pos2::new(cx - 5.0, cy - 3.0),
            Pos2::new(cx + 5.0, cy - 3.0),
            Pos2::new(cx, cy + 4.0),
        ],
        t.txt_pri,
        Stroke::NONE,
    ));

    let mut selected = current;
    let mut clicked = false;

    if enabled {
        // Open/close: mouse click, or Enter/Space/↓ while the card has focus.
        let was_open = ui.memory(|m| m.is_popup_open(popup_id));
        let kb_open = resp.has_focus()
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Space)
                    || (!was_open && i.consume_key(Modifiers::NONE, Key::ArrowDown))
            });
        clicked = resp.clicked();
        if clicked || kb_open {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        // Did this frame open it? If so, focus the current row so arrow keys
        // start from the selection (egui's geometric arrow-nav then moves rows).
        let just_opened = !was_open && ui.memory(|m| m.is_popup_open(popup_id));

        popup_below_widget(ui, popup_id, &resp, PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(width);
            ui.spacing_mut().button_padding = Vec2::ZERO;
            ui.spacing_mut().item_spacing.y = 2.0;
            ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                // The current row is normally the one to focus on open, but it can
                // itself be disabled (a drive locked mid-selection, or the
                // Benchmark page's volume-less selection); fall back to the first
                // row the user can actually reach.
                let focus_idx = if entries.get(current).is_some_and(|e| e.disabled.is_none()) {
                    Some(current)
                } else {
                    entries.iter().position(|e| e.disabled.is_none())
                };
                for (idx, entry) in entries.iter().enumerate() {
                    if let Some(reason) = entry.disabled {
                        // Grayed, not focusable (arrow-nav skips it), not
                        // clickable; a tooltip explains why.
                        let (rect, r) =
                            ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
                        paint_entry(ui.painter(), ui.ctx(), rect, entry, t, 0.0);
                        ui.painter().rect_filled(
                            rect,
                            0.0,
                            Color32::from_rgba_unmultiplied(
                                t.bg_pri.r(),
                                t.bg_pri.g(),
                                t.bg_pri.b(),
                                140,
                            ),
                        );
                        r.on_hover_text(reason);
                        continue;
                    }
                    let r =
                        ui.add_sized([width, ROW_H], egui::SelectableLabel::new(idx == current, ""));
                    if just_opened && Some(idx) == focus_idx {
                        r.request_focus();
                    }
                    if r.gained_focus() {
                        r.scroll_to_me(Some(Align::Center));
                    }
                    paint_entry(ui.painter(), ui.ctx(), r.rect, entry, t, 0.0);
                    // Visible keyboard-focus indicator for the highlighted row.
                    if r.has_focus() {
                        ui.painter().rect_stroke(
                            r.rect.shrink(1.0),
                            4.0,
                            Stroke::new(2.0_f32, t.accent),
                            StrokeKind::Inside,
                        );
                    }
                    if r.clicked() {
                        selected = idx;
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
        });
    } else {
        // Disabled: make sure a stale popup can't linger, and gray the card with
        // a translucent scrim of the panel background.
        if ui.memory(|m| m.is_popup_open(popup_id)) {
            ui.memory_mut(|m| m.close_popup());
        }
        ui.painter().rect_filled(
            card_rect,
            0.0,
            Color32::from_rgba_unmultiplied(t.bg_pri.r(), t.bg_pri.g(), t.bg_pri.b(), 140),
        );
    }

    if focused && enabled {
        ui.painter().rect_stroke(
            card_rect.expand(2.0),
            4.0,
            Stroke::new(2.0_f32, t.accent),
            StrokeKind::Outside,
        );
    }

    ComboOut { id: combo_id, selected, clicked }
}

/// Icon-only refresh button. Glows accent on hover when active; while `busy`
/// (a scan is running or the minimum-visible window hasn't elapsed) it is
/// disabled and grayed out. Returns the response so the caller can store its id
/// (focus) and act on clicks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_button(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    t: &Theme,
    id: Id,
    rect: Rect,
    enabled: bool,
    busy: bool,
    focused: bool,
) -> Response {
    let active = enabled && !busy;
    let sense = if active { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, id, sense);
    let hovered = active && resp.hovered();

    if hovered {
        ui.painter().rect_filled(
            rect,
            8.0,
            Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 48),
        );
    }

    let col = if !active {
        t.txt_sec.gamma_multiply(0.5)
    } else if hovered {
        t.accent
    } else {
        t.txt_pri
    };
    let font = FontId::new(26.0, FontFamily::Proportional);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, ICON_REFRESH, font, col);

    if focused && active {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            4.0,
            Stroke::new(2.0_f32, t.accent),
            StrokeKind::Outside,
        );
    }

    // While the minimum-visible window is counting down (drives may have already
    // finished loading) keep the loop awake so the button re-enables on time.
    if busy {
        ctx.request_repaint();
    }

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_and_bus_chips_use_kind_labels() {
        assert_eq!(ChipSpec::media(MediaKind::Ssd).label, "SSD");
        assert_eq!(ChipSpec::media(MediaKind::EMmc).label, "eMMC");
        assert_eq!(ChipSpec::bus(BusKind::Nvme).label, "NVMe");
        assert_eq!(ChipSpec::bus(BusKind::Ufs).label, "UFS");
    }

    #[test]
    fn neutral_chip_inverts_theme_text_and_bg() {
        let t = Theme::new(true, Color32::from_rgb(0, 120, 215));
        let chip = ChipSpec::neutral(&t, "954 GB");
        assert_eq!(chip.label, "954 GB");
        assert_eq!(chip.bg, t.txt_pri);
        assert_eq!(chip.fg, t.bg_pri);
    }
}
