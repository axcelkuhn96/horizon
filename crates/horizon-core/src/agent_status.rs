//! Per-panel agent status: what a Claude session is doing right now.
//!
//! Two sources feed the same state machine, [`AgentStatusState`]:
//! the `HORIZON_STATUS:<state>[:<session_id>]` OSC title that the optional Claude hooks write to
//! the panel's TTY (`horizon hook <event>`), and the per-process session file Claude Code keeps
//! under `<config>/sessions/<pid>.json` ([`live_session`]). Nothing here polls or owns a timer;
//! callers push events in and read the state out.

mod live_session;

pub use live_session::{
    LiveSessionInfo, LiveSessionRead, LiveSessionStatus, LiveSessionTracker, read_live_session,
    read_live_session_if_changed,
};

use crate::attention::AttentionSeverity;

/// Prefix of the OSC title that carries a status event.
pub const STATUS_OSC_PREFIX: &str = "HORIZON_STATUS:";
/// Longest OSC title body that is looked at; anything longer is ignored unparsed.
const MAX_STATUS_BODY_LEN: usize = 128;
const SESSION_ID_MIN_LEN: usize = 8;
const SESSION_ID_MAX_LEN: usize = 64;

/// What the agent in a panel is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentStatus {
    /// No agent, or nothing known yet. Nothing is painted for it.
    #[default]
    Unknown,
    /// Working on a prompt.
    Working,
    /// Blocked until the user answers (permission, question).
    Waiting,
    /// Finished the turn; ready for the next prompt.
    Done,
    /// Stopped with an error.
    Error,
}

impl AgentStatus {
    /// Sidebar priority of a workspace's dots: the highest one stands for the workspace.
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Done => 1,
            Self::Working => 2,
            Self::Error => 3,
            Self::Waiting => 4,
        }
    }
}

/// One status report: the new state and the session it belongs to, when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEvent {
    pub state: AgentStatus,
    pub session_id: Option<String>,
}

/// `true` for `[A-Za-z0-9-]{8,64}`.
#[must_use]
pub fn is_valid_session_id(session_id: &str) -> bool {
    (SESSION_ID_MIN_LEN..=SESSION_ID_MAX_LEN).contains(&session_id.len())
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Parses the OSC title `HORIZON_STATUS:<working|waiting|done|error|idle|end>[:<session_id>]`.
///
/// `idle` is a session that is ready for input (`Done`); `end` clears the status (`Unknown`).
/// Unknown states, a malformed or out-of-range session id, a missing prefix and oversized
/// bodies are all `None` (ignored).
#[must_use]
pub fn parse_status_osc(title: &str) -> Option<StatusEvent> {
    let body = title.strip_prefix(STATUS_OSC_PREFIX)?;
    if body.len() > MAX_STATUS_BODY_LEN {
        return None;
    }
    let (state, session_id) = match body.split_once(':') {
        Some((state, session_id)) => (state, Some(session_id)),
        None => (body, None),
    };
    let state = match state {
        "working" => AgentStatus::Working,
        "waiting" => AgentStatus::Waiting,
        "done" | "idle" => AgentStatus::Done,
        "error" => AgentStatus::Error,
        "end" => AgentStatus::Unknown,
        _ => return None,
    };
    let session_id = match session_id {
        Some(id) if is_valid_session_id(id) => Some(id.to_owned()),
        Some(_) => return None,
        None => None,
    };
    Some(StatusEvent { state, session_id })
}

/// The OSC title for `state` (and `session_id` when it is valid); the inverse of
/// [`parse_status_osc`]. `Unknown` is written as `end`.
#[must_use]
pub fn status_osc_title(state: AgentStatus, session_id: Option<&str>) -> String {
    let name = match state {
        AgentStatus::Unknown => "end",
        AgentStatus::Working => "working",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Done => "done",
        AgentStatus::Error => "error",
    };
    match session_id.filter(|id| is_valid_session_id(id)) {
        Some(id) => format!("{STATUS_OSC_PREFIX}{name}:{id}"),
        None => format!("{STATUS_OSC_PREFIX}{name}"),
    }
}

/// What a status change asks of the user interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusAttention {
    None,
    Notify { severity: AttentionSeverity },
}

/// Whether moving from `prev` to `next` deserves a toast and an attention entry: the agent now
/// needs the user (`Waiting`, `Error`) or finished a prompt (`Done` after `Working`), the panel
/// is not the focused one, and the state really changed.
#[must_use]
pub fn attention_policy(prev: AgentStatus, next: AgentStatus, panel_focused: bool) -> StatusAttention {
    if prev == next || panel_focused {
        return StatusAttention::None;
    }
    match next {
        AgentStatus::Waiting | AgentStatus::Error => StatusAttention::Notify {
            severity: AttentionSeverity::High,
        },
        AgentStatus::Done if prev == AgentStatus::Working => StatusAttention::Notify {
            severity: AttentionSeverity::Medium,
        },
        AgentStatus::Done | AgentStatus::Working | AgentStatus::Unknown => StatusAttention::None,
    }
}

/// The status of one panel plus whether its latest `Done` was already seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStatusState {
    status: AgentStatus,
    seen: bool,
    session_id: Option<String>,
    /// Session name as `ListAgents` shows it, from Claude's live-session file.
    session_name: Option<String>,
}

impl Default for AgentStatusState {
    fn default() -> Self {
        Self {
            status: AgentStatus::Unknown,
            seen: true,
            session_id: None,
            session_name: None,
        }
    }
}

impl AgentStatusState {
    #[must_use]
    pub const fn status(&self) -> AgentStatus {
        self.status
    }

    /// `false` while a `Done` that finished in an unfocused panel waits to be looked at.
    #[must_use]
    pub const fn seen(&self) -> bool {
        self.seen
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    #[must_use]
    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    /// Records the session name; allocates only when it changed.
    pub fn set_session_name(&mut self, name: Option<&str>) {
        if self.session_name.as_deref() != name {
            self.session_name = name.map(str::to_owned);
        }
    }

    /// Applies `event`. The same state twice changes nothing (a hook that repeats `working` on
    /// every tool call is a no-op); `Unknown` clears everything. Returns what the user
    /// interface should do about the change.
    pub fn apply(&mut self, event: StatusEvent, panel_focused: bool) -> StatusAttention {
        if event.state == AgentStatus::Unknown {
            self.clear_on_exit();
            return StatusAttention::None;
        }
        if event.session_id.is_some() {
            self.session_id = event.session_id;
        }
        let prev = self.status;
        if prev == event.state {
            return StatusAttention::None;
        }
        self.status = event.state;
        self.seen = !(event.state == AgentStatus::Done && prev == AgentStatus::Working && !panel_focused);
        attention_policy(prev, event.state, panel_focused)
    }

    /// The panel was looked at: a pending `Done` counts as seen.
    pub fn mark_seen(&mut self) {
        self.seen = true;
    }

    /// The process is gone (or the session ended): back to nothing known.
    pub fn clear_on_exit(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

    fn event(state: AgentStatus) -> StatusEvent {
        StatusEvent {
            state,
            session_id: None,
        }
    }

    #[test]
    fn parse_status_accepts_known_states() {
        for (text, expected) in [
            ("working", AgentStatus::Working),
            ("waiting", AgentStatus::Waiting),
            ("done", AgentStatus::Done),
            ("error", AgentStatus::Error),
            ("idle", AgentStatus::Done),
            ("end", AgentStatus::Unknown),
        ] {
            let parsed = parse_status_osc(&format!("HORIZON_STATUS:{text}"));
            assert_eq!(parsed, Some(event(expected)), "state {text}");
        }
        assert_eq!(
            parse_status_osc(&format!("HORIZON_STATUS:working:{SESSION}")),
            Some(StatusEvent {
                state: AgentStatus::Working,
                session_id: Some(SESSION.to_owned()),
            })
        );
    }

    #[test]
    fn parse_status_rejects_unknown_state_and_bad_session_id() {
        assert_eq!(parse_status_osc("HORIZON_STATUS:dancing"), None);
        assert_eq!(parse_status_osc("HORIZON_STATUS:"), None);
        assert_eq!(parse_status_osc("HORIZON_STATUS:working:short"), None);
        assert_eq!(parse_status_osc("HORIZON_STATUS:working:has space in it"), None);
        assert_eq!(parse_status_osc("HORIZON_STATUS:working:abcdefgh/../../x"), None);
        assert_eq!(parse_status_osc("HORIZON_STATUS:working:"), None);
        assert_eq!(
            parse_status_osc(&format!("HORIZON_STATUS:working:{}", "a".repeat(65))),
            None
        );
        assert_eq!(parse_status_osc("HORIZON_STATUS:working:ab\u{0}cdefgh"), None);
        assert_eq!(parse_status_osc("working"), None, "the prefix is required");
    }

    #[test]
    fn parse_status_oversized_body_is_ignored() {
        let body = "x".repeat(MAX_STATUS_BODY_LEN + 1);
        assert_eq!(parse_status_osc(&format!("HORIZON_STATUS:{body}")), None);
        assert_eq!(parse_status_osc(&format!("HORIZON_STATUS:working:{body}")), None);
        let limit = format!("HORIZON_STATUS:working:{}", "a".repeat(64));
        assert!(parse_status_osc(&limit).is_some());
    }

    #[test]
    fn osc_title_round_trips_through_the_parser() {
        for state in [
            AgentStatus::Working,
            AgentStatus::Waiting,
            AgentStatus::Done,
            AgentStatus::Error,
        ] {
            let with_id = status_osc_title(state, Some(SESSION));
            assert_eq!(
                parse_status_osc(&with_id),
                Some(StatusEvent {
                    state,
                    session_id: Some(SESSION.to_owned()),
                })
            );
            assert_eq!(parse_status_osc(&status_osc_title(state, None)), Some(event(state)));
        }
        assert_eq!(status_osc_title(AgentStatus::Unknown, None), "HORIZON_STATUS:end");
        assert_eq!(
            status_osc_title(AgentStatus::Done, Some("bad id")),
            "HORIZON_STATUS:done",
            "an invalid session id is dropped, not sent"
        );
    }

    #[test]
    fn working_then_done_notifies_medium_when_unfocused() {
        let mut state = AgentStatusState::default();
        assert_eq!(state.apply(event(AgentStatus::Working), false), StatusAttention::None);
        assert_eq!(
            state.apply(event(AgentStatus::Done), false),
            StatusAttention::Notify {
                severity: AttentionSeverity::Medium
            }
        );
    }

    #[test]
    fn waiting_notifies_high() {
        let mut state = AgentStatusState::default();
        state.apply(event(AgentStatus::Working), false);
        assert_eq!(
            state.apply(event(AgentStatus::Waiting), false),
            StatusAttention::Notify {
                severity: AttentionSeverity::High
            }
        );
        assert_eq!(
            attention_policy(AgentStatus::Working, AgentStatus::Error, false),
            StatusAttention::Notify {
                severity: AttentionSeverity::High
            }
        );
    }

    #[test]
    fn focused_panel_does_not_notify() {
        let mut state = AgentStatusState::default();
        state.apply(event(AgentStatus::Working), true);
        assert_eq!(state.apply(event(AgentStatus::Waiting), true), StatusAttention::None);
        assert_eq!(state.apply(event(AgentStatus::Done), true), StatusAttention::None);
        assert!(state.seen(), "a Done finished in the focused panel is already seen");
    }

    #[test]
    fn done_without_a_working_turn_before_it_is_quiet() {
        let mut state = AgentStatusState::default();
        assert_eq!(state.apply(event(AgentStatus::Done), false), StatusAttention::None);
        assert!(state.seen());
        state.apply(event(AgentStatus::Waiting), false);
        assert_eq!(state.apply(event(AgentStatus::Done), false), StatusAttention::None);
    }

    #[test]
    fn same_state_twice_is_deduped() {
        let mut state = AgentStatusState::default();
        state.apply(event(AgentStatus::Working), false);
        assert_eq!(state.apply(event(AgentStatus::Working), false), StatusAttention::None);
        state.apply(event(AgentStatus::Waiting), false);
        assert_eq!(
            state.apply(event(AgentStatus::Waiting), false),
            StatusAttention::None,
            "a repeated Waiting must not notify again"
        );
        assert_eq!(
            attention_policy(AgentStatus::Error, AgentStatus::Error, false),
            StatusAttention::None
        );
    }

    #[test]
    fn process_exit_resets_to_unknown() {
        let mut state = AgentStatusState::default();
        state.apply(
            StatusEvent {
                state: AgentStatus::Working,
                session_id: Some(SESSION.to_owned()),
            },
            false,
        );
        state.clear_on_exit();
        assert_eq!(state, AgentStatusState::default());
        assert_eq!(state.status(), AgentStatus::Unknown);
        assert_eq!(state.session_id(), None);

        state.apply(event(AgentStatus::Working), false);
        state.apply(event(AgentStatus::Unknown), false);
        assert_eq!(
            state.status(),
            AgentStatus::Unknown,
            "an `end` event clears the status too"
        );
    }

    #[test]
    fn done_marks_unseen_until_panel_focused() {
        let mut state = AgentStatusState::default();
        state.apply(event(AgentStatus::Working), false);
        state.apply(event(AgentStatus::Done), false);
        assert!(!state.seen());
        state.mark_seen();
        assert!(state.seen());
        state.apply(event(AgentStatus::Working), false);
        assert!(state.seen(), "only a finished turn is waiting to be seen");
    }

    #[test]
    fn session_id_is_kept_across_events_without_one() {
        let mut state = AgentStatusState::default();
        state.apply(
            StatusEvent {
                state: AgentStatus::Working,
                session_id: Some(SESSION.to_owned()),
            },
            false,
        );
        state.apply(event(AgentStatus::Done), false);
        assert_eq!(state.session_id(), Some(SESSION));
    }

    #[test]
    fn session_name_is_set_once_and_cleared_with_the_status() {
        let mut state = AgentStatusState::default();
        assert_eq!(state.session_name(), None);

        state.set_session_name(Some("expia-11"));
        state.set_session_name(Some("expia-11"));
        assert_eq!(state.session_name(), Some("expia-11"));

        state.clear_on_exit();
        assert_eq!(state.session_name(), None);
    }

    #[test]
    fn sidebar_priority_puts_waiting_first() {
        let order = [
            AgentStatus::Waiting,
            AgentStatus::Error,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Unknown,
        ];
        for pair in order.windows(2) {
            assert!(pair[0].priority() > pair[1].priority());
        }
    }
}
