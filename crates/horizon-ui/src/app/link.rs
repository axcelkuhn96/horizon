//! Panel link on the UI side: the socket server lifecycle, request draining,
//! the panel context-menu entries, the "Send to panel" dialog and the
//! "message received" titlebar badge.
//! Visual spec: `docs/design/claude-hub-ui.md` §6.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Context, CornerRadius, EventFilter, FontId, Id, Key, Layout, Margin, Modifiers, Painter, Pos2, Rect,
    Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::link::{
    LinkOk, LinkRequest, LinkResponse, LinkServer, MAX_REQUEST_BYTES, PanelInfo, resolve_target, socket_path,
};
use horizon_core::{Board, PanelId, PanelKind};

use super::HorizonApp;
use super::hub_ui::{self, ModalPlacement};
use super::panel_chrome::panel_kind_icon;
use crate::theme;

/// Requests answered per frame; the rest wait for the next frame.
const MAX_REQUESTS_PER_FRAME: usize = 8;
/// Sender label shown on the badge for requests that arrive over the socket.
const SOCKET_SENDER: &str = "link";

/// Badge fully opaque until here, then fades out linearly until `BADGE_TOTAL`.
const BADGE_FULL: Duration = Duration::from_millis(2300);
const BADGE_TOTAL: Duration = Duration::from_secs(3);
/// Repaint cadence while the badge fades (about one frame at 60 Hz).
const BADGE_FADE_FRAME: Duration = Duration::from_millis(16);
const BADGE_NAME_CHARS: usize = 14;
const COPIED_TTL: Duration = Duration::from_millis(1500);

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

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

fn elapsed(received_at: Instant, now: Instant) -> Duration {
    now.saturating_duration_since(received_at)
}

/// True while the badge still paints (3 s after the message arrived).
#[must_use]
pub(super) fn badge_visible(received_at: Instant, now: Instant) -> bool {
    elapsed(received_at, now) < BADGE_TOTAL
}

/// Time left until the badge disappears.
#[must_use]
pub(super) fn badge_remaining(received_at: Instant, now: Instant) -> Duration {
    BADGE_TOTAL.saturating_sub(elapsed(received_at, now))
}

/// 1.0 for the first 2.3 s, then a linear fade to 0.0 at 3 s.
#[must_use]
fn badge_opacity(received_at: Instant, now: Instant) -> f32 {
    let elapsed = elapsed(received_at, now);
    if elapsed <= BADGE_FULL {
        1.0
    } else if elapsed >= BADGE_TOTAL {
        0.0
    } else {
        1.0 - (elapsed - BADGE_FULL).as_secs_f32() / (BADGE_TOTAL - BADGE_FULL).as_secs_f32()
    }
}

/// When the UI must wake for this badge: at the start of the fade, then once
/// per frame until it is gone. `None` once the badge no longer paints.
#[must_use]
fn badge_wake(received_at: Instant, now: Instant) -> Option<Duration> {
    let elapsed = elapsed(received_at, now);
    if elapsed < BADGE_FULL {
        Some(BADGE_FULL - elapsed)
    } else if elapsed < BADGE_TOTAL {
        Some(BADGE_FADE_FRAME.min(badge_remaining(received_at, now)))
    } else {
        None
    }
}

/// Address another panel can use to reach `info`: its name, or its id when the
/// name would not resolve to this panel alone.
#[must_use]
fn panel_address(info: &PanelInfo, all: &[PanelInfo]) -> String {
    match resolve_target(all, &info.name) {
        Ok(found) if found.id == info.id => info.name.clone(),
        _ => info.id.clone(),
    }
}

/// Takes at most `cap` items from `next`.
fn drain_capped<T>(mut next: impl FnMut() -> Option<T>, cap: usize) -> Vec<T> {
    let mut taken = Vec::new();
    while taken.len() < cap {
        match next() {
            Some(item) => taken.push(item),
            None => break,
        }
    }
    taken
}

/// Name shortened to `max` characters, ending in an ellipsis when cut.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// Case-insensitive subsequence match: every char of `needle` appears in
/// `haystack` in order.
fn subsequence_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| hay.any(|c| c == wanted))
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct LinkBadge {
    received_at: Instant,
    from: String,
}

/// What the titlebar paints for a panel that just received a message.
pub(super) struct LinkBadgeView {
    pub(super) from: String,
    pub(super) opacity: f32,
}

/// Panel-link state owned by `HubState`.
#[derive(Default)]
pub(super) struct LinkUiState {
    /// Dropped on shutdown, which removes the socket file.
    server: Option<LinkServer>,
    badges: HashMap<PanelId, LinkBadge>,
    send: Option<SendToPanelState>,
    copied_at: Option<Instant>,
}

impl LinkUiState {
    pub(super) fn set_server(&mut self, server: Option<LinkServer>) {
        self.server = server;
    }

    fn badge_view(&self, panel_id: PanelId, now: Instant) -> Option<LinkBadgeView> {
        let badge = self.badges.get(&panel_id)?;
        let opacity = badge_opacity(badge.received_at, now);
        (opacity > 0.0).then(|| LinkBadgeView {
            from: badge.from.clone(),
            opacity,
        })
    }

    /// Drops expired badges and returns when the UI must wake next for the rest.
    fn prune_and_next_wake(&mut self, now: Instant) -> Option<Duration> {
        self.badges.retain(|_, badge| badge_visible(badge.received_at, now));
        self.badges
            .values()
            .filter_map(|badge| badge_wake(badge.received_at, now))
            .min()
    }
}

/// Starts the link socket for this process, or logs once and returns `None`.
pub(super) fn start_link_server() -> Option<LinkServer> {
    // Unit tests build many apps in one process; none should bind a real socket.
    if cfg!(test) {
        return None;
    }
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if xdg.as_deref().is_none_or(|dir| dir.as_os_str().is_empty()) && home.as_os_str().is_empty() {
        tracing::warn!("panel link disabled: neither XDG_RUNTIME_DIR nor HOME is set");
        return None;
    }
    let path = socket_path(std::process::id(), xdg.as_deref(), &home);
    match LinkServer::start(path) {
        Ok(server) => Some(server),
        Err(error) => {
            tracing::warn!("panel link disabled: {error}");
            None
        }
    }
}

impl HorizonApp {
    /// Drops the link server so the socket file is removed. Idempotent.
    pub(super) fn shutdown_link(&mut self) {
        self.hub.link.server = None;
    }

    /// Per-frame link work: answer queued requests, then schedule the single
    /// wake-up the badges need and draw the "Copied" notice.
    pub(super) fn poll_link(&mut self, ctx: &Context) {
        self.drain_link_requests(ctx);
        let now = Instant::now();
        if let Some(wake) = self.hub.link.prune_and_next_wake(now) {
            ctx.request_repaint_after(wake);
        }
        self.render_copied_notice(ctx, now);
    }

    fn drain_link_requests(&mut self, ctx: &Context) {
        let Some(server) = self.hub.link.server.as_ref() else {
            return;
        };
        let batch = drain_capped(|| server.try_recv(), MAX_REQUESTS_PER_FRAME);
        // A full batch means more may be waiting for the next frame.
        let mut repaint = batch.len() == MAX_REQUESTS_PER_FRAME;
        for pending in batch {
            // The client was already told "no answer"; acting now could double a retried send.
            if pending.is_abandoned() {
                continue;
            }
            let response = self.execute_link_request(pending.request, SOCKET_SENDER);
            let _ = pending.reply.send(response);
            repaint = true;
        }
        if repaint {
            ctx.request_repaint();
        }
    }

    /// Runs one request on the board (the same sanitizing path for the socket
    /// and the dialog) and flags the receiving panel's badge after a delivery.
    fn execute_link_request(&mut self, request: LinkRequest, from: &str) -> LinkResponse {
        let receiver = match &request {
            LinkRequest::Send { target, .. } => self.link_receiver(target),
            _ => None,
        };
        let response = self.board.handle_link_request(request);
        if matches!(response, LinkResponse::Ok(LinkOk::Sent))
            && let Some(panel_id) = receiver
        {
            self.hub.link.badges.insert(
                panel_id,
                LinkBadge {
                    received_at: Instant::now(),
                    from: truncate_chars(from, BADGE_NAME_CHARS),
                },
            );
        }
        response
    }

    /// Panel a `send` to `target` would reach, resolved the way the board does.
    fn link_receiver(&mut self, target: &str) -> Option<PanelId> {
        let LinkResponse::Ok(LinkOk::Panels(panels)) = self.board.handle_link_request(LinkRequest::List) else {
            return None;
        };
        let info = resolve_target(&panels, target).ok()?;
        self.board.panel_id_by_local_id(&info.id)
    }

    /// Badge to paint on `panel_id`'s titlebar this frame, if any.
    pub(super) fn link_badge_view(&self, panel_id: PanelId) -> Option<LinkBadgeView> {
        if self.hub.link.badges.is_empty() {
            return None;
        }
        self.hub.link.badge_view(panel_id, Instant::now())
    }

    /// "Send to panel…" and "Copy panel address" entries of the panel context menu.
    pub(super) fn show_link_menu_entries(&mut self, ui: &mut Ui, panel_id: PanelId) {
        if self
            .board
            .panel(panel_id)
            .is_none_or(|panel| panel.terminal().is_none())
        {
            return;
        }
        ui.separator();
        let has_target = self
            .board
            .panels
            .iter()
            .any(|panel| panel.id != panel_id && panel.terminal().is_some());
        let send = ui.add_enabled(has_target, egui::Button::new("Send to panel\u{2026}"));
        let send = send.on_disabled_hover_text("No other terminal panel to send to");
        if send.clicked() {
            self.hub.link.send = SendToPanelState::open(&mut self.board, panel_id);
            ui.close();
        }
        if ui.button("Copy panel address").clicked() {
            self.copy_panel_address(ui.ctx(), panel_id);
            ui.close();
        }
    }

    fn copy_panel_address(&mut self, ctx: &Context, panel_id: PanelId) {
        let Some(local_id) = self.board.panel(panel_id).map(|panel| panel.local_id.clone()) else {
            return;
        };
        let LinkResponse::Ok(LinkOk::Panels(all)) = self.board.handle_link_request(LinkRequest::List) else {
            return;
        };
        if let Some(info) = all.iter().find(|info| info.id == local_id) {
            ctx.copy_text(panel_address(info, &all));
            self.hub.link.copied_at = Some(Instant::now());
            ctx.request_repaint();
        }
    }

    fn render_copied_notice(&mut self, ctx: &Context, now: Instant) {
        let Some(copied_at) = self.hub.link.copied_at else {
            return;
        };
        let Some(remaining) = COPIED_TTL
            .checked_sub(elapsed(copied_at, now))
            .filter(|left| !left.is_zero())
        else {
            self.hub.link.copied_at = None;
            return;
        };
        ctx.request_repaint_after(remaining);
        egui::Area::new(Id::new("link_copied_notice"))
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Copied").color(theme::FG()).size(12.0));
                });
            });
    }
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
    fn open(board: &mut Board, source: PanelId) -> Option<Self> {
        let panel = board.panel(source)?;
        let source_local_id = panel.local_id.clone();
        let source_name = panel.display_title().into_owned();
        let source_label = format!("{} \u{b7} {source_name}", panel.kind.display_name());
        let message = panel
            .terminal()
            .and_then(horizon_core::Terminal::selection_to_string)
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
    pub(super) fn render_send_to_panel(&mut self, ctx: &Context) {
        let Some(mut state) = self.hub.link.send.take() else {
            return;
        };
        state.refilter();
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

        if let SendAction::Send { enter } = action
            && let Some(request) = send_request(&state, enter)
        {
            match self.execute_link_request(request, &state.source_name) {
                LinkResponse::Ok(_) => {
                    ctx.request_repaint();
                    return;
                }
                LinkResponse::Err { message, .. } => state.error = Some(message),
            }
        }
        if !outcome.dismissed {
            self.hub.link.send = Some(state);
        }
    }
}

/// `Send` request for the selected target, `None` while something is missing.
fn send_request(state: &SendToPanelState, enter: bool) -> Option<LinkRequest> {
    if state.missing().is_some() {
        return None;
    }
    let row = state.selected_row()?;
    Some(LinkRequest::Send {
        target: row.id.clone(),
        text: state.message.clone(),
        enter,
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
        ui.memory_mut(|memory| memory.request_focus(message_id()));
    }
}

/// Keeps Tab for the dialog (list/message toggle) while `response` has focus.
fn hold_tab(ui: &Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: false,
                    vertical_arrows: false,
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

// ---------------------------------------------------------------------------
// Titlebar badge
// ---------------------------------------------------------------------------

/// Titlebar badge `from <name>` with a leading dot, painted with `view.opacity`.
/// `stack_offset` is the width already taken by badges that sit in the same
/// slot, so this one stacks to their left.
pub(super) fn paint_link_badge(
    painter: &Painter,
    titlebar_rect: Rect,
    right_edge: f32,
    stack_offset: f32,
    view: &LinkBadgeView,
) {
    let accent = theme::ACCENT();
    let text = format!("from {}", truncate_chars(&view.from, BADGE_NAME_CHARS));
    let galley = painter.layout_no_wrap(text, FontId::proportional(10.0), accent.gamma_multiply(view.opacity));
    let badge_width = galley.size().x + 8.0 + 5.0 + 5.0 + 8.0;
    let badge_height = 18.0;
    let badge_right = right_edge - stack_offset;
    let badge_left = (badge_right - badge_width).max(titlebar_rect.min.x + 60.0);
    let badge_rect = Rect::from_min_size(
        Pos2::new(badge_left, titlebar_rect.center().y - badge_height * 0.5),
        Vec2::new(badge_right - badge_left, badge_height),
    );
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(4),
        theme::alpha(theme::blend(theme::TITLEBAR_BG(), accent, 0.18), 200).gamma_multiply(view.opacity),
    );
    painter.rect_stroke(
        badge_rect,
        CornerRadius::same(4),
        Stroke::new(1.0_f32, theme::alpha(accent, 140).gamma_multiply(view.opacity)),
        StrokeKind::Inside,
    );
    painter.circle_filled(
        Pos2::new(badge_rect.min.x + 8.0 + 2.5, badge_rect.center().y),
        2.5,
        accent.gamma_multiply(view.opacity),
    );
    painter.galley(
        Pos2::new(
            badge_rect.min.x + 8.0 + 5.0 + 5.0,
            badge_rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        accent,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, name: &str) -> PanelInfo {
        PanelInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            workspace: "ws".to_owned(),
            kind: "shell".to_owned(),
            is_agent: false,
        }
    }

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
    fn badge_visible_for_3_seconds() {
        let start = Instant::now();
        assert!(badge_visible(start, start));
        assert!(badge_visible(start, start + Duration::from_millis(2999)));
        assert!(!badge_visible(start, start + Duration::from_secs(3)));
        assert_eq!(
            badge_remaining(start, start + Duration::from_secs(1)),
            Duration::from_secs(2)
        );
        assert_eq!(badge_remaining(start, start + Duration::from_secs(5)), Duration::ZERO);
    }

    #[test]
    fn badge_is_opaque_then_fades_linearly() {
        let start = Instant::now();
        assert!((badge_opacity(start, start + Duration::from_millis(2300)) - 1.0).abs() < 1e-6);
        let mid = badge_opacity(start, start + Duration::from_millis(2650));
        assert!((mid - 0.5).abs() < 1e-3, "mid-fade opacity {mid}");
        assert!(badge_opacity(start, start + Duration::from_secs(3)).abs() < 1e-6);
    }

    #[test]
    fn badge_wakes_once_for_the_fade_then_per_frame_then_never() {
        let start = Instant::now();
        assert_eq!(badge_wake(start, start), Some(BADGE_FULL));
        assert_eq!(
            badge_wake(start, start + Duration::from_millis(2000)),
            Some(Duration::from_millis(300))
        );
        assert_eq!(
            badge_wake(start, start + Duration::from_millis(2400)),
            Some(BADGE_FADE_FRAME)
        );
        assert_eq!(
            badge_wake(start, start + Duration::from_millis(2995)),
            Some(Duration::from_millis(5))
        );
        assert_eq!(badge_wake(start, start + Duration::from_secs(3)), None);
    }

    #[test]
    fn expired_badges_are_pruned_and_no_wake_is_scheduled_when_idle() {
        let start = Instant::now();
        let mut link = LinkUiState::default();
        link.badges.insert(
            PanelId(1),
            LinkBadge {
                received_at: start,
                from: "a".to_owned(),
            },
        );
        link.badges.insert(
            PanelId(2),
            LinkBadge {
                received_at: start + Duration::from_secs(2),
                from: "b".to_owned(),
            },
        );

        let wake = link.prune_and_next_wake(start + Duration::from_millis(3100));
        assert_eq!(link.badges.len(), 1);
        assert_eq!(wake, Some(Duration::from_millis(1200)));

        assert_eq!(link.prune_and_next_wake(start + Duration::from_secs(10)), None);
        assert!(link.badges.is_empty());
    }

    #[test]
    fn badge_view_hides_after_the_fade() {
        let start = Instant::now();
        let mut link = LinkUiState::default();
        link.badges.insert(
            PanelId(7),
            LinkBadge {
                received_at: start,
                from: "api".to_owned(),
            },
        );
        assert!(link.badge_view(PanelId(7), start).is_some());
        assert!(link.badge_view(PanelId(7), start + Duration::from_secs(3)).is_none());
        assert!(link.badge_view(PanelId(8), start).is_none());
    }

    #[test]
    fn panel_address_uses_name_when_unique() {
        let all = [info("1", "api"), info("2", "logs")];
        assert_eq!(panel_address(&all[0], &all), "api");
    }

    #[test]
    fn panel_address_uses_id_when_name_duplicated() {
        let same = [info("4", "claude"), info("5", "claude"), info("6", "logs")];
        assert_eq!(panel_address(&same[0], &same), "4");
        assert_eq!(panel_address(&same[1], &same), "5");
        assert_eq!(panel_address(&same[2], &same), "logs");
    }

    #[test]
    fn panel_address_keeps_the_name_when_only_case_differs() {
        // An exact-name match wins over the case-insensitive tier, so both resolve.
        let all = [info("1", "claude"), info("2", "Claude")];
        assert_eq!(panel_address(&all[0], &all), "claude");
        assert_eq!(panel_address(&all[1], &all), "Claude");
    }

    #[test]
    fn panel_address_uses_id_when_name_is_empty_or_another_panels_id() {
        let all = [info("1", ""), info("2", "1")];
        assert_eq!(panel_address(&all[0], &all), "1");
        assert_eq!(panel_address(&all[1], &all), "2");
    }

    #[test]
    fn drain_respects_per_frame_cap() {
        let mut queue: Vec<u32> = (0..20).collect();
        queue.reverse();
        let taken = drain_capped(|| queue.pop(), MAX_REQUESTS_PER_FRAME);
        assert_eq!(taken.len(), MAX_REQUESTS_PER_FRAME);
        assert_eq!(queue.len(), 20 - MAX_REQUESTS_PER_FRAME);
    }

    #[test]
    fn drain_stops_when_the_queue_is_empty() {
        let mut queue = vec![1, 2, 3];
        assert_eq!(drain_capped(|| queue.pop(), MAX_REQUESTS_PER_FRAME), vec![3, 2, 1]);
        assert!(drain_capped(|| queue.pop(), MAX_REQUESTS_PER_FRAME).is_empty());
    }

    #[test]
    fn truncate_chars_cuts_with_an_ellipsis_at_the_limit() {
        assert_eq!(truncate_chars("short", 14), "short");
        assert_eq!(truncate_chars("exactly14chars", 14), "exactly14chars");
        let cut = truncate_chars("a-very-long-panel-name", 14);
        assert_eq!(cut.chars().count(), 14);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(truncate_chars("ação-ação-ação-ação", 6), "ação-\u{2026}");
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
        assert!(send_request(&empty, true).is_none());

        let blank = state_with(vec![row("a", "w", PanelKind::Shell)], "  \n ");
        assert_eq!(blank.missing(), Some("Type a message"));
        assert!(send_request(&blank, true).is_none());
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
            Some(LinkRequest::Send {
                target: "b".to_owned(),
                text: "hello".to_owned(),
                enter: false,
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
}
