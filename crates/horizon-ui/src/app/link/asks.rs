//! UI side of the link's message effects: badges, link lines and toasts for delivered
//! messages, and the per-frame resolution of pending asks.

use std::time::Instant;

use egui::Context;
use horizon_core::link::{LinkEffect, LinkEffectKind};

use super::visual::{self, LinkVisual, LinkVisualKind};
use super::{BadgeKind, FALLBACK_SENDER, HorizonApp, LinkBadge, sender_label};
use crate::app::hub_toast::{Toast, ToastKind};

/// Toast for the result of a broadcast: `Sent to 3` plus `· skipped 1` when some were left out.
fn broadcast_toast(delivered: usize, skipped: usize) -> Toast {
    let text = if skipped > 0 {
        format!("Sent to {delivered} \u{b7} skipped {skipped}")
    } else {
        format!("Sent to {delivered}")
    };
    Toast {
        kind: if skipped > 0 { ToastKind::Warn } else { ToastKind::Info },
        text,
        panel: None,
    }
}

impl HorizonApp {
    /// Marks the panels a handled request touched: the receiver's badge, the link line
    /// and, for a broadcast, the result toast. `send_label` is the sender name a plain
    /// `send` carried; asks and replies use the panel titles.
    pub(super) fn apply_link_effect(&mut self, effect: &LinkEffect, send_label: &str) {
        let now = Instant::now();
        let (kind, visual_kind) = match effect.kind {
            LinkEffectKind::Send | LinkEffectKind::Broadcast => (BadgeKind::Message, LinkVisualKind::Send),
            LinkEffectKind::Ask => (BadgeKind::AskReceived, LinkVisualKind::Ask),
            LinkEffectKind::Reply => (BadgeKind::Reply, LinkVisualKind::Reply),
        };
        let label = match effect.kind {
            LinkEffectKind::Send | LinkEffectKind::Broadcast => send_label.to_owned(),
            LinkEffectKind::Ask | LinkEffectKind::Reply => effect.from.and_then(|id| self.board.panel(id)).map_or_else(
                || FALLBACK_SENDER.to_owned(),
                |panel| sender_label(Some(&panel.display_title())),
            ),
        };
        for &to in &effect.to {
            self.hub.link.badges.insert(
                to,
                LinkBadge {
                    received_at: now,
                    from: label.clone(),
                    kind,
                },
            );
            // A waiting ask is drawn from the pending table until it resolves.
            if let Some(from) = effect.from
                && effect.kind != LinkEffectKind::Ask
            {
                visual::push_visual(
                    &mut self.hub.link_visuals,
                    LinkVisual {
                        from,
                        to,
                        started: now,
                        kind: visual_kind,
                    },
                );
            }
        }
        if effect.kind == LinkEffectKind::Broadcast {
            self.hub.toasts.push(broadcast_toast(effect.to.len(), effect.skipped));
        }
    }

    /// Fails the asks whose panel closed or whose time ran out, lets their lines fade, and
    /// schedules the repaint for the next deadline (nothing animates while an ask waits).
    pub(super) fn sweep_link_asks(&mut self, ctx: &Context, now: Instant) {
        if self.hub.link_asks.is_empty() {
            return;
        }
        let ended = self.board.resolve_link_asks(&mut self.hub.link_asks, now);
        if !ended.is_empty() {
            for end in ended {
                visual::push_visual(
                    &mut self.hub.link_visuals,
                    LinkVisual::fading(end.asker, end.target, now),
                );
            }
            ctx.request_repaint();
        }
        if let Some(deadline) = self.hub.link_asks.next_deadline() {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::PanelId;

    use super::*;

    #[test]
    fn broadcast_toast_reports_delivered_and_skipped() {
        assert_eq!(broadcast_toast(3, 0).text, "Sent to 3");
        assert_eq!(broadcast_toast(3, 0).kind, ToastKind::Info);
        let partial = broadcast_toast(3, 1);
        assert_eq!(partial.text, "Sent to 3 \u{b7} skipped 1");
        assert_eq!(partial.kind, ToastKind::Warn);
    }

    fn app_with_two_agents() -> (HorizonApp, PanelId, PanelId) {
        let mut app = crate::app::test_support::test_app();
        let workspace = app.board.ensure_workspace();
        let first = app
            .board
            .create_panel(
                crate::app::test_support::command_options("first", "exec cat"),
                workspace,
            )
            .expect("panel");
        let second = app
            .board
            .create_panel(
                crate::app::test_support::command_options("second", "exec cat"),
                workspace,
            )
            .expect("panel");
        (app, first, second)
    }

    #[test]
    fn a_broadcast_effect_badges_every_receiver_draws_lines_and_toasts_the_result() {
        let (mut app, first, second) = app_with_two_agents();
        let effect = LinkEffect {
            kind: LinkEffectKind::Broadcast,
            from: Some(first),
            to: vec![second],
            skipped: 1,
        };

        app.apply_link_effect(&effect, "first");

        let view = app.link_badge_view(second).expect("badge");
        assert_eq!(view.from, "first");
        assert_eq!(app.hub.link_visuals.len(), 1);
        assert_eq!(app.hub.link_visuals[0].kind, LinkVisualKind::Send);
        let toasts = app.hub.toasts.drain_visible(Instant::now());
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].text, "Sent to 1 \u{b7} skipped 1");
    }

    #[test]
    fn an_ask_effect_badges_the_target_with_the_asker_title_and_draws_no_transient_line() {
        let (mut app, first, second) = app_with_two_agents();
        let effect = LinkEffect {
            kind: LinkEffectKind::Ask,
            from: Some(first),
            to: vec![second],
            skipped: 0,
        };

        app.apply_link_effect(&effect, "ignored");

        assert_eq!(app.link_badge_view(second).expect("badge").from, "first");
        assert!(app.hub.link_visuals.is_empty(), "the pending table draws a waiting ask");
        assert!(app.hub.toasts.drain_visible(Instant::now()).is_empty());
    }

    #[test]
    fn a_reply_effect_draws_a_reply_line_back_to_the_asker() {
        let (mut app, first, second) = app_with_two_agents();
        let effect = LinkEffect {
            kind: LinkEffectKind::Reply,
            from: Some(second),
            to: vec![first],
            skipped: 0,
        };

        app.apply_link_effect(&effect, "");

        assert_eq!(app.hub.link_visuals[0].kind, LinkVisualKind::Reply);
        assert_eq!(app.link_badge_view(first).expect("badge").from, "second");
    }

    #[test]
    fn a_waiting_ask_shows_the_asking_badge_on_the_asker_until_it_resolves() {
        let (mut app, first, second) = app_with_two_agents();
        let (reply, answer) = std::sync::mpsc::channel();
        let now = Instant::now();
        app.hub.link_asks.insert(first, second, 30, reply, now).expect("insert");

        let view = app.link_badge_view(first).expect("asking badge");
        assert!(view.pending);
        assert_eq!(view.from, "second");
        assert!((view.opacity - 1.0).abs() < f32::EPSILON);
        assert!(app.link_badge_view(second).is_none());

        app.board.close_panel(second);
        app.sweep_link_asks(&Context::default(), now);

        assert!(app.link_badge_view(first).is_none());
        assert!(answer.try_recv().is_ok(), "the asker is told the target closed");
        assert_eq!(
            app.hub.link_visuals.len(),
            1,
            "the dashed line fades instead of vanishing"
        );
    }
}
