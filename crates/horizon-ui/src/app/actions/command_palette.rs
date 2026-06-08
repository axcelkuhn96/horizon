use egui::Context;
use horizon_core::{Direction, PanelKind};

use crate::app::HorizonApp;
use crate::app::shortcuts::shortcut_pressed;
use crate::command_palette::{CommandPalette, PaletteAction};
use crate::command_registry::CommandId;
use crate::search_overlay::SearchOverlay;

use super::align_attached_workspaces;
use super::support::{
    command_palette_panel_entries, command_palette_preset_entries, command_palette_workspace_entries,
    detached_workspace_ids,
};

/// Returns the command `Ctrl+Shift+F` should trigger given the focused panel
/// kind. When the File Explorer is focused it maps to a content search across
/// files; otherwise it keeps the existing terminal search toggle.
fn search_shortcut_command(focused_kind: Option<PanelKind>) -> CommandId {
    match focused_kind {
        Some(PanelKind::FileExplorer) => CommandId::SearchFileContents,
        _ => CommandId::ToggleSearch,
    }
}

impl HorizonApp {
    pub(in crate::app) fn open_command_palette(&mut self) {
        self.command_palette = Some(CommandPalette::new());
    }

    fn toggle_command_palette(&mut self) {
        self.command_palette = if self.command_palette.is_some() {
            None
        } else {
            Some(CommandPalette::new())
        };
    }

    /// Open (or re-focus) the content-search panel on the focused File Explorer.
    /// No-op if no panel is focused or the focused panel is not an explorer
    /// (the Ctrl+Shift+F dispatch already gates this, but stay defensive).
    fn open_explorer_search(&mut self) {
        let Some(id) = self.board.focused else {
            return;
        };
        let Some(panel) = self.board.panel_mut(id) else {
            return;
        };
        if let Some(state) = panel.content.file_explorer_mut() {
            state.open_search();
        }
    }

    pub(in crate::app) fn render_command_palette(&mut self, ctx: &Context) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };

        let detached_workspace_ids = detached_workspace_ids(&self.board, &self.detached_workspaces);
        let workspace_entries =
            command_palette_workspace_entries(&self.board, &detached_workspace_ids, self.board.active_workspace);
        let panel_entries = command_palette_panel_entries(&self.board, &detached_workspace_ids);
        let preset_entries = command_palette_preset_entries(&self.presets);

        let action = palette.show(
            ctx,
            &workspace_entries,
            &panel_entries,
            &preset_entries,
            &self.action_commands_cache,
        );
        match action {
            PaletteAction::None => {}
            PaletteAction::Cancelled => self.command_palette = None,
            PaletteAction::Execute(cmd) => {
                self.command_palette = None;
                self.execute_command(ctx, &cmd);
            }
        }
    }

    pub(in crate::app) fn execute_command(&mut self, ctx: &Context, cmd: &CommandId) {
        match *cmd {
            CommandId::SwitchWorkspace(workspace_id) => {
                let _ = self.focus_workspace_visible(ctx, workspace_id, true);
            }
            CommandId::FocusPanel(panel_id) => {
                self.board.focus(panel_id);
                if let Some(workspace_id) = self.board.panel(panel_id).map(|panel| panel.workspace_id)
                    && let Some((min, max)) = self.board.workspace_bounds(workspace_id)
                {
                    self.focus_workspace_bounds(ctx, min, max, true);
                }
            }
            CommandId::FocusPanelDirection(direction) => {
                self.focus_panel_in_direction(ctx, direction);
            }
            CommandId::FocusActiveWorkspace => {
                let _ = self.focus_active_workspace(ctx, false);
            }
            CommandId::FitActiveWorkspace => {
                let _ = self.fit_active_workspace(ctx);
            }
            CommandId::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            CommandId::ToggleHud => self.hud_visible = !self.hud_visible,
            CommandId::ToggleMinimap => self.minimap_visible = !self.minimap_visible,
            CommandId::ToggleFullscreenWindow => {
                let is_fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen));
            }
            CommandId::ToggleFullscreenPanel => {
                self.fullscreen_panel = if self.fullscreen_panel.is_some() {
                    None
                } else {
                    self.board.focused
                };
            }
            CommandId::ZoomReset => {
                let canvas_rect = self.canvas_rect(ctx);
                let _ = self.zoom_reset(canvas_rect, canvas_rect.center());
            }
            CommandId::ZoomIn => {
                let canvas_rect = self.canvas_rect(ctx);
                let _ = self.zoom_canvas_at(canvas_rect, canvas_rect.center(), self.canvas_view.zoom * 1.1);
            }
            CommandId::ZoomOut => {
                let canvas_rect = self.canvas_rect(ctx);
                let _ = self.zoom_canvas_at(canvas_rect, canvas_rect.center(), self.canvas_view.zoom / 1.1);
            }
            CommandId::AlignWorkspacesHorizontally => {
                if let Some(workspace_id) = align_attached_workspaces(&mut self.board, &self.detached_workspaces)
                    && let Some((min, max)) = self.board.workspace_bounds(workspace_id)
                {
                    self.focus_workspace_bounds(ctx, min, max, true);
                    self.mark_runtime_dirty();
                }
            }
            CommandId::NewPanel => {
                let workspace_id = self.ensure_workspace_visible(ctx);
                if let Some(preset) = self.presets.first().cloned() {
                    self.add_panel_to_workspace(workspace_id, preset, None);
                } else {
                    self.create_panel(ctx);
                }
            }
            CommandId::OpenRemoteHosts => self.toggle_remote_hosts_overlay(ctx),
            CommandId::ToggleSessions => self.toggle_session_manager(),
            CommandId::CreatePanelFromPreset(index) => {
                if let Some(preset) = self.presets.get(index).cloned() {
                    let workspace_id = self
                        .board
                        .active_workspace
                        .unwrap_or_else(|| self.ensure_workspace_visible(ctx));
                    self.add_panel_to_workspace(workspace_id, preset, None);
                }
            }
            CommandId::ToggleSettings => self.toggle_settings(),
            CommandId::ToggleSearch => {
                // Focus the toolbar search input (or create it with focus
                // if it doesn't exist yet).
                if let Some(overlay) = &mut self.search_overlay {
                    overlay.focus();
                } else {
                    self.search_overlay = Some(SearchOverlay::new());
                }
            }
            CommandId::SearchFileContents => self.open_explorer_search(),
            CommandId::ToggleScrollPan => {
                self.scroll_pans_over_panels = !self.scroll_pans_over_panels;
                tracing::info!(
                    "scroll-pan over panels: {}",
                    if self.scroll_pans_over_panels { "on" } else { "off" }
                );
            }
        }
    }

    /// Move focus to the nearest panel in `direction` within the focused
    /// panel's workspace. This only moves focus: the current zoom and pan are
    /// left unchanged (no auto-fit/zoom). No-op when there is no focused panel
    /// or no neighbor in that direction.
    fn focus_panel_in_direction(&mut self, ctx: &Context, direction: Direction) {
        let Some(current) = self.board.focused else {
            return;
        };
        let Some(target) = self.board.panel_in_direction(current, direction) else {
            return;
        };

        self.board.focus(target);
        // Deliver egui keyboard focus to the target terminal so the user can
        // type immediately, mirroring what a click does
        // (`interaction.body.request_focus()`). Without this the previously
        // focused terminal keeps egui focus and the target only receives input
        // after a click. The body Id is published each frame by the terminal
        // widget; if the target was never rendered as a terminal we leave focus
        // unchanged.
        if let Some(body_id) = crate::terminal_widget::terminal_focus_id(ctx, target) {
            ctx.memory_mut(|memory| memory.request_focus(body_id));
        }
    }

    pub(in crate::app) fn handle_shortcuts(&mut self, ctx: &Context) {
        let shortcut_bindings: &[(_, CommandId)] = &[
            (self.shortcuts.zoom_reset, CommandId::ZoomReset),
            (self.shortcuts.zoom_in, CommandId::ZoomIn),
            (self.shortcuts.zoom_out, CommandId::ZoomOut),
            (self.shortcuts.focus_active_workspace, CommandId::FocusActiveWorkspace),
            (self.shortcuts.fit_active_workspace, CommandId::FitActiveWorkspace),
            (
                self.shortcuts.focus_panel_left,
                CommandId::FocusPanelDirection(Direction::Left),
            ),
            (
                self.shortcuts.focus_panel_right,
                CommandId::FocusPanelDirection(Direction::Right),
            ),
            (
                self.shortcuts.focus_panel_up,
                CommandId::FocusPanelDirection(Direction::Up),
            ),
            (
                self.shortcuts.focus_panel_down,
                CommandId::FocusPanelDirection(Direction::Down),
            ),
            (
                self.shortcuts.align_workspaces_horizontally,
                CommandId::AlignWorkspacesHorizontally,
            ),
            (self.shortcuts.toggle_settings, CommandId::ToggleSettings),
            (self.shortcuts.toggle_sidebar, CommandId::ToggleSidebar),
            (self.shortcuts.toggle_hud, CommandId::ToggleHud),
            (self.shortcuts.toggle_minimap, CommandId::ToggleMinimap),
            (self.shortcuts.open_remote_hosts, CommandId::OpenRemoteHosts),
            (self.shortcuts.toggle_sessions, CommandId::ToggleSessions),
            (self.shortcuts.new_terminal, CommandId::NewPanel),
            (self.shortcuts.toggle_scroll_pan, CommandId::ToggleScrollPan),
        ];

        // The search shortcut (Ctrl+Shift+F) is handled separately so it can be
        // contextual: explorer focused -> content search, otherwise the terminal
        // search toggle. Every other binding keeps its existing behavior.
        let search_binding = self.shortcuts.search;
        let (toggle_palette, search_pressed, triggered_command) = ctx.input(|input| {
            let palette = shortcut_pressed(input, self.shortcuts.command_palette);
            let search = shortcut_pressed(input, search_binding);
            let command = shortcut_bindings
                .iter()
                .find(|(binding, _)| shortcut_pressed(input, *binding))
                .map(|(_, id)| id.clone());
            (palette, search, command)
        });

        if toggle_palette {
            self.toggle_command_palette();
        }
        if search_pressed {
            let focused_kind = self
                .board
                .focused
                .and_then(|id| self.board.panel(id))
                .map(|panel| panel.kind);
            let command_id = search_shortcut_command(focused_kind);
            self.execute_command(ctx, &command_id);
        }
        if let Some(command_id) = triggered_command {
            self.execute_command(ctx, &command_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::PanelKind;

    use super::{CommandId, search_shortcut_command};

    #[test]
    fn search_shortcut_maps_explorer_to_content_search() {
        assert_eq!(
            search_shortcut_command(Some(PanelKind::FileExplorer)),
            CommandId::SearchFileContents
        );
    }

    #[test]
    fn search_shortcut_maps_editor_to_terminal_search() {
        assert_eq!(
            search_shortcut_command(Some(PanelKind::Editor)),
            CommandId::ToggleSearch
        );
    }

    #[test]
    fn search_shortcut_maps_terminal_to_terminal_search() {
        assert_eq!(
            search_shortcut_command(Some(PanelKind::Shell)),
            CommandId::ToggleSearch
        );
    }

    #[test]
    fn search_shortcut_maps_none_to_terminal_search() {
        assert_eq!(search_shortcut_command(None), CommandId::ToggleSearch);
    }
}
