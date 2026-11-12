//! Applies the actions picked on a worktree row: open a terminal or a Claude
//! session in the worktree, create a workspace for it, copy its path.

use std::path::PathBuf;
use std::time::Instant;

use egui::Context;
use horizon_core::{PanelKind, PanelOptions, WorkspaceId};

use super::{Copied, GitAction, GitActionKind};
use crate::app::HorizonApp;

impl HorizonApp {
    /// Applies `action` if its worktree is still listed for the workspace; the
    /// overview may have changed (worktree removed, rows reordered) since it
    /// was picked.
    pub(in crate::app) fn apply_git_action(&mut self, ctx: &Context, action: &GitAction) {
        let Some(row) = self.hub.git.row_for_path(action.workspace, &action.path) else {
            return;
        };
        let key = row.key;
        let path = row.path.clone();
        let workspace_name = row.workspace_name.clone();
        let prunable = row.prunable;
        match action.kind {
            GitActionKind::CopyPath => {
                ctx.copy_text(path.to_string_lossy().into_owned());
                self.hub.git.copied = Some(Copied {
                    workspace: action.workspace,
                    key,
                    at: Instant::now(),
                });
                ctx.request_repaint();
            }
            _ if prunable => {}
            GitActionKind::OpenTerminal => {
                let options = self.shell_options_at(path);
                self.open_panel_in_worktree(action.workspace, options);
            }
            GitActionKind::OpenClaude { preset } => {
                let Some(preset) = self.presets.get(preset) else {
                    return;
                };
                let mut options = preset.to_panel_options();
                options.cwd = Some(path);
                self.open_panel_in_worktree(action.workspace, options);
            }
            GitActionKind::NewWorkspace => self.new_workspace_from_worktree(ctx, &workspace_name, path),
        }
    }

    /// Shell panel options (the configured shell preset when there is one)
    /// with the working directory set to `cwd`.
    fn shell_options_at(&self, cwd: PathBuf) -> PanelOptions {
        let mut options = self
            .presets
            .iter()
            .find(|preset| preset.kind == PanelKind::Shell)
            .map_or_else(
                || PanelOptions {
                    kind: PanelKind::Shell,
                    ..PanelOptions::default()
                },
                horizon_core::PresetConfig::to_panel_options,
            );
        options.cwd = Some(cwd);
        options
    }

    /// Creates the panel (its explicit cwd wins over the workspace cwd) and focuses it.
    fn open_panel_in_worktree(&mut self, workspace: WorkspaceId, options: PanelOptions) {
        match self.create_panel_with_options(options, workspace) {
            Ok(panel_id) => {
                self.board.focus(panel_id);
                self.mark_runtime_dirty();
            }
            Err(error) => tracing::error!("failed to open panel in worktree: {error}"),
        }
    }

    fn new_workspace_from_worktree(&mut self, ctx: &Context, name: &str, path: PathBuf) {
        let workspace = self.create_workspace_visible(ctx, name);
        if let Some(created) = self.board.workspace_mut(workspace) {
            created.cwd = Some(path.clone());
        }
        let options = self.shell_options_at(path);
        match self.create_panel_with_options(options, workspace) {
            Ok(panel_id) => self.board.focus(panel_id),
            Err(error) => tracing::error!("failed to open terminal in new workspace: {error}"),
        }
        self.board.focus_workspace(workspace);
        self.fit_workspace_visible(ctx, workspace);
        self.mark_runtime_dirty();
    }
}
