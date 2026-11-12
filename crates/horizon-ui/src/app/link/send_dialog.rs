//! The "Send to panel" dialog: target list with filter, message editor and
//! the keyboard handling around them. Visual spec: `docs/design/claude-hub-ui.md` §6.

use egui::{
    Align, Align2, Context, CornerRadius, EventFilter, FontId, Id, Key, Layout, Margin, Modifiers, Painter, Pos2, Rect,
    Sense, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::link::{LinkOk, LinkRequest, LinkResponse, MAX_REQUEST_BYTES, strip_escapes};
use horizon_core::{Board, PanelId, PanelKind};

use super::super::HorizonApp;
use super::super::hub_ui::{self, ModalPlacement};
use super::super::panel_chrome::panel_kind_icon;
use crate::theme;

const MODAL_WIDTH: f32 = 520.0;
const HEADER_HEIGHT: f32 = 44.0;
const ROW_HEIGHT: f32 = 40.0;
const ROW_RADIUS: u8 = 8;
const LIST_MAX_HEIGHT: f32 = 200.0;
const MESSAGE_HEIGHT: f32 = 96.0;
const SECTION_PADDING: i8 = 16;
const WORKSPACE_COLUMN_WIDTH: f32 = 120.0;
const KIND_COLUMN_WIDTH: f32 = 150.0;
/// The CLI caps a request at `MAX_REQUEST_BYTES`; the dialog caps the text so
/// that even four-byte characters stay inside the same limit.
const MAX_MESSAGE_CHARS: usize = MAX_REQUEST_BYTES / 4;
/// Shown when the message would exceed what the socket reader accepts.
const TOO_LARGE_MESSAGE: &str = "Message is too large to send";

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Case-insensitive subsequence match: every char of `needle` appears in
/// `haystack` in order.
fn subsequence_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| hay.any(|c| c == wanted))
}

/// `text` cut to the dialog's message limits: at most `MAX_MESSAGE_CHARS`
/// characters and `MAX_REQUEST_BYTES` bytes, always on a char boundary.
fn clip_message(text: &str) -> String {
    let mut end = text
        .char_indices()
        .nth(MAX_MESSAGE_CHARS)
        .map_or(text.len(), |(index, _)| index);
    end = end.min(MAX_REQUEST_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

// ---------------------------------------------------------------------------
// Send to panel dialog
// ---------------------------------------------------------------------------

/// One selectable destination, captured when the dialog opens.
struct TargetRow {
    /// Panel-link id, unique, used as the send target.
    id: String,
    name: String,
    workspace: String,
    kind: PanelKind,
    is_agent: bool,
}

impl TargetRow {
    fn matches(&self, filter: &str) -> bool {
        filter.is_empty()
            || [self.name.as_str(), self.workspace.as_str(), self.kind.display_name()]
                .iter()
                .any(|field| subsequence_match(filter, field))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SendFocus {
    List,
    Message,
}

enum SendAction {
    None,
    Send { enter: bool },
}

pub(super) struct SendToPanelState {
    source_label: String,
    source_name: String,
    rows: Vec<TargetRow>,
    filter: String,
    applied_filter: String,
    filtered: Vec<usize>,
    selected: usize,
    message: String,
    error: Option<String>,
    focus: SendFocus,
    /// Tab asked to move keyboard focus; applied before the widgets draw.
    focus_pending: bool,
    reveal_selected: bool,
}

fn filter_rows(rows: &[TargetRow], filter: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.matches(filter))
        .map(|(index, _)| index)
        .collect()
}

impl SendToPanelState {
    /// Snapshot of the other terminal panels plus the source's current
    /// selection as the message. `None` when the source is gone.
    pub(super) fn open(board: &mut Board, source: PanelId) -> Option<Self> {
        let panel = board.panel(source)?;
        let source_local_id = panel.local_id.clone();
        // Titles can carry terminal-controlled escape sequences.
        let source_name = strip_escapes(&panel.display_title());
        let source_label = format!("{} \u{b7} {source_name}", panel.kind.display_name());
        let message = panel
            .terminal()
            .and_then(horizon_core::Terminal::selection_to_string)
            .map(|selection| clip_message(&selection))
            .unwrap_or_default();
        let LinkResponse::Ok(LinkOk::Panels(infos)) = board.handle_link_request(LinkRequest::List) else {
            return None;
        };
        let rows: Vec<TargetRow> = infos
            .into_iter()
            .filter(|info| info.id != source_local_id)
            .map(|info| {
                let kind = board
                    .panel_id_by_local_id(&info.id)
                    .and_then(|id| board.panel(id))
                    .map_or(PanelKind::Shell, |panel| panel.kind);
                TargetRow {
                    id: info.id,
                    name: info.name,
                    workspace: info.workspace,
                    kind,
                    is_agent: info.is_agent,
                }
            })
            .collect();
        let filtered = filter_rows(&rows, "");
        Some(Self {
            source_label,
            source_name,
            rows,
            filter: String::new(),
            applied_filter: String::new(),
            filtered,
            selected: 0,
            message,
            error: None,
            focus: SendFocus::List,
            focus_pending: false,
            reveal_selected: false,
        })
    }

    fn refilter(&mut self) {
        if self.filter != self.applied_filter {
            self.error = None;
            self.filtered = filter_rows(&self.rows, &self.filter);
            self.applied_filter.clone_from(&self.filter);
            self.selected = 0;
            self.reveal_selected = true;
        }
    }

    fn selected_row(&self) -> Option<&TargetRow> {
        self.filtered.get(self.selected).and_then(|&index| self.rows.get(index))
    }

    /// Why Send is unavailable, if it is.
    fn missing(&self) -> Option<&'static str> {
        if self.selected_row().is_none() {
            Some("Select a target panel")
        } else if self.message.trim().is_empty() {
            Some("Type a message")
        } else {
            None
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let len_i = i64::try_from(len).unwrap_or(i64::MAX);
        let next = (i64::try_from(self.selected).unwrap_or(0) + i64::from(delta)).rem_euclid(len_i);
        self.selected = usize::try_from(next).unwrap_or(0);
        self.reveal_selected = true;
    }
}

fn filter_id() -> Id {
    Id::new("send_to_panel_filter")
}

fn message_id() -> Id {
    Id::new("send_to_panel_message")
}

impl HorizonApp {
    pub(in crate::app) fn render_send_to_panel(&mut self, ctx: &Context) {
        let Some(mut state) = self.hub.link.send.take() else {
            return;
        };
        state.refilter();
        let selected_before = state.selected;
        let mut action = handle_send_keys(ctx, &mut state);
        let outcome = hub_ui::show_modal(
            ctx,
            Id::new("send_to_panel"),
            MODAL_WIDTH,
            ModalPlacement::Center,
            Some(filter_id()),
            |ui| {
                let from_ui = show_send_contents(ui, &mut state);
                if matches!(action, SendAction::None) {
                    action = from_ui;
                }
            },
        );

        if state.selected != selected_before {
            state.error = None;
        }
        if let SendAction::Send { enter } = action {
            match send_request(&state, enter) {
                Ok(request) => match self.execute_link_request(request) {
                    LinkResponse::Ok(_) => {
                        ctx.request_repaint();
                        return;
                    }
                    LinkResponse::Err { message, .. } => state.error = Some(message),
                },
                Err(SendBlocked::TooLarge) => state.error = Some(TOO_LARGE_MESSAGE.to_owned()),
                Err(SendBlocked::Missing) => {}
            }
        }
        if !outcome.dismissed {
            self.hub.link.send = Some(state);
        }
    }
}

/// Why `send_request` produced no request.
#[derive(Debug, PartialEq, Eq)]
enum SendBlocked {
    /// No target selected or an empty message; the Send buttons say why.
    Missing,
    /// The message is over `MAX_REQUEST_BYTES`; the board has no cap of its own.
    TooLarge,
}

/// `Send` request for the selected target, or why there is none.
fn send_request(state: &SendToPanelState, enter: bool) -> Result<LinkRequest, SendBlocked> {
    if state.missing().is_some() {
        return Err(SendBlocked::Missing);
    }
    if state.message.len() > MAX_REQUEST_BYTES {
        return Err(SendBlocked::TooLarge);
    }
    let row = state.selected_row().ok_or(SendBlocked::Missing)?;
    Ok(LinkRequest::Send {
        target: row.id.clone(),
        text: state.message.clone(),
        enter,
        from: Some(state.source_name.clone()),
    })
}

/// Keys handled before the widgets draw so a consumed key never reaches them.
fn handle_send_keys(ctx: &Context, state: &mut SendToPanelState) -> SendAction {
    let message_focused = ctx.memory(|memory| memory.has_focus(message_id()));
    state.focus = if message_focused {
        SendFocus::Message
    } else {
        SendFocus::List
    };
    ctx.input_mut(|input| {
        if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter) {
            return SendAction::Send { enter: false };
        }
        if input.consume_key(Modifiers::COMMAND, Key::Enter) {
            return SendAction::Send { enter: true };
        }
        if input.consume_key(Modifiers::NONE, Key::Tab) {
            state.focus = match state.focus {
                SendFocus::List => SendFocus::Message,
                SendFocus::Message => SendFocus::List,
            };
            state.focus_pending = true;
        }
        if state.focus == SendFocus::List {
            if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
                state.move_selection(1);
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
                state.move_selection(-1);
            }
        }
        SendAction::None
    })
}

fn show_send_contents(ui: &mut Ui, state: &mut SendToPanelState) -> SendAction {
    // Every block is sized explicitly; the theme's item spacing must not add to it.
    ui.spacing_mut().item_spacing.y = 0.0;
    if state.focus_pending {
        state.focus_pending = false;
        let target = match state.focus {
            SendFocus::List => filter_id(),
            SendFocus::Message => message_id(),
        };
        ui.memory_mut(|memory| memory.request_focus(target));
    }
    let width = ui.available_width();
    paint_header(ui, width, &state.source_label);
    paint_separator(ui, width);
    show_filter(ui, state);
    show_target_list(ui, state);
    paint_separator(ui, width);
    show_message(ui, state);
    if let Some(error) = &state.error {
        ui.add_space(6.0);
        padded(ui, 0, |ui| {
            ui.label(egui::RichText::new(error).size(11.0).color(theme::PALETTE_RED()));
        });
    }
    ui.add_space(8.0);
    show_footer(ui, state)
}

/// Content with the modal's 16 px side padding.
fn padded(ui: &mut Ui, top: i8, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .inner_margin(Margin {
            left: SECTION_PADDING,
            right: SECTION_PADDING,
            top,
            bottom: 0,
        })
        .show(ui, add);
}

fn paint_separator(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SUBTLE());
}

fn paint_header(ui: &mut Ui, width: f32, source_label: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEADER_HEIGHT), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, "Send to panel"));
    let painter = ui.painter();
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y - 7.0),
        Align2::LEFT_CENTER,
        "Send to panel",
        FontId::proportional(13.0),
        theme::FG(),
    );
    let from = hub_ui::elided(
        painter,
        &format!("from  {source_label}"),
        FontId::proportional(10.5),
        width - 32.0,
    );
    painter.galley(
        Pos2::new(rect.left() + 16.0, rect.center().y + 3.0),
        from,
        theme::FG_DIM(),
    );
}

fn show_filter(ui: &mut Ui, state: &mut SendToPanelState) {
    let mut enter_pressed = false;
    padded(ui, 12, |ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .id(filter_id())
                .hint_text("filter\u{2026}")
                .font(FontId::proportional(12.0))
                .margin(Margin::symmetric(8, 6))
                .desired_width(f32::INFINITY)
                .min_size(Vec2::new(0.0, 28.0)),
        );
        hold_tab(ui, &response);
        enter_pressed = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
    });
    ui.add_space(8.0);
    if enter_pressed {
        // Consumed so the multiline editor, focused in this same frame, does not insert a newline.
        ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
        ui.memory_mut(|memory| memory.request_focus(message_id()));
    }
}

/// Keeps Tab and the arrows from moving egui focus while `response` has focus.
fn hold_tab(ui: &Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                // egui must not use Tab or the arrows to move focus away from the
                // field; the dialog reads them itself. Escape stays with the modal.
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        });
    }
}

fn show_target_list(ui: &mut Ui, state: &mut SendToPanelState) {
    if state.filtered.is_empty() {
        let text = if state.rows.is_empty() {
            "No other panels"
        } else {
            "No panels match"
        };
        padded(ui, 8, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(theme::FG_DIM()));
        });
        ui.add_space(12.0);
        return;
    }
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
    let SendToPanelState {
        rows,
        filtered,
        selected,
        reveal_selected,
        ..
    } = state;
    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| {
                for (position, &row_index) in filtered.iter().enumerate() {
                    let Some(row) = rows.get(row_index) else {
                        continue;
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
                    response
                        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Send to {}", row.name)));
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        if pointer_moved {
                            *selected = position;
                        }
                    }
                    if response.clicked() {
                        *selected = position;
                    }
                    if *reveal_selected && *selected == position {
                        ui.scroll_to_rect(rect, None);
                        *reveal_selected = false;
                    }
                    if ui.is_rect_visible(rect) {
                        paint_target_row(ui.painter(), rect, row, *selected == position, response.hovered());
                        hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
                    }
                }
            });
        });
}

fn paint_target_row(painter: &Painter, rect: Rect, row: &TargetRow, selected: bool, hovered: bool) {
    if selected || hovered {
        let tint = if selected { 0.22 } else { 0.12 };
        painter.rect_filled(
            rect,
            CornerRadius::same(ROW_RADIUS),
            theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), tint),
        );
    }
    if selected {
        let bar = Rect::from_center_size(Pos2::new(rect.left() + 4.5, rect.center().y), Vec2::new(3.0, 24.0));
        painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT());
    }
    let (icon, icon_color) = panel_kind_icon(row.kind, theme::FG_SOFT(), selected);
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        FontId::monospace(10.0),
        icon_color,
    );
    let name_left = rect.left() + 46.0;
    let kind_left = rect.right() - 12.0 - KIND_COLUMN_WIDTH;
    let workspace_left = kind_left - WORKSPACE_COLUMN_WIDTH;
    let name = hub_ui::elided(
        painter,
        &row.name,
        FontId::proportional(12.0),
        (workspace_left - 8.0 - name_left).max(1.0),
    );
    painter.galley(
        Pos2::new(name_left, rect.center().y - name.size().y / 2.0),
        name,
        theme::FG(),
    );
    let workspace = hub_ui::elided(
        painter,
        &row.workspace,
        FontId::proportional(11.0),
        WORKSPACE_COLUMN_WIDTH - 8.0,
    );
    painter.galley(
        Pos2::new(workspace_left, rect.center().y - workspace.size().y / 2.0),
        workspace,
        theme::FG_DIM(),
    );
    let kind_text = painter.layout_no_wrap(
        row.kind.display_name().to_owned(),
        FontId::proportional(10.5),
        theme::FG_DIM(),
    );
    let kind_width = kind_text.size().x;
    painter.galley(
        Pos2::new(kind_left, rect.center().y - kind_text.size().y / 2.0),
        kind_text,
        theme::FG_DIM(),
    );
    if row.is_agent {
        let pill_rect = hub_ui::pill_rect(
            Pos2::new(kind_left + kind_width + 6.0, rect.center().y - 9.0),
            hub_ui::pill_width(painter, "agent"),
        );
        hub_ui::pill(painter, pill_rect, "agent", theme::ACCENT());
    }
}

fn show_message(ui: &mut Ui, state: &mut SendToPanelState) {
    padded(ui, 10, |ui| {
        ui.label(egui::RichText::new("Message").size(10.5).color(theme::FG_DIM()));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(MESSAGE_HEIGHT).show(ui, |ui| {
            let response = ui.add(
                egui::TextEdit::multiline(&mut state.message)
                    .id(message_id())
                    .hint_text("Type a message\u{2026}")
                    .font(FontId::monospace(11.5))
                    .char_limit(MAX_MESSAGE_CHARS)
                    .lock_focus(false)
                    .desired_width(f32::INFINITY)
                    .desired_rows(6),
            );
            hold_tab(ui, &response);
            if response.changed() {
                state.error = None;
            }
        });
    });
}

fn show_footer(ui: &mut Ui, state: &SendToPanelState) -> SendAction {
    let mut action = SendAction::None;
    let missing = state.missing();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 30.0 + 12.0), Sense::hover());
    let builder = UiBuilder::new()
        .max_rect(Rect::from_min_max(
            Pos2::new(rect.left() + 16.0, rect.top()),
            Pos2::new(rect.right() - 16.0, rect.bottom() - 12.0),
        ))
        .layout(Layout::right_to_left(Align::Center));
    // `new_child`, not `scope_builder`: the latter would allocate again in `ui`.
    let mut footer = ui.new_child(builder);
    footer.spacing_mut().item_spacing.x = 8.0;
    let send = hub_ui::primary_button(&mut footer, "Send", missing.is_none());
    let send = match missing {
        Some(reason) => send.on_hover_text(reason),
        None => send,
    };
    if send.clicked() {
        action = SendAction::Send { enter: true };
    }
    let without_enter = footer
        .add_enabled_ui(missing.is_none(), |ui| {
            hub_ui::secondary_button(ui, "Send without Enter")
        })
        .inner;
    let without_enter = match missing {
        Some(reason) => without_enter.on_disabled_hover_text(reason),
        None => without_enter,
    };
    if without_enter.clicked() {
        action = SendAction::Send { enter: false };
    }
    footer.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        hub_ui::kbd_hint(ui, "Esc", "close");
        hub_ui::kbd_hint(ui, "Ctrl+Enter", "send");
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, workspace: &str, kind: PanelKind) -> TargetRow {
        TargetRow {
            id: name.to_owned(),
            name: name.to_owned(),
            workspace: workspace.to_owned(),
            kind,
            is_agent: false,
        }
    }

    #[test]
    fn subsequence_match_is_ordered_and_case_insensitive() {
        assert!(subsequence_match("", "anything"));
        assert!(subsequence_match("ACM", "api-core-main"));
        assert!(subsequence_match("lg", "Logs"));
        assert!(!subsequence_match("gl", "logs"));
        assert!(!subsequence_match("x", "logs"));
    }

    #[test]
    fn filter_matches_name_workspace_or_kind() {
        let rows = vec![
            row("api-shell", "api-core", PanelKind::Shell),
            row("main", "infra", PanelKind::Claude),
            row("logs", "infra", PanelKind::Shell),
        ];
        assert_eq!(filter_rows(&rows, ""), vec![0, 1, 2]);
        assert_eq!(filter_rows(&rows, "infra"), vec![1, 2]);
        assert_eq!(filter_rows(&rows, "claude"), vec![1]);
        assert_eq!(filter_rows(&rows, "apic"), vec![0]);
        assert!(filter_rows(&rows, "zzz").is_empty());
    }

    fn state_with(rows: Vec<TargetRow>, message: &str) -> SendToPanelState {
        let filtered = filter_rows(&rows, "");
        SendToPanelState {
            source_label: "Claude".to_owned(),
            source_name: "src".to_owned(),
            rows,
            filter: String::new(),
            applied_filter: String::new(),
            filtered,
            selected: 0,
            message: message.to_owned(),
            error: None,
            focus: SendFocus::List,
            focus_pending: false,
            reveal_selected: false,
        }
    }

    #[test]
    fn send_needs_a_target_and_a_message() {
        let empty = state_with(Vec::new(), "hi");
        assert_eq!(empty.missing(), Some("Select a target panel"));
        assert_eq!(send_request(&empty, true), Err(SendBlocked::Missing));

        let blank = state_with(vec![row("a", "w", PanelKind::Shell)], "  \n ");
        assert_eq!(blank.missing(), Some("Type a message"));
        assert_eq!(send_request(&blank, true), Err(SendBlocked::Missing));
    }

    #[test]
    fn send_request_targets_the_selected_row_by_id() {
        let mut state = state_with(
            vec![row("a", "w", PanelKind::Shell), row("b", "w", PanelKind::Claude)],
            "hello",
        );
        state.selected = 1;
        assert_eq!(
            send_request(&state, false),
            Ok(LinkRequest::Send {
                target: "b".to_owned(),
                text: "hello".to_owned(),
                enter: false,
                from: Some("src".to_owned()),
            })
        );
    }

    #[test]
    fn move_selection_wraps_in_both_directions() {
        let mut state = state_with(
            vec![
                row("a", "w", PanelKind::Shell),
                row("b", "w", PanelKind::Shell),
                row("c", "w", PanelKind::Shell),
            ],
            "m",
        );
        state.move_selection(-1);
        assert_eq!(state.selected, 2);
        state.move_selection(1);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn refilter_resets_the_selection_only_when_the_text_changed() {
        let mut state = state_with(
            vec![row("a", "w", PanelKind::Shell), row("bb", "w", PanelKind::Shell)],
            "m",
        );
        state.selected = 1;
        state.refilter();
        assert_eq!(state.selected, 1);
        state.filter = "b".to_owned();
        state.refilter();
        assert_eq!(state.filtered, vec![1]);
        assert_eq!(state.selected, 0);
    }
    #[test]
    fn send_request_refuses_a_message_over_the_request_cap() {
        let at_cap = state_with(vec![row("a", "w", PanelKind::Shell)], &"x".repeat(MAX_REQUEST_BYTES));
        assert!(send_request(&at_cap, true).is_ok());
        let over = state_with(
            vec![row("a", "w", PanelKind::Shell)],
            &"x".repeat(MAX_REQUEST_BYTES + 1),
        );
        assert_eq!(send_request(&over, true), Err(SendBlocked::TooLarge));
    }

    #[test]
    fn clip_message_keeps_short_text_untouched() {
        assert_eq!(clip_message("hello\nworld"), "hello\nworld");
        assert_eq!(clip_message(""), "");
    }

    #[test]
    fn clip_message_caps_chars_and_bytes_on_a_char_boundary() {
        let ascii = "a".repeat(MAX_REQUEST_BYTES * 2);
        let clipped = clip_message(&ascii);
        assert_eq!(clipped.chars().count(), MAX_MESSAGE_CHARS);
        assert!(clipped.len() <= MAX_REQUEST_BYTES);

        // Four-byte chars: the char cap lands exactly on the byte cap, never past it.
        let wide = "\u{1F600}".repeat(MAX_MESSAGE_CHARS + 10);
        let clipped = clip_message(&wide);
        assert!(clipped.len() <= MAX_REQUEST_BYTES);
        assert!(clipped.chars().all(|c| c == '\u{1F600}'));
    }

    #[test]
    fn editing_state_changes_clear_the_error() {
        let mut state = state_with(
            vec![row("a", "w", PanelKind::Shell), row("bb", "w", PanelKind::Shell)],
            "m",
        );
        state.error = Some("boom".to_owned());
        state.refilter();
        assert!(state.error.is_some(), "same filter text keeps the error");
        state.filter = "b".to_owned();
        state.refilter();
        assert!(state.error.is_none());
    }
}
