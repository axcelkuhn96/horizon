//! What the modal's actions do: select, expand, ask for an account and resume.
//!
//! A resume opens a new Claude panel for the session's account, started in the folder the
//! session ran in with the panel bound to the session (Horizon then launches `claude --resume
//! <id>`), or types the same command into the focused idle shell.

use egui::Context;
use horizon_core::claude_sessions::{SessionSummary, attribute_accounts, resume_command_args};
use horizon_core::{PanelId, PanelOptions, PanelResume, PresetConfig, WorkspaceId, claude_transcript_exists_for_env};

use super::model::{ListWindow, ResumeAccount, Running, resolve_resume_account, resume_cwd};
use super::{PendingPick, SessionsAction, SessionsFlow, SessionsModal};
use crate::app::HorizonApp;
use crate::app::claude_launcher::find_profile_preset;
use crate::app::hub_toast::{Toast, ToastKind};

const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

impl HorizonApp {
    pub(super) fn apply_sessions_action(
        &mut self,
        ctx: &Context,
        modal: &mut SessionsModal,
        action: SessionsAction,
    ) -> SessionsFlow {
        match action {
            SessionsAction::None => {}
            SessionsAction::Select(position) => modal.selected = position,
            SessionsAction::ToggleAgents => modal.agents_expanded = !modal.agents_expanded,
            SessionsAction::ShowAll => modal.window = ListWindow::all(),
            SessionsAction::CancelPick => {
                modal.pick = None;
                ctx.memory_mut(|memory| memory.request_focus(super::view::filter_id()));
            }
            SessionsAction::Resume { session, here } => {
                let Some(summary) = modal.sessions.get(session) else {
                    return SessionsFlow::Stay;
                };
                // A session that is live in another panel or process is never resumed a second time.
                if let Some(running) = self.running_now(modal, &summary.id) {
                    return self.refuse_running(ctx, running);
                }
                match resolve_resume_account(&summary.account, &modal.profile_names()) {
                    ResumeAccount::Use(profile) => {
                        let summary = summary.clone();
                        return self.finish_resume(ctx, modal.workspace, &summary, &profile, here);
                    }
                    ResumeAccount::Ask => {
                        modal.pick = Some(PendingPick {
                            id: summary.id.clone(),
                            here,
                        });
                    }
                }
            }
            SessionsAction::PickAccount(index) => return self.resume_with_picked_account(ctx, modal, index),
            SessionsAction::FocusPanel(panel) => return self.focus_session_panel(ctx, panel),
        }
        SessionsFlow::Stay
    }

    /// A resume that must not happen because the session is live: its Horizon panel is brought to
    /// the front, or the user is told it runs elsewhere.
    fn refuse_running(&mut self, ctx: &Context, running: Running) -> SessionsFlow {
        match running.panel {
            Some(panel) if self.board.panel(panel).is_some() => self.focus_session_panel(ctx, panel),
            _ => {
                self.hub.toasts.push(Toast {
                    kind: ToastKind::Info,
                    text: "Already open\nThis session is running in another Claude, outside this window".to_owned(),
                    panel: None,
                });
                SessionsFlow::Stay
            }
        }
    }

    fn focus_session_panel(&mut self, ctx: &Context, panel: PanelId) -> SessionsFlow {
        if self.board.panel(panel).is_none() {
            return SessionsFlow::Stay;
        }
        self.focus_panel_from_toast(ctx, panel);
        SessionsFlow::Close
    }

    /// The chip the user chose in the "Which account?" step: remember it, then resume.
    fn resume_with_picked_account(&mut self, ctx: &Context, modal: &mut SessionsModal, chip: usize) -> SessionsFlow {
        let (Some(pick), Some(profile)) = (
            modal.pick.take(),
            modal.profiles.get(chip).map(|chip| chip.name.clone()),
        ) else {
            return SessionsFlow::Stay;
        };
        let Some(summary) = modal.sessions.iter().find(|session| session.id == pick.id).cloned() else {
            return SessionsFlow::Stay;
        };
        self.remember_session_account(ctx, &summary.id, &profile);
        attribute_accounts(&mut modal.sessions, &self.hub.sessions.index);
        self.finish_resume(ctx, modal.workspace, &summary, &profile, pick.here)
    }

    /// Resumes and says why when it cannot. The modal closes only on success.
    fn finish_resume(
        &mut self,
        ctx: &Context,
        workspace: WorkspaceId,
        session: &SessionSummary,
        profile: &str,
        here: bool,
    ) -> SessionsFlow {
        match self.resume_session(ctx, workspace, session, profile, here) {
            Ok(()) => SessionsFlow::Close,
            Err(reason) => {
                tracing::warn!(profile, here, "could not resume a Claude session: {reason}");
                self.hub.toasts.push(Toast {
                    kind: ToastKind::Warn,
                    text: format!("Could not resume the session\n{reason}"),
                    panel: None,
                });
                SessionsFlow::Stay
            }
        }
    }

    fn resume_session(
        &mut self,
        ctx: &Context,
        workspace: WorkspaceId,
        session: &SessionSummary,
        profile: &str,
        here: bool,
    ) -> Result<(), String> {
        resume_command_args(&session.id).map_err(|_| "The session id is not valid".to_owned())?;
        let preset = find_profile_preset(&self.presets, profile)
            .cloned()
            .ok_or_else(|| format!("{profile} has no Claude preset"))?;
        let cwd = resume_cwd(session).to_path_buf();
        if !cwd.is_dir() {
            return Err("The folder this session ran in no longer exists".to_owned());
        }
        // A panel that cannot see the transcript would start a NEW session under the same id.
        if !claude_transcript_exists_for_env(&preset.env)(&session.id) {
            return Err(format!("{profile} cannot see this session's transcript"));
        }
        if here {
            let panel = self
                .board
                .focused
                .ok_or_else(|| "Focus an idle shell first, or use Resume".to_owned())?;
            self.resume_session_in_shell(ctx, panel, profile, &session.id, &cwd)
                .map_err(|failure| failure.message().to_owned())?;
        } else {
            let options = resume_panel_options(&preset, session);
            let target = if self.board.workspace(workspace).is_some() {
                Some(workspace)
            } else {
                self.board.active_workspace
            };
            let opened = target.is_some_and(|target| self.open_panel_in_worktree(target, options));
            if !opened {
                return Err("The panel could not be opened".to_owned());
            }
        }
        self.remember_session_account(ctx, &session.id, profile);
        self.hub.last_launched_profile = Some(profile.to_owned());
        Ok(())
    }

    /// Records that `session_id` runs under `profile` (its config dir comes from the profile's preset).
    pub(super) fn remember_session_account(&mut self, ctx: &Context, session_id: &str, profile: &str) {
        let Some(config_dir) = find_profile_preset(&self.presets, profile)
            .and_then(|preset| preset.env.get(CONFIG_DIR_ENV))
            .cloned()
        else {
            return;
        };
        self.hub.sessions.record(ctx, session_id, profile, &config_dir);
    }
}

/// Options of the panel a resume opens: the profile's preset (name, command and the
/// `CLAUDE_CONFIG_DIR` of its account) started in the folder the session ran in and bound to the
/// session, so Horizon launches `claude --resume <id>` there.
fn resume_panel_options(preset: &PresetConfig, session: &SessionSummary) -> PanelOptions {
    let mut options = preset.to_panel_options();
    options.cwd = Some(resume_cwd(session).to_path_buf());
    options.resume = PanelResume::Session {
        session_id: session.id.clone(),
    };
    options
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use horizon_core::PanelKind;
    use horizon_core::claude_sessions::AccountIndex;

    use super::*;
    use crate::app::test_support::test_app;

    fn profile_preset(alias: &str, dir: &str) -> PresetConfig {
        PresetConfig {
            name: format!("Claude \u{b7} {alias}"),
            alias: Some(alias.to_owned()),
            kind: PanelKind::Claude,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: BTreeMap::from([(CONFIG_DIR_ENV.to_owned(), dir.to_owned())]),
        }
    }

    fn summary_in(worktree: &str) -> SessionSummary {
        let origin = horizon_core::claude_sessions::SessionOrigin {
            worktree: std::path::PathBuf::from(worktree),
            branch: Some("feat".to_owned()),
            is_main: false,
            is_agent: false,
        };
        SessionSummary {
            cwd: origin.worktree.clone(),
            origin,
            id: "f054f32b-7840-4c95-9f84-e61f37db5f5c".to_owned(),
            title: "t".to_owned(),
            first_prompt: String::new(),
            updated: std::time::SystemTime::UNIX_EPOCH,
            size: 1,
            account: horizon_core::claude_sessions::SessionAccount::Unknown,
        }
    }

    #[test]
    fn resume_options_start_in_the_session_worktree_bound_to_the_session_with_the_profile_env() {
        let preset = profile_preset("conta2", "/home/u/.claude-conta2");
        let session = summary_in("/home/u/proj-feat");

        let options = resume_panel_options(&preset, &session);

        assert_eq!(options.cwd.as_deref(), Some(std::path::Path::new("/home/u/proj-feat")));
        assert!(matches!(
            &options.resume,
            PanelResume::Session { session_id } if session_id == &session.id
        ));
        assert_eq!(
            options.env.get(CONFIG_DIR_ENV).map(String::as_str),
            Some("/home/u/.claude-conta2")
        );
        assert!(matches!(options.kind, PanelKind::Claude));
        assert_eq!(options.name.as_deref(), Some("Claude \u{b7} conta2"));
    }

    #[test]
    fn a_session_running_elsewhere_is_not_resumed_and_a_missing_panel_cannot_be_focused() {
        use std::collections::{BTreeMap, BTreeSet};

        use super::super::model::LiveSessions;

        let mut app = test_app();
        let ctx = Context::default();
        let session = summary_in("/home/u/proj");
        let mut modal = SessionsModal::new(WorkspaceId(1), "api".to_owned(), true, 1, Vec::new());
        modal.sessions = vec![session.clone()];
        modal.live = LiveSessions::new(BTreeSet::from([session.id.clone()]), BTreeMap::new());
        let panels_before = app.board.panels.len();

        let flow = app.apply_sessions_action(
            &ctx,
            &mut modal,
            SessionsAction::Resume {
                session: 0,
                here: false,
            },
        );

        assert!(
            matches!(flow, SessionsFlow::Stay),
            "told it is open elsewhere, list kept"
        );
        assert_eq!(modal.pick, None, "no account question for a session that cannot resume");
        assert_eq!(app.board.panels.len(), panels_before, "no panel was opened");

        // A panel that no longer exists cannot be focused either.
        let flow = app.apply_sessions_action(&ctx, &mut modal, SessionsAction::FocusPanel(PanelId(u64::MAX)));
        assert!(matches!(flow, SessionsFlow::Stay));
    }

    #[test]
    fn remembering_an_account_uses_the_presets_config_dir_and_counts_a_change_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.hub.sessions.index = AccountIndex::empty(dir.path().join("idx.json"));
        app.presets.push(profile_preset("conta2", "/home/u/.claude-conta2"));
        let ctx = Context::default();
        let id = "f054f32b-7840-4c95-9f84-e61f37db5f5c";
        let before = app.hub.sessions.accounts_version;

        app.remember_session_account(&ctx, id, "conta2");
        let after_first = app.hub.sessions.accounts_version;
        app.remember_session_account(&ctx, id, "conta2");

        assert_eq!(
            app.hub.sessions.index.lookup(id).map(|entry| entry.config_dir.as_str()),
            Some("/home/u/.claude-conta2")
        );
        assert_ne!(before, after_first);
        assert_eq!(
            after_first, app.hub.sessions.accounts_version,
            "a repeat is not a change"
        );
    }

    #[test]
    fn an_unconfigured_profile_is_not_remembered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        app.hub.sessions.index = AccountIndex::empty(dir.path().join("idx.json"));

        app.remember_session_account(&Context::default(), "f054f32b-7840-4c95-9f84-e61f37db5f5c", "ghost");

        assert!(app.hub.sessions.index.is_empty());
    }
}
