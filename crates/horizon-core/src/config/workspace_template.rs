//! Workspace templates: layout of template terminals and helpers that turn a
//! live workspace into a saved template (and keep the template list tidy).

use std::collections::BTreeMap;

use super::{PresetConfig, TerminalConfig, WorkspaceConfig};
use crate::layout::{TILE_GAP, WS_INNER_PAD, usize_to_f32};
use crate::panel::{DEFAULT_PANEL_SIZE, PanelKind, PanelResume};
use crate::ssh::SshConnection;

/// Columns used when laying out template terminals that have no saved position.
const TEMPLATE_GRID_COLUMNS: usize = 3;

/// Live panel state captured when saving a workspace as a template.
///
/// `position` is already relative to the workspace origin (the same convention
/// as `TerminalConfig.position`).
#[derive(Clone, Debug)]
pub struct PanelSnapshotForTemplate {
    pub name: String,
    pub kind: PanelKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub env: BTreeMap<String, String>,
    pub resume: PanelResume,
    pub ssh_connection: Option<SshConnection>,
}

/// Workspace-relative position of the `index`-th tile in a left-to-right grid.
/// A `columns` of zero behaves as a single column.
#[must_use]
pub fn grid_position(index: usize, panel_size: [f32; 2], columns: usize) -> [f32; 2] {
    let columns = columns.max(1);
    let column = usize_to_f32(index % columns);
    let row = usize_to_f32(index / columns);
    [
        WS_INNER_PAD + column * (panel_size[0] + TILE_GAP),
        WS_INNER_PAD + row * (panel_size[1] + TILE_GAP),
    ]
}

/// Build a template terminal from a launcher preset, placed on the default grid.
#[must_use]
pub fn terminal_from_preset(preset: &PresetConfig, name: Option<String>, index: usize) -> TerminalConfig {
    TerminalConfig {
        name: name.unwrap_or_else(|| preset.name.clone()),
        command: preset.command.clone(),
        args: preset.args.clone(),
        kind: preset.kind,
        resume: preset.resume.clone(),
        ssh_connection: preset.ssh_connection.clone(),
        env: preset.env.clone(),
        position: Some(grid_position(index, DEFAULT_PANEL_SIZE, TEMPLATE_GRID_COLUMNS)),
        size: Some(DEFAULT_PANEL_SIZE),
        ..TerminalConfig::default()
    }
}

/// Turn a live workspace's panels into a workspace template.
#[must_use]
pub fn template_from_workspace(
    name: &str,
    color: Option<String>,
    cwd: Option<String>,
    panels: &[PanelSnapshotForTemplate],
) -> WorkspaceConfig {
    WorkspaceConfig {
        name: name.to_string(),
        color,
        cwd,
        position: None,
        terminals: panels
            .iter()
            .map(|panel| TerminalConfig {
                name: panel.name.clone(),
                command: panel.command.clone(),
                args: panel.args.clone(),
                cwd: panel.cwd.clone(),
                kind: panel.kind,
                resume: panel.resume.clone(),
                position: Some(panel.position),
                size: Some(panel.size),
                ssh_connection: panel.ssh_connection.clone(),
                env: panel.env.clone(),
                ..TerminalConfig::default()
            })
            .collect(),
        sidebar_collapsed: false,
    }
}

/// Replace the entry matching `linked_name` (case-insensitive) with `template`,
/// or append `template` when there is no link or no match. Never removes others.
pub fn upsert_template(workspaces: &mut Vec<WorkspaceConfig>, template: WorkspaceConfig, linked_name: Option<&str>) {
    let linked = linked_name.and_then(|linked| {
        workspaces
            .iter()
            .position(|workspace| workspace.name.eq_ignore_ascii_case(linked))
    });
    match linked {
        Some(index) => workspaces[index] = template,
        None => workspaces.push(template),
    }
}

/// Move the item at `from` so it ends up at index `to`. Out-of-range indices are a no-op.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() || to >= v.len() || from == to {
        return;
    }
    let item = v.remove(from);
    v.insert(to, item);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{
        PanelSnapshotForTemplate, grid_position, move_item, template_from_workspace, terminal_from_preset,
        upsert_template,
    };
    use crate::config::{ClaudeProfile, Config, PresetConfig, WorkspaceConfig, claude_profile_preset};
    use crate::layout::{TILE_GAP, WS_INNER_PAD};
    use crate::panel::{DEFAULT_PANEL_SIZE, PanelKind, PanelResume};

    fn workspace(name: &str) -> WorkspaceConfig {
        WorkspaceConfig {
            name: name.to_string(),
            color: None,
            cwd: None,
            position: None,
            terminals: Vec::new(),
            sidebar_collapsed: false,
        }
    }

    #[test]
    fn grid_positions_fill_rows_left_to_right() {
        let size = [100.0, 50.0];
        let step_x = 100.0 + TILE_GAP;
        let step_y = 50.0 + TILE_GAP;

        assert_eq!(grid_position(0, size, 3), [WS_INNER_PAD, WS_INNER_PAD]);
        assert_eq!(grid_position(1, size, 3), [WS_INNER_PAD + step_x, WS_INNER_PAD]);
        assert_eq!(grid_position(2, size, 3), [WS_INNER_PAD + 2.0 * step_x, WS_INNER_PAD]);
        assert_eq!(grid_position(3, size, 3), [WS_INNER_PAD, WS_INNER_PAD + step_y]);
        assert_eq!(
            grid_position(4, size, 3),
            [WS_INNER_PAD + step_x, WS_INNER_PAD + step_y]
        );
        // Zero columns must not divide by zero: behaves as a single column.
        assert_eq!(grid_position(2, size, 0), [WS_INNER_PAD, WS_INNER_PAD + 2.0 * step_y]);
    }

    #[test]
    fn terminal_from_claude_profile_preset_keeps_env() {
        let base = PresetConfig {
            name: "Claude Code".to_string(),
            alias: None,
            kind: PanelKind::Claude,
            command: Some("claude".to_string()),
            args: vec!["--verbose".to_string()],
            resume: PanelResume::Last,
            ssh_connection: None,
            env: BTreeMap::new(),
        };
        let profile = ClaudeProfile {
            name: "work".to_string(),
            config_dir: "~/.claude-work".to_string(),
        };
        let preset = claude_profile_preset(&base, &profile, Path::new("/home/u"));

        let terminal = terminal_from_preset(&preset, None, 4);

        assert_eq!(terminal.name, preset.name);
        assert_eq!(terminal.kind, PanelKind::Claude);
        assert_eq!(terminal.command.as_deref(), Some("claude"));
        assert_eq!(terminal.args, vec!["--verbose".to_string()]);
        assert_eq!(terminal.resume, PanelResume::Last);
        assert_eq!(terminal.env, preset.env);
        assert_eq!(
            terminal.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-work")
        );
        assert_eq!(terminal.position, Some(grid_position(4, DEFAULT_PANEL_SIZE, 3)));
        assert_eq!(terminal.size, Some(DEFAULT_PANEL_SIZE));

        let named = terminal_from_preset(&preset, Some("Review".to_string()), 0);
        assert_eq!(named.name, "Review");
    }

    #[test]
    fn template_from_workspace_keeps_panels_and_positions() {
        let panels = vec![
            PanelSnapshotForTemplate {
                name: "Shell".to_string(),
                kind: PanelKind::Shell,
                command: None,
                args: Vec::new(),
                cwd: Some("/work/app".to_string()),
                position: [20.0, 20.0],
                size: [520.0, 340.0],
                env: BTreeMap::new(),
                resume: PanelResume::Fresh,
                ssh_connection: None,
            },
            PanelSnapshotForTemplate {
                name: "Claude".to_string(),
                kind: PanelKind::Claude,
                command: Some("claude".to_string()),
                args: vec!["--x".to_string()],
                cwd: None,
                position: [560.0, 40.0],
                size: [600.0, 400.0],
                env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude-work".to_string())]),
                resume: PanelResume::Last,
                ssh_connection: None,
            },
        ];

        let template = template_from_workspace(
            "Backend",
            Some("#aabbcc".to_string()),
            Some("/work".to_string()),
            &panels,
        );

        assert_eq!(template.name, "Backend");
        assert_eq!(template.color.as_deref(), Some("#aabbcc"));
        assert_eq!(template.cwd.as_deref(), Some("/work"));
        assert_eq!(template.position, None);
        assert_eq!(template.terminals.len(), 2);
        assert_eq!(template.terminals[0].name, "Shell");
        assert_eq!(template.terminals[0].cwd.as_deref(), Some("/work/app"));
        assert_eq!(template.terminals[0].position, Some([20.0, 20.0]));
        assert_eq!(template.terminals[1].position, Some([560.0, 40.0]));
        assert_eq!(template.terminals[1].size, Some([600.0, 400.0]));
        assert_eq!(template.terminals[1].kind, PanelKind::Claude);
        assert_eq!(template.terminals[1].resume, PanelResume::Last);
        assert_eq!(
            template.terminals[1].env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-work")
        );
    }

    #[test]
    fn upsert_replaces_linked_and_keeps_others() {
        let mut workspaces = vec![workspace("Alpha"), workspace("Beta"), workspace("Gamma")];
        let mut template = workspace("Beta v2");
        template.cwd = Some("/new".to_string());

        upsert_template(&mut workspaces, template, Some("bETA"));

        let names: Vec<&str> = workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta v2", "Gamma"]);
        assert_eq!(workspaces[1].cwd.as_deref(), Some("/new"));
    }

    #[test]
    fn upsert_appends_when_no_link() {
        let mut workspaces = vec![workspace("Alpha")];

        upsert_template(&mut workspaces, workspace("Beta"), None);
        upsert_template(&mut workspaces, workspace("Gamma"), Some("missing"));

        let names: Vec<&str> = workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn move_item_reorders_and_ignores_out_of_range() {
        let mut items = vec!['a', 'b', 'c', 'd'];

        move_item(&mut items, 0, 2);
        assert_eq!(items, ['b', 'c', 'a', 'd']);

        move_item(&mut items, 3, 0);
        assert_eq!(items, ['d', 'b', 'c', 'a']);

        move_item(&mut items, 1, 1);
        move_item(&mut items, 9, 0);
        move_item(&mut items, 0, 9);
        assert_eq!(items, ['d', 'b', 'c', 'a']);
    }

    #[test]
    fn template_yaml_round_trip_valid() {
        let preset = PresetConfig {
            name: "Claude Code".to_string(),
            alias: None,
            kind: PanelKind::Claude,
            command: Some("claude".to_string()),
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude-work".to_string())]),
        };
        let mut template = workspace("Tpl");
        template.terminals = (0..4).map(|i| terminal_from_preset(&preset, None, i)).collect();

        let mut config = Config::default();
        upsert_template(&mut config.workspaces, template, None);

        let yaml = config.to_yaml().expect("config should serialize");
        let parsed = Config::from_yaml(&yaml).expect("round-tripped config should validate");

        let restored = parsed
            .workspaces
            .iter()
            .find(|w| w.name == "Tpl")
            .expect("template should survive round trip");
        assert_eq!(restored.terminals.len(), 4);
        assert_eq!(
            restored.terminals[3].position,
            Some(grid_position(3, DEFAULT_PANEL_SIZE, 3))
        );
        assert_eq!(restored.terminals[0].env, preset.env);
    }
}
