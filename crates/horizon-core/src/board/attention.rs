use std::time::Duration;

use crate::attention::{AttentionId, AttentionItem, AttentionSeverity};
use crate::panel::{PanelId, current_unix_millis};
use crate::workspace::WorkspaceId;

use super::Board;
use super::agent_status::STATUS_ATTENTION_SOURCE;

/// Resolved or dismissed entries kept; older ones are dropped as new entries arrive.
const MAX_CLOSED_ATTENTION: usize = 64;

impl Board {
    pub(super) fn update_attention(&mut self) {
        // Only inspect panels that received new terminal output this frame.
        // `detect_attention()` locks the terminal and iterates the full
        // display, so skipping idle panels avoids significant per-frame cost.
        let panel_states: Vec<_> = self
            .panels
            .iter_mut()
            .filter(|panel| panel.had_recent_output)
            .map(|panel| {
                let bell = panel.take_bell();
                let notification = panel.take_notification();
                (
                    panel.id,
                    panel.workspace_id,
                    panel.kind,
                    panel.detect_attention(),
                    bell,
                    notification,
                    panel.launched_at_millis,
                )
            })
            .collect();

        for (panel_id, workspace_id, kind, attention, bell, notification, launched_at) in panel_states {
            if let Some(notif) = notification {
                let severity = match notif.severity.as_str() {
                    "attention" => AttentionSeverity::High,
                    "done" => AttentionSeverity::Medium,
                    _ => AttentionSeverity::Low,
                };
                self.create_attention(workspace_id, Some(panel_id), "agent-notify", notif.message, severity);
            }

            if let Some(summary) = attention {
                self.reconcile_agent_attention_signal(panel_id, workspace_id, summary);
            } else {
                self.reconcile_agent_attention_signal(panel_id, workspace_id, "");
            }

            let has_open = self.unresolved_attention_for_panel(panel_id).is_some();

            if attention.is_none() && bell && kind.is_agent() {
                let age_ms = current_unix_millis().saturating_sub(launched_at);
                if !has_open && age_ms >= 10_000 {
                    self.create_attention(
                        workspace_id,
                        Some(panel_id),
                        "agent",
                        "Needs attention",
                        AttentionSeverity::High,
                    );
                }
            } else if attention.is_none() && has_open {
                let ids_to_resolve: Vec<_> = self
                    .attention
                    .iter()
                    .filter(|item| {
                        item.panel_id == Some(panel_id) && item.is_open() && item.source != STATUS_ATTENTION_SOURCE
                    })
                    .map(|item| item.id)
                    .collect();
                for id in ids_to_resolve {
                    let _ = self.resolve_attention(id);
                }
            }
        }

        self.dismiss_expired_ready_attention(super::READY_FOR_INPUT_AUTO_DISMISS_AFTER);
    }

    pub(crate) fn reconcile_agent_attention_signal(
        &mut self,
        panel_id: PanelId,
        workspace_id: WorkspaceId,
        summary: &str,
    ) {
        let next_signal = (!summary.is_empty()).then_some(summary);
        let previous_signal = self.panel_attention_signals.get(&panel_id).map(String::as_str);
        if previous_signal == next_signal {
            return;
        }

        self.resolve_open_attention_for_panel(panel_id);

        match next_signal {
            Some(summary) => {
                self.create_attention(workspace_id, Some(panel_id), "agent", summary, AttentionSeverity::High);
                self.panel_attention_signals.insert(panel_id, summary.to_string());
            }
            None => {
                self.panel_attention_signals.remove(&panel_id);
            }
        }
    }

    /// Resolves the panel's open entries except the agent-status one, which only the status
    /// machine resolves (when the agent works again or is gone).
    fn resolve_open_attention_for_panel(&mut self, panel_id: PanelId) {
        let ids_to_resolve: Vec<_> = self
            .attention
            .iter()
            .filter(|item| item.panel_id == Some(panel_id) && item.is_open() && item.source != STATUS_ATTENTION_SOURCE)
            .map(|item| item.id)
            .collect();
        for id in ids_to_resolve {
            let _ = self.resolve_attention(id);
        }
    }

    pub(crate) fn dismiss_expired_ready_attention(&mut self, max_age: Duration) {
        let now = std::time::SystemTime::now();
        for item in &mut self.attention {
            let should_dismiss = item.is_open()
                && item.is_agent_ready_for_input()
                && now.duration_since(item.created_at).is_ok_and(|age| age >= max_age);
            if should_dismiss {
                item.dismiss();
            }
        }
    }

    pub fn create_attention(
        &mut self,
        workspace_id: WorkspaceId,
        panel_id: Option<PanelId>,
        source: impl Into<String>,
        summary: impl Into<String>,
        severity: AttentionSeverity,
    ) -> AttentionId {
        let id = AttentionId(self.next_attention_id);
        self.next_attention_id += 1;
        self.attention.push(AttentionItem::new(
            id,
            workspace_id,
            panel_id,
            source,
            summary,
            severity,
        ));
        self.trim_closed_attention();
        id
    }

    /// Keeps the newest [`MAX_CLOSED_ATTENTION`] resolved/dismissed entries (the list is in
    /// creation order); open ones are never dropped.
    fn trim_closed_attention(&mut self) {
        let closed = self.attention.iter().filter(|item| !item.is_open()).count();
        let mut excess = closed.saturating_sub(MAX_CLOSED_ATTENTION);
        if excess == 0 {
            return;
        }
        self.attention.retain(|item| {
            if item.is_open() || excess == 0 {
                return true;
            }
            excess -= 1;
            false
        });
    }

    #[must_use]
    pub fn resolve_attention(&mut self, id: AttentionId) -> bool {
        if let Some(item) = self.attention.iter_mut().find(|item| item.id == id) {
            item.resolve();
            return true;
        }

        false
    }

    #[must_use]
    pub fn dismiss_attention(&mut self, id: AttentionId) -> bool {
        if let Some(item) = self.attention.iter_mut().find(|item| item.id == id) {
            item.dismiss();
            return true;
        }

        false
    }

    pub fn unresolved_attention(&self) -> impl Iterator<Item = &AttentionItem> + '_ {
        self.attention.iter().filter(|item| item.is_open())
    }

    #[must_use]
    pub fn unresolved_attention_for_panel(&self, panel_id: PanelId) -> Option<&AttentionItem> {
        self.unresolved_attention()
            .filter(|item| item.panel_id == Some(panel_id))
            .max_by(|left, right| {
                left.severity
                    .cmp(&right.severity)
                    .then_with(|| left.id.0.cmp(&right.id.0))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::MAX_CLOSED_ATTENTION;
    use crate::attention::AttentionSeverity;
    use crate::board::Board;

    #[test]
    fn resolved_entries_are_capped_and_open_ones_survive() {
        let mut board = Board::new();
        let workspace_id = board.create_workspace("work");
        let open = board.create_attention(workspace_id, None, "test", "stays open", AttentionSeverity::Low);
        let mut first_closed = None;
        for index in 0..(MAX_CLOSED_ATTENTION + 10) {
            let id = board.create_attention(
                workspace_id,
                None,
                "test",
                format!("closed {index}"),
                AttentionSeverity::Low,
            );
            first_closed.get_or_insert(id);
            assert!(board.resolve_attention(id));
        }
        board.create_attention(workspace_id, None, "test", "one more", AttentionSeverity::Low);

        let closed = board.attention.iter().filter(|item| !item.is_open()).count();
        assert_eq!(closed, MAX_CLOSED_ATTENTION);
        assert!(board.attention.iter().any(|item| item.id == open && item.is_open()));
        assert!(
            board.attention.iter().all(|item| Some(item.id) != first_closed),
            "the oldest resolved entry is dropped first"
        );
    }
}
