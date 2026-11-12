//! Toasts shared by the claude-hub features: a small queue (`HubToasts`) and the one place
//! that paints it, bottom-right in `Order::Tooltip`. Producers (link, status, usage, ...)
//! only call [`HubToasts::push`]. Visual spec: `docs/design/claude-hub-ui.md` §8.

use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, Context, CornerRadius, Id, Margin, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, WidgetInfo,
    WidgetType,
};
use horizon_core::PanelId;

use super::HorizonApp;
use crate::theme;

/// Toasts on screen at once; later ones wait in the queue.
const MAX_VISIBLE: usize = 3;
/// Queued toasts kept; the oldest drop first so a runaway producer cannot grow memory.
const MAX_QUEUED: usize = 16;
const TOAST_TTL: Duration = Duration::from_secs(6);
const TOAST_WIDTH: f32 = 320.0;
const EDGE_MARGIN: f32 = 12.0;
const TOAST_GAP: f32 = 6.0;
const BAR_WIDTH: f32 = 3.0;
const CLOSE_SIZE: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToastKind {
    Info,
    Warn,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "raised by the status and usage toasts of later tasks")
    )]
    Error,
}

/// `text`'s first line is the title, anything after the first newline the detail line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Toast {
    pub(super) kind: ToastKind,
    pub(super) text: String,
    /// Panel to focus when the toast is clicked.
    pub(super) panel: Option<PanelId>,
}

struct Entry {
    toast: Toast,
    /// When the toast first became visible; its clock runs from here.
    shown_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct HubToasts {
    entries: Vec<Entry>,
}

impl HubToasts {
    /// Queues `toast`. A toast with the same kind and text that is still queued or visible
    /// only restarts its clock (and takes the new panel) instead of stacking up.
    pub(super) fn push(&mut self, toast: Toast) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.toast.kind == toast.kind && entry.toast.text == toast.text)
        {
            entry.toast.panel = toast.panel;
            entry.shown_at = entry.shown_at.map(|_| Instant::now());
            return;
        }
        if self.entries.len() >= MAX_QUEUED {
            self.entries.remove(0);
        }
        self.entries.push(Entry { toast, shown_at: None });
    }

    /// Drops expired toasts and returns the (at most three) visible ones, oldest first.
    /// A toast's clock starts when it first appears here.
    pub(super) fn drain_visible(&mut self, now: Instant) -> Vec<Toast> {
        self.entries.retain(|entry| {
            entry
                .shown_at
                .is_none_or(|shown| now.saturating_duration_since(shown) < TOAST_TTL)
        });
        self.entries
            .iter_mut()
            .take(MAX_VISIBLE)
            .map(|entry| {
                entry.shown_at.get_or_insert(now);
                entry.toast.clone()
            })
            .collect()
    }

    /// Restarts the clock of `toast` (the pointer is over it).
    pub(super) fn hold(&mut self, toast: &Toast, now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.toast == *toast) {
            entry.shown_at = Some(now);
        }
    }

    pub(super) fn dismiss(&mut self, toast: &Toast) {
        self.entries.retain(|entry| entry.toast != *toast);
    }

    /// When the next visible toast expires; the UI must repaint then.
    pub(super) fn next_wake(&self, now: Instant) -> Option<Duration> {
        self.entries
            .iter()
            .take(MAX_VISIBLE)
            .filter_map(|entry| entry.shown_at)
            .map(|shown| TOAST_TTL.saturating_sub(now.saturating_duration_since(shown)))
            .min()
    }
}

fn kind_color(kind: ToastKind) -> Color32 {
    match kind {
        ToastKind::Info => theme::ACCENT(),
        ToastKind::Warn => theme::PALETTE_YELLOW(),
        ToastKind::Error => theme::PALETTE_RED(),
    }
}

/// Title and optional detail of a toast text.
fn split_lines(text: &str) -> (&str, Option<&str>) {
    match text.split_once('\n') {
        Some((title, detail)) => (title, Some(detail)),
        None => (text, None),
    }
}

/// What the user did to the painted toasts this frame.
struct ToastClicks {
    focus: Option<(Toast, PanelId)>,
    dismissed: Vec<Toast>,
    hovered: Vec<Toast>,
}

fn paint_toasts(ctx: &Context, visible: &[Toast]) -> ToastClicks {
    let mut clicks = ToastClicks {
        focus: None,
        dismissed: Vec::new(),
        hovered: Vec::new(),
    };
    egui::Area::new(Id::new("hub_toasts"))
        .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-EDGE_MARGIN, -EDGE_MARGIN))
        .order(Order::Tooltip)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = TOAST_GAP;
            ui.set_width(TOAST_WIDTH);
            for (index, toast) in visible.iter().enumerate() {
                paint_toast(ui, index, toast, &mut clicks);
            }
        });
    clicks
}

fn paint_toast(ui: &mut egui::Ui, index: usize, toast: &Toast, clicks: &mut ToastClicks) {
    let color = kind_color(toast.kind);
    let (title, detail) = split_lines(&toast.text);
    let frame = egui::Frame::new()
        .fill(theme::PANEL_BG_ALT())
        .stroke(Stroke::new(1.0_f32, theme::alpha(theme::FG_DIM(), 90)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin {
            left: 14,
            right: 24,
            top: 8,
            bottom: 8,
        });
    let inner = frame.show(ui, |ui| {
        ui.set_width(TOAST_WIDTH - 14.0 - 24.0);
        ui.add(
            egui::Label::new(egui::RichText::new(title).strong().size(12.0).color(theme::FG()))
                .truncate()
                .selectable(false),
        );
        if let Some(detail) = detail {
            ui.add(
                egui::Label::new(egui::RichText::new(detail).size(10.5).color(theme::FG_DIM()))
                    .truncate()
                    .selectable(false),
            );
        }
    });
    let rect = inner.response.rect;
    let response = ui.interact(rect, Id::new(("hub_toast", index)), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title));
    let painter = ui.painter();
    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + 1.0, rect.top() + 4.0),
        Pos2::new(rect.left() + 1.0 + BAR_WIDTH, rect.bottom() - 4.0),
    );
    painter.rect_filled(bar, CornerRadius::same(2), color);
    let close = Rect::from_center_size(
        Pos2::new(
            rect.right() - 6.0 - CLOSE_SIZE / 2.0,
            rect.top() + 6.0 + CLOSE_SIZE / 2.0,
        ),
        Vec2::splat(CLOSE_SIZE),
    );
    let over_close = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos())
        .is_some_and(|pos| close.contains(pos));
    if response.hovered() {
        clicks.hovered.push(toast.clone());
        let stroke = Stroke::new(1.2_f32, if over_close { theme::FG() } else { theme::FG_SOFT() });
        let inset = 4.0;
        painter.line_segment(
            [
                Pos2::new(close.left() + inset, close.top() + inset),
                Pos2::new(close.right() - inset, close.bottom() - inset),
            ],
            stroke,
        );
        painter.line_segment(
            [
                Pos2::new(close.right() - inset, close.top() + inset),
                Pos2::new(close.left() + inset, close.bottom() - inset),
            ],
            stroke,
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0_f32, theme::alpha(color, 120)),
            StrokeKind::Inside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        if over_close {
            clicks.dismissed.push(toast.clone());
        } else if let Some(panel) = toast.panel {
            clicks.focus = Some((toast.clone(), panel));
        } else {
            clicks.dismissed.push(toast.clone());
        }
    }
}

impl HorizonApp {
    /// Paints the hub toasts; clicking one focuses its panel and closes it. Called once per
    /// frame (from `poll_link`, which already runs with the other hub overlays).
    pub(super) fn render_hub_toasts(&mut self, ctx: &Context) {
        let now = Instant::now();
        let visible = self.hub.toasts.drain_visible(now);
        if visible.is_empty() {
            return;
        }
        let clicks = paint_toasts(ctx, &visible);
        for toast in &clicks.hovered {
            self.hub.toasts.hold(toast, now);
        }
        for toast in &clicks.dismissed {
            self.hub.toasts.dismiss(toast);
        }
        if let Some((toast, panel_id)) = clicks.focus {
            self.hub.toasts.dismiss(&toast);
            self.focus_panel_from_toast(ctx, panel_id);
        }
        if let Some(wake) = self.hub.toasts.next_wake(now) {
            ctx.request_repaint_after(wake);
        }
        if !clicks.dismissed.is_empty() {
            ctx.request_repaint();
        }
    }

    fn focus_panel_from_toast(&mut self, ctx: &Context, panel_id: PanelId) {
        if self.board.panel(panel_id).is_none() {
            return;
        }
        self.board.focus(panel_id);
        if let Some(workspace_id) = self.board.panel(panel_id).map(|panel| panel.workspace_id)
            && let Some((min, max)) = self.board.workspace_bounds(workspace_id)
        {
            self.focus_workspace_bounds(ctx, min, max, true);
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(kind: ToastKind, text: &str) -> Toast {
        Toast {
            kind,
            text: text.to_owned(),
            panel: None,
        }
    }

    #[test]
    fn toasts_dedupe_and_cap_three() {
        let mut toasts = HubToasts::default();
        let now = Instant::now();
        toasts.push(toast(ToastKind::Info, "a"));
        toasts.push(toast(ToastKind::Info, "a"));
        toasts.push(toast(ToastKind::Warn, "a"));
        assert_eq!(
            toasts.drain_visible(now).len(),
            2,
            "same kind+text dedupes; kind tells apart"
        );

        toasts.push(toast(ToastKind::Info, "b"));
        toasts.push(toast(ToastKind::Info, "c"));
        toasts.push(toast(ToastKind::Error, "d"));
        let visible = toasts.drain_visible(now);
        assert_eq!(visible.len(), MAX_VISIBLE);
        assert_eq!(visible[0].text, "a");
        assert_eq!(visible[2].text, "b");
    }

    #[test]
    fn toast_expires_after_ttl() {
        let mut toasts = HubToasts::default();
        let start = Instant::now();
        toasts.push(toast(ToastKind::Info, "gone soon"));
        assert_eq!(toasts.drain_visible(start).len(), 1);
        assert_eq!(toasts.next_wake(start), Some(TOAST_TTL));

        assert_eq!(toasts.drain_visible(start + Duration::from_millis(5_999)).len(), 1);
        assert!(toasts.drain_visible(start + TOAST_TTL).is_empty());
        assert_eq!(toasts.next_wake(start + TOAST_TTL), None);
    }

    #[test]
    fn a_queued_toast_starts_its_clock_when_it_becomes_visible() {
        let mut toasts = HubToasts::default();
        let start = Instant::now();
        for name in ["a", "b", "c", "d"] {
            toasts.push(toast(ToastKind::Info, name));
        }
        assert_eq!(toasts.drain_visible(start).len(), 3);

        let later = start + TOAST_TTL;
        let visible = toasts.drain_visible(later);
        assert_eq!(visible.len(), 1, "the first three expired, the fourth just appeared");
        assert_eq!(visible[0].text, "d");
        assert_eq!(toasts.next_wake(later), Some(TOAST_TTL));
    }

    #[test]
    fn hold_restarts_the_clock_and_dismiss_removes() {
        let mut toasts = HubToasts::default();
        let start = Instant::now();
        let kept = toast(ToastKind::Info, "kept");
        toasts.push(kept.clone());
        toasts.push(toast(ToastKind::Info, "closed"));
        toasts.drain_visible(start);

        toasts.hold(&kept, start + Duration::from_secs(5));
        let visible = toasts.drain_visible(start + Duration::from_secs(7));
        assert_eq!(visible, vec![kept.clone()], "held toast survives, the other expired");
        toasts.dismiss(&kept);
        assert!(toasts.drain_visible(start + Duration::from_secs(8)).is_empty());
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut toasts = HubToasts::default();
        for index in 0..(MAX_QUEUED + 10) {
            toasts.push(toast(ToastKind::Info, &index.to_string()));
        }
        assert_eq!(toasts.entries.len(), MAX_QUEUED);
        assert_eq!(toasts.entries[0].toast.text, "10");
    }

    #[test]
    fn split_lines_takes_title_then_detail() {
        assert_eq!(split_lines("only title"), ("only title", None));
        assert_eq!(split_lines("title\ndetail"), ("title", Some("detail")));
    }
}
