//! The scrolling list of the recent-sessions modal (spec section 10): session rows, the agent
//! worktree group header, the "Showing N of M" note and the loading skeleton.

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Margin, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2,
    WidgetInfo, WidgetType,
};

use super::model::{Item, ListRow};
use super::{SessionsAction, SessionsModal};
use crate::app::claude_launcher::{CachedGalley, cached_elided, layout_key};
use crate::app::hub_ui;
use crate::theme;

const ROW_HEIGHT: f32 = 44.0;
const GROUP_HEADER_HEIGHT: f32 = 26.0;
const NOTE_HEIGHT: f32 = 28.0;
const LIST_MAX_HEIGHT: f32 = 320.0;
/// Corner radius shared with the account chips of the footer.
pub(super) const ROW_RADIUS: u8 = 8;
const ROW_PADDING_X: f32 = 14.0;
const LIST_MARGIN_X: i8 = 8;
const LIST_MARGIN_Y: i8 = 4;
const META_GAP: f32 = 12.0;
/// Said wherever a running session cannot be resumed.
pub(super) const ALREADY_OPEN: &str = "Already open in another session";

/// Three placeholder rows while the first scan runs (no animation).
pub(super) fn show_skeleton(ui: &mut Ui, width: f32) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, ROW_HEIGHT * 3.0 + f32::from(LIST_MARGIN_Y) * 2.0),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, "Loading Claude sessions"));
    let fill = theme::alpha(theme::FG_DIM(), 38);
    let painter = ui.painter();
    for index in 0..3_u8 {
        let top = rect.top() + f32::from(LIST_MARGIN_Y) + ROW_HEIGHT * f32::from(index);
        let left = rect.left() + 16.0 + 14.0;
        for (offset, size) in [
            (9.0, Vec2::new(width * 0.5, 10.0)),
            (27.0, Vec2::new(width * 0.28, 8.0)),
        ] {
            let bar = Rect::from_min_size(Pos2::new(left, top + offset), size);
            painter.rect_filled(bar, CornerRadius::same(4), fill);
        }
    }
}

pub(super) fn show_list(ui: &mut Ui, modal: &mut SessionsModal) -> SessionsAction {
    let mut action = SessionsAction::None;
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
    // Enter resumes the selected row; only with nothing to resume may it toggle the group.
    let enter_resumes = modal.pick.is_none() && !modal.selectable.is_empty();
    let SessionsModal {
        view,
        selected,
        reveal_selected,
        profiles,
        title_galleys,
        ..
    } = modal;
    let accent_of = |name: &str| {
        profiles
            .iter()
            .find(|chip| chip.name == name)
            .map_or_else(theme::FG_DIM, |chip| chip.accent)
    };
    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT + f32::from(LIST_MARGIN_Y) * 2.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(LIST_MARGIN_X, LIST_MARGIN_Y))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut position = 0;
                    for (index, item) in view.items.iter().enumerate() {
                        match item {
                            Item::AgentHeader { count, expanded } => {
                                if show_group_header(ui, *count, *expanded, enter_resumes) {
                                    action = SessionsAction::ToggleAgents;
                                }
                            }
                            Item::Row(row) => {
                                let Some(slot) = title_galleys.get_mut(index) else {
                                    continue;
                                };
                                let is_selected = *selected == position;
                                let cursor = RowCursor {
                                    position,
                                    selected: is_selected,
                                    pointer_moved,
                                    reveal: is_selected && *reveal_selected,
                                };
                                if cursor.reveal {
                                    *reveal_selected = false;
                                }
                                let picked = show_row(ui, row, &cursor, &accent_of, slot);
                                if picked != SessionsAction::None {
                                    action = picked;
                                }
                                position += 1;
                            }
                        }
                    }
                    if view.matching > view.shown && show_window_note(ui, view.shown, view.matching) {
                        action = SessionsAction::ShowAll;
                    }
                });
        });
    action
}

/// Where a row sits in the list and what the frame wants from it.
struct RowCursor {
    /// Position among the selectable rows.
    position: usize,
    selected: bool,
    pointer_moved: bool,
    /// Scroll this row into view now.
    reveal: bool,
}

fn show_row(
    ui: &mut Ui,
    row: &ListRow,
    cursor: &RowCursor,
    accent_of: &dyn Fn(&str) -> Color32,
    slot: &mut Option<CachedGalley>,
) -> SessionsAction {
    let mut action = SessionsAction::None;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        let label = if row.running.is_some() {
            format!("{} (already open)", row.title)
        } else {
            format!("Resume {}", row.title)
        };
        WidgetInfo::labeled(WidgetType::Button, true, label)
    });
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if cursor.pointer_moved && !cursor.selected {
            action = SessionsAction::Select(cursor.position);
        }
    }
    if cursor.reveal {
        ui.scroll_to_rect(rect, None);
    }
    if response.double_clicked() {
        action = SessionsAction::Resume {
            session: row.session,
            here: false,
        };
    } else if response.clicked() {
        action = SessionsAction::Select(cursor.position);
    }
    if ui.is_rect_visible(rect) {
        let state = RowState {
            selected: cursor.selected,
            hovered: response.hovered(),
            pressed: response.is_pointer_button_down_on(),
        };
        paint_row(ui.painter(), rect, row, state, accent_of, slot);
        hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
        response.on_hover_ui(|ui| {
            ui.label(egui::RichText::new(&row.tooltip).size(11.0));
            if row.running.is_some() {
                ui.label(
                    egui::RichText::new(ALREADY_OPEN)
                        .size(11.0)
                        .color(theme::PALETTE_GREEN()),
                );
            }
        });
    }
    action
}

/// Whether a click on the group header toggles the group. The release of Enter counts as a click
/// on a focused header, but while a row can be resumed Enter belongs to the row.
fn header_toggles(clicked: bool, enter_released: bool, enter_resumes: bool) -> bool {
    clicked && !(enter_released && enter_resumes)
}

/// `▸ Agent worktrees (7)`; true when it should toggle.
fn show_group_header(ui: &mut Ui, count: usize, expanded: bool, enter_resumes: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GROUP_HEADER_HEIGHT), Sense::click());
    let label = format!("Agent worktrees ({count})");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label.clone()));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(ROW_RADIUS), theme::alpha(theme::FG_DIM(), 24));
        }
        let center = Pos2::new(rect.left() + ROW_PADDING_X + 4.0, rect.center().y);
        // Painted as a triangle so it does not depend on a font glyph.
        let points = if expanded {
            vec![
                center + Vec2::new(-4.0, -2.5),
                center + Vec2::new(4.0, -2.5),
                center + Vec2::new(0.0, 3.0),
            ]
        } else {
            vec![
                center + Vec2::new(-2.5, -4.0),
                center + Vec2::new(3.0, 0.0),
                center + Vec2::new(-2.5, 4.0),
            ]
        };
        painter.add(Shape::convex_polygon(points, theme::FG_DIM(), Stroke::NONE));
        painter.text(
            Pos2::new(rect.left() + ROW_PADDING_X + 14.0, rect.center().y),
            Align2::LEFT_CENTER,
            &label,
            FontId::proportional(11.0),
            theme::FG_DIM(),
        );
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    let enter_released = ui.input(|input| input.key_released(Key::Enter));
    header_toggles(response.clicked(), enter_released, enter_resumes)
}

/// `Showing 30 of 112 · Show all`; true when "Show all" is clicked.
fn show_window_note(ui: &mut Ui, shown: usize, matching: usize) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), NOTE_HEIGHT), Sense::click());
    let text = format!("Showing {shown} of {matching} \u{b7} ");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Show all sessions"));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let lead = painter.layout_no_wrap(text, FontId::proportional(11.0), theme::FG_DIM());
        let link_color = if response.hovered() {
            theme::blend(theme::ACCENT(), theme::FG(), 0.2)
        } else {
            theme::ACCENT()
        };
        let link = painter.layout_no_wrap("Show all".to_owned(), FontId::proportional(11.0), link_color);
        let start = rect.left() + ROW_PADDING_X;
        let y = rect.center().y - lead.size().y / 2.0;
        let link_left = start + lead.size().x;
        painter.galley(Pos2::new(start, y), lead, theme::FG_DIM());
        let underline_y = y + link.size().y;
        let underline = [
            Pos2::new(link_left, underline_y),
            Pos2::new(link_left + link.size().x, underline_y),
        ];
        painter.galley(Pos2::new(link_left, y), link, link_color);
        if response.hovered() {
            painter.line_segment(underline, Stroke::new(1.0_f32, link_color));
        }
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    response.clicked()
}

#[derive(Clone, Copy)]
struct RowState {
    selected: bool,
    hovered: bool,
    pressed: bool,
}

fn paint_row(
    painter: &Painter,
    rect: Rect,
    row: &ListRow,
    state: RowState,
    accent_of: &dyn Fn(&str) -> Color32,
    title_slot: &mut Option<CachedGalley>,
) {
    if state.selected {
        let tint = if state.pressed { 0.30 } else { 0.22 };
        painter.rect_filled(
            rect,
            CornerRadius::same(ROW_RADIUS),
            theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), tint),
        );
        let bar = Rect::from_center_size(Pos2::new(rect.left() + 4.5, rect.center().y), Vec2::new(3.0, 28.0));
        painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT());
    } else if state.hovered {
        painter.rect_filled(
            rect,
            CornerRadius::same(ROW_RADIUS),
            theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.12),
        );
    }

    let left = rect.left() + ROW_PADDING_X;
    let title_width = (rect.width() - ROW_PADDING_X * 2.0).max(0.0);
    let title = cached_elided(
        title_slot,
        layout_key(painter, title_width),
        painter,
        &row.title,
        FontId::proportional(12.0),
        title_width,
    );
    painter.galley(Pos2::new(left, rect.top() + 8.0), title, theme::FG());

    let y = rect.top() + 31.0;
    let mut x = left;
    x += paint_account(painter, Pos2::new(x, y), row.account.as_deref(), accent_of) + META_GAP;
    if row.running.is_some() {
        x += paint_running_pill(painter, Pos2::new(x, y)) + META_GAP;
    }
    x += paint_meta_text(painter, Pos2::new(x, y), &row.age) + META_GAP;
    if let Some(chip) = &row.chip {
        x += paint_branch_chip(painter, Pos2::new(x, y), chip) + META_GAP;
    }
    paint_meta_text(painter, Pos2::new(x, y), &row.size);
}

/// Dot and name of the account, or the hollow dot and `account unknown` pill. Returns the width.
fn paint_account(painter: &Painter, at: Pos2, account: Option<&str>, accent_of: &dyn Fn(&str) -> Color32) -> f32 {
    if let Some(name) = account {
        painter.circle_filled(at + Vec2::new(3.5, 0.0), 3.5, accent_of(name));
        return 7.0 + 6.0 + paint_meta_text(painter, at + Vec2::new(13.0, 0.0), name);
    }
    painter.circle_stroke(at + Vec2::new(3.5, 0.0), 3.0, Stroke::new(1.0_f32, theme::FG_DIM()));
    let text = "account unknown";
    let width = hub_ui::pill_width(painter, text);
    let pill = hub_ui::pill_rect(Pos2::new(at.x + 13.0, at.y - 9.0), width);
    hub_ui::pill(painter, pill, text, theme::FG_DIM());
    13.0 + width
}

/// The `running` pill of a session that is live now. Returns its width.
fn paint_running_pill(painter: &Painter, at: Pos2) -> f32 {
    let text = "running";
    let width = hub_ui::pill_width(painter, text);
    hub_ui::pill(
        painter,
        hub_ui::pill_rect(Pos2::new(at.x, at.y - 9.0), width),
        text,
        theme::PALETTE_GREEN(),
    );
    width
}

/// 10.5 `FG_DIM` text vertically centred on `at.y`; returns its width.
fn paint_meta_text(painter: &Painter, at: Pos2, text: &str) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(10.5), theme::FG_DIM());
    let width = galley.size().x;
    painter.galley(Pos2::new(at.x, at.y - galley.size().y / 2.0), galley, theme::FG_DIM());
    width
}

/// Branch icon and name in a small monospace chip; returns the chip's width.
fn paint_branch_chip(painter: &Painter, at: Pos2, text: &str) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::monospace(10.0), theme::FG_SOFT());
    let icon_width = 12.0;
    let width = 6.0 + icon_width + galley.size().x + 6.0;
    let rect = Rect::from_min_size(Pos2::new(at.x, at.y - 8.0), Vec2::new(width, 16.0));
    painter.rect_filled(rect, CornerRadius::same(4), theme::alpha(theme::FG_DIM(), 40));
    paint_branch_icon(
        painter,
        Pos2::new(rect.left() + 6.0 + 3.0, rect.center().y),
        theme::FG_SOFT(),
    );
    painter.galley(
        Pos2::new(rect.left() + 6.0 + icon_width, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::FG_SOFT(),
    );
    width
}

/// A tiny fork: a stem with a node at each end and a branch node on the right.
fn paint_branch_icon(painter: &Painter, center: Pos2, color: Color32) {
    let stroke = Stroke::new(1.2_f32, color);
    let top = center + Vec2::new(-1.5, -3.5);
    let bottom = center + Vec2::new(-1.5, 3.5);
    let side = center + Vec2::new(2.5, -1.5);
    painter.line_segment([top, bottom], stroke);
    painter.line_segment([side, center + Vec2::new(-1.5, 1.0)], stroke);
    for node in [top, bottom, side] {
        painter.circle_filled(node, 1.5, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_on_the_group_header_resumes_the_row_instead_of_toggling() {
        // A pointer click or Space always toggles.
        assert!(header_toggles(true, false, true));
        assert!(header_toggles(true, false, false));
        // The release of Enter is a click on a focused header: ignored while a row can be resumed.
        assert!(!header_toggles(true, true, true));
        // With nothing to resume, Enter may toggle the group.
        assert!(header_toggles(true, true, false));
        assert!(!header_toggles(false, false, true));
    }
}
