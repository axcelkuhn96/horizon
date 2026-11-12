//! Painting and keyboard of the recent-sessions modal (spec section 10). Strings come from
//! [`super::model`] and galleys of titles are cached per list build, so a frame lays out only the
//! few rows that are on screen.

use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Key, Layout, Margin, Modifiers, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};

use super::model::header_count_label;
use super::rows::{ALREADY_OPEN, ROW_RADIUS, show_list, show_skeleton};
use super::{Phase, SessionsAction, SessionsModal};
use crate::app::claude_launcher::{cached_elided, layout_key, move_selection};
use crate::app::hub_ui;
use crate::theme;

const HEADER_HEIGHT: f32 = 44.0;
const FILTER_HEIGHT: f32 = 44.0;
const FOOTER_HEIGHT: f32 = 48.0;
const BODY_HEIGHT: f32 = 120.0;
const HEADER_RIGHT_WIDTH: f32 = 240.0;
const CHIP_HEIGHT: f32 = 28.0;
const CHIP_NAME_WIDTH: f32 = 90.0;
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
        let count = header_count_label(modal.sessions.len(), modal.total_found);
        format!("{} \u{b7} {count}", modal.workspace_name)
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

fn show_footer(ui: &mut Ui, width: f32, modal: &SessionsModal) -> SessionsAction {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FOOTER_HEIGHT), Sense::hover());
    let inner = rect.shrink2(Vec2::new(16.0, 0.0));
    let selected = modal.selected_session();
    let ready = selected.is_some() && modal.phase == Phase::Ready;
    let running = modal.selected_row().and_then(|row| row.running).filter(|_| ready);
    // A session open in a Horizon panel is focused rather than resumed.
    let focus = running.and_then(|running| running.panel);

    let mut hints = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    hints.spacing_mut().item_spacing.x = 12.0;
    hub_ui::kbd_hint(&mut hints, "Esc", "close");
    hub_ui::kbd_hint(&mut hints, "\u{2191}\u{2193}", "select");
    hub_ui::kbd_hint(
        &mut hints,
        "Enter",
        if focus.is_some() { "focus panel" } else { "resume" },
    );

    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 8.0;
    let mut action = SessionsAction::None;
    let can_resume = ready && running.is_none();
    if let Some(panel) = focus {
        if hub_ui::primary_button(&mut buttons, "Focus panel", true)
            .on_hover_text("This session is open in a Horizon panel")
            .clicked()
        {
            action = SessionsAction::FocusPanel(panel);
        }
        // Resume stays visible so the reason it is unavailable can be read.
        let resume = buttons
            .add_enabled_ui(false, |ui| hub_ui::secondary_button(ui, "Resume"))
            .inner;
        let _ = resume.on_disabled_hover_text(ALREADY_OPEN);
        return action;
    }
    let resume = hub_ui::primary_button(&mut buttons, "Resume", can_resume);
    let resume = if running.is_some() {
        resume.on_hover_text(ALREADY_OPEN)
    } else {
        resume
    };
    if resume.clicked()
        && let Some(session) = selected
    {
        action = SessionsAction::Resume { session, here: false };
    }
    let here = buttons
        .add_enabled_ui(can_resume, |ui| hub_ui::secondary_button(ui, "Resume here"))
        .inner;
    let here = if running.is_some() {
        here.on_disabled_hover_text(ALREADY_OPEN)
    } else {
        here.on_hover_text("Type the command into the focused idle shell")
    };
    if here.clicked()
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
