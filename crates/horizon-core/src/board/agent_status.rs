//! Board side of the agent status: routes status events to panels, raises the attention entry
//! and queues the toast requests the interface drains.

use crate::agent_status::{AgentStatus, StatusAttention, StatusEvent};
use crate::attention::AttentionSeverity;
use crate::panel::PanelId;
use crate::workspace::WorkspaceId;

use super::Board;

/// `AttentionItem::source` of the entries raised from a status change.
pub const STATUS_ATTENTION_SOURCE: &str = "claude-status";

/// A status change the user should hear about; the interface turns it into a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusNotice {
    pub panel_id: PanelId,
    pub workspace_id: WorkspaceId,
    pub status: AgentStatus,
    pub severity: AttentionSeverity,
    /// Short detail line, also the attention entry's summary.
    pub summary: &'static str,
}

fn notice_summary(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Waiting => "Waiting for you",
        AgentStatus::Error => "Stopped with an error",
        AgentStatus::Done | AgentStatus::Working | AgentStatus::Unknown => "Ready for next prompt",
    }
}

impl Board {
    /// Applies `event` to `panel_id`: the panel's state moves, and when the change asks for
    /// attention (see [`crate::agent_status::attention_policy`]) an attention entry
    /// (source [`STATUS_ATTENTION_SOURCE`]) and a [`StatusNotice`] are raised. Entries of this
    /// source resolve when the agent is `Working` again or gone. Unknown panels are ignored.
    pub fn apply_agent_status(&mut self, panel_id: PanelId, event: StatusEvent) {
        let focused = self.focused == Some(panel_id);
        let Some(panel) = self.panel_mut(panel_id) else {
            return;
        };
        let workspace_id = panel.workspace_id;
        let attention = panel.apply_status_event(event, focused);
        let status = panel.agent_status();

        if matches!(status, AgentStatus::Working | AgentStatus::Unknown) {
            self.resolve_status_attention(panel_id);
        }
        if let StatusAttention::Notify { severity } = attention {
            let summary = notice_summary(status);
            if self.attention_enabled {
                self.resolve_status_attention(panel_id);
                self.create_attention(workspace_id, Some(panel_id), STATUS_ATTENTION_SOURCE, summary, severity);
            }
            self.status_notices.push(StatusNotice {
                panel_id,
                workspace_id,
                status,
                severity,
                summary,
            });
        }
    }

    /// Takes the toast requests raised since the last call, oldest first.
    pub fn take_status_notices(&mut self) -> Vec<StatusNotice> {
        std::mem::take(&mut self.status_notices)
    }

    /// Per frame: status events the terminals received, panels whose process died, and the
    /// focused panel counting as seen.
    pub(super) fn process_agent_status(&mut self) {
        let mut events: Vec<(PanelId, StatusEvent)> = Vec::new();
        for panel in &mut self.panels {
            for event in panel.take_status_events() {
                events.push((panel.id, event));
            }
            if panel.agent_status() != AgentStatus::Unknown && panel.child_exited() {
                events.push((
                    panel.id,
                    StatusEvent {
                        state: AgentStatus::Unknown,
                        session_id: None,
                    },
                ));
            }
        }
        for (panel_id, event) in events {
            self.apply_agent_status(panel_id, event);
        }
        if let Some(panel) = self.focused.and_then(|id| self.panel_mut(id)) {
            panel.mark_agent_status_seen();
        }
    }

    fn resolve_status_attention(&mut self, panel_id: PanelId) {
        for item in &mut self.attention {
            if item.panel_id == Some(panel_id) && item.source == STATUS_ATTENTION_SOURCE {
                item.resolve();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::agent_status::{AgentStatus, StatusEvent};
    use crate::attention::AttentionSeverity;
    use crate::board::Board;
    use crate::panel::{PanelId, PanelKind, PanelOptions};

    use super::{STATUS_ATTENTION_SOURCE, StatusNotice};

    /// A board with two editor panels (no terminal needed), in one workspace.
    fn editor_board() -> (Board, PanelId, PanelId) {
        let mut board = Board::new();
        let workspace_id = board.create_workspace("work");
        let options = || PanelOptions {
            kind: PanelKind::Editor,
            ..PanelOptions::default()
        };
        let first = board.create_panel(options(), workspace_id).expect("first panel");
        let second = board.create_panel(options(), workspace_id).expect("second panel");
        (board, first, second)
    }

    fn event(state: AgentStatus) -> StatusEvent {
        StatusEvent {
            state,
            session_id: None,
        }
    }

    #[test]
    fn unfocused_waiting_raises_attention_and_a_notice_until_working_again() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);
        board.attention_enabled = true;

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Waiting));

        let notices = board.take_status_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].status, AgentStatus::Waiting);
        assert_eq!(notices[0].severity, AttentionSeverity::High);
        assert!(board.take_status_notices().is_empty());
        let open: Vec<_> = board.unresolved_attention().collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].source, STATUS_ATTENTION_SOURCE);

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        assert!(board.unresolved_attention().next().is_none());
    }

    #[test]
    fn focused_panel_raises_nothing() {
        let (mut board, panel_id, _) = editor_board();
        board.focused = Some(panel_id);
        board.attention_enabled = true;

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Done));

        assert!(board.take_status_notices().is_empty());
        assert!(board.unresolved_attention().next().is_none());
    }

    #[test]
    fn disabled_attention_still_queues_the_notice_but_creates_no_entry() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);
        board.attention_enabled = false;

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Done));

        assert_eq!(
            board.take_status_notices(),
            vec![StatusNotice {
                panel_id,
                workspace_id: board.panel(panel_id).expect("panel").workspace_id,
                status: AgentStatus::Done,
                severity: AttentionSeverity::Medium,
                summary: "Ready for next prompt",
            }]
        );
        assert!(board.attention.is_empty());
    }

    #[test]
    fn done_stays_unseen_until_the_panel_is_focused() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Done));
        board.process_agent_status();
        assert!(!board.panel(panel_id).expect("panel").agent_status_seen());

        board.focus(panel_id);
        board.process_agent_status();
        assert!(board.panel(panel_id).expect("panel").agent_status_seen());
        assert_eq!(board.panel(panel_id).expect("panel").agent_status(), AgentStatus::Done);
    }

    #[test]
    fn an_unknown_event_clears_the_status_and_resolves_the_entry() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);
        board.attention_enabled = true;
        board.apply_agent_status(panel_id, event(AgentStatus::Waiting));
        assert_eq!(board.unresolved_attention().count(), 1);

        board.apply_agent_status(panel_id, event(AgentStatus::Unknown));

        assert_eq!(
            board.panel(panel_id).expect("panel").agent_status(),
            AgentStatus::Unknown
        );
        assert_eq!(board.unresolved_attention().count(), 0);
    }

    #[test]
    fn events_for_a_closed_panel_are_ignored() {
        let mut board = Board::new();
        board.apply_agent_status(PanelId(99), event(AgentStatus::Waiting));
        assert!(board.take_status_notices().is_empty());
    }
}
