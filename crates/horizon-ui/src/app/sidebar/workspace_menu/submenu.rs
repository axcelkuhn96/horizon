//! "New terminal" presets submenu of the workspace context menu.
//!
//! The submenu is a custom `Order::Tooltip` area (egui `SubMenu` popups paint
//! below the `Order::Tooltip` sidebar). It stays open while the pointer is on
//! the row, on the bridge between row and submenu, or on the submenu, with a
//! short grace period, and it is fully keyboard reachable: Right/Enter/Space on
//! the row opens it and focuses the first item, Up/Down move inside it, and
//! Left/Esc return to the row.

use std::time::{Duration, Instant};

use egui::{Area, Context, Frame, Id, Key, Modifiers, Order, Pos2, Rect, Response};
use horizon_core::PresetConfig;

use super::{MenuRow, RowLead, SUBMENU_OFFSET_X, SUBMENU_WIDTH, menu_row, section_header, split_presets};
use crate::app::hub_ui;
use crate::app::panels::panel_kind_icon;
use crate::theme;

/// How long the submenu survives the pointer leaving the row/submenu zone.
const CLOSE_GRACE: Duration = Duration::from_millis(150);

/// What the keyboard asks the submenu to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyIntent {
    None,
    /// Open the submenu and focus its first item.
    Open,
    /// Close the submenu and give focus back to its row.
    Return,
}

/// Right/Enter/Space on the focused row opens; Left/Esc inside the submenu
/// returns (Esc is then consumed so it does not close the whole menu).
fn key_intent(row_focused: bool, focus_inside: bool, open_key: bool, back_key: bool) -> KeyIntent {
    if focus_inside && back_key {
        KeyIntent::Return
    } else if row_focused && open_key {
        KeyIntent::Open
    } else {
        KeyIntent::None
    }
}

/// Whether the pointer is on the row, the submenu, or the bridge between them.
/// The bridge spans the row's height across the gap, so crossing it never
/// counts as leaving.
fn pointer_in_zone(pointer: Option<Pos2>, row: Rect, submenu: Option<Rect>) -> bool {
    let Some(pos) = pointer else {
        return false;
    };
    if row.contains(pos) {
        return true;
    }
    let Some(submenu) = submenu else {
        return false;
    };
    let bridge = Rect::from_min_max(
        Pos2::new(row.right(), row.top()),
        Pos2::new(submenu.left(), row.bottom()),
    );
    submenu.contains(pos) || bridge.contains(pos)
}

/// Stay open while inside, and for [`CLOSE_GRACE`] after the pointer left.
fn stay_open(inside: bool, since_inside: Option<Duration>) -> bool {
    inside || since_inside.is_some_and(|elapsed| elapsed < CLOSE_GRACE)
}

#[derive(Clone)]
struct SubmenuState {
    rect: Rect,
    last_inside: Instant,
    focus_inside: bool,
}

/// Hover/keyboard-opened presets list next to the "New terminal" row. Returns
/// the index of the picked preset.
pub(super) fn show_presets_submenu(ctx: &Context, id: Id, row: &Response, presets: &[PresetConfig]) -> Option<usize> {
    let state_id = id.with("state");
    let state: Option<SubmenuState> = ctx.data(|data| data.get_temp(state_id));
    let now = Instant::now();

    let (open_key, back_key) = ctx.input(|input| {
        (
            input.key_pressed(Key::ArrowRight) || input.key_pressed(Key::Enter) || input.key_pressed(Key::Space),
            input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::Escape),
        )
    });
    let focus_inside = state.as_ref().is_some_and(|s| s.focus_inside);
    let intent = key_intent(row.has_focus(), focus_inside, open_key, back_key);
    if intent == KeyIntent::Return {
        ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
        ctx.memory_mut(|memory| memory.request_focus(row.id));
        ctx.data_mut(|data| data.remove::<SubmenuState>(state_id));
        return None;
    }

    let inside = pointer_in_zone(ctx.pointer_hover_pos(), row.rect, state.as_ref().map(|s| s.rect));
    let since_inside = state.as_ref().map(|s| now.saturating_duration_since(s.last_inside));
    let was_open = state.is_some();
    let open = intent == KeyIntent::Open
        || row.clicked()
        || (was_open && (stay_open(inside, since_inside) || focus_inside || row.has_focus()))
        || (!was_open && inside);
    if !open {
        ctx.data_mut(|data| data.remove::<SubmenuState>(state_id));
        return None;
    }
    if was_open && !inside && !focus_inside && !row.has_focus() {
        // In the grace period: come back and re-check when it elapses.
        let remaining = CLOSE_GRACE.saturating_sub(since_inside.unwrap_or_default());
        ctx.request_repaint_after(remaining);
    }

    let groups = split_presets(presets);
    let mut picked = None;
    let mut row_ids = Vec::new();
    let area = Area::new(id.with("area"))
        .order(Order::Tooltip)
        .fixed_pos(Pos2::new(row.rect.right() + SUBMENU_OFFSET_X, row.rect.top() - 4.0))
        .constrain(true)
        .show(ctx, |ui| {
            Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_width(SUBMENU_WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                if !groups.general.is_empty() {
                    section_header(ui, "PRESETS");
                    for &index in &groups.general {
                        let preset = &presets[index];
                        let (icon, color) = panel_kind_icon(preset.kind, theme::FG_SOFT(), false);
                        let item = MenuRow {
                            lead: RowLead::Icon(icon, color),
                            ..MenuRow::new(&preset.name)
                        };
                        let response = menu_row(ui, &item);
                        row_ids.push(response.id);
                        if response.clicked() {
                            picked = Some(index);
                        }
                    }
                }
                if !groups.profiles.is_empty() {
                    if !groups.general.is_empty() {
                        ui.separator();
                    }
                    section_header(ui, "CLAUDE PROFILES");
                    for &(index, slot) in &groups.profiles {
                        let item = MenuRow {
                            lead: RowLead::Dot(hub_ui::accent_color(slot)),
                            ..MenuRow::new(&presets[index].name)
                        };
                        let response = menu_row(ui, &item);
                        row_ids.push(response.id);
                        if response.clicked() {
                            picked = Some(index);
                        }
                    }
                }
            });
        });
    ctx.move_to_top(area.response.layer_id);

    if intent == KeyIntent::Open
        && let Some(first) = row_ids.first()
    {
        ctx.memory_mut(|memory| memory.request_focus(*first));
    }
    let focused = ctx.memory(|memory| memory.focused());
    let focus_now = focused.is_some_and(|id| row_ids.contains(&id)) || intent == KeyIntent::Open;
    let inside_now = pointer_in_zone(ctx.pointer_hover_pos(), row.rect, Some(area.response.rect));
    let last_inside = if inside || inside_now {
        now
    } else {
        state.as_ref().map_or(now, |previous| previous.last_inside)
    };
    ctx.data_mut(|data| {
        data.insert_temp(
            state_id,
            SubmenuState {
                rect: area.response.rect,
                last_inside,
                focus_inside: focus_now,
            },
        );
    });
    picked
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use egui::{Pos2, Rect};

    use super::{KeyIntent, key_intent, pointer_in_zone, stay_open};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
    }

    #[test]
    fn workspace_menu_submenu_bridge_keeps_open_across_the_gap() {
        let row = rect(0.0, 100.0, 220.0, 128.0);
        let submenu = rect(226.0, 96.0, 466.0, 300.0);

        assert!(pointer_in_zone(Some(Pos2::new(110.0, 110.0)), row, Some(submenu)));
        assert!(
            pointer_in_zone(Some(Pos2::new(223.0, 115.0)), row, Some(submenu)),
            "gap between row and submenu is bridged"
        );
        assert!(pointer_in_zone(Some(Pos2::new(300.0, 250.0)), row, Some(submenu)));
        assert!(!pointer_in_zone(Some(Pos2::new(223.0, 200.0)), row, Some(submenu)));
        assert!(!pointer_in_zone(Some(Pos2::new(110.0, 200.0)), row, Some(submenu)));
        assert!(!pointer_in_zone(None, row, Some(submenu)));
    }

    #[test]
    fn workspace_menu_submenu_grace_period_delays_closing() {
        assert!(stay_open(true, None));
        assert!(stay_open(false, Some(Duration::from_millis(149))));
        assert!(!stay_open(false, Some(Duration::from_millis(150))));
        assert!(!stay_open(false, None));
    }

    #[test]
    fn workspace_menu_submenu_keyboard_opens_from_row_and_returns_from_inside() {
        assert_eq!(key_intent(true, false, true, false), KeyIntent::Open);
        assert_eq!(key_intent(false, true, false, true), KeyIntent::Return);
        assert_eq!(
            key_intent(true, true, true, true),
            KeyIntent::Return,
            "back wins inside the submenu"
        );
        assert_eq!(key_intent(false, false, true, true), KeyIntent::None);
        assert_eq!(key_intent(true, false, false, false), KeyIntent::None);
        assert_eq!(key_intent(false, true, true, false), KeyIntent::None);
    }
}
