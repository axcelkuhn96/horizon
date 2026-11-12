//! What the modal's actions do: select, expand, ask for an account and resume.
//!
//! A resume opens a new Claude panel for the session's account, started in the folder the
//! session ran in with the panel bound to the session (Horizon then launches `claude --resume
//! <id>`), or types the same command into the focused idle shell.

use egui::Context;
use horizon_core::claude_sessions::{SessionSummary, attribute_accounts, resume_command_args};
use horizon_core::{PanelResume, WorkspaceId, claude_transcript_exists_for_env};

use super::model::{ListWindow, ResumeAccount, resolve_resume_account, resume_cwd};
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
        }
        SessionsFlow::Stay
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
            let mut options = preset.to_panel_options();
            options.cwd = Some(cwd);
            options.resume = PanelResume::Session {
                session_id: session.id.clone(),
            };
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use horizon_core::claude_sessions::AccountIndex;
    use horizon_core::{PanelKind, PresetConfig};

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
