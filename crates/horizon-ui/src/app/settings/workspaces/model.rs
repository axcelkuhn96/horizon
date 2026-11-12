//! Pure template-editing logic for the Workspaces tab (no egui).

use std::path::Path;

use horizon_core::{
    ClaudeProfile, DEFAULT_PANEL_SIZE, PresetConfig, TerminalConfig, WORKSPACE_COLORS, WorkspaceConfig,
    expand_config_dir, grid_position, move_item, terminal_from_preset, upsert_template, workspace_color_index,
};

use crate::app::hub_ui;

pub(super) const DEFAULT_TEMPLATE_NAME: &str = "New workspace";

/// Column count horizon-core uses when it lays terminals out on the template grid.
const TEMPLATE_GRID_COLUMNS: usize = 3;
const POSITION_TOLERANCE: f32 = 0.5;
const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Display names for [`WORKSPACE_COLORS`], used as swatch labels.
pub(super) const COLOR_NAMES: [&str; 8] = ["Blue", "Green", "Yellow", "Red", "Pink", "Teal", "Mauve", "Peach"];

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NameError {
    Empty,
    Duplicate,
}

impl NameError {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Empty => "Name cannot be empty",
            Self::Duplicate => "Another template already uses this name",
        }
    }
}

/// Why `name` cannot be used for the template at `index` (`None` = new template).
/// Collisions are case-insensitive, matching `upsert_template`.
pub(super) fn name_conflict(workspaces: &[WorkspaceConfig], index: Option<usize>, name: &str) -> Option<NameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some(NameError::Empty);
    }
    workspaces
        .iter()
        .enumerate()
        .any(|(other, workspace)| Some(other) != index && workspace.name.trim().eq_ignore_ascii_case(trimmed))
        .then_some(NameError::Duplicate)
}

/// First `New workspace`, `New workspace 2`, ... not used by any template.
pub(super) fn default_template_name(workspaces: &[WorkspaceConfig]) -> String {
    let mut candidate = DEFAULT_TEMPLATE_NAME.to_string();
    let mut counter = 1_u32;
    while name_conflict(workspaces, None, &candidate).is_some() {
        counter += 1;
        candidate = format!("{DEFAULT_TEMPLATE_NAME} {counter}");
    }
    candidate
}

/// Renames the template at `index` to the trimmed `name`; rejects empty and
/// colliding names without touching the config.
pub(super) fn rename_template(workspaces: &mut [WorkspaceConfig], index: usize, name: &str) -> Result<(), NameError> {
    if let Some(error) = name_conflict(workspaces, Some(index), name) {
        return Err(error);
    }
    if let Some(workspace) = workspaces.get_mut(index) {
        name.trim().clone_into(&mut workspace.name);
    }
    Ok(())
}

/// Text being typed into the Name field. The config only ever receives valid
/// names, so an in-progress empty or colliding edit never reaches the YAML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameDraft {
    pub(super) index: usize,
    pub(super) text: String,
    /// Config name the draft was last aligned with.
    synced: String,
}

impl NameDraft {
    /// Keeps `slot` aligned with the template at `index`: reloads when the
    /// selection changes or the name changed behind the draft's back (YAML tab,
    /// revert, reset).
    pub(super) fn sync(slot: &mut Option<Self>, index: usize, config_name: &str) {
        let aligned = slot
            .as_ref()
            .is_some_and(|draft| draft.index == index && draft.synced == config_name);
        if !aligned {
            *slot = Some(Self {
                index,
                text: config_name.to_string(),
                synced: config_name.to_string(),
            });
        }
    }

    pub(super) fn error(&self, workspaces: &[WorkspaceConfig]) -> Option<NameError> {
        name_conflict(workspaces, Some(self.index), &self.text)
    }

    /// Writes the draft into the config when valid. Returns `true` when the config changed.
    pub(super) fn commit(&mut self, workspaces: &mut [WorkspaceConfig]) -> bool {
        if rename_template(workspaces, self.index, &self.text).is_err() {
            return false;
        }
        let name = self.text.trim();
        let changed = self.synced != name;
        name.clone_into(&mut self.synced);
        changed
    }
}

// ---------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------

/// Saved color format (`#rrggbb` of a palette entry) is owned by horizon-core,
/// which also maps it back to a palette slot when workspaces start.
pub(super) use horizon_core::workspace_color_hex as color_hex;

/// Palette slot of a template: its saved color when it is a palette entry,
/// else the slot the board would assign by list position.
pub(super) fn template_color_index(template: &WorkspaceConfig, position: usize) -> usize {
    template
        .color
        .as_deref()
        .and_then(workspace_color_index)
        .unwrap_or(position % WORKSPACE_COLORS.len())
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

/// Appends a default-named template and returns its index.
pub(super) fn add_template(workspaces: &mut Vec<WorkspaceConfig>) -> usize {
    let index = workspaces.len();
    let template = WorkspaceConfig {
        name: default_template_name(workspaces),
        color: Some(color_hex(index)),
        cwd: None,
        position: None,
        terminals: Vec::new(),
        sidebar_collapsed: false,
        prompts: Vec::new(),
    };
    upsert_template(workspaces, template, None);
    index
}

/// Moves a template and returns where a selection on `selected` ends up.
pub(super) fn move_template(workspaces: &mut Vec<WorkspaceConfig>, from: usize, to: usize, selected: usize) -> usize {
    if from >= workspaces.len() || to >= workspaces.len() || from == to {
        return selected;
    }
    move_item(workspaces, from, to);
    if selected == from {
        to
    } else if from < selected && selected <= to {
        selected - 1
    } else if to <= selected && selected < from {
        selected + 1
    } else {
        selected
    }
}

/// Selection after removing `removed` from a list that now has `len_after` entries.
pub(super) fn selection_after_remove(selected: Option<usize>, removed: usize, len_after: usize) -> Option<usize> {
    if len_after == 0 {
        return None;
    }
    selected.map(|index| {
        let index = if index > removed { index - 1 } else { index };
        index.min(len_after - 1)
    })
}

// ---------------------------------------------------------------------------
// Terminals
// ---------------------------------------------------------------------------

fn near(position: Option<[f32; 2]>, expected: [f32; 2]) -> bool {
    position.is_some_and(|p| {
        (p[0] - expected[0]).abs() < POSITION_TOLERANCE && (p[1] - expected[1]).abs() < POSITION_TOLERANCE
    })
}

fn slot_position(index: usize, terminal: &TerminalConfig) -> [f32; 2] {
    grid_position(
        index,
        terminal.size.unwrap_or(DEFAULT_PANEL_SIZE),
        TEMPLATE_GRID_COLUMNS,
    )
}

/// `true` when every terminal still sits on the slot its list index implies
/// (the layout `terminal_from_preset` produces), so reordering can re-flow it.
fn on_default_grid(terminals: &[TerminalConfig]) -> bool {
    !terminals.is_empty()
        && terminals
            .iter()
            .enumerate()
            .all(|(index, terminal)| near(terminal.position, slot_position(index, terminal)))
}

fn regrid(terminals: &mut [TerminalConfig]) {
    for (index, terminal) in terminals.iter_mut().enumerate() {
        terminal.position = Some(slot_position(index, terminal));
    }
}

pub(super) fn add_terminal(template: &mut WorkspaceConfig, preset: &PresetConfig) {
    let index = template.terminals.len();
    template.terminals.push(terminal_from_preset(preset, None, index));
}

/// Reorders a terminal; terminals still on the default grid follow the new order.
pub(super) fn move_terminal(template: &mut WorkspaceConfig, from: usize, to: usize) {
    let grid = on_default_grid(&template.terminals);
    move_item(&mut template.terminals, from, to);
    if grid {
        regrid(&mut template.terminals);
    }
}

pub(super) fn remove_terminal(template: &mut WorkspaceConfig, index: usize) {
    if index >= template.terminals.len() {
        return;
    }
    let grid = on_default_grid(&template.terminals);
    template.terminals.remove(index);
    if grid {
        regrid(&mut template.terminals);
    }
}

/// Index into `profiles` of the Claude profile a terminal was created from
/// (its `CLAUDE_CONFIG_DIR` points at the profile directory).
pub(super) fn terminal_profile_index(
    terminal: &TerminalConfig,
    profiles: &[ClaudeProfile],
    home: &Path,
) -> Option<usize> {
    let dir = terminal.env.get(CLAUDE_CONFIG_DIR_ENV)?;
    profiles
        .iter()
        .position(|profile| expand_config_dir(&profile.config_dir, home).as_os_str() == dir.as_str())
}

// ---------------------------------------------------------------------------
// Add-terminal choices
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(super) struct PresetChoice {
    pub(super) preset: PresetConfig,
    /// Accent slot (see `hub_ui::accent_color`) when this is a derived `Claude · <profile>` preset.
    pub(super) profile_slot: Option<usize>,
}

/// Splits resolved presets into plain presets (first) and derived Claude
/// profile presets (in profile order, with their accent slot).
pub(super) fn classify_presets(resolved: Vec<PresetConfig>, profiles: &[ClaudeProfile]) -> Vec<PresetChoice> {
    let names: Vec<&str> = profiles.iter().map(|profile| profile.name.as_str()).collect();
    let slots = hub_ui::assign_accent_indices(&names);
    let derived_names: Vec<String> = profiles
        .iter()
        .map(|profile| format!("Claude \u{b7} {}", profile.name))
        .collect();

    let mut general = Vec::new();
    let mut by_profile: Vec<(usize, PresetConfig)> = Vec::new();
    for preset in resolved {
        match derived_names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&preset.name))
        {
            Some(index) => by_profile.push((index, preset)),
            None => general.push(preset),
        }
    }
    by_profile.sort_by_key(|(index, _)| *index);

    general
        .into_iter()
        .map(|preset| PresetChoice {
            preset,
            profile_slot: None,
        })
        .chain(by_profile.into_iter().map(|(index, preset)| PresetChoice {
            preset,
            profile_slot: slots.get(index).copied(),
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use horizon_core::{PanelKind, PanelResume};

    use super::*;

    fn template(name: &str) -> WorkspaceConfig {
        WorkspaceConfig {
            name: name.to_string(),
            color: None,
            cwd: None,
            position: None,
            terminals: Vec::new(),
            sidebar_collapsed: false,
            prompts: Vec::new(),
        }
    }

    fn preset(name: &str, kind: PanelKind) -> PresetConfig {
        PresetConfig {
            name: name.to_string(),
            alias: None,
            kind,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: BTreeMap::new(),
        }
    }

    fn profile(name: &str, dir: &str) -> ClaudeProfile {
        ClaudeProfile {
            name: name.to_string(),
            config_dir: dir.to_string(),
        }
    }

    #[test]
    fn add_template_appends_default_named() {
        let mut workspaces = vec![template("api")];
        let index = add_template(&mut workspaces);
        assert_eq!(index, 1);
        assert_eq!(workspaces[1].name, "New workspace");
        assert!(workspaces[1].terminals.is_empty());
        assert_eq!(workspaces[1].color.as_deref(), Some(color_hex(1).as_str()));

        let index = add_template(&mut workspaces);
        assert_eq!(index, 2);
        assert_eq!(workspaces[2].name, "New workspace 2");
    }

    #[test]
    fn default_name_skips_case_insensitive_collisions() {
        let workspaces = vec![template("NEW WORKSPACE")];
        assert_eq!(default_template_name(&workspaces), "New workspace 2");
    }

    #[test]
    fn rename_rejects_empty_name() {
        let mut workspaces = vec![template("api")];
        assert_eq!(rename_template(&mut workspaces, 0, "   "), Err(NameError::Empty));
        assert_eq!(rename_template(&mut workspaces, 0, ""), Err(NameError::Empty));
        assert_eq!(workspaces[0].name, "api");

        assert_eq!(rename_template(&mut workspaces, 0, "  web  "), Ok(()));
        assert_eq!(workspaces[0].name, "web");
    }

    #[test]
    fn rename_rejects_case_insensitive_collision_but_allows_own_name() {
        let mut workspaces = vec![template("api"), template("web")];
        assert_eq!(rename_template(&mut workspaces, 1, "API"), Err(NameError::Duplicate));
        assert_eq!(workspaces[1].name, "web");
        assert_eq!(rename_template(&mut workspaces, 0, "Api"), Ok(()));
        assert_eq!(workspaces[0].name, "Api");
    }

    #[test]
    fn name_draft_only_commits_valid_names_and_resyncs_on_external_change() {
        let mut workspaces = vec![template("api"), template("web")];
        let mut slot = None;
        NameDraft::sync(&mut slot, 0, "api");
        let draft = slot.as_mut().expect("draft");

        draft.text = String::new();
        assert_eq!(draft.error(&workspaces), Some(NameError::Empty));
        assert!(!draft.commit(&mut workspaces));
        assert_eq!(workspaces[0].name, "api");

        draft.text = "web".to_string();
        assert_eq!(draft.error(&workspaces), Some(NameError::Duplicate));
        assert!(!draft.commit(&mut workspaces));

        draft.text = "core".to_string();
        assert!(draft.commit(&mut workspaces));
        assert_eq!(workspaces[0].name, "core");
        assert!(!draft.commit(&mut workspaces));

        // Same config name, same index: draft is kept (mid-edit state survives).
        draft.text = String::new();
        NameDraft::sync(&mut slot, 0, "core");
        assert_eq!(slot.as_ref().map(|d| d.text.as_str()), Some(""));

        // Config changed behind the draft (YAML edit): reload.
        NameDraft::sync(&mut slot, 0, "renamed");
        assert_eq!(slot.as_ref().map(|d| d.text.as_str()), Some("renamed"));
        // Selection changed: reload.
        NameDraft::sync(&mut slot, 1, "web");
        assert_eq!(slot.as_ref().map(|d| d.index), Some(1));
    }

    #[test]
    fn adding_terminal_by_preset_uses_terminal_from_preset() {
        let mut tpl = template("api");
        let mut claude = preset("Claude \u{b7} claude3", PanelKind::Claude);
        claude.command = Some("claude".to_string());
        claude
            .env
            .insert("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude-claude3".to_string());

        add_terminal(&mut tpl, &preset("Shell", PanelKind::Shell));
        add_terminal(&mut tpl, &claude);

        assert_eq!(tpl.terminals.len(), 2);
        let expected = terminal_from_preset(&claude, None, 1);
        let added = &tpl.terminals[1];
        assert_eq!(added.name, expected.name);
        assert_eq!(added.kind, PanelKind::Claude);
        assert_eq!(added.command.as_deref(), Some("claude"));
        assert_eq!(added.env, claude.env);
        assert_eq!(added.position, expected.position);
        assert_eq!(tpl.terminals[0].kind, PanelKind::Shell);
    }

    #[test]
    fn reordering_default_grid_terminals_reflows_positions() {
        let mut tpl = template("api");
        add_terminal(&mut tpl, &preset("A", PanelKind::Shell));
        add_terminal(&mut tpl, &preset("B", PanelKind::Shell));
        add_terminal(&mut tpl, &preset("C", PanelKind::Shell));

        move_terminal(&mut tpl, 0, 2);

        let names: Vec<_> = tpl.terminals.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["B", "C", "A"]);
        for (index, terminal) in tpl.terminals.iter().enumerate() {
            assert!(near(terminal.position, slot_position(index, terminal)));
        }

        remove_terminal(&mut tpl, 0);
        let names: Vec<_> = tpl.terminals.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["C", "A"]);
        assert!(near(tpl.terminals[0].position, slot_position(0, &tpl.terminals[0])));
    }

    #[test]
    fn reordering_custom_positioned_terminals_keeps_positions() {
        let mut tpl = template("api");
        add_terminal(&mut tpl, &preset("A", PanelKind::Shell));
        add_terminal(&mut tpl, &preset("B", PanelKind::Shell));
        tpl.terminals[0].position = Some([900.0, 700.0]);

        move_terminal(&mut tpl, 0, 1);

        assert_eq!(tpl.terminals[0].name, "B");
        assert_eq!(tpl.terminals[1].name, "A");
        assert_eq!(tpl.terminals[1].position, Some([900.0, 700.0]));
    }

    #[test]
    fn move_template_tracks_selection() {
        let mut workspaces = vec![template("a"), template("b"), template("c")];
        assert_eq!(move_template(&mut workspaces, 0, 1, 0), 1);
        assert_eq!(workspaces[1].name, "a");
        // Selection on a neighbour that shifted.
        let mut workspaces = vec![template("a"), template("b"), template("c")];
        assert_eq!(move_template(&mut workspaces, 0, 2, 1), 0);
        assert_eq!(move_template(&mut workspaces, 2, 0, 0), 1);
        // Out of range is a no-op.
        assert_eq!(move_template(&mut workspaces, 0, 9, 2), 2);
    }

    #[test]
    fn selection_after_remove_clamps() {
        assert_eq!(selection_after_remove(Some(0), 0, 0), None);
        assert_eq!(selection_after_remove(Some(2), 2, 2), Some(1));
        assert_eq!(selection_after_remove(Some(2), 0, 2), Some(1));
        assert_eq!(selection_after_remove(Some(0), 1, 2), Some(0));
        assert_eq!(selection_after_remove(None, 0, 2), None);
    }

    #[test]
    fn color_round_trips_through_palette() {
        for index in 0..WORKSPACE_COLORS.len() {
            let mut tpl = template("x");
            tpl.color = Some(color_hex(index).to_uppercase());
            assert_eq!(template_color_index(&tpl, 5), index);
        }
        let mut tpl = template("x");
        tpl.color = Some("#123456".to_string());
        assert_eq!(template_color_index(&tpl, 10), 10 % WORKSPACE_COLORS.len());
        assert_eq!(color_hex(0), "#89b4fa");
        assert_eq!(COLOR_NAMES.len(), WORKSPACE_COLORS.len());
    }

    #[test]
    fn classify_presets_puts_profiles_last_with_stable_slots() {
        let profiles = vec![profile("conta2", "~/.claude-conta2"), profile("claude3", "~/.claude3")];
        let resolved = vec![
            preset("Claude \u{b7} claude3", PanelKind::Claude),
            preset("Shell", PanelKind::Shell),
            preset("Claude \u{b7} conta2", PanelKind::Claude),
            preset("Codex", PanelKind::Codex),
        ];

        let choices = classify_presets(resolved, &profiles);

        let summary: Vec<_> = choices
            .iter()
            .map(|choice| (choice.preset.name.as_str(), choice.profile_slot.is_some()))
            .collect();
        assert_eq!(
            summary,
            [
                ("Shell", false),
                ("Codex", false),
                ("Claude \u{b7} conta2", true),
                ("Claude \u{b7} claude3", true),
            ]
        );
        let slots = hub_ui::assign_accent_indices(&["conta2", "claude3"]);
        assert_eq!(choices[2].profile_slot, Some(slots[0]));
        assert_eq!(choices[3].profile_slot, Some(slots[1]));
    }

    #[test]
    fn terminal_profile_index_matches_config_dir_env() {
        let profiles = vec![profile("a", "~/.claude-a"), profile("b", "/data/claude-b")];
        let home = PathBuf::from("/home/u");
        let mut terminal = TerminalConfig::default();
        assert_eq!(terminal_profile_index(&terminal, &profiles, &home), None);

        terminal
            .env
            .insert("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude-a".to_string());
        assert_eq!(terminal_profile_index(&terminal, &profiles, &home), Some(0));

        terminal
            .env
            .insert("CLAUDE_CONFIG_DIR".to_string(), "/data/claude-b".to_string());
        assert_eq!(terminal_profile_index(&terminal, &profiles, &home), Some(1));

        terminal
            .env
            .insert("CLAUDE_CONFIG_DIR".to_string(), "/elsewhere".to_string());
        assert_eq!(terminal_profile_index(&terminal, &profiles, &home), None);
    }
}
