//! Workspace template actions: save a live workspace as a template in
//! `config.yaml`, and delete a workspace together with its terminals.

use std::path::Path;

use horizon_core::{
    Config, Error, PanelKind, PanelResume, PanelSnapshotForTemplate, Result, WorkspaceConfig, WorkspaceId,
    template_from_workspace, upsert_template,
};

use crate::app::HorizonApp;
use crate::app::util::atomic_write;

/// What saving a template did to the template list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TemplateSaveOutcome {
    Created,
    Updated,
}

/// Result of "Save as template".
pub(in crate::app) struct TemplateSaved {
    pub(in crate::app) name: String,
    /// An existing template was replaced (`false` = a new one was created).
    pub(in crate::app) updated: bool,
    /// The template went into the open Settings editor and is only written to
    /// disk when Settings are saved.
    pub(in crate::app) pending_in_settings: bool,
}

/// Insert or replace `template` in `workspaces` without ever producing two
/// templates whose names differ only by case.
///
/// The entry matching `linked_name` is replaced; with no usable link, the entry
/// with the same name (case-insensitive) is replaced; otherwise `template` is
/// appended. Linking to one template while the new name already belongs to a
/// different one is refused, so no template is silently overwritten or doubled.
pub(in crate::app) fn merge_template(
    workspaces: &mut Vec<WorkspaceConfig>,
    template: WorkspaceConfig,
    linked_name: Option<&str>,
) -> Result<TemplateSaveOutcome> {
    let find = |name: &str| workspaces.iter().position(|w| w.name.eq_ignore_ascii_case(name));
    let linked = linked_name.and_then(find);
    let same_name = find(&template.name);
    let target = match (linked, same_name) {
        (Some(linked), Some(same)) if linked != same => {
            return Err(Error::Config(format!(
                "a template named \"{}\" already exists",
                template.name
            )));
        }
        (Some(index), _) | (None, Some(index)) => Some(index),
        (None, None) => None,
    };
    let existing_name = target.map(|index| workspaces[index].name.clone());
    upsert_template(workspaces, template, existing_name.as_deref());
    Ok(if existing_name.is_some() {
        TemplateSaveOutcome::Updated
    } else {
        TemplateSaveOutcome::Created
    })
}

/// Name of the saved template that saving a workspace called `name` (linked to
/// `linked_name`) would update, if any. Drives the "Update template" menu label.
pub(in crate::app) fn existing_template_name<'a>(
    saved: &'a [WorkspaceConfig],
    linked_name: Option<&str>,
    name: &str,
) -> Option<&'a str> {
    let find = |wanted: &str| saved.iter().find(|w| w.name.eq_ignore_ascii_case(wanted));
    linked_name
        .and_then(find)
        .or_else(|| find(name))
        .map(|workspace| workspace.name.as_str())
}

/// Load `path` (or `fallback` when the file does not exist yet), merge
/// `template` into its workspaces and write it back atomically. Returns the
/// written config so the caller can apply it to the running app.
pub(in crate::app) fn save_template_to_path(
    path: &Path,
    fallback: &Config,
    template: WorkspaceConfig,
    linked_name: Option<&str>,
) -> Result<(TemplateSaveOutcome, Config)> {
    let mut config = if path.exists() {
        Config::load(Some(path))?
    } else {
        fallback.clone()
    };
    let outcome = merge_template(&mut config.workspaces, template, linked_name)?;
    atomic_write(path, &config.to_yaml()?)?;
    Ok((outcome, config))
}

/// Saving a workspace without terminals would overwrite a template with an
/// empty one, so it is refused.
fn require_terminals(panels: Vec<PanelSnapshotForTemplate>) -> Result<Vec<PanelSnapshotForTemplate>> {
    if panels.is_empty() {
        return Err(Error::State("No terminals to save".to_string()));
    }
    Ok(panels)
}

/// Merge `template` into the configuration the open Settings editor holds
/// (`buffer` is the source of truth: it may carry unsaved YAML edits), so the
/// editor's next apply/save keeps the template instead of dropping it.
pub(in crate::app) fn merge_into_settings_buffer(
    editing_config: &mut Option<Config>,
    buffer: &mut String,
    template: WorkspaceConfig,
    linked_name: Option<&str>,
) -> Result<TemplateSaveOutcome> {
    let mut config = Config::from_yaml(buffer)
        .map_err(|_| Error::State("Fix the configuration in Settings before saving a template".to_string()))?;
    let outcome = merge_template(&mut config.workspaces, template, linked_name)?;
    *buffer = config.to_yaml()?;
    *editing_config = Some(config);
    Ok(outcome)
}

/// Workspace-relative position of a panel (templates store positions relative
/// to the workspace origin).
fn relative_position(panel: [f32; 2], workspace: [f32; 2]) -> [f32; 2] {
    [panel[0] - workspace[0], panel[1] - workspace[1]]
}

/// A pinned session id must not leak into a reusable template, so a panel that
/// was resumed from a specific session is saved as a fresh start.
fn template_resume(resume: &PanelResume) -> PanelResume {
    match resume {
        PanelResume::Session { .. } => PanelResume::Fresh,
        other => other.clone(),
    }
}

fn color_hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl HorizonApp {
    /// Saved template that saving workspace `id` would update, if any.
    pub(in crate::app) fn workspace_template_target(&self, id: WorkspaceId) -> Option<&str> {
        let workspace = self.board.workspace(id)?;
        let linked = workspace.template.as_ref().map(|link| link.workspace_name.as_str());
        existing_template_name(&self.template_config.workspaces, linked, &workspace.name)
    }

    fn template_snapshot(&self, id: WorkspaceId) -> Result<(WorkspaceConfig, Option<String>)> {
        let workspace = self
            .board
            .workspace(id)
            .ok_or_else(|| Error::State("workspace no longer exists".to_string()))?;
        let panels: Vec<PanelSnapshotForTemplate> = workspace
            .panels
            .iter()
            .filter_map(|panel_id| self.board.panel(*panel_id))
            .filter(|panel| panel.terminal().is_some())
            .map(|panel| PanelSnapshotForTemplate {
                name: panel.title.clone(),
                kind: panel.kind,
                command: panel.launch_command.clone(),
                args: panel.launch_args.clone(),
                cwd: if panel.kind.is_agent() || panel.kind == PanelKind::Ssh {
                    panel.launch_cwd.clone()
                } else {
                    panel
                        .terminal()
                        .and_then(horizon_core::Terminal::current_cwd)
                        .or_else(|| panel.launch_cwd.clone())
                }
                .map(|path| path.display().to_string()),
                position: relative_position(panel.layout.position, workspace.position),
                size: panel.layout.size,
                env: panel.env.clone(),
                resume: template_resume(&panel.resume),
                ssh_connection: panel.ssh_connection.clone(),
            })
            .collect();
        let panels = require_terminals(panels)?;
        let template = template_from_workspace(
            &workspace.name,
            Some(color_hex(workspace.accent())),
            workspace.cwd.as_ref().map(|path| path.display().to_string()),
            &panels,
        );
        let linked = workspace.template.as_ref().map(|link| link.workspace_name.clone());
        Ok((template, linked))
    }

    /// Save workspace `id` as a workspace template (replacing its linked or
    /// same-named template). With the Settings window open the template goes
    /// into the editor's buffer instead of the file, so the editor's own
    /// apply/save keeps it; otherwise `config.yaml` is written and applied.
    pub(in crate::app) fn save_workspace_as_template(&mut self, id: WorkspaceId) -> Result<TemplateSaved> {
        let (template, linked) = self.template_snapshot(id)?;
        let name = template.name.clone();
        if let Some(editor) = self.settings.as_mut() {
            let outcome = merge_into_settings_buffer(
                &mut editor.editing_config,
                &mut editor.buffer,
                template,
                linked.as_deref(),
            )?;
            return Ok(TemplateSaved {
                name,
                updated: outcome == TemplateSaveOutcome::Updated,
                pending_in_settings: true,
            });
        }
        let (outcome, config) =
            save_template_to_path(&self.config_path, &self.template_config, template, linked.as_deref())?;
        self.apply_runtime_config(&config);
        tracing::info!("workspace template \"{name}\" saved to {}", self.config_path.display());
        Ok(TemplateSaved {
            name,
            updated: outcome == TemplateSaveOutcome::Updated,
            pending_in_settings: false,
        })
    }

    /// Close every terminal of workspace `id` and remove it. The last
    /// remaining workspace is never removed.
    pub(in crate::app) fn delete_workspace(&mut self, id: WorkspaceId) {
        if self.board.workspaces.len() <= 1 || self.board.workspace(id).is_none() {
            return;
        }
        self.reattach_workspace(id);
        self.close_workspace_panels(id);
        if self.renaming_workspace == Some(id) {
            self.clear_workspace_rename();
        }
        self.board.remove_workspace(id);
        self.mark_runtime_dirty();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use horizon_core::{Config, PanelKind, PanelResume, TerminalConfig, WorkspaceConfig};

    use crate::app::test_support::{command_options, test_app};

    use super::{
        TemplateSaveOutcome, color_hex, existing_template_name, merge_into_settings_buffer, merge_template,
        relative_position, require_terminals, save_template_to_path, template_resume,
    };

    fn template(name: &str, terminal: &str) -> WorkspaceConfig {
        WorkspaceConfig {
            name: name.to_string(),
            color: None,
            cwd: None,
            position: None,
            terminals: vec![TerminalConfig {
                name: terminal.to_string(),
                ..TerminalConfig::default()
            }],
            sidebar_collapsed: false,
            prompts: Vec::new(),
        }
    }

    fn names(config: &Config) -> Vec<&str> {
        config.workspaces.iter().map(|w| w.name.as_str()).collect()
    }

    fn seeded_config() -> Config {
        Config {
            workspaces: vec![template("Alpha", "old"), template("Beta", "keep")],
            ..Config::default()
        }
    }

    #[test]
    fn workspace_templates_save_as_template_replaces_same_name_keeps_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, seeded_config().to_yaml().expect("yaml")).expect("write");

        let (outcome, saved) = save_template_to_path(&path, &Config::default(), template("alpha", "new"), None)
            .expect("save should succeed");

        assert_eq!(outcome, TemplateSaveOutcome::Updated);
        let reloaded = Config::load(Some(&path)).expect("reload");
        assert_eq!(names(&reloaded), vec!["alpha", "Beta"]);
        assert_eq!(reloaded.workspaces[0].terminals[0].name, "new");
        assert_eq!(reloaded.workspaces[1].terminals[0].name, "keep");
        assert_eq!(names(&saved), names(&reloaded));
    }

    #[test]
    fn workspace_templates_save_as_template_creates_when_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.yaml");

        let (outcome, _) =
            save_template_to_path(&path, &seeded_config(), template("Gamma", "g"), None).expect("save should succeed");

        assert_eq!(outcome, TemplateSaveOutcome::Created);
        let reloaded = Config::load(Some(&path)).expect("reload");
        assert_eq!(names(&reloaded), vec!["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn workspace_templates_linked_template_is_replaced_even_after_rename() {
        let mut workspaces = seeded_config().workspaces;

        let outcome = merge_template(&mut workspaces, template("Renamed", "x"), Some("beta")).expect("merge");

        assert_eq!(outcome, TemplateSaveOutcome::Updated);
        let names: Vec<_> = workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Renamed"]);
    }

    #[test]
    fn workspace_templates_refuse_name_owned_by_another_template() {
        let mut workspaces = seeded_config().workspaces;

        let result = merge_template(&mut workspaces, template("ALPHA", "x"), Some("Beta"));

        assert!(result.is_err());
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].terminals[0].name, "old");
    }

    #[test]
    fn workspace_templates_stale_link_falls_back_to_name_match() {
        let mut workspaces = seeded_config().workspaces;

        let outcome = merge_template(&mut workspaces, template("Alpha", "n"), Some("gone")).expect("merge");

        assert_eq!(outcome, TemplateSaveOutcome::Updated);
        assert_eq!(workspaces.len(), 2);
    }

    #[test]
    fn workspace_templates_existing_name_prefers_link_then_name() {
        let saved = seeded_config().workspaces;

        assert_eq!(existing_template_name(&saved, Some("beta"), "Alpha"), Some("Beta"));
        assert_eq!(existing_template_name(&saved, None, "alpha"), Some("Alpha"));
        assert_eq!(existing_template_name(&saved, Some("gone"), "alpha"), Some("Alpha"));
        assert_eq!(existing_template_name(&saved, None, "Zed"), None);
    }

    #[test]
    fn workspace_templates_keep_panel_env_through_the_saved_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        let mut with_env = template("Work", "claude");
        with_env.terminals[0].kind = PanelKind::Claude;
        with_env.terminals[0].env =
            BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude-work".to_string())]);

        save_template_to_path(&path, &Config::default(), with_env, None).expect("save");

        let reloaded = Config::load(Some(&path)).expect("reload");
        let saved = reloaded
            .workspaces
            .iter()
            .find(|w| w.name == "Work")
            .expect("saved template");
        assert_eq!(
            saved.terminals[0].env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-work")
        );
    }

    #[test]
    fn workspace_templates_positions_are_relative_to_the_workspace() {
        let position = relative_position([300.0, 220.0], [100.0, 20.0]);
        assert!((position[0] - 200.0).abs() < 1e-3 && (position[1] - 200.0).abs() < 1e-3);
    }

    #[test]
    fn workspace_templates_pinned_session_is_saved_fresh() {
        let pinned = PanelResume::Session {
            session_id: "abc".to_string(),
        };
        assert_eq!(template_resume(&pinned), PanelResume::Fresh);
        assert_eq!(template_resume(&PanelResume::Last), PanelResume::Last);
    }

    #[test]
    fn workspace_templates_color_is_lowercase_hex() {
        assert_eq!(color_hex((137, 180, 250)), "#89b4fa");
    }

    #[test]
    fn workspace_templates_open_settings_receives_the_template_in_its_buffer() {
        let mut buffer = seeded_config().to_yaml().expect("yaml");
        let mut editing = None;

        let outcome =
            merge_into_settings_buffer(&mut editing, &mut buffer, template("Gamma", "g"), None).expect("merge");

        assert_eq!(outcome, TemplateSaveOutcome::Created);
        let from_buffer = Config::from_yaml(&buffer).expect("buffer stays valid yaml");
        assert_eq!(names(&from_buffer), vec!["Alpha", "Beta", "Gamma"]);
        let editing = editing.expect("editor config is refreshed from the buffer");
        assert_eq!(names(&editing), vec!["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn workspace_templates_open_settings_keeps_unsaved_yaml_edits() {
        let mut edited = seeded_config();
        edited.workspaces[1].terminals[0].name = "typed-in-settings".to_string();
        let mut buffer = edited.to_yaml().expect("yaml");
        let mut stale = Some(seeded_config());

        merge_into_settings_buffer(&mut stale, &mut buffer, template("Alpha", "new"), None).expect("merge");

        let merged = Config::from_yaml(&buffer).expect("yaml");
        assert_eq!(merged.workspaces[0].terminals[0].name, "new");
        assert_eq!(merged.workspaces[1].terminals[0].name, "typed-in-settings");
    }

    #[test]
    fn workspace_templates_open_settings_with_invalid_yaml_is_refused_untouched() {
        let mut buffer = "workspaces: [unclosed".to_string();
        let mut editing = None;

        let result = merge_into_settings_buffer(&mut editing, &mut buffer, template("Gamma", "g"), None);

        assert!(result.is_err());
        assert_eq!(buffer, "workspaces: [unclosed");
        assert!(editing.is_none());
    }

    #[test]
    fn workspace_templates_workspace_without_terminals_is_not_saved() {
        let error = require_terminals(Vec::new()).expect_err("empty snapshot must be refused");

        assert!(error.to_string().contains("No terminals to save"), "{error}");
    }

    #[test]
    fn delete_workspace_never_removes_the_last_workspace() {
        let mut app = test_app();
        let only = app.board.create_workspace("only");
        let panel = app
            .create_panel_with_options(command_options("p", "exec cat"), only)
            .expect("panel should spawn");

        app.delete_workspace(only);

        assert_eq!(app.board.workspaces.len(), 1);
        assert!(app.board.workspace(only).is_some());
        assert!(app.board.panel(panel).is_some(), "the last workspace keeps its panels");
    }

    #[test]
    fn delete_workspace_closes_only_its_own_panels() {
        let mut app = test_app();
        let doomed = app.board.create_workspace("doomed");
        let kept = app.board.create_workspace("kept");
        let doomed_panels: Vec<_> = (0..2)
            .map(|index| {
                app.create_panel_with_options(command_options(&format!("d{index}"), "exec cat"), doomed)
                    .expect("panel should spawn")
            })
            .collect();
        let kept_panel = app
            .create_panel_with_options(command_options("k", "exec cat"), kept)
            .expect("panel should spawn");

        app.delete_workspace(doomed);

        assert!(app.board.workspace(doomed).is_none());
        assert!(app.board.workspace(kept).is_some());
        assert!(doomed_panels.iter().all(|panel| app.board.panel(*panel).is_none()));
        assert!(app.board.panel(kept_panel).is_some());
        assert_eq!(app.board.workspaces.len(), 1);
    }

    #[test]
    fn delete_workspace_ignores_an_unknown_workspace() {
        let mut app = test_app();
        let first = app.board.create_workspace("a");
        let _ = app.board.create_workspace("b");

        app.delete_workspace(horizon_core::WorkspaceId(9_999));

        assert_eq!(app.board.workspaces.len(), 2);
        assert!(app.board.workspace(first).is_some());
    }
}
