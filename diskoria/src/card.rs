//! Standardized card layout — the rounded panel every page stacks vertically.
//!
//! Cards used to be painted entirely by hand: each site computed a `card_h` from
//! a chain of magic addends (`pad + 22.0 + 12.0 + row_h + row_h + pad`), called
//! `ui.allocate_space(card_h + N)` to reserve the space, painted the frame at
//! that height, then painted its rows at absolute `y` coordinates. Two failure
//! modes kept recurring (known-issues KI-18):
//!
//! **(a) the hand-counted height drifts from the real content** as rows are
//! added, removed, or skipped by a branch — leaving a card padded out with empty
//! space, or clipped.
//!
//! **(b) a child `allocate_rect` rewinds the layout cursor.** `Ui::allocate_space`
//! and `Ui::advance_cursor_after_rect` both funnel into
//! `Placer::advance_after_rects`, which sets the cursor *unconditionally* from
//! the rect — so it moves **backward** when the rect ends above the current
//! cursor. Any widget inside a card that allocates (the Settings sliders via
//! `allocate_rect`, the accent hex field via `ui.put`) therefore discarded the
//! card's whole reservation, and the next card painted on top of this one.
//! `expand_to_include_rect` only ever *grows* `min_rect`, which is why the
//! symptom was a cursor problem and never a scroll-height problem.
//!
//! [`CardLayout`] removes both by construction:
//!
//! - Rows are handed out by [`CardLayout::row`], which accumulates the true
//!   height as it goes — nothing to keep in sync.
//! - The frame is painted *last*, at the measured height, into placeholder
//!   shapes reserved at `begin` time (the idiom egui's own `Frame` uses). The
//!   shapes keep their original position in the paint list, so the frame still
//!   renders underneath the row content.
//! - [`CardLayout::end`] performs the one and only cursor advance. Being last,
//!   it overrides any rewind a widget inside the card caused.
//!
//! `row()` deliberately takes neither `ui` nor a closure: card bodies mutate
//! `DiskoriaApp` freely, and a closure capturing `&mut self` while the card is
//! alive would fight the borrow checker at every call site. `CardLayout` borrows
//! nothing — it is two `ShapeIdx` and some `f32` — so `card.row(h)` and
//! `self.whatever()` coexist.

use egui::epaint::RectShape;
use egui::layers::ShapeIdx;
use egui::{Color32, Pos2, Rect, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{
    Theme, CARD_BORDER_W, CARD_GAP, CARD_PAD, CARD_RADIUS, CARD_TITLE_GAP, CARD_TITLE_H,
};

/// Lead gap to leave above a card so the *on-screen* gap is `gap_before`.
///
/// `advance_cursor_after_rect` appends `item_spacing.y` after the rect it is
/// given, so the previous card's `end()` has already contributed that much —
/// subtract it or every gap comes out too big. Same gotcha the manual
/// `add_space` calls in `smart_health_page.rs` work around.
fn lead_gap(gap_before: f32, item_spacing_y: f32) -> f32 {
    (gap_before - item_spacing_y).max(0.0)
}

/// Pure geometry half of a card — no `Ui`, no painting, so it can be unit tested.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardGeom {
    left: f32,
    width: f32,
    pad: f32,
    /// Cursor position when the card started, *before* the lead gap.
    outer_top: f32,
    /// Top edge of the painted frame.
    top: f32,
    /// Running content baseline: bottom of the last thing added.
    y: f32,
}

impl CardGeom {
    fn new(left: f32, width: f32, pad: f32, outer_top: f32, lead: f32) -> Self {
        let top = outer_top + lead;
        Self {
            left,
            width,
            pad,
            outer_top,
            top,
            y: top + pad,
        }
    }

    fn inner_x(&self) -> f32 {
        self.left + self.pad
    }

    fn inner_w(&self) -> f32 {
        (self.width - self.pad * 2.0).max(0.0)
    }

    /// Reserve the title row and return the rect it occupies.
    fn title_row(&mut self) -> Rect {
        let r = Rect::from_min_size(
            Pos2::new(self.inner_x(), self.y),
            Vec2::new(self.inner_w(), CARD_TITLE_H),
        );
        self.y += CARD_TITLE_H + CARD_TITLE_GAP;
        r
    }

    fn row(&mut self, h: f32) -> Rect {
        let r = Rect::from_min_size(Pos2::new(self.inner_x(), self.y), Vec2::new(self.inner_w(), h));
        self.y += h;
        r
    }

    fn add_gap(&mut self, h: f32) {
        self.y += h;
    }

    /// The card frame, sized to everything added so far plus the bottom padding.
    fn rect(&self) -> Rect {
        Rect::from_min_size(
            Pos2::new(self.left, self.top),
            Vec2::new(self.width, (self.y + self.pad - self.top).max(0.0)),
        )
    }
}

/// Configure a card before it starts painting. See [`CardLayout::new`].
pub(crate) struct CardBuilder<'a> {
    left: f32,
    width: f32,
    pad: f32,
    gap_before: f32,
    title: Option<&'a str>,
}

impl<'a> CardBuilder<'a> {
    /// Inner padding on all four sides. Defaults to [`CARD_PAD`].
    #[cfg_attr(not(windows), allow(dead_code))] // callers are Windows-gated cards today
    pub fn pad(mut self, pad: f32) -> Self {
        self.pad = pad;
        self
    }

    /// Vertical gap above this card. Defaults to [`CARD_GAP`]; pass `0.0` for
    /// the first card on a page, which sits directly under the page's own
    /// leading `add_space`.
    pub fn gap_before(mut self, gap: f32) -> Self {
        self.gap_before = gap;
        self
    }

    /// Bold section heading painted as the card's first row.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// Reserve the frame shapes, apply the lead gap, and paint the title.
    pub fn begin(self, ui: &mut Ui, t: &Theme) -> CardLayout {
        // Mirror what `allocate_space(Vec2::new(ui.available_width(), h))` would
        // have reserved, captured now because both shrink as content is added.
        let outer_left = ui.cursor().left();
        let outer_width = ui.available_width();
        let outer_top = ui.cursor().min.y;
        let lead = lead_gap(self.gap_before, ui.spacing().item_spacing.y);

        // Two placeholders, filled in by `end()` once the height is known. Order
        // matters: they hold this spot in the paint list, so the frame lands
        // under everything the card draws afterwards. Two shapes rather than one
        // `RectShape` with both fill and stroke, so this is byte-identical to
        // the `rect_filled` + `rect_stroke` pair it replaces.
        let bg = ui.painter().add(Shape::Noop);
        let border = ui.painter().add(Shape::Noop);

        let mut card = CardLayout {
            geom: CardGeom::new(self.left, self.width, self.pad, outer_top, lead),
            outer_left,
            outer_width,
            bg,
            border,
            fill: t.bg_pri,
            stroke: Stroke::new(CARD_BORDER_W, t.border),
        };

        if let Some(title) = self.title {
            card.section_title(ui, t, title);
        }

        card
    }
}

/// A card being laid out. Add rows, then call [`CardLayout::end`].
pub(crate) struct CardLayout {
    geom: CardGeom,
    outer_left: f32,
    outer_width: f32,
    bg: ShapeIdx,
    border: ShapeIdx,
    fill: Color32,
    stroke: Stroke,
}

impl CardLayout {
    /// Start describing a card at `left` with the given width. Configure it,
    /// then call [`CardBuilder::begin`] to start laying rows out.
    pub fn builder<'a>(left: f32, width: f32) -> CardBuilder<'a> {
        CardBuilder {
            left,
            width,
            pad: CARD_PAD,
            gap_before: CARD_GAP,
            title: None,
        }
    }

    /// Paint a bold section heading and reserve its row plus the gap below it.
    ///
    /// `begin` calls this for the card's own title; call it again for a card
    /// that holds several titled sections behind one frame (Settings → Theme,
    /// which also carries "Accent").
    pub fn section_title(&mut self, ui: &Ui, t: &Theme, title: &str) -> Rect {
        let row = self.geom.title_row();
        ui.painter().text(
            Pos2::new(row.left(), row.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::new(14.0, egui::FontFamily::Name("InterBold".into())),
            t.txt_pri,
        );
        row
    }

    /// Reserve the next row and return its rect. Rows are contiguous; the card
    /// grows to fit whatever it is handed.
    pub fn row(&mut self, h: f32) -> Rect {
        self.geom.row(h)
    }

    /// Reserve vertical space without a row rect (spacing between groups).
    pub fn add_gap(&mut self, h: f32) {
        self.geom.add_gap(h);
    }

    /// Left edge of the content column (card left + padding).
    pub fn inner_x(&self) -> f32 {
        self.geom.inner_x()
    }

    /// Width of the content column.
    #[cfg_attr(not(windows), allow(dead_code))] // callers are Windows-gated cards today
    pub fn inner_w(&self) -> f32 {
        self.geom.inner_w()
    }

    /// Left edge of the card frame (outside the padding).
    pub fn left(&self) -> f32 {
        self.geom.left
    }

    /// Right edge of the card frame — the anchor for right-aligned controls.
    pub fn right(&self) -> f32 {
        self.geom.left + self.geom.width
    }

    /// This card's inner padding.
    pub fn pad(&self) -> f32 {
        self.geom.pad
    }

    /// Paint the frame at the measured height and advance the cursor once.
    ///
    /// This is the authoritative advance: it runs after anything inside the card
    /// that may have rewound the cursor, so the next card starts below this one
    /// whatever happened in between (KI-18).
    pub fn end(self, ui: &mut Ui) -> Rect {
        let rect = self.geom.rect();

        ui.painter()
            .set(self.bg, RectShape::filled(rect, CARD_RADIUS, self.fill));
        ui.painter().set(
            self.border,
            RectShape::stroke(rect, CARD_RADIUS, self.stroke, StrokeKind::Middle),
        );

        let outer = Rect::from_min_size(
            Pos2::new(self.outer_left, self.geom.outer_top),
            Vec2::new(self.outer_width, (rect.bottom() - self.geom.outer_top).max(0.0)),
        );
        ui.advance_cursor_after_rect(outer);

        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(pad: f32, gap_before: f32, item_spacing_y: f32) -> CardGeom {
        CardGeom::new(100.0, 400.0, pad, 50.0, lead_gap(gap_before, item_spacing_y))
    }

    #[test]
    fn lead_gap_compensates_item_spacing() {
        // The previous card's `end()` already appended item_spacing, so the card
        // only needs to add the remainder to land on a CARD_GAP on-screen gap.
        assert_eq!(lead_gap(12.0, 3.0), 9.0);
        assert_eq!(lead_gap(12.0, 3.0) + 3.0, CARD_GAP);
        // Never negative, however large the spacing.
        assert_eq!(lead_gap(12.0, 20.0), 0.0);
        // The first card on a page opts out entirely.
        assert_eq!(lead_gap(0.0, 3.0), 0.0);
    }

    #[test]
    fn height_is_pad_title_rows_pad() {
        let mut g = geom(16.0, 0.0, 3.0);
        g.title_row();
        g.row(34.0);
        g.row(34.0);
        assert_eq!(
            g.rect().height(),
            16.0 + CARD_TITLE_H + CARD_TITLE_GAP + 34.0 + 34.0 + 16.0
        );
    }

    #[test]
    fn rows_added_later_grow_the_card() {
        // The (a) invariant: the frame follows the content, it isn't predicted.
        let mut g = geom(16.0, 0.0, 3.0);
        g.title_row();
        g.row(34.0);
        let two_rows = g.rect().height();
        g.row(34.0);
        assert_eq!(g.rect().height(), two_rows + 34.0);
    }

    #[test]
    fn titleless_card_is_just_padding_and_rows() {
        let mut g = geom(16.0, 0.0, 3.0);
        g.row(70.0);
        assert_eq!(g.rect().height(), 16.0 + 70.0 + 16.0);
    }

    #[test]
    fn empty_card_collapses_to_padding_only() {
        let g = geom(16.0, 0.0, 3.0);
        let r = g.rect();
        assert_eq!(r.height(), 32.0);
        assert!(!r.any_nan());
        assert!(r.height() >= 0.0);
    }

    #[test]
    fn rows_are_contiguous_and_inside_the_frame() {
        let mut g = geom(16.0, 12.0, 3.0);
        g.title_row();
        let a = g.row(34.0);
        let b = g.row(40.0);
        g.add_gap(8.0);
        let c = g.row(24.0);
        let card = g.rect();

        assert_eq!(a.bottom(), b.top(), "rows must not overlap or gap");
        assert_eq!(b.bottom() + 8.0, c.top(), "add_gap must separate rows");
        for r in [a, b, c] {
            assert!(card.contains_rect(r), "{r:?} escaped {card:?}");
            assert_eq!(r.left(), card.left() + 16.0);
            assert_eq!(r.right(), card.right() - 16.0);
        }
        assert_eq!(card.bottom() - c.bottom(), 16.0, "bottom padding");
    }

    #[test]
    fn frame_starts_below_the_lead_gap() {
        let g = geom(16.0, 12.0, 3.0);
        assert_eq!(g.outer_top, 50.0);
        assert_eq!(g.rect().top(), 59.0); // 50 + (12 - 3)
    }

    #[test]
    fn custom_pad_applies_to_both_axes() {
        let mut g = geom(10.0, 0.0, 3.0);
        let r = g.row(20.0);
        let card = g.rect();
        assert_eq!(card.height(), 10.0 + 20.0 + 10.0);
        assert_eq!(r.width(), 400.0 - 20.0);
    }
}
