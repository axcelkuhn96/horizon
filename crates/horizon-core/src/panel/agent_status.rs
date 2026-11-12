//! Agent status of a panel: accessors and the entry point for status events.

use super::Panel;
use crate::agent_status::{AgentStatus, StatusAttention, StatusEvent, StatusSource};

impl Panel {
    /// What the agent in this panel is doing; `Unknown` when there is none.
    #[must_use]
    pub fn agent_status(&self) -> AgentStatus {
        self.agent_status.status()
    }

    /// `false` while a finished turn in this panel has not been looked at yet.
    #[must_use]
    pub fn agent_status_seen(&self) -> bool {
        self.agent_status.seen()
    }

    /// The agent session the latest status event named, if any.
    #[must_use]
    pub fn agent_session_id(&self) -> Option<&str> {
        self.agent_status.session_id()
    }

    /// The Claude session name (as `ListAgents` shows it) once the live-session file named it.
    #[must_use]
    pub fn agent_session_name(&self) -> Option<&str> {
        self.agent_status.session_name()
    }

    /// Records the Claude session name read from the live-session file (`None` clears it).
    pub fn set_agent_session_name(&mut self, name: Option<&str>) {
        self.agent_status.set_session_name(name);
    }

    /// Feeds a status event into the panel's state machine (`panel_focused` decides whether the
    /// change asks for attention). Prefer `Board::apply_agent_status`, which also raises the
    /// attention entry and the toast request.
    pub fn apply_status_event(&mut self, event: StatusEvent, panel_focused: bool) -> StatusAttention {
        self.agent_status.apply(event, panel_focused)
    }

    /// [`Self::apply_status_event`] for an event of a known `source` (see
    /// [`crate::agent_status::AgentStatusState::apply_from`]).
    pub fn apply_status_event_from(
        &mut self,
        event: StatusEvent,
        panel_focused: bool,
        source: StatusSource,
    ) -> StatusAttention {
        self.agent_status.apply_from(event, panel_focused, source)
    }

    /// The panel was looked at: a pending `Done` counts as seen.
    pub fn mark_agent_status_seen(&mut self) {
        if !self.agent_status.seen() {
            self.agent_status.mark_seen();
        }
    }

    /// The process is gone: back to nothing known.
    pub fn clear_agent_status_on_exit(&mut self) {
        self.agent_status.clear_on_exit();
    }

    /// Status events the terminal received (OSC `HORIZON_STATUS`) since the last call.
    pub(crate) fn take_status_events(&mut self) -> Vec<StatusEvent> {
        self.content
            .terminal_mut()
            .map(crate::terminal::Terminal::take_status_events)
            .unwrap_or_default()
    }
}
