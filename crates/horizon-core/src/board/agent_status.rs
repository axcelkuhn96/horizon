//! Board side of the agent status: routes status events to panels, raises the attention entry
//! and queues the toast requests the interface drains.

use crate::agent_status::{AgentStatus, StatusAttention, StatusEvent, StatusSource};
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
        self.apply_agent_status_from(panel_id, event, StatusSource::Hook);
    }

    /// [`Self::apply_agent_status`] for an event of a known `source`.
    pub fn apply_agent_status_from(&mut self, panel_id: PanelId, event: StatusEvent, source: StatusSource) {
        // A panel only counts as looked at while its window has the OS focus too.
        let focused = self.focused == Some(panel_id) && self.window_focused;
        let Some(panel) = self.panel_mut(panel_id) else {
            return;
        };
        let workspace_id = panel.workspace_id;
        let attention = panel.apply_status_event_from(event, focused, source);
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
        if self.window_focused
            && let Some(panel) = self.focused.and_then(|id| self.panel_mut(id))
        {
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
    use crate::agent_status::{AgentStatus, StatusEvent, StatusSource};
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
    fn a_focused_panel_in_an_unfocused_window_still_notifies() {
        let (mut board, panel_id, _) = editor_board();
        board.focused = Some(panel_id);
        board.attention_enabled = true;
        board.set_window_focused(false);

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Done));
        board.process_agent_status();

        assert_eq!(board.take_status_notices().len(), 1);
        assert!(!board.panel(panel_id).expect("panel").agent_status_seen());

        board.set_window_focused(true);
        board.process_agent_status();
        assert!(board.panel(panel_id).expect("panel").agent_status_seen());
    }

    #[test]
    fn a_session_file_done_does_not_overwrite_an_error_until_working_again() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);

        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Error));
        board.apply_agent_status_from(panel_id, event(AgentStatus::Done), StatusSource::SessionFile);
        assert_eq!(board.panel(panel_id).expect("panel").agent_status(), AgentStatus::Error);

        board.apply_agent_status_from(panel_id, event(AgentStatus::Working), StatusSource::SessionFile);
        board.apply_agent_status_from(panel_id, event(AgentStatus::Done), StatusSource::SessionFile);
        assert_eq!(board.panel(panel_id).expect("panel").agent_status(), AgentStatus::Done);
    }

    #[test]
    fn a_hook_done_still_replaces_an_error() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);

        board.apply_agent_status(panel_id, event(AgentStatus::Error));
        board.apply_agent_status(panel_id, event(AgentStatus::Done));

        assert_eq!(board.panel(panel_id).expect("panel").agent_status(), AgentStatus::Done);
    }

    /// Runs `process_output` until `done` holds, or panics after a deadline.
    #[cfg(unix)]
    fn pump_until(board: &mut Board, what: &str, done: impl Fn(&Board) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            board.process_output();
            if done(board) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    fn open_status_entries(board: &Board, panel_id: PanelId) -> usize {
        board
            .unresolved_attention()
            .filter(|item| item.panel_id == Some(panel_id) && item.source == STATUS_ATTENTION_SOURCE)
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn the_status_entry_survives_the_blanket_attention_resolvers_of_a_terminal_panel() {
        let mut board = Board::new();
        let workspace_id = board.create_workspace("work");
        let script = "printf '\\033]2;HORIZON_STATUS:working\\007'; sleep 0.3; \
                      printf '\\033]2;HORIZON_STATUS:waiting\\007'; sleep 1; \
                      printf 'after-marker\\n'; sleep 30";
        let panel_id = board
            .create_panel(
                PanelOptions {
                    kind: PanelKind::Command,
                    command: Some("/bin/sh".to_owned()),
                    args: vec!["-c".to_owned(), script.to_owned()],
                    ..PanelOptions::default()
                },
                workspace_id,
            )
            .expect("command panel");
        board.focused = None;
        board.attention_enabled = true;

        pump_until(&mut board, "the status entry", |board| {
            open_status_entries(board, panel_id) == 1
        });
        // Later output makes `update_attention` run again (the shell prompt's detector finds
        // nothing, which used to resolve every open entry of the panel).
        pump_until(&mut board, "later terminal output", |board| {
            board
                .panel(panel_id)
                .and_then(|panel| panel.terminal())
                .is_some_and(|terminal| terminal.last_lines_text(16).contains("after-marker"))
        });
        board.process_output();

        assert_eq!(open_status_entries(&board, panel_id), 1);
        board.shutdown_terminal_panels();
    }

    #[test]
    fn the_agent_signal_reconciler_leaves_the_status_entry_open() {
        let (mut board, panel_id, other_id) = editor_board();
        board.focused = Some(other_id);
        board.attention_enabled = true;
        board.apply_agent_status(panel_id, event(AgentStatus::Working));
        board.apply_agent_status(panel_id, event(AgentStatus::Waiting));
        let workspace_id = board.panel(panel_id).expect("panel").workspace_id;

        board.reconcile_agent_attention_signal(panel_id, workspace_id, "Ready for input");
        board.reconcile_agent_attention_signal(panel_id, workspace_id, "");

        assert_eq!(open_status_entries(&board, panel_id), 1);
    }

    #[test]
    fn events_for_a_closed_panel_are_ignored() {
        let mut board = Board::new();
        board.apply_agent_status(PanelId(99), event(AgentStatus::Waiting));
        assert!(board.take_status_notices().is_empty());
    }
}
