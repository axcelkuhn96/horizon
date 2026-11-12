//! Toast requests for agent status changes.
//!
//! The text and kind are decided here; the request waits in [`StatusUi`] until
//! `HorizonApp::poll_agent_status` drains it (`StatusUi::take_toasts`) into the hub toast queue
//! (`HubToasts`) as one `Toast { kind, text, panel: Some(panel) }` per request.

use horizon_core::agent_status::AgentStatus;
use horizon_core::{PanelId, StatusNotice};

use super::StatusUi;
use crate::app::hub_toast::ToastKind;

/// Longest subject (session name or panel title) in a toast title.
const SUBJECT_MAX_CHARS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum StatusToastKind {
    Info,
    Warn,
    Error,
}

/// A toast to show: first line is the title, anything after the first newline the detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct StatusToast {
    pub(in crate::app) kind: StatusToastKind,
    pub(in crate::app) text: String,
    /// Panel a click on the toast focuses.
    pub(in crate::app) panel: PanelId,
}

impl StatusToastKind {
    /// The kind the hub toast queue draws.
    pub(in crate::app) const fn hub_kind(self) -> ToastKind {
        match self {
            Self::Info => ToastKind::Info,
            Self::Warn => ToastKind::Warn,
            Self::Error => ToastKind::Error,
        }
    }
}

fn elide(text: &str) -> String {
    let cleaned: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if cleaned.chars().count() <= SUBJECT_MAX_CHARS {
        return cleaned;
    }
    let mut short: String = cleaned.chars().take(SUBJECT_MAX_CHARS - 1).collect();
    short.push('\u{2026}');
    short
}

/// The toast for `notice`: `conta2 · expia-11 needs you` / `... finished` / `... stopped with an
/// error`, then the notice's summary. `account` is the profile name when known and `subject` the
/// session name (or the panel title).
#[must_use]
pub(in crate::app) fn toast_for_notice(notice: &StatusNotice, account: Option<&str>, subject: &str) -> StatusToast {
    let (kind, verb) = match notice.status {
        AgentStatus::Waiting => (StatusToastKind::Warn, "needs you"),
        AgentStatus::Error => (StatusToastKind::Error, "stopped with an error"),
        AgentStatus::Done | AgentStatus::Working | AgentStatus::Unknown => (StatusToastKind::Info, "finished"),
    };
    let subject = elide(subject);
    let title = match account {
        Some(account) => format!("{} \u{b7} {subject} {verb}", elide(account)),
        None => format!("{subject} {verb}"),
    };
    StatusToast {
        kind,
        text: format!("{title}\n{}", notice.summary),
        panel: notice.panel_id,
    }
}

impl StatusUi {
    /// Takes the queued toast requests, oldest first.
    pub(in crate::app) fn take_toasts(&mut self) -> Vec<StatusToast> {
        std::mem::take(&mut self.toasts)
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::{AttentionSeverity, WorkspaceId};

    use super::*;

    fn notice(status: AgentStatus, summary: &'static str) -> StatusNotice {
        StatusNotice {
            panel_id: PanelId(7),
            workspace_id: WorkspaceId(1),
            status,
            severity: AttentionSeverity::High,
            summary,
        }
    }

    #[test]
    fn waiting_is_a_warning_naming_account_and_session() {
        let toast = toast_for_notice(
            &notice(AgentStatus::Waiting, "Waiting for you"),
            Some("conta2"),
            "api-core",
        );

        assert_eq!(toast.kind, StatusToastKind::Warn);
        assert_eq!(toast.text, "conta2 \u{b7} api-core needs you\nWaiting for you");
        assert_eq!(toast.panel, PanelId(7));
    }

    #[test]
    fn done_is_info_and_error_is_error_without_an_account() {
        let done = toast_for_notice(&notice(AgentStatus::Done, "Ready for next prompt"), None, "api-core");
        assert_eq!(done.kind, StatusToastKind::Info);
        assert_eq!(done.text, "api-core finished\nReady for next prompt");

        let error = toast_for_notice(&notice(AgentStatus::Error, "Stopped with an error"), None, "api-core");
        assert_eq!(error.kind, StatusToastKind::Error);
        assert!(error.text.starts_with("api-core stopped with an error\n"));
    }

    #[test]
    fn subject_is_bounded_and_stripped_of_control_characters() {
        let toast = toast_for_notice(
            &notice(AgentStatus::Done, "x"),
            None,
            &format!("a\u{1b}[31m{}\nsecond line", "n".repeat(100)),
        );

        let title = toast.text.lines().next().expect("title line");
        assert!(!title.chars().any(char::is_control));
        assert!(title.chars().count() <= SUBJECT_MAX_CHARS + " finished".chars().count());
        assert!(title.contains('\u{2026}'));
    }

    #[test]
    fn status_kinds_map_to_the_hub_kinds() {
        assert_eq!(StatusToastKind::Info.hub_kind(), ToastKind::Info);
        assert_eq!(StatusToastKind::Warn.hub_kind(), ToastKind::Warn);
        assert_eq!(StatusToastKind::Error.hub_kind(), ToastKind::Error);
    }

    #[test]
    fn queued_toasts_are_taken_once_in_order() {
        let mut ui = StatusUi::default();
        ui.queue_capped(toast_for_notice(&notice(AgentStatus::Done, "a"), None, "one"));
        ui.queue_capped(toast_for_notice(&notice(AgentStatus::Done, "b"), None, "two"));

        let taken = ui.take_toasts();

        assert_eq!(taken.len(), 2);
        assert!(taken[0].text.starts_with("one "));
        assert!(ui.take_toasts().is_empty());
    }
}
