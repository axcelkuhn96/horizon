mod context_menu;
mod ime;
mod input;
mod layout;
mod render;
mod scrollbar;

use std::path::Path;

use egui::{Context, FontId, Id, Vec2};
use horizon_core::{Panel, PanelId};

use crate::theme;

use alacritty_terminal::term::TermMode;

use self::context_menu::{apply_terminal_context_action, show_terminal_context_menu};
use self::ime::{clear_terminal_ime_state, publish_terminal_ime_output};
pub(crate) use self::input::SSH_RECONNECT_SHORTCUT;
pub(crate) use self::input::TerminalSelectionDragState;
use self::input::{
    PointerSupport, handle_terminal_keyboard_input, handle_terminal_pointer_input, pty_mouse_reporting_enabled,
};
use self::layout::{GridMetrics, terminal_interaction, terminal_layout, terminal_viewport_size};
pub(crate) use self::render::TerminalGridCache;
use self::render::{render_cursor, render_grid};
use self::scrollbar::render_scrollbar;
use super::primary_selection::PrimarySelection;

/// Whether to draw the host scrollbar for a terminal in this mode. A full-screen
/// TUI in alt-screen (e.g. claude code) has no host scrollback — `history_size()`
/// is always 0 there — so the scrollbar would render as a useless full-height
/// pill. Hide it in alt-screen; a normal shell with scrollback keeps it.
#[must_use]
fn terminal_scrollbar_visible(mode: TermMode) -> bool {
    !mode.contains(TermMode::ALT_SCREEN)
}

const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT_FACTOR: f32 = 1.3;

/// Stable, controller-reachable egui [`Id`] under which a terminal panel
/// publishes the egui focus [`Id`] of its body widget each frame. The body
/// widget's own [`Id`] is derived from the surrounding [`egui::Ui`] id stack at
/// render time and cannot be reconstructed outside the render closure, so the
/// widget stores it here and directional-focus navigation reads it back to
/// deliver keyboard focus the same way a click does. See
/// [`store_terminal_focus_id`] / [`terminal_focus_id`].
pub(crate) fn terminal_focus_request_id(panel_id: PanelId) -> Id {
    Id::new(("terminal_focus_request", panel_id.0))
}

/// Records the egui focus [`Id`] of `panel_id`'s terminal body so directional
/// navigation can request focus on it without a prior click.
fn store_terminal_focus_id(ctx: &Context, panel_id: PanelId, body_id: Id) {
    ctx.data_mut(|data| data.insert_temp(terminal_focus_request_id(panel_id), body_id));
}

/// Looks up the egui focus [`Id`] last published for `panel_id`'s terminal body.
/// Returns `None` if the panel has not been rendered as a terminal this session
/// (e.g. it was never on screen) — callers then leave focus unchanged.
pub(crate) fn terminal_focus_id(ctx: &Context, panel_id: PanelId) -> Option<Id> {
    ctx.data(|data| data.get_temp::<Id>(terminal_focus_request_id(panel_id)))
}

const fn hover_requires_grid_refresh(body_hovered: bool, mouse_reporting_enabled: bool) -> bool {
    body_hovered && mouse_reporting_enabled
}

const fn grid_cache_allowed(content_requires_refresh: bool, hover_requires_refresh: bool, drag_active: bool) -> bool {
    !(content_requires_refresh || hover_requires_refresh || drag_active)
}

pub struct TerminalView<'a> {
    panel: &'a mut Panel,
    grid_cache: Option<&'a mut TerminalGridCache>,
}

pub struct TerminalKeyboardContext<'a> {
    pub keyboard_events: &'a [super::input::TerminalInputEvent],
    pub primary_selection: &'a PrimarySelection,
    pub local_ssh_reconnect_enabled: bool,
    pub reconnect_requested: &'a mut bool,
    /// Directory under which a pasted clipboard image is saved as a temp PNG so
    /// the right-click "Paste Image" action can feed its file path into the PTY.
    pub pasted_dir: &'a Path,
}

impl<'a> TerminalView<'a> {
    pub fn new(panel: &'a mut Panel, grid_cache: Option<&'a mut TerminalGridCache>) -> Self {
        Self { panel, grid_cache }
    }

    /// Renders the terminal panel. Returns `true` if clicked (for focus tracking).
    #[profiling::function]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        is_active_panel: bool,
        interactive: bool,
        selection_drag: &mut TerminalSelectionDragState,
        keyboard: TerminalKeyboardContext<'_>,
    ) -> bool {
        let metrics = grid_metrics(ui.ctx());
        let char_width = metrics.char_width;
        let line_height = metrics.line_height;
        let layout = terminal_layout(ui.available_size(), char_width, line_height);
        let viewport = terminal_viewport_size(ui.available_size(), char_width, line_height);
        let new_cols = viewport.cols;
        let new_rows = viewport.rows;

        self.panel
            .resize(new_rows, new_cols, viewport.cell_width, viewport.cell_height);

        // Alt-screen TUIs (e.g. claude code) hide the scrollbar; compute this
        // once (one term-lock) and reuse it both to make the rect
        // non-interactive (so the invisible zone can't be clicked/dragged) and
        // to gate the paint below.
        let scrollbar_visible = self
            .panel
            .terminal()
            .is_none_or(|terminal| terminal_scrollbar_visible(terminal.mode()));
        let interaction = terminal_interaction(ui, layout, self.panel.id.0, interactive, scrollbar_visible);
        // Publish the body's egui focus Id so directional-focus navigation can
        // deliver keyboard focus the same way a click does (mirror of the
        // `interaction.body.request_focus()` call in the pointer handler).
        store_terminal_focus_id(ui.ctx(), self.panel.id, interaction.body.id);
        if interactive {
            handle_terminal_pointer_input(
                ui,
                self.panel,
                &interaction,
                is_active_panel,
                PointerSupport {
                    metrics: &metrics,
                    visible_rows: new_rows,
                    visible_cols: new_cols,
                    primary_selection: keyboard.primary_selection,
                    selection_drag,
                },
            );
        }
        let window_focused = ui.input(|input| input.viewport().focused.unwrap_or(true));
        let other_widget_has_focus = ui
            .memory(egui::Memory::focused)
            .is_some_and(|focused| focused != interaction.body.id);
        let has_terminal_focus =
            window_focused && (interaction.body.has_focus() || (is_active_panel && !other_widget_has_focus));
        self.panel.set_focused(has_terminal_focus);

        if interactive && has_terminal_focus {
            ui.memory_mut(|mem| {
                mem.set_focus_lock_filter(
                    interaction.body.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
            publish_terminal_ime_output(ui, self.panel, &interaction, &metrics);
        } else {
            clear_terminal_ime_state(ui, interaction.body.id);
        }

        let had_recent_output = self.panel.had_recent_output();
        let body_hovered = interaction.body.hovered();
        let modifiers = body_hovered.then(|| ui.input(|input| input.modifiers));
        let drag_active = interaction.body.dragged() || interaction.scrollbar.dragged();

        if ui.is_rect_visible(interaction.layout.outer)
            && let Some(terminal) = self.panel.terminal_mut()
        {
            let history_size = terminal.history_size();
            let scrollbar_highlighted = interaction.scrollbar.hovered() || interaction.scrollbar.dragged();
            let mut grid_cache = self.grid_cache.take();
            terminal.with_renderable_content(|content| {
                let cursor = content.cursor;
                let display_offset = content.display_offset;
                let mouse_reporting_enabled =
                    modifiers.is_some_and(|modifiers| pty_mouse_reporting_enabled(content.mode, modifiers));
                let hover_requires_refresh = hover_requires_grid_refresh(body_hovered, mouse_reporting_enabled);
                let content_requires_refresh = had_recent_output || content.selection.is_some();
                let allow_grid_cache =
                    grid_cache_allowed(content_requires_refresh, hover_requires_refresh, drag_active);
                render_grid(
                    ui,
                    interaction.layout.body,
                    content,
                    &metrics,
                    grid_cache.as_deref_mut(),
                    allow_grid_cache,
                );
                render_cursor(
                    ui,
                    interaction.layout.body,
                    cursor,
                    display_offset,
                    &metrics,
                    has_terminal_focus,
                );
                if scrollbar_visible {
                    render_scrollbar(
                        ui,
                        interaction.layout.scrollbar,
                        display_offset,
                        usize::from(new_rows),
                        history_size,
                        scrollbar_highlighted,
                    );
                }
            });
            self.grid_cache = grid_cache;
        }

        if self.panel.process_exited() && ui.is_rect_visible(interaction.layout.body) {
            render_process_exited_banner(ui, interaction.layout.body);
        }

        if interactive && has_terminal_focus {
            *keyboard.reconnect_requested |= handle_terminal_keyboard_input(
                ui,
                interaction.body.id,
                self.panel,
                keyboard.keyboard_events,
                keyboard.primary_selection,
                keyboard.local_ssh_reconnect_enabled,
            );
        }

        if interactive {
            self.show_terminal_context_menu(ui, &interaction, keyboard.primary_selection, keyboard.pasted_dir);
        }

        interaction.body.clicked()
    }

    /// Offer the right-click context menu (Copy / Paste / Paste Image) on the
    /// terminal body. The menu is suppressed while the PTY is in mouse-reporting
    /// mode (so apps that consume right-clicks keep receiving them), matching how
    /// the pointer handler already gates local interactions; holding Shift, which
    /// bypasses mouse reporting, re-enables the menu.
    fn show_terminal_context_menu(
        &mut self,
        ui: &egui::Ui,
        interaction: &self::layout::TerminalInteraction,
        primary_selection: &PrimarySelection,
        pasted_dir: &Path,
    ) {
        if self.context_menu_suppressed_by_mouse_reporting(ui) {
            return;
        }

        let has_selection = self.panel.terminal().is_some_and(horizon_core::Terminal::has_selection);

        let mut chosen = None;
        interaction.body.context_menu(|ui| {
            chosen = show_terminal_context_menu(ui, has_selection);
        });

        if let Some(action) = chosen {
            apply_terminal_context_action(ui, self.panel, primary_selection, pasted_dir, &action);
        }
    }

    /// Whether right-click should be left to the PTY (mouse reporting on, no
    /// Shift) instead of opening the context menu.
    fn context_menu_suppressed_by_mouse_reporting(&self, ui: &egui::Ui) -> bool {
        let shift_held = ui.input(|input| input.modifiers.shift);
        if shift_held {
            return false;
        }
        self.panel.terminal().is_some_and(|terminal| {
            terminal
                .mode()
                .intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)
        })
    }
}

fn grid_metrics(ctx: &Context) -> GridMetrics {
    let font_id = FontId::monospace(FONT_SIZE);
    let char_width = ctx.fonts_mut(|fonts| fonts.glyph_width(&font_id, 'M'));
    let line_height = FONT_SIZE * LINE_HEIGHT_FACTOR;

    GridMetrics {
        char_width,
        line_height,
        font_id,
    }
}

/// Non-modal footer strip painted over a dead panel's body so the exited
/// state is obvious at a glance (the titlebar badge alone is easy to miss).
fn render_process_exited_banner(ui: &egui::Ui, body: egui::Rect) {
    use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Stroke};

    const BANNER_HEIGHT: f32 = 24.0;
    let banner = Rect::from_min_max(
        Pos2::new(body.min.x, (body.max.y - BANNER_HEIGHT).max(body.min.y)),
        body.max,
    );
    let painter = ui.painter().with_clip_rect(body);
    let red = theme::PALETTE_RED();
    painter.rect_filled(
        banner,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(red.r() / 5, red.g() / 5, red.b() / 5, 230),
    );
    painter.line_segment(
        [banner.left_top(), banner.right_top()],
        Stroke::new(1.0, theme::alpha(red, 160)),
    );
    painter.text(
        banner.center(),
        Align2::CENTER_CENTER,
        "Process exited — Ctrl+Shift+R to restart",
        FontId::proportional(11.0),
        theme::alpha(theme::FG(), 230),
    );
}

pub(crate) fn viewport_for_available_space(ctx: &Context, available: Vec2) -> layout::TerminalViewportSize {
    let metrics = grid_metrics(ctx);
    terminal_viewport_size(available, metrics.char_width, metrics.line_height)
}

#[cfg(test)]
mod tests {
    use super::{
        grid_cache_allowed, hover_requires_grid_refresh, terminal_focus_request_id, terminal_scrollbar_visible,
    };
    use alacritty_terminal::term::TermMode;
    use horizon_core::PanelId;

    #[test]
    fn scrollbar_hidden_in_alt_screen() {
        assert!(!terminal_scrollbar_visible(TermMode::ALT_SCREEN));
        // Alt-screen combined with other flags is still hidden.
        assert!(!terminal_scrollbar_visible(
            TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK
        ));
    }

    #[test]
    fn scrollbar_shown_in_normal_screen() {
        assert!(terminal_scrollbar_visible(TermMode::NONE));
        assert!(terminal_scrollbar_visible(TermMode::APP_CURSOR));
    }

    #[test]
    fn focus_request_id_is_stable_per_panel() {
        assert_eq!(
            terminal_focus_request_id(PanelId(7)),
            terminal_focus_request_id(PanelId(7))
        );
    }

    #[test]
    fn focus_request_id_differs_across_panels() {
        assert_ne!(
            terminal_focus_request_id(PanelId(1)),
            terminal_focus_request_id(PanelId(2))
        );
    }

    #[test]
    fn mouse_reporting_hover_bypasses_grid_cache() {
        assert!(hover_requires_grid_refresh(true, true));
        assert!(!grid_cache_allowed(false, true, false));
    }

    #[test]
    fn normal_terminal_hover_keeps_grid_cache() {
        assert!(!hover_requires_grid_refresh(true, false));
        assert!(!hover_requires_grid_refresh(false, true));
        assert!(grid_cache_allowed(false, false, false));
    }

    #[test]
    fn dynamic_content_and_drags_bypass_grid_cache() {
        assert!(!grid_cache_allowed(true, false, false));
        assert!(!grid_cache_allowed(false, false, true));
    }
}
