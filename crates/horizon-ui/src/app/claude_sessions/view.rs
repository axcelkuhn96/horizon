//! Painting and keyboard of the recent-sessions modal (spec section 10). Strings come from
//! [`super::model`] and galleys of titles are cached per list build, so a frame lays out only the
//! few rows that are on screen.

use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Key, Layout, Margin, Modifiers, Painter, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};

use super::model::{Item, ListRow};
use super::{Phase, SessionsAction, SessionsModal};
use crate::app::claude_launcher::{CachedGalley, cached_elided, layout_key, move_selection};
use crate::app::hub_ui;
use crate::theme;

const HEADER_HEIGHT: f32 = 44.0;
const FILTER_HEIGHT: f32 = 44.0;
const ROW_HEIGHT: f32 = 44.0;
const GROUP_HEADER_HEIGHT: f32 = 26.0;
const NOTE_HEIGHT: f32 = 28.0;
const LIST_MAX_HEIGHT: f32 = 320.0;
const FOOTER_HEIGHT: f32 = 48.0;
const BODY_HEIGHT: f32 = 120.0;
const ROW_RADIUS: u8 = 8;
const ROW_PADDING_X: f32 = 14.0;
const LIST_MARGIN_X: i8 = 8;
const LIST_MARGIN_Y: i8 = 4;
const HEADER_RIGHT_WIDTH: f32 = 240.0;
const CHIP_HEIGHT: f32 = 28.0;
const CHIP_NAME_WIDTH: f32 = 90.0;
const META_GAP: f32 = 12.0;
const NUMBER_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// Id of the filter field (focused when the modal opens).
pub(super) fn filter_id() -> egui::Id {
    egui::Id::new("claude_sessions_filter")
}

/// Keyboard for one frame, before any widget sees the keys: arrows move the selection, Enter
/// resumes in a new panel and Shift+Enter in the active shell. In the "Which account?" step
/// `1`-`9` pick a chip and Esc goes back instead of closing the modal.
pub(super) fn handle_keys(ctx: &Context, modal: &mut SessionsModal) -> SessionsAction {
    if modal.pick.is_some() {
        // The filter is disabled in that step; digits must not be typed into it.
        ctx.memory_mut(|memory| memory.surrender_focus(filter_id()));
    }
    ctx.input_mut(|input| {
        if modal.pick.is_some() {
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                return SessionsAction::CancelPick;
            }
            for (index, key) in NUMBER_KEYS.iter().enumerate().take(modal.profiles.len()) {
                if input.consume_key(Modifiers::NONE, *key) {
                    return SessionsAction::PickAccount(index);
                }
            }
            return SessionsAction::None;
        }
        if modal.phase != Phase::Ready || modal.selectable.is_empty() {
            return SessionsAction::None;
        }
        let selected = modal.selected_session();
        if input.consume_key(Modifiers::SHIFT, Key::Enter) {
            return selected.map_or(SessionsAction::None, |session| SessionsAction::Resume {
                session,
                here: true,
            });
        }
        if input.consume_key(Modifiers::NONE, Key::Enter) {
            return selected.map_or(SessionsAction::None, |session| SessionsAction::Resume {
                session,
                here: false,
            });
        }
        let before = modal.selected;
        if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
            modal.selected = move_selection(modal.selected, 1, modal.selectable.len());
        }
        if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
            modal.selected = move_selection(modal.selected, -1, modal.selectable.len());
        }
        modal.reveal_selected |= modal.selected != before;
        SessionsAction::None
    })
}

pub(super) fn show_contents(ui: &mut Ui, modal: &mut SessionsModal) -> SessionsAction {
    // Every block is sized explicitly; the theme's item spacing must not add to it.
    ui.spacing_mut().item_spacing.y = 0.0;
    let width = ui.available_width();
    paint_header(ui, width, modal);
    separator(ui, width);
    show_filter(ui, width, modal);
    separator(ui, width);
    let mut action = match modal.phase {
        Phase::Loading => {
            show_skeleton(ui, width);
            SessionsAction::None
        }
        Phase::Failed => {
            show_message(ui, width, "Could not read the Claude sessions", theme::PALETTE_RED());
            SessionsAction::None
        }
        Phase::Ready if modal.view.matching == 0 => {
            show_message(ui, width, empty_text(modal), theme::FG_DIM());
            SessionsAction::None
        }
        Phase::Ready => show_list(ui, modal),
    };
    separator(ui, width);
    let footer = if modal.pick.is_some() {
        show_account_step(ui, width, modal)
    } else {
        show_footer(ui, width, modal)
    };
    if action == SessionsAction::None {
        action = footer;
    }
    modal.selected = modal.selected.min(modal.selectable.len().saturating_sub(1));
    action
}

fn empty_text(modal: &SessionsModal) -> &'static str {
    if !modal.has_folder {
        "This workspace has no folder yet"
    } else if modal.filter.trim().is_empty() {
        "No Claude sessions for this project yet"
    } else {
        "No sessions match the filter"
    }
}

fn separator(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SUBTLE());
}

fn paint_header(ui: &mut Ui, width: f32, modal: &mut SessionsModal) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEADER_HEIGHT), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, "Recent Claude sessions"));
    let painter = ui.painter();
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Recent Claude sessions",
        FontId::proportional(13.0),
        theme::FG(),
    );
    let right_text = if modal.phase == Phase::Ready {
        let count = modal.sessions.len();
        let noun = if count == 1 { "session" } else { "sessions" };
        format!("{} \u{b7} {count} {noun}", modal.workspace_name)
    } else {
        modal.workspace_name.clone()
    };
    let galley = cached_elided(
        &mut modal.header_galley,
        layout_key(painter, HEADER_RIGHT_WIDTH),
        painter,
        &right_text,
        FontId::proportional(10.5),
        HEADER_RIGHT_WIDTH,
    );
    painter.galley(
        Pos2::new(
            rect.right() - 16.0 - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        theme::FG_DIM(),
    );
}

fn show_filter(ui: &mut Ui, width: f32, modal: &mut SessionsModal) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FILTER_HEIGHT), Sense::hover());
    let builder = UiBuilder::new()
        .max_rect(rect.shrink2(Vec2::new(16.0, 8.0)))
        .layout(Layout::left_to_right(Align::Center));
    // `new_child`, not `scope_builder`: the latter would allocate again in `ui`.
    let mut row = ui.new_child(builder);
    row.add_enabled(
        modal.pick.is_none(),
        egui::TextEdit::singleline(&mut modal.filter)
            .id(filter_id())
            .hint_text("Filter by title, branch or account")
            .font(FontId::proportional(12.0))
            .margin(Margin::symmetric(8, 6))
            .desired_width(f32::INFINITY),
    );
}

fn show_message(ui: &mut Ui, width: f32, text: &str, color: Color32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, BODY_HEIGHT), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.5),
        color,
    );
}

/// Three placeholder rows while the first scan runs (no animation).
fn show_skeleton(ui: &mut Ui, width: f32) {
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

fn show_list(ui: &mut Ui, modal: &mut SessionsModal) -> SessionsAction {
    let mut action = SessionsAction::None;
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
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
                                if show_group_header(ui, *count, *expanded) {
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
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Resume {}", row.title)));
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
        });
    }
    action
}

/// `▸ Agent worktrees (7)`; true when clicked.
fn show_group_header(ui: &mut Ui, count: usize, expanded: bool) -> bool {
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
    response.clicked()
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

fn show_footer(ui: &mut Ui, width: f32, modal: &SessionsModal) -> SessionsAction {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FOOTER_HEIGHT), Sense::hover());
    let inner = rect.shrink2(Vec2::new(16.0, 0.0));
    let mut hints = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    hints.spacing_mut().item_spacing.x = 12.0;
    hub_ui::kbd_hint(&mut hints, "Esc", "close");
    hub_ui::kbd_hint(&mut hints, "\u{2191}\u{2193}", "select");
    hub_ui::kbd_hint(&mut hints, "Enter", "resume");

    let selected = modal.selected_session();
    let enabled = selected.is_some() && modal.phase == Phase::Ready;
    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 8.0;
    let mut action = SessionsAction::None;
    if hub_ui::primary_button(&mut buttons, "Resume", enabled).clicked()
        && let Some(session) = selected
    {
        action = SessionsAction::Resume { session, here: false };
    }
    let here = buttons
        .add_enabled_ui(enabled, |ui| hub_ui::secondary_button(ui, "Resume here"))
        .inner;
    if here
        .on_hover_text("Type the command into the focused idle shell")
        .clicked()
        && let Some(session) = selected
    {
        action = SessionsAction::Resume { session, here: true };
    }
    action
}

/// The footer of the "Which account?" step: one chip per profile, `1`-`9` to pick.
fn show_account_step(ui: &mut Ui, width: f32, modal: &SessionsModal) -> SessionsAction {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FOOTER_HEIGHT), Sense::hover());
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(16.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = 8.0;
    let mut action = SessionsAction::None;
    row.label(egui::RichText::new("Which account?").size(12.0).color(theme::FG_SOFT()));
    for (index, chip) in modal.profiles.iter().enumerate() {
        if account_chip(&mut row, index, &chip.name, chip.accent).clicked() {
            action = SessionsAction::PickAccount(index);
        }
    }
    if modal.profiles.is_empty() {
        row.label(
            egui::RichText::new("No Claude profiles configured")
                .size(11.0)
                .color(theme::PALETTE_RED()),
        );
    }
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        hub_ui::kbd_hint(ui, "Esc", "back");
    });
    action
}

fn account_chip(ui: &mut Ui, index: usize, name: &str, accent: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), FontId::proportional(12.0), theme::FG());
    let text_width = galley.size().x.min(CHIP_NAME_WIDTH);
    let width = 10.0 + 8.0 + 6.0 + text_width + 6.0 + 20.0 + 6.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, CHIP_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Resume under {name}")));
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered() || response.is_pointer_button_down_on();
        let painter = ui.painter();
        let fill = if hovered {
            theme::blend(theme::PANEL_BG_ALT(), accent, 0.22)
        } else {
            theme::PANEL_BG_ALT()
        };
        painter.rect(
            rect,
            CornerRadius::same(ROW_RADIUS),
            fill,
            Stroke::new(1.0_f32, theme::BORDER_SUBTLE()),
            StrokeKind::Inside,
        );
        painter.circle_filled(Pos2::new(rect.left() + 14.0, rect.center().y), 4.0, accent);
        let text_pos = Pos2::new(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0);
        painter
            .with_clip_rect(Rect::from_min_size(text_pos, Vec2::new(text_width, galley.size().y)))
            .galley(text_pos, galley, theme::FG());
        let badge = Rect::from_min_size(
            Pos2::new(rect.right() - 6.0 - 20.0, rect.center().y - 10.0),
            Vec2::splat(20.0),
        );
        hub_ui::key_badge(painter, badge, index + 1, hovered, accent);
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    response
}
