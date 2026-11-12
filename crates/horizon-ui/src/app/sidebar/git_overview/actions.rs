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
        // Repository-level actions are not tied to a row.
        match action.kind {
            GitActionKind::NewWorktree => {
                self.open_new_worktree(ctx, action.workspace, None);
                return;
            }
            GitActionKind::CleanupWorktrees => {
                self.open_worktree_cleanup(ctx, action.workspace, None);
                return;
            }
            _ => {}
        }
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
            GitActionKind::RemoveWorktree => self.open_worktree_cleanup(ctx, action.workspace, Some(path)),
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
            GitActionKind::NewWorktree | GitActionKind::CleanupWorktrees => {}
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
    pub(in crate::app) fn open_panel_in_worktree(&mut self, workspace: WorkspaceId, options: PanelOptions) {
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use egui::Context;
    use horizon_core::WorkspaceId;
    use horizon_core::git_overview::{RepoOverview, WorktreeInfo};

    use super::super::build_cache;
    use super::{GitAction, GitActionKind};
    use crate::app::HorizonApp;
    use crate::app::test_support::test_app;

    fn worktree(path: &str, prunable: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            branch: Some(path.rsplit('/').next().unwrap_or(path).to_owned()),
            head_short: Some("abc1234".to_owned()),
            is_main: false,
            is_current: false,
            locked: false,
            prunable,
            dirty_count: None,
            is_agent: false,
        }
    }

    fn app_with_worktrees(worktrees: Vec<WorktreeInfo>) -> (HorizonApp, WorkspaceId) {
        let mut app = test_app();
        let workspace = app.board.ensure_workspace();
        let overview = RepoOverview {
            repo_root: PathBuf::from("/work/api"),
            branch: Some("main".to_owned()),
            detached: false,
            head_short: Some("abc1234".to_owned()),
            dirty_count: 0,
            upstream: None,
            ahead: 0,
            behind: 0,
            worktrees,
        };
        app.hub.git.caches.insert(workspace, build_cache(&overview));
        (app, workspace)
    }

    fn action(workspace: WorkspaceId, path: &str, kind: GitActionKind) -> GitAction {
        GitAction {
            workspace,
            path: Path::new(path).to_path_buf(),
            kind,
        }
    }

    #[test]
    fn an_action_on_a_worktree_that_is_no_longer_listed_does_nothing() {
        let (mut app, workspace) = app_with_worktrees(vec![worktree("/work/api-live", false)]);
        let ctx = Context::default();

        for kind in [
            GitActionKind::CopyPath,
            GitActionKind::OpenTerminal,
            GitActionKind::NewWorkspace,
        ] {
            app.apply_git_action(&ctx, &action(workspace, "/work/api-removed", kind));
        }

        assert!(app.hub.git.copied.is_none());
        assert!(app.board.panels.is_empty());
        assert_eq!(app.board.workspaces.len(), 1);
    }

    #[test]
    fn a_prunable_worktree_only_allows_copying_its_path() {
        let (mut app, workspace) = app_with_worktrees(vec![worktree("/work/api-stale", true)]);
        let ctx = Context::default();

        for kind in [
            GitActionKind::OpenTerminal,
            GitActionKind::OpenClaude { preset: 0 },
            GitActionKind::NewWorkspace,
        ] {
            app.apply_git_action(&ctx, &action(workspace, "/work/api-stale", kind));
        }
        assert!(app.board.panels.is_empty(), "nothing may open in a prunable worktree");
        assert_eq!(app.board.workspaces.len(), 1);
        assert!(app.hub.git.copied.is_none());

        app.apply_git_action(&ctx, &action(workspace, "/work/api-stale", GitActionKind::CopyPath));
        let copied = app.hub.git.copied.expect("copy feedback");
        assert_eq!(copied.workspace, workspace);
    }

    #[test]
    fn copying_a_listed_worktree_records_its_row() {
        let (mut app, workspace) = app_with_worktrees(vec![worktree("/work/api-live", false)]);
        let ctx = Context::default();

        app.apply_git_action(&ctx, &action(workspace, "/work/api-live", GitActionKind::CopyPath));

        let key = app
            .hub
            .git
            .row_for_path(workspace, Path::new("/work/api-live"))
            .expect("row")
            .key;
        let copied = app.hub.git.copied.expect("copy feedback");
        assert_eq!((copied.workspace, copied.key), (workspace, key));
    }

    #[test]
    fn opening_claude_with_an_unknown_preset_does_nothing() {
        let (mut app, workspace) = app_with_worktrees(vec![worktree("/work/api-live", false)]);
        let ctx = Context::default();

        app.apply_git_action(
            &ctx,
            &action(
                workspace,
                "/work/api-live",
                GitActionKind::OpenClaude { preset: usize::MAX },
            ),
        );

        assert!(app.board.panels.is_empty());
    }
}
