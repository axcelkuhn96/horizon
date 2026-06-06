//! Shared helpers for making panel-body `ScrollArea`s scroll with the mouse
//! wheel and stay bounded inside the panel body.
//!
//! Panel `Area`s are built with `interactable(false)` (see `app/panels.rs`), so
//! egui's `layer_id_at` skips the layer during hover detection. A `ScrollArea`
//! therefore never sees the pointer as hovering and silently ignores
//! mouse-wheel events. [`forward_scroll_to_scroll_area`] detects hover itself
//! and applies the wheel delta to the stored scroll state, consuming the delta
//! so the canvas pan-by-scroll handler does not also act on the same gesture.

use egui::Id;
use egui::Rect;
use egui::containers::scroll_area::State as ScrollAreaState;

/// Computes the height available for a panel-body `ScrollArea` so it stays
/// clipped inside the panel instead of overflowing onto neighbouring panels.
///
/// `body_bottom` is the panel body rect's bottom (`ui.max_rect().bottom()`),
/// `cursor_top` is where the scroll area starts (`ui.cursor().top()`, i.e. just
/// below the header), and `footer_height` is the space to reserve at the bottom
/// (zero when no footer is shown). The result is never negative.
#[must_use]
pub(crate) fn scroll_viewport_height(body_bottom: f32, cursor_top: f32, footer_height: f32) -> f32 {
    (body_bottom - cursor_top - footer_height).max(0.0)
}

/// Forwards the current mouse-wheel delta to a panel-body `ScrollArea`.
///
/// The panel `Area` uses `interactable(false)`, which makes egui's
/// `layer_id_at` skip the layer during hover detection. `ScrollArea`
/// therefore never sees the pointer as hovering and silently ignores
/// mouse-wheel events. We detect hover ourselves and apply the delta
/// to the stored scroll state, then consume the delta so the canvas
/// pan-by-scroll handler does not also pan on the same gesture.
pub(crate) fn forward_scroll_to_scroll_area(ui: &egui::Ui, scroll_id: Id, inner_rect: Rect, content_height: f32) {
    let from_global = ui.ctx().layer_transform_from_global(ui.layer_id());
    let pointer_in_area = ui.input(|i| i.pointer.hover_pos()).is_some_and(|pos| {
        let local = from_global.map_or(pos, |t| t * pos);
        inner_rect.contains(local)
    });
    if !pointer_in_area {
        return;
    }

    let scroll_delta = ui.ctx().input(|i| i.smooth_scroll_delta.y);
    if scroll_delta == 0.0 {
        return;
    }

    let max_offset = (content_height - inner_rect.height()).max(0.0);
    if let Some(mut state) = ScrollAreaState::load(ui.ctx(), scroll_id) {
        let new_offset = (state.offset.y - scroll_delta).clamp(0.0, max_offset);
        if (new_offset - state.offset.y).abs() > f32::EPSILON {
            state.offset.y = new_offset;
            state.store(ui.ctx(), scroll_id);
            ui.ctx().input_mut(|i| i.smooth_scroll_delta.y = 0.0);
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scroll_viewport_height;

    #[test]
    fn viewport_height_reserves_footer_and_header_offset() {
        // body spans y in [0, 300], scroll area starts at 28 (header), footer 20.
        assert!((scroll_viewport_height(300.0, 28.0, 20.0) - 252.0).abs() < f32::EPSILON);
    }

    #[test]
    fn viewport_height_without_footer() {
        assert!((scroll_viewport_height(300.0, 28.0, 0.0) - 272.0).abs() < f32::EPSILON);
    }

    #[test]
    fn viewport_height_never_negative() {
        // Footer + header offset exceed the body height -> clamp to zero.
        assert!((scroll_viewport_height(100.0, 90.0, 50.0)).abs() < f32::EPSILON);
    }
}
