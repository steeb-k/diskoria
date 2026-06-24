//! Manual keyboard focus for custom egui layouts (tab order + `request_focus` on widget ids).
//! egui's automatic focus traversal is not used — see `docs/gui-architecture.md`.

use egui::{ComboBox, Context, EventFilter, Id, Key, Modifiers, Rect, Ui};

/// Syncs egui focus when the manual slot matches `slot` and `id` is the widget to focus.
pub fn bind_text_focus_slot(ctx: &Context, focused_slot: Option<usize>, slot: usize, id: Option<Id>) {
    if let Some(wid) = id {
        if focused_slot == Some(slot) {
            if !ctx.memory(|m| m.has_focus(wid)) {
                ctx.memory_mut(|m| m.request_focus(wid));
            }
        } else if ctx.memory(|m| m.has_focus(wid)) {
            ctx.memory_mut(|m| m.surrender_focus(wid));
        }
    }
}

pub fn bind_combo_focus_slot(ctx: &Context, focused_slot: Option<usize>, slot: usize, id: Option<Id>) {
    if let Some(wid) = id {
        if focused_slot == Some(slot) {
            let popup_open = ComboBox::is_open(ctx, wid);
            if !popup_open && !ctx.memory(|m| m.has_focus(wid)) {
                ctx.memory_mut(|m| m.request_focus(wid));
            }
        } else {
            // `Memory::is_popup_open` treats *every* popup as open when `everything_is_visible` is on
            // (egui debug / inspection). Using that here would call `close_popup` on inactive slots
            // whenever *any* combo is open, killing arrow navigation in the active dropdown.
            let close_inactive = ctx.memory(|m| {
                !m.everything_is_visible() && m.is_popup_open(wid.with("popup"))
            });
            if close_inactive {
                ctx.memory_mut(|m| m.close_popup());
            }
            // While any popup is open, do not surrender inactive slots here: another combo's list
            // may be active and `surrender_focus` runs *after* widgets process input, too late to fix
            // Enter/Space routing. Cross-combo cleanup runs earlier in the frame (see app).
            let no_popup_open = ctx.memory(|m| {
                !m.everything_is_visible() && !m.any_popup_open()
            });
            if no_popup_open && ctx.memory(|m| m.has_focus(wid)) {
                ctx.memory_mut(|m| m.surrender_focus(wid));
            }
        }
    }
}

/// After manual focus binding, lock Tab so egui does not run its default Tab focus pass (we use
/// [`tab_cycle_slots`] instead). For non-combo controls, pass `lock_arrows: true` so egui does not
/// move focus with arrow keys. For **ComboBox button** ids, pass `lock_arrows: false` (tab only);
/// locking vertical arrows on the button breaks list navigation after the popup opens.
pub fn apply_manual_focus_event_filter(ctx: &Context, widget_id: Option<Id>, lock_arrows: bool) {
    let Some(id) = widget_id else {
        return;
    };
    ctx.memory_mut(|m| {
        m.set_focus_lock_filter(
            id,
            EventFilter {
                tab: true,
                horizontal_arrows: lock_arrows,
                vertical_arrows: lock_arrows,
                escape: false,
            },
        );
    });
}

pub fn tab_cycle_slots(ctx: &Context, focus: &mut Option<usize>, slots: usize) {
    if slots == 0 {
        return;
    }
    let (tab, shift_tab) = consume_tab_navigation(ctx);
    if tab {
        *focus = Some(focus.map_or(0, |n| (n + 1) % slots));
    }
    if shift_tab {
        *focus = Some(focus.map_or(slots - 1, |n| if n == 0 { slots - 1 } else { n - 1 }));
    }
}

pub fn consume_tab_navigation(ctx: &Context) -> (bool, bool) {
    ctx.input_mut(|i| {
        let shift_tab = i.consume_key(Modifiers::SHIFT, Key::Tab);
        let tab = !shift_tab && i.consume_key(Modifiers::NONE, Key::Tab);
        (tab, shift_tab)
    })
}

pub fn keyboard_activate(ui: &mut Ui, focused: bool) -> bool {
    focused
        && ui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Space)
        })
}

/// One-call page focus: clamps the slot if the count shrank, runs [`tab_cycle_slots`], and
/// calls [`bind_text_focus_slot`] for every `TextEdit` listed in `text_edit_slots`.
///
/// Returns `true` when the focused slot changed this frame.
pub fn manage_page_focus(
    ctx: &Context,
    focus: &mut Option<usize>,
    slot_count: usize,
    text_edit_slots: &[(usize, Option<Id>)],
) -> bool {
    if let Some(f) = *focus {
        if f >= slot_count {
            *focus = Some(slot_count.saturating_sub(1));
        }
    }
    let old = *focus;
    tab_cycle_slots(ctx, focus, slot_count);
    for &(slot, id) in text_edit_slots {
        bind_text_focus_slot(ctx, *focus, slot, id);
    }
    *focus != old
}

/// Records `rect` into `pending` so the caller can scroll to it after `ScrollArea::show()`
/// returns. Pair with [`apply_pending_scroll`].
pub fn scroll_to_focused(
    pending: &mut Option<Rect>,
    rect: Rect,
    is_focused: bool,
    should_scroll: bool,
) {
    if is_focused && should_scroll {
        *pending = Some(rect);
    }
}

/// Scrolls the `ScrollArea` so that `target` is visible by directly modifying stored state.
/// Call **after** `ScrollArea::show()` returns, using the `ScrollAreaOutput`.
pub fn apply_pending_scroll(
    ctx: &egui::Context,
    scroll_id: egui::Id,
    inner_rect: Rect,
    state: &egui::scroll_area::State,
    target: Rect,
) {
    let offset = state.offset.y;
    let viewport_h = inner_rect.height();
    let content_top = target.top() - inner_rect.top() + offset;
    let content_bottom = target.bottom() - inner_rect.top() + offset;

    let new_offset = if content_top < offset {
        content_top
    } else if content_bottom > offset + viewport_h {
        content_bottom - viewport_h
    } else {
        return;
    };

    let mut new_state = *state;
    new_state.offset.y = new_offset.max(0.0);
    new_state.store(ctx, scroll_id);
    ctx.request_repaint();
}
