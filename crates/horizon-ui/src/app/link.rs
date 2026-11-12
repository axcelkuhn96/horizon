//! Panel link on the UI side: the socket server lifecycle, request draining,
//! the panel context-menu entries and the "message received" titlebar badge.
//! The "Send to panel" dialog lives in [`send_dialog`].
//! Visual spec: `docs/design/claude-hub-ui.md` §6.

mod asks;
mod send_dialog;
mod visual;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use egui::{Align2, Context, CornerRadius, FontId, Id, Painter, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};
use horizon_core::link::{
    LinkOk, LinkRequest, LinkResponse, LinkServer, PanelInfo, PendingRequest, resolve_target, socket_path,
    strip_escapes,
};
use horizon_core::{PanelId, Waker};

use self::send_dialog::SendToPanelState;
pub(super) use self::visual::LinkVisual;
use super::HorizonApp;
use crate::theme;

/// Requests answered per frame; the rest wait for the next frame.
const MAX_REQUESTS_PER_FRAME: usize = 8;
/// Badge sender label for a request that names no sender.
const FALLBACK_SENDER: &str = "link";

/// Badge fully opaque until here, then fades out linearly until `BADGE_TOTAL`.
const BADGE_FULL: Duration = Duration::from_millis(2300);
const BADGE_TOTAL: Duration = Duration::from_secs(3);
/// Repaint cadence while the badge fades (about one frame at 60 Hz).
const BADGE_FADE_FRAME: Duration = Duration::from_millis(16);
const BADGE_NAME_CHARS: usize = 14;
const COPIED_TTL: Duration = Duration::from_millis(1500);

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
        1.0 - elapsed.saturating_sub(BADGE_FULL).as_secs_f32() / BADGE_TOTAL.saturating_sub(BADGE_FULL).as_secs_f32()
    }
}

/// When the UI must wake for this badge: at the start of the fade, then once
/// per frame until it is gone. `None` once the badge no longer paints.
#[must_use]
fn badge_wake(received_at: Instant, now: Instant) -> Option<Duration> {
    let elapsed = elapsed(received_at, now);
    if elapsed < BADGE_FULL {
        Some(BADGE_FULL.saturating_sub(elapsed))
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

/// Hands each queued request to `run`, which answers through the sender it is given (an
/// `ask` is answered later, not now) and keeps the `abandoned` flag of an ask to notice its
/// client leaving. Returns true when the UI should repaint: something was handled, or the
/// batch hit `cap` so more may be waiting for the next frame.
fn answer_batch(
    batch: Vec<PendingRequest>,
    cap: usize,
    mut run: impl FnMut(LinkRequest, &std::sync::mpsc::Sender<LinkResponse>, &Arc<AtomicBool>),
) -> bool {
    let mut repaint = batch.len() >= cap;
    for pending in batch {
        // The client was already told "no answer"; acting now could double a retried send.
        if pending.is_abandoned() {
            continue;
        }
        let abandoned = pending.abandoned_flag();
        run(pending.request, &pending.reply, &abandoned);
        repaint = true;
    }
    repaint
}

/// Badge label of a request's `from`: escape-free (it may come from a terminal-controlled
/// title), control characters dropped, capped to the badge width; `link` when there is no
/// usable name.
fn sender_label(from: Option<&str>) -> String {
    let clean: String = from
        .map(strip_escapes)
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        FALLBACK_SENDER.to_owned()
    } else {
        truncate_chars(clean, BADGE_NAME_CHARS)
    }
}

/// Badge label of a `send` request's sender (`link` for other requests).
fn send_label(request: &LinkRequest) -> String {
    match request {
        LinkRequest::Send { from, .. } => sender_label(from.as_deref()),
        _ => FALLBACK_SENDER.to_owned(),
    }
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the badge says about the message: a plain one, a question, or an answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BadgeKind {
    Message,
    AskReceived,
    Reply,
}

impl BadgeKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Message => "from",
            Self::AskReceived => "ask from",
            Self::Reply => "reply from",
        }
    }
}

struct LinkBadge {
    received_at: Instant,
    from: String,
    kind: BadgeKind,
}

/// What the titlebar paints: for a panel that just received a message, or one that waits
/// for the answer to its ask (`pending`: yellow, static).
pub(super) struct LinkBadgeView {
    pub(super) from: String,
    pub(super) opacity: f32,
    prefix: &'static str,
    pending: bool,
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
            prefix: badge.kind.prefix(),
            pending: false,
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
pub(super) fn start_link_server(ctx: &Context) -> Option<LinkServer> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if xdg.as_deref().is_none_or(|dir| dir.as_os_str().is_empty()) && home.as_os_str().is_empty() {
        tracing::warn!("panel link disabled: neither XDG_RUNTIME_DIR nor HOME is set");
        return None;
    }
    let path = socket_path(std::process::id(), xdg.as_deref(), &home);
    // Requests are drained on the UI thread: wake it as soon as one arrives.
    let waker: Waker = {
        let ctx = ctx.clone();
        Arc::new(move || ctx.request_repaint())
    };
    match LinkServer::start_with_waker(path, Some(waker)) {
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

    /// Per-frame link work: answer queued requests, fail the asks whose panel closed or
    /// whose time ran out, then schedule the single wake-up the badges and lines need and
    /// draw the "Copied" notice and the hub toasts.
    pub(super) fn poll_link(&mut self, ctx: &Context) {
        self.drain_link_requests(ctx);
        let now = Instant::now();
        self.sweep_link_asks(ctx, now);
        let wakes = [
            self.hub.link.prune_and_next_wake(now),
            visual::prune_visuals(&mut self.hub.link_visuals, now),
        ];
        if let Some(wake) = wakes.into_iter().flatten().min() {
            ctx.request_repaint_after(wake);
        }
        self.render_copied_notice(ctx, now);
        self.render_hub_toasts(ctx);
    }

    fn drain_link_requests(&mut self, ctx: &Context) {
        let Some(server) = self.hub.link.server.as_ref() else {
            return;
        };
        let batch = drain_capped(|| server.try_recv(), MAX_REQUESTS_PER_FRAME);
        let now = Instant::now();
        let mut effects = Vec::new();
        let repaint = answer_batch(batch, MAX_REQUESTS_PER_FRAME, |request, reply, abandoned| {
            let label = send_label(&request);
            if let Some(effect) = self
                .board
                .serve_link(&mut self.hub.link_asks, request, reply, abandoned, now)
            {
                effects.push((effect, label));
            }
        });
        for (effect, label) in effects {
            self.apply_link_effect(&effect, &label);
        }
        if repaint {
            ctx.request_repaint();
        }
    }

    /// Runs one `send` on the board (the same sanitizing path as the socket) for the
    /// dialog, and badges the receivers after a delivery.
    fn execute_link_request(&mut self, request: LinkRequest) -> LinkResponse {
        let label = send_label(&request);
        let (response, effect) = self.board.handle_link_request_with_effect(request);
        if let Some(effect) = effect {
            self.apply_link_effect(&effect, &label);
        }
        response
    }

    /// Badge to paint on `panel_id`'s titlebar this frame, if any.
    pub(super) fn link_badge_view(&self, panel_id: PanelId) -> Option<LinkBadgeView> {
        if let Some(view) = self.hub.link.badge_view(panel_id, Instant::now()) {
            return Some(view);
        }
        let target = self.hub.link_asks.waiting_on(panel_id)?;
        let name = self.board.panel(target)?.display_title();
        Some(LinkBadgeView {
            from: sender_label(Some(&name)),
            opacity: 1.0,
            prefix: "asking",
            pending: true,
        })
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
    let accent = if view.pending {
        theme::PALETTE_YELLOW()
    } else {
        theme::ACCENT()
    };
    let ellipsis = if view.pending { "\u{2026}" } else { "" };
    let text = format!(
        "{} {}{ellipsis}",
        view.prefix,
        truncate_chars(&strip_escapes(&view.from), BADGE_NAME_CHARS)
    );
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
    use horizon_core::link::SendTarget;

    use super::*;
    use crate::app::test_support::command_options;

    fn info(id: &str, name: &str) -> PanelInfo {
        PanelInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            workspace: "ws".to_owned(),
            kind: "shell".to_owned(),
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
            badge_wake(start, start + Duration::from_secs(2)),
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
                kind: BadgeKind::Message,
            },
        );
        link.badges.insert(
            PanelId(2),
            LinkBadge {
                received_at: start + Duration::from_secs(2),
                from: "b".to_owned(),
                kind: BadgeKind::Message,
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
                kind: BadgeKind::Message,
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
    fn pending(
        text: &str,
    ) -> (
        PendingRequest,
        std::sync::mpsc::Receiver<LinkResponse>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        PendingRequest::for_test(LinkRequest::Send {
            target: SendTarget::panel("a"),
            text: text.to_owned(),
            enter: false,
            from: None,
            from_id: None,
        })
    }

    fn ok_sent(_: LinkRequest, reply: &std::sync::mpsc::Sender<LinkResponse>, _: &Arc<AtomicBool>) {
        let _ = reply.send(LinkResponse::Ok(LinkOk::sent_to("a")));
    }

    #[test]
    fn answer_batch_sends_the_reply_and_asks_for_a_repaint() {
        let (request, answer, _) = pending("hi");
        let mut seen = Vec::new();
        let repaint = answer_batch(vec![request], MAX_REQUESTS_PER_FRAME, |request, reply, abandoned| {
            seen.push(request.clone());
            ok_sent(request, reply, abandoned);
        });
        assert!(repaint);
        assert_eq!(seen.len(), 1);
        assert_eq!(answer.try_recv().ok(), Some(LinkResponse::Ok(LinkOk::sent_to("a"))));
    }

    #[test]
    fn answer_batch_skips_abandoned_requests_without_running_or_repainting() {
        let (request, answer, abandoned) = pending("late");
        abandoned.store(true, std::sync::atomic::Ordering::Release);
        let mut ran = 0;
        let repaint = answer_batch(vec![request], MAX_REQUESTS_PER_FRAME, |request, reply, abandoned| {
            ran += 1;
            ok_sent(request, reply, abandoned);
        });
        assert!(!repaint, "nothing handled, batch below the cap");
        assert_eq!(ran, 0);
        assert!(answer.try_recv().is_err(), "an abandoned request gets no reply");
    }

    #[test]
    fn answer_batch_handles_live_requests_around_an_abandoned_one() {
        let (first, first_answer, _) = pending("one");
        let (dead, dead_answer, abandoned) = pending("two");
        let (last, last_answer, _) = pending("three");
        abandoned.store(true, std::sync::atomic::Ordering::Release);
        let repaint = answer_batch(vec![first, dead, last], MAX_REQUESTS_PER_FRAME, ok_sent);
        assert!(repaint);
        assert!(first_answer.try_recv().is_ok());
        assert!(dead_answer.try_recv().is_err());
        assert!(last_answer.try_recv().is_ok());
    }

    #[test]
    fn answer_batch_does_not_repaint_for_an_empty_queue() {
        assert!(!answer_batch(Vec::new(), MAX_REQUESTS_PER_FRAME, ok_sent));
    }

    #[test]
    fn answer_batch_repaints_when_a_full_batch_of_abandoned_requests_hits_the_cap() {
        let mut batch = Vec::new();
        for _ in 0..MAX_REQUESTS_PER_FRAME {
            let (request, _, abandoned) = pending("x");
            abandoned.store(true, std::sync::atomic::Ordering::Release);
            batch.push(request);
        }
        assert!(answer_batch(batch, MAX_REQUESTS_PER_FRAME, ok_sent));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_server_gives_restored_panels_the_socket() {
        use std::time::Duration;

        use horizon_core::{Config, PanelKind, PanelState, RuntimeState, WorkspaceState};

        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sock").join("1.sock");
        let server = LinkServer::start(socket.clone()).expect("link server");
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "w".to_owned(),
                name: "W".to_owned(),
                panels: vec![PanelState {
                    local_id: "p".to_owned(),
                    name: "restored".to_owned(),
                    kind: PanelKind::Command,
                    command: Some("sh".to_owned()),
                    args: vec!["-c".to_owned(), "echo \"R:$HORIZON_SOCKET\"; exec cat".to_owned()],
                    ..PanelState::default()
                }],
                ..WorkspaceState::default()
            }],
            ..RuntimeState::default()
        };

        let mut app = crate::app::test_support::test_app_with(dir.path(), &Config::default(), state, Some(server));

        assert_eq!(app.board.link_socket(), Some(socket.as_path()));
        let id = app.board.panels.first().expect("restored panel").id;
        let needle = format!("R:{}", socket.display());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while !seen && Instant::now() < deadline {
            let panel = app.board.panel_mut(id).expect("panel");
            let _ = panel.process_output();
            seen = panel
                .terminal()
                .is_some_and(|terminal| terminal.last_lines_text(24).contains(&needle));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(seen, "the restored panel never printed {needle}");
    }

    #[test]
    fn sender_label_cleans_caps_and_falls_back() {
        assert_eq!(sender_label(Some("backend")), "backend");
        assert_eq!(sender_label(Some("a\u{1b}[31mb\u{7}c\n")), "abc");
        assert_eq!(
            sender_label(Some("a-very-long-panel-name")).chars().count(),
            BADGE_NAME_CHARS
        );
        assert_eq!(sender_label(Some("  \u{1b}[2J ")), FALLBACK_SENDER);
        assert_eq!(sender_label(Some("")), FALLBACK_SENDER);
        assert_eq!(sender_label(None), FALLBACK_SENDER);
    }

    fn app_with_command_panel() -> (HorizonApp, PanelId) {
        let mut app = crate::app::test_support::test_app();
        let workspace = app.board.ensure_workspace();
        let panel = app
            .board
            .create_panel(command_options("receiver", "exec cat"), workspace)
            .expect("panel should spawn");
        (app, panel)
    }

    fn send_from(app: &mut HorizonApp, from: Option<&str>) -> LinkResponse {
        app.execute_link_request(LinkRequest::Send {
            target: SendTarget::panel("receiver"),
            text: "hi".to_owned(),
            enter: false,
            from: from.map(str::to_owned),
            from_id: None,
        })
    }

    #[test]
    fn a_delivered_send_badges_the_receiver_with_the_sender_name() {
        let (mut app, panel) = app_with_command_panel();

        let response = send_from(&mut app, Some("backend"));

        let local_id = app.board.panel(panel).expect("panel").local_id.clone();
        assert_eq!(response, LinkResponse::Ok(LinkOk::sent_to(&local_id)));
        let view = app.link_badge_view(panel).expect("badge");
        assert_eq!(view.from, "backend");
    }

    #[test]
    fn a_send_without_a_sender_badges_the_receiver_as_from_link() {
        let (mut app, panel) = app_with_command_panel();

        send_from(&mut app, None);

        assert_eq!(app.link_badge_view(panel).expect("badge").from, FALLBACK_SENDER);
    }

    #[test]
    fn a_failed_send_leaves_no_badge() {
        let (mut app, panel) = app_with_command_panel();

        let response = app.execute_link_request(LinkRequest::Send {
            target: SendTarget::panel("nobody"),
            text: "hi".to_owned(),
            enter: false,
            from: Some("backend".to_owned()),
            from_id: None,
        });

        assert!(matches!(response, LinkResponse::Err { .. }));
        assert!(app.link_badge_view(panel).is_none());
    }
}
