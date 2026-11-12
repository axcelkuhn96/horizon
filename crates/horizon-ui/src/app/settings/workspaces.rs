//! Settings "Workspaces" tab: edit workspace templates without touching YAML
//! (spec: `docs/design/claude-hub-ui.md` §4). Edits go to `editing_config.workspaces`
//! and are mirrored into the YAML buffer, so the YAML tab always sees them.

mod model;
mod widgets;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, Vec2};
use horizon_core::{Config, WORKSPACE_COLORS, WorkspaceConfig};

use self::model::{NameDraft, PresetChoice};
use self::widgets::{ListKey, ListRowAction, TerminalRowAction};
use super::SettingsEditor;
use crate::app::HorizonApp;
use crate::app::hub_ui::{self, ModalPlacement};
use crate::theme;

const LIST_WIDTH: f32 = 240.0;
const COLUMN_GAP: f32 = 16.0;
/// Below this width the template list stacks above the editor instead of beside it.
const TWO_COLUMN_MIN_WIDTH: f32 = 600.0;
/// Stacked layout shows about five template rows (36 + 2 gap each) before scrolling.
const STACKED_LIST_MAX_HEIGHT: f32 = 5.0 * 38.0;
const LABEL_WIDTH: f32 = 52.0;
const FIELD_HEIGHT: f32 = 28.0;
const ADD_LIST_WIDTH: f32 = 260.0;
const OPENED_FEEDBACK: Duration = Duration::from_secs(3);

/// "Browse" request for the app to open the directory picker (see `actions/pickers.rs`).
pub(in crate::app) struct BrowseRequest {
    pub(in crate::app) index: usize,
    pub(in crate::app) name: String,
    pub(in crate::app) seed: Option<PathBuf>,
}

struct OpenFeedback {
    at: Instant,
    failed_panels: usize,
}

#[derive(Default)]
pub(super) struct WorkspacesTabState {
    selected: Option<usize>,
    /// Template awaiting delete confirmation.
    confirm_remove: Option<usize>,
    /// Highlighted row of the add-terminal list (keyboard).
    preset_pick: usize,
    add_open: bool,
    /// Set by `open_add_list`; the next list render skips key reading (see `list_key_for_frame`).
    add_just_opened: bool,
    /// Resolved presets, refreshed each time the add-terminal list opens.
    choices: Vec<PresetChoice>,
    name_draft: Option<NameDraft>,
    /// "Open now" request for the app to materialize (see `open_template.rs`).
    pending_open: Option<WorkspaceConfig>,
    pending_browse: Option<BrowseRequest>,
    feedback: Option<OpenFeedback>,
}

impl WorkspacesTabState {
    fn request_remove(&mut self, index: usize) {
        self.confirm_remove = Some(index);
    }

    fn cancel_remove(&mut self) {
        self.confirm_remove = None;
    }

    /// Removes the template awaiting confirmation. Live workspaces are untouched.
    fn confirm_pending_remove(&mut self, workspaces: &mut Vec<WorkspaceConfig>) -> bool {
        let Some(index) = self.confirm_remove.take() else {
            return false;
        };
        if index >= workspaces.len() {
            return false;
        }
        workspaces.remove(index);
        self.selected = model::selection_after_remove(self.selected, index, workspaces.len());
        self.name_draft = None;
        self.add_open = false;
        true
    }

    fn open_add_list(&mut self, config: &Config) {
        self.choices = model::classify_presets(config.resolved_presets(), &config.claude_profiles);
        self.preset_pick = 0;
        self.add_open = true;
        self.add_just_opened = true;
    }

    fn select(&mut self, index: usize) {
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.add_open = false;
            self.feedback = None;
        }
    }
}

impl HorizonApp {
    /// Takes the "Open now" request raised by the Workspaces tab, if any.
    pub(in crate::app) fn take_pending_template_open(&mut self) -> Option<WorkspaceConfig> {
        self.settings.as_mut()?.workspaces_tab.pending_open.take()
    }

    /// Takes the "Browse" request raised by the Workspaces tab, if any.
    pub(in crate::app) fn take_pending_template_browse(&mut self) -> Option<BrowseRequest> {
        self.settings.as_mut()?.workspaces_tab.pending_browse.take()
    }

    /// Writes a directory chosen in the picker into template `index`, provided
    /// it is still the template named `name`, and mirrors it into the YAML buffer.
    pub(in crate::app) fn apply_template_cwd(&mut self, index: usize, name: &str, path: &std::path::Path) {
        let Some(editor) = self.settings.as_mut() else {
            return;
        };
        let Some(config) = editor.editing_config.as_mut() else {
            return;
        };
        let Some(template) = config
            .workspaces
            .get_mut(index)
            .filter(|template| template.name == name)
        else {
            return;
        };
        template.cwd = Some(horizon_core::dir_search::abbreviate_home(path));
        if let Ok(yaml) = config.to_yaml() {
            editor.buffer = yaml;
        }
    }

    /// Shows the "Opened in current session" confirmation under the Open now button.
    pub(in crate::app) fn record_template_opened(&mut self, failed_panels: usize) {
        if let Some(editor) = self.settings.as_mut() {
            editor.workspaces_tab.feedback = Some(OpenFeedback {
                at: Instant::now(),
                failed_panels,
            });
        }
    }
}

pub(super) fn show(ui: &mut Ui, editor: &mut SettingsEditor) {
    let changed = {
        let Some(config) = editor.editing_config.as_mut() else {
            ui.label(
                RichText::new("Unable to parse current configuration")
                    .color(theme::PALETTE_RED())
                    .size(12.0),
            );
            return;
        };
        render(ui, config, &mut editor.workspaces_tab)
    };
    if changed
        && let Some(config) = editor.editing_config.as_ref()
        && let Ok(yaml) = config.to_yaml()
    {
        editor.buffer = yaml;
    }
}

/// Renders the tab; returns `true` when `config.workspaces` changed.
fn render(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState) -> bool {
    let mut changed = false;
    let count = config.workspaces.len();
    state.selected = match state.selected {
        Some(index) if index < count => Some(index),
        _ if count > 0 => Some(0),
        _ => None,
    };
    if state.confirm_remove.is_some_and(|index| index >= count) {
        state.cancel_remove();
    }

    super::section_heading(ui, "Workspace templates");
    super::dim_label(
        ui,
        "Templates describe a workspace and its terminals. Open one now, or let it start with your session.",
    );
    ui.add_space(10.0);

    let total_width = ui.available_width();
    if total_width >= TWO_COLUMN_MIN_WIDTH {
        let top = ui.cursor().top();
        let mut divider_x = 0.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let list = ui.allocate_ui_with_layout(
                Vec2::new(LIST_WIDTH, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(LIST_WIDTH);
                    changed |= template_list(ui, config, state, false);
                },
            );
            divider_x = list.response.rect.right() + COLUMN_GAP / 2.0;
            ui.add_space(COLUMN_GAP);
            ui.vertical(|ui| {
                changed |= editor_column(ui, config, state);
            });
        });
        let bottom = ui.min_rect().bottom();
        ui.painter()
            .vline(divider_x, top..=bottom, Stroke::new(1.0_f32, theme::BORDER_SUBTLE()));
    } else {
        changed |= template_list(ui, config, state, true);
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);
        changed |= editor_column(ui, config, state);
    }

    changed |= delete_modal(ui, config, state);
    if let Some(feedback) = &state.feedback {
        let elapsed = feedback.at.elapsed();
        if elapsed >= OPENED_FEEDBACK {
            state.feedback = None;
        } else {
            ui.ctx().request_repaint_after(OPENED_FEEDBACK - elapsed);
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Template list
// ---------------------------------------------------------------------------

fn template_list(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState, stacked: bool) -> bool {
    let mut changed = false;
    ui.label(RichText::new("TEMPLATES").size(9.5).color(theme::FG_DIM()));
    ui.add_space(4.0);

    let count = config.workspaces.len();
    if count == 0 {
        ui.add(
            egui::Label::new(
                RichText::new("No templates yet. Create one or use Save as template from a workspace.")
                    .size(11.0)
                    .color(theme::FG_DIM()),
            )
            .wrap(),
        );
        ui.add_space(8.0);
    }

    let mut move_request: Option<(usize, usize)> = None;
    let max_height = if stacked {
        STACKED_LIST_MAX_HEIGHT
    } else {
        f32::INFINITY
    };
    egui::ScrollArea::vertical()
        .id_salt("workspace_template_list")
        .max_height(max_height)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for index in 0..count {
                let color_index = model::template_color_index(&config.workspaces[index], index);
                let (r, g, b) = WORKSPACE_COLORS[color_index % WORKSPACE_COLORS.len()];
                let action = widgets::template_list_row(
                    ui,
                    &config.workspaces[index].name,
                    Color32::from_rgb(r, g, b),
                    state.selected == Some(index),
                    index > 0,
                    index + 1 < count,
                );
                match action {
                    ListRowAction::None => {}
                    ListRowAction::Select => state.select(index),
                    ListRowAction::MoveUp => move_request = Some((index, index - 1)),
                    ListRowAction::MoveDown => move_request = Some((index, index + 1)),
                }
            }
        });
    if let Some((from, to)) = move_request {
        let selected = state.selected.unwrap_or(from);
        state.selected = Some(model::move_template(&mut config.workspaces, from, to, selected));
        changed = true;
    }

    ui.add_space(8.0);
    if widgets::wide_secondary_button(ui, "+ New template").clicked() {
        let index = model::add_template(&mut config.workspaces);
        state.select(index);
        state.name_draft = None;
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------------------
// Editor column
// ---------------------------------------------------------------------------

fn field_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(LABEL_WIDTH, FIELD_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| ui.label(RichText::new(label).size(11.0).color(theme::FG_DIM())),
        );
        add(ui)
    })
    .inner
}

fn text_field(ui: &mut Ui, text: &mut String, font: egui::FontId, hint: &str) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(ui.available_width())
            .hint_text(hint)
            .font(font)
            .margin(Margin::symmetric(8, 6)),
    )
}

fn editor_column(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState) -> bool {
    let Some(index) = state.selected else {
        ui.add_space(24.0);
        super::dim_label(ui, "Select a template, or create a new one.");
        return false;
    };
    let mut changed = false;
    ui.label(RichText::new("EDITOR").size(9.5).color(theme::FG_DIM()));
    ui.add_space(4.0);

    changed |= name_row(ui, config, state, index);
    ui.add_space(4.0);

    field_row(ui, "Cwd", |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if hub_ui::icon_button(ui, hub_ui::IconKind::Browse, "Browse for folder").clicked() {
                let seed = config.workspaces[index]
                    .cwd
                    .as_deref()
                    .filter(|cwd| !cwd.trim().is_empty())
                    .map(Config::expand_tilde);
                state.pending_browse = Some(BrowseRequest {
                    index,
                    name: config.workspaces[index].name.clone(),
                    seed,
                });
            }
            let mut cwd = config.workspaces[index].cwd.clone().unwrap_or_default();
            if text_field(ui, &mut cwd, egui::FontId::monospace(11.0), "~/code/project").changed() {
                config.workspaces[index].cwd = if cwd.trim().is_empty() { None } else { Some(cwd) };
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    let current_color = model::template_color_index(&config.workspaces[index], index);
    field_row(ui, "Color", |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.add_space(2.0);
        for (slot, (r, g, b)) in WORKSPACE_COLORS.iter().copied().enumerate() {
            let clicked = hub_ui::swatch(
                ui,
                Color32::from_rgb(r, g, b),
                slot == current_color,
                model::COLOR_NAMES[slot % model::COLOR_NAMES.len()],
            )
            .clicked();
            if clicked {
                config.workspaces[index].color = Some(model::color_hex(slot));
                changed = true;
            }
        }
    });

    ui.add_space(14.0);
    changed |= terminals_section(ui, config, state, index);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    footer(ui, config, state, index);
    changed
}

fn name_row(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState, index: usize) -> bool {
    let mut changed = false;
    NameDraft::sync(&mut state.name_draft, index, &config.workspaces[index].name);
    let Some(draft) = state.name_draft.as_mut() else {
        return false;
    };
    let invalid = draft.error(&config.workspaces).is_some();
    let response = field_row(ui, "Name", |ui| {
        widgets::error_scope(ui, invalid, |ui| {
            text_field(ui, &mut draft.text, egui::FontId::proportional(12.0), "Template name")
        })
    });
    if response.changed() {
        changed = draft.commit(&mut config.workspaces);
    }
    if let Some(error) = draft.error(&config.workspaces) {
        ui.horizontal(|ui| {
            ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
            ui.label(RichText::new(error.message()).size(11.0).color(theme::PALETTE_RED()));
        });
    }
    changed
}

fn terminals_section(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState, index: usize) -> bool {
    let mut changed = false;
    let count = config.workspaces[index].terminals.len();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("Terminals ({count})"))
                .size(13.0)
                .strong()
                .color(theme::FG_SOFT()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if hub_ui::secondary_button(ui, "+ Add terminal").clicked() {
                if state.add_open {
                    state.add_open = false;
                } else {
                    state.open_add_list(config);
                }
            }
        });
    });
    ui.add_space(6.0);

    if state.add_open {
        changed |= add_terminal_list(ui, config, state, index);
        ui.add_space(8.0);
    }

    if count == 0 {
        super::dim_label(ui, "No terminals yet. Add a shell or a Claude profile.");
        return changed;
    }

    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let names: Vec<&str> = config
        .claude_profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    let slots = hub_ui::assign_accent_indices(&names);
    let profiles = &config.claude_profiles;
    let terminals = &mut config.workspaces[index].terminals;

    let mut row_action: Option<(usize, TerminalRowAction)> = None;
    ui.spacing_mut().item_spacing.y = 4.0;
    for (position, terminal) in terminals.iter_mut().enumerate() {
        let profile_accent = home
            .as_deref()
            .and_then(|home| model::terminal_profile_index(terminal, profiles, home))
            .and_then(|profile| slots.get(profile).copied())
            .map(hub_ui::accent_color);
        let kind = terminal.kind;
        let bar = widgets::kind_bar_color(kind, profile_accent);
        let icon_accent = profile_accent.unwrap_or_else(theme::ACCENT);
        let outcome = ui
            .push_id(("workspace_terminal", index, position), |ui| {
                widgets::terminal_row(ui, terminal, bar, icon_accent, position == 0, position + 1 == count)
            })
            .inner;
        changed |= outcome.changed;
        if outcome.action != TerminalRowAction::None {
            row_action = Some((position, outcome.action));
        }
    }
    if let Some((position, action)) = row_action {
        let template = &mut config.workspaces[index];
        match action {
            TerminalRowAction::MoveUp => model::move_terminal(template, position, position - 1),
            TerminalRowAction::MoveDown => model::move_terminal(template, position, position + 1),
            TerminalRowAction::Remove => model::remove_terminal(template, position),
            TerminalRowAction::None => {}
        }
        changed = true;
    }
    changed
}

/// Key for this frame's add-terminal list. The frame that opened the list is
/// skipped so the Enter that activated "+ Add terminal" is not read as Confirm.
fn list_key_for_frame(state: &mut WorkspacesTabState, read: impl FnOnce() -> ListKey) -> ListKey {
    if std::mem::take(&mut state.add_just_opened) {
        ListKey::None
    } else {
        read()
    }
}

fn add_terminal_list(ui: &mut Ui, config: &mut Config, state: &mut WorkspacesTabState, index: usize) -> bool {
    let len = state.choices.len();
    let mut picked: Option<usize> = None;
    match list_key_for_frame(state, || widgets::read_list_key(ui)) {
        ListKey::Down => state.preset_pick = (state.preset_pick + 1).min(len.saturating_sub(1)),
        ListKey::Up => state.preset_pick = state.preset_pick.saturating_sub(1),
        ListKey::Confirm => picked = (len > 0).then_some(state.preset_pick),
        ListKey::Cancel => {
            state.add_open = false;
            return false;
        }
        ListKey::None => {}
    }
    let mut hovered: Option<usize> = None;
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        Frame::new()
            .fill(theme::PANEL_BG())
            .stroke(Stroke::new(1.0_f32, theme::BORDER_STRONG()))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.set_width(ADD_LIST_WIDTH - 12.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                if len == 0 {
                    super::dim_label(ui, "No presets available.");
                }
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    let mut section_started = [false, false];
                    for (position, choice) in state.choices.iter().enumerate() {
                        let is_profile = choice.profile_slot.is_some();
                        let section = usize::from(is_profile);
                        if !section_started[section] {
                            if is_profile && section_started[0] {
                                ui.add_space(2.0);
                                ui.separator();
                            }
                            let title = if is_profile { "CLAUDE PROFILES" } else { "PRESETS" };
                            ui.label(RichText::new(title).size(9.5).color(theme::FG_DIM()));
                            section_started[section] = true;
                        }
                        let accent = choice.profile_slot.map(hub_ui::accent_color);
                        let response = widgets::preset_row(
                            ui,
                            &choice.preset.name,
                            choice.preset.kind,
                            accent,
                            state.preset_pick == position,
                        );
                        if response.hovered() {
                            hovered = Some(position);
                        }
                        if response.clicked() {
                            picked = Some(position);
                        }
                    }
                });
            });
    });
    if let Some(position) = hovered {
        state.preset_pick = position;
    }
    let Some(position) = picked else {
        return false;
    };
    let Some(preset) = state.choices.get(position).map(|choice| choice.preset.clone()) else {
        return false;
    };
    model::add_terminal(&mut config.workspaces[index], &preset);
    state.add_open = false;
    true
}

fn footer(ui: &mut Ui, config: &Config, state: &mut WorkspacesTabState, index: usize) {
    let terminal_count = config.workspaces[index].terminals.len();
    ui.horizontal(|ui| {
        if widgets::danger_button(ui, "Delete template").clicked() {
            state.request_remove(index);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_open = terminal_count > 0;
            let response = hub_ui::primary_button(ui, "Open now", can_open);
            if !can_open {
                let _ = response.on_hover_text("Add at least one terminal first");
            } else if response.clicked() {
                state.pending_open = Some(config.workspaces[index].clone());
                state.feedback = None;
            }
            if let Some(feedback) = &state.feedback {
                let (text, color) = if feedback.failed_panels == 0 {
                    ("Opened in current session".to_string(), theme::PALETTE_GREEN())
                } else {
                    (
                        format!("Opened, but {} terminal(s) failed to start", feedback.failed_panels),
                        theme::PALETTE_RED(),
                    )
                };
                ui.label(RichText::new(text).size(11.0).color(color));
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Delete confirmation
// ---------------------------------------------------------------------------

fn delete_modal(ui: &Ui, config: &mut Config, state: &mut WorkspacesTabState) -> bool {
    let Some(index) = state.confirm_remove else {
        return false;
    };
    let Some(name) = config.workspaces.get(index).map(|workspace| workspace.name.clone()) else {
        state.cancel_remove();
        return false;
    };
    let mut confirmed = false;
    let mut cancelled = false;
    let outcome = hub_ui::show_modal(
        ui.ctx(),
        egui::Id::new("workspaces_delete_modal"),
        380.0,
        ModalPlacement::Center,
        None,
        |ui| {
            Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.label(RichText::new("Delete template").size(13.0).strong().color(theme::FG()));
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Delete template \"{name}\"? Running panels are not affected."))
                            .size(12.0)
                            .color(theme::FG_SOFT()),
                    )
                    .wrap(),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::danger_button(ui, "Delete").clicked() {
                        confirmed = true;
                    }
                    if hub_ui::secondary_button(ui, "Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        },
    );
    if confirmed {
        return state.confirm_pending_remove(&mut config.workspaces);
    }
    if cancelled || outcome.dismissed {
        state.cancel_remove();
    }
    false
}

#[cfg(test)]
mod tests {
    use horizon_core::{PanelKind, PanelResume, PresetConfig};

    use super::*;

    fn template(name: &str) -> WorkspaceConfig {
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
    fn remove_requires_confirmation_state() {
        let mut workspaces = vec![template("a"), template("b")];
        let mut state = WorkspacesTabState {
            selected: Some(1),
            ..WorkspacesTabState::default()
        };

        // Nothing is removed until a request is confirmed.
        assert!(!state.confirm_pending_remove(&mut workspaces));
        assert_eq!(workspaces.len(), 2);

        state.request_remove(1);
        assert_eq!(state.confirm_remove, Some(1));
        assert_eq!(workspaces.len(), 2);

        state.cancel_remove();
        assert!(!state.confirm_pending_remove(&mut workspaces));
        assert_eq!(workspaces.len(), 2);

        state.request_remove(1);
        assert!(state.confirm_pending_remove(&mut workspaces));
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].name, "a");
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.confirm_remove, None);

        state.request_remove(0);
        assert!(state.confirm_pending_remove(&mut workspaces));
        assert!(workspaces.is_empty());
        assert_eq!(state.selected, None);
    }

    #[test]
    fn opening_frame_does_not_read_list_keys() {
        let config = Config::default();
        let mut state = WorkspacesTabState::default();
        state.open_add_list(&config);

        let first = list_key_for_frame(&mut state, || panic!("must not read keys on the opening frame"));
        assert_eq!(first, ListKey::None);

        let second = list_key_for_frame(&mut state, || ListKey::Confirm);
        assert_eq!(second, ListKey::Confirm);
    }

    #[test]
    fn select_clears_open_feedback() {
        let mut state = WorkspacesTabState {
            selected: Some(0),
            feedback: Some(OpenFeedback {
                at: Instant::now(),
                failed_panels: 0,
            }),
            ..WorkspacesTabState::default()
        };
        state.select(1);
        assert!(state.feedback.is_none());
    }

    #[test]
    fn stale_remove_request_is_ignored() {
        let mut workspaces = vec![template("a")];
        let mut state = WorkspacesTabState::default();
        state.request_remove(5);
        assert!(!state.confirm_pending_remove(&mut workspaces));
        assert_eq!(workspaces.len(), 1);
    }

    #[test]
    fn tab_switch_preserves_edits_through_yaml() {
        let mut config = Config::default();
        config.workspaces.clear();
        let index = model::add_template(&mut config.workspaces);
        model::rename_template(&mut config.workspaces, index, "api-core").expect("valid name");
        config.workspaces[index].cwd = Some("~/code/api".to_string());
        config.workspaces[index].color = Some(model::color_hex(3));
        let preset = PresetConfig {
            name: "Claude \u{b7} claude3".to_string(),
            alias: Some("claude3".to_string()),
            kind: PanelKind::Claude,
            command: Some("claude".to_string()),
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: std::iter::once(("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.claude3".to_string())).collect(),
        };
        model::add_terminal(&mut config.workspaces[index], &preset);

        let yaml = config.to_yaml().expect("serialize");
        let parsed = Config::from_yaml(&yaml).expect("parse");

        assert_eq!(parsed.workspaces.len(), 1);
        let template = &parsed.workspaces[0];
        assert_eq!(template.name, "api-core");
        assert_eq!(template.cwd.as_deref(), Some("~/code/api"));
        assert_eq!(template.color, Some(model::color_hex(3)));
        assert_eq!(template.terminals.len(), 1);
        assert_eq!(template.terminals[0].kind, PanelKind::Claude);
        assert_eq!(
            template.terminals[0].env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude3")
        );
        assert_eq!(parsed.to_yaml().expect("re-serialize"), yaml);
    }
}
