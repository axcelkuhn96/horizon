//! "Open now" for workspace templates: materializes a template in the live board.

use egui::Context;
use horizon_core::{Config, WorkspaceConfig, WorkspaceState, workspace_color_index};

use crate::app::HorizonApp;

impl HorizonApp {
    /// Runs the "Open now" request raised by Settings → Workspaces, if any.
    /// Call once per frame while settings are open.
    pub(in crate::app) fn process_pending_template_open(&mut self, ctx: &Context) {
        let Some(template) = self.take_pending_template_open() else {
            return;
        };
        let failed_panels = self.open_template_now(ctx, &template);
        self.record_template_opened(failed_panels);
    }

    /// Creates a workspace from `template` and spawns its terminals in the live
    /// board. The workspace is standalone (no link to the saved template), so
    /// reordering or deleting templates never touches it. Returns how many
    /// terminals failed to start.
    pub(in crate::app) fn open_template_now(&mut self, ctx: &Context, template: &WorkspaceConfig) -> usize {
        let workspace_id = self.create_workspace_visible(ctx, template.name.trim());
        let origin = self
            .board
            .workspace(workspace_id)
            .map_or([0.0, 0.0], |workspace| workspace.position);
        if let Some(workspace) = self.board.workspace_mut(workspace_id) {
            workspace.cwd = template
                .cwd
                .as_deref()
                .filter(|cwd| !cwd.trim().is_empty())
                .map(Config::expand_tilde);
            workspace.sidebar_collapsed = template.sidebar_collapsed;
            if let Some(slot) = template.color.as_deref().and_then(workspace_color_index) {
                workspace.color_idx = slot;
            }
        }

        let state = WorkspaceState::from_config(0, template, origin);
        let mut failed_panels = 0;
        for panel in &state.panels {
            let mut options = panel.to_panel_options();
            options.local_id = None;
            options.template = None;
            options.is_restore = false;
            if let Err(error) = self.create_panel_with_options(options, workspace_id) {
                failed_panels += 1;
                tracing::error!(template = %template.name, "failed to open template terminal: {error}");
            }
        }
        if let Some(workspace) = self.board.workspace_mut(workspace_id) {
            workspace.layout = state.layout;
        }

        self.board.focus_workspace(workspace_id);
        self.fit_workspace_visible(ctx, workspace_id);
        self.mark_runtime_dirty();
        failed_panels
    }
}

#[cfg(test)]
mod tests {
    use egui::Context;
    use horizon_core::{PanelKind, TerminalConfig, WorkspaceConfig, workspace_color_index};

    use crate::app::test_support::test_app;

    fn command_terminal(name: &str, program: &str, args: &[&str]) -> TerminalConfig {
        TerminalConfig {
            name: name.to_owned(),
            kind: PanelKind::Command,
            command: Some(program.to_owned()),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ..TerminalConfig::default()
        }
    }

    fn template(color: Option<&str>, terminals: Vec<TerminalConfig>) -> WorkspaceConfig {
        WorkspaceConfig {
            name: " Backend ".to_owned(),
            color: color.map(str::to_owned),
            cwd: None,
            position: None,
            terminals,
            sidebar_collapsed: false,
            prompts: Vec::new(),
        }
    }

    #[test]
    fn open_now_applies_the_template_color_and_counts_no_failures() {
        let mut app = test_app();
        let ctx = Context::default();
        let red = "#f38ba8";
        let slot = workspace_color_index(red).expect("palette color");

        let failed = app.open_template_now(
            &ctx,
            &template(Some(red), vec![command_terminal("cat", "sh", &["-c", "exec cat"])]),
        );

        assert_eq!(failed, 0);
        let workspace = app.board.workspaces.last().expect("workspace");
        assert_eq!(workspace.name, "Backend");
        assert_eq!(workspace.color_idx, slot);
        assert_eq!(workspace.panels.len(), 1);
    }

    #[test]
    fn open_now_counts_terminals_that_fail_to_start_and_keeps_the_rest() {
        let mut app = test_app();
        let ctx = Context::default();

        let failed = app.open_template_now(
            &ctx,
            &template(
                None,
                vec![
                    command_terminal("ok", "sh", &["-c", "exec cat"]),
                    command_terminal("broken", "/nonexistent/horizon-test-binary", &[]),
                    command_terminal("also-broken", "/nonexistent/horizon-test-binary", &[]),
                ],
            ),
        );

        assert_eq!(failed, 2);
        let workspace = app.board.workspaces.last().expect("workspace");
        assert_eq!(workspace.panels.len(), 1, "the working terminal still opens");
    }

    #[test]
    fn open_now_ignores_a_color_outside_the_palette() {
        let mut app = test_app();
        let ctx = Context::default();

        app.open_template_now(&ctx, &template(Some("#123456"), Vec::new()));

        let workspace = app.board.workspaces.last().expect("workspace");
        assert!(workspace_color_index("#123456").is_none());
        assert_eq!(workspace.color_idx, 0, "first workspace keeps its default slot");
    }
}
