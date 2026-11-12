mod auto_hide;
mod git_overview;
mod header;
mod labels;
mod menu;
mod routing;
mod rows;
mod toolbar;
mod workspace_menu;
mod workspace_rename;

use header::{paint_workspace_row_bg, render_sidebar_workspace_header_row};
use labels::{LabelInputs, ProfileRef, claude_profile_for};
use routing::{
    should_focus_workspace_on_row_click, sidebar_workspace_drop_should_dock, sidebar_workspace_insert_dock_side,
};
use rows::{GROUP_GAP, GitChipPlacement, HEADER_HEIGHT, RowIcon, SidebarPanelEntry, git_chip_placement};

use std::collections::HashMap;
use std::time::Instant;

use egui::{
    Align, Button, Color32, Context, CornerRadius, CursorIcon, Id, Layout, Order, Pos2, Rect, Sense, Stroke, UiBuilder,
    Vec2,
};
use horizon_core::{AttentionSeverity, PanelId, PanelKind, WorkspaceId, WorkspaceLayout};

use crate::theme;

use super::root_chrome::effective_sidebar_width;
use super::util;
use super::{HorizonApp, TOOLBAR_HEIGHT, WS_BG_PAD, WS_TITLE_HEIGHT};

pub(super) use auto_hide::SidebarAutoHideState;
use auto_hide::{
    SIDEBAR_AUTO_HIDE_DELAY, SIDEBAR_STRIP_HOVER_WIDTH, SIDEBAR_STRIP_WIDTH, SidebarReveal, reserved_sidebar_width,
    sidebar_reveal_state,
};
pub(super) use git_overview::GitSidebarState;
pub(super) use labels::PanelLabelCache;
use workspace_menu::{WorkspaceMenuAction, WorkspaceMenuEnv};

struct WorkspaceSidebarEntry {
    id: WorkspaceId,
    name: String,
    color: Color32,
    is_active: bool,
    detached: bool,
    panels: Vec<SidebarPanelEntry>,
    attention_count: usize,
    sidebar_collapsed: bool,
}

#[derive(Clone, Default)]
struct SidebarActions {
    create_workspace: bool,
    fit_active_workspace: bool,
    workspace_drop: Option<SidebarWorkspaceDropAction>,
    focus_panel: Option<PanelId>,
    pan_to_panel: Option<PanelId>,
    pan_to_workspace: Option<WorkspaceId>,
    detach_workspace: Option<WorkspaceId>,
    reattach_workspace: Option<WorkspaceId>,
    close_panel: Option<PanelId>,
    close_all_in_workspace: Option<WorkspaceId>,
    clear_layout: Option<WorkspaceId>,
    arrange_layout: Option<(WorkspaceId, WorkspaceLayout)>,
    toggle_sidebar_collapse: Option<WorkspaceId>,
    workspace_menu: Option<WorkspaceMenuAction>,
    git_action: Option<git_overview::GitAction>,
}

#[derive(Clone, Copy)]
pub(super) enum SidebarWorkspaceInsert {
    Before,
    After,
}

#[derive(Clone, Copy)]
struct SidebarWorkspaceDropAction {
    dragged_workspace_id: WorkspaceId,
    target_workspace_id: WorkspaceId,
    insert: SidebarWorkspaceInsert,
}

#[derive(Default)]
struct SidebarWorkspaceDragState {
    active_this_frame: bool,
    drop_requested: bool,
    drop_action: Option<SidebarWorkspaceDropAction>,
}

impl HorizonApp {
    fn has_attached_workspace(&self) -> bool {
        self.board
            .workspaces
            .iter()
            .any(|workspace| !self.workspace_is_detached(workspace.id))
    }

    /// Horizontal space the sidebar reserves from the canvas on the left edge,
    /// for the given viewport width.
    ///
    /// When auto-hide is active and the sidebar is collapsed to its thin strip,
    /// only the strip width is reserved so the freed band becomes usable canvas.
    /// Otherwise the full effective sidebar width is reserved (unchanged layout).
    pub(in crate::app) fn reserved_sidebar_width(&self, viewport_width: f32) -> f32 {
        reserved_sidebar_width(
            self.sidebar_visible,
            self.template_config.overlays.sidebar_auto_hide,
            self.sidebar_auto_hide.revealed,
            effective_sidebar_width(viewport_width),
        )
    }

    pub(super) fn render_sidebar(&mut self, ctx: &Context) {
        self.render_workspace_menu_overlays(ctx);
        if !self.sidebar_visible {
            return;
        }

        let viewport = util::viewport_local_rect(ctx);
        let sidebar_origin = Pos2::new(viewport.min.x, viewport.min.y + TOOLBAR_HEIGHT);
        let sidebar_width = effective_sidebar_width(viewport.width());
        let sidebar_height = viewport.height() - TOOLBAR_HEIGHT;
        let auto_hide = self.template_config.overlays.sidebar_auto_hide;

        // When auto-hide is off, preserve the original full-width behavior exactly.
        if !auto_hide {
            self.render_sidebar_full(
                ctx,
                sidebar_origin,
                Vec2::new(sidebar_width, sidebar_height),
                sidebar_width,
            );
            return;
        }

        let full_rect = Rect::from_min_size(sidebar_origin, Vec2::new(sidebar_width, sidebar_height));
        let strip_hover_rect =
            Rect::from_min_size(sidebar_origin, Vec2::new(SIDEBAR_STRIP_HOVER_WIDTH, sidebar_height));

        // The region that counts as "over the sidebar" depends on what we drew last
        // frame: the full rect while revealed, otherwise only the thin edge strip.
        let hover_rect = if self.sidebar_revealed_last_frame() {
            full_rect
        } else {
            strip_hover_rect
        };

        let pointer_over = ctx.pointer_interact_pos().is_some_and(|pos| hover_rect.contains(pos))
            || self.workspace_rename_needs_sidebar();

        let now = Instant::now();
        if pointer_over {
            self.sidebar_auto_hide.last_hover = Some(now);
        }
        let since_left = self.sidebar_auto_hide.last_hover.map(|t| now.duration_since(t));
        let reveal = sidebar_reveal_state(true, pointer_over, since_left, SIDEBAR_AUTO_HIDE_DELAY);
        self.sidebar_auto_hide.revealed = matches!(reveal, SidebarReveal::FullVisible);

        match reveal {
            SidebarReveal::FullVisible => {
                self.render_sidebar_full(
                    ctx,
                    sidebar_origin,
                    Vec2::new(sidebar_width, sidebar_height),
                    sidebar_width,
                );
                // Re-check the hide timer once the grace period would elapse, but only
                // while the pointer is away (so no continuous repaint while hovering).
                if !pointer_over && let Some(elapsed) = since_left {
                    let remaining = SIDEBAR_AUTO_HIDE_DELAY.saturating_sub(elapsed);
                    ctx.request_repaint_after(remaining);
                }
            }
            SidebarReveal::ThinStrip => {
                Self::render_sidebar_strip(ctx, sidebar_origin, sidebar_height);
            }
        }
    }

    /// Whether the sidebar was rendered at full width on the previous frame.
    fn sidebar_revealed_last_frame(&self) -> bool {
        self.sidebar_auto_hide.revealed
    }

    fn render_sidebar_full(&mut self, ctx: &Context, sidebar_origin: Pos2, sidebar_size: Vec2, sidebar_width: f32) {
        let workspace_data = self.sidebar_workspace_data();
        let mut actions = SidebarActions::default();

        egui::Area::new(Id::new("sidebar"))
            .fixed_pos(sidebar_origin)
            .constrain(false)
            .order(Order::Tooltip)
            .show(ctx, |ui| {
                Self::paint_sidebar_frame(ui, sidebar_origin, sidebar_size, sidebar_width);
                self.render_sidebar_contents(ui, &workspace_data, &mut actions);
            });

        self.apply_sidebar_actions(ctx, &actions);
    }

    /// Render the collapsed auto-hide handle: a thin, theme-consistent strip on the
    /// left edge that reads as a deliberate affordance to reveal the sidebar.
    fn render_sidebar_strip(ctx: &Context, sidebar_origin: Pos2, sidebar_height: f32) {
        let strip_size = Vec2::new(SIDEBAR_STRIP_WIDTH, sidebar_height);
        egui::Area::new(Id::new("sidebar_strip"))
            .fixed_pos(sidebar_origin)
            .constrain(false)
            .order(Order::Tooltip)
            .show(ctx, |ui| {
                let strip_rect = Rect::from_min_size(sidebar_origin, strip_size);
                let hovered = ui.rect_contains_pointer(strip_rect);
                let painter = ui.painter_at(strip_rect);
                painter.rect_filled(strip_rect, CornerRadius::ZERO, theme::BG_ELEVATED());
                // Accent grab-handle; brightens on hover to read as a reveal affordance.
                let handle_height = (sidebar_height * 0.18).clamp(28.0, 96.0);
                let handle_rect = Rect::from_center_size(
                    Pos2::new(strip_rect.center().x, strip_rect.center().y),
                    Vec2::new(SIDEBAR_STRIP_WIDTH - 2.0, handle_height),
                );
                let handle = theme::alpha(theme::ACCENT(), if hovered { 230 } else { 170 });
                painter.rect_filled(handle_rect, CornerRadius::same(2), handle);
                // Two short grip notches centered on the handle so it reads as a
                // grab affordance (like a drawer pull) rather than a plain bar.
                let grip_color = theme::alpha(theme::BG_ELEVATED(), if hovered { 235 } else { 200 });
                let grip_half_w = (SIDEBAR_STRIP_WIDTH - 2.0) * 0.28;
                for dy in [-3.0_f32, 3.0_f32] {
                    let gy = handle_rect.center().y + dy;
                    painter.line_segment(
                        [
                            Pos2::new(handle_rect.center().x - grip_half_w, gy),
                            Pos2::new(handle_rect.center().x + grip_half_w, gy),
                        ],
                        Stroke::new(1.0_f32, grip_color),
                    );
                }
                let edge_top = Pos2::new(strip_rect.max.x, strip_rect.min.y);
                let edge_bottom = Pos2::new(strip_rect.max.x, strip_rect.max.y);
                painter.line_segment([edge_top, edge_bottom], Stroke::new(1.0_f32, theme::BORDER_SUBTLE()));
                ui.advance_cursor_after_rect(strip_rect);
                if hovered {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
            });
    }

    fn sidebar_workspace_data(&mut self) -> Vec<WorkspaceSidebarEntry> {
        let attention_enabled = self.template_config.features.attention_feed;
        let panel_data = self.sidebar_panel_entries(attention_enabled);

        self.board
            .workspaces
            .iter()
            .map(|workspace| {
                let (r, g, b) = workspace.accent();
                let panels = workspace
                    .panels
                    .iter()
                    .filter_map(|panel_id| panel_data.get(panel_id).cloned())
                    .collect::<Vec<_>>();
                let attention_count = panels.iter().filter(|panel| panel.attention.is_some()).count();

                WorkspaceSidebarEntry {
                    id: workspace.id,
                    name: workspace.name.clone(),
                    color: Color32::from_rgb(r, g, b),
                    is_active: self.board.active_workspace == Some(workspace.id),
                    detached: self.workspace_is_detached(workspace.id),
                    panels,
                    attention_count,
                    sidebar_collapsed: workspace.sidebar_collapsed,
                }
            })
            .collect()
    }

    /// Rows of every panel. Labels and hints come from the label cache, which
    /// rebuilds a panel's strings only when its title, name, kind or profile changed.
    fn sidebar_panel_entries(&mut self, attention_enabled: bool) -> HashMap<PanelId, SidebarPanelEntry> {
        let board = &self.board;
        let hub = &mut self.hub;
        let presets = &self.presets;
        hub.sidebar_labels.begin_frame();
        let entries = board
            .panels
            .iter()
            .map(|panel| {
                let attention = if attention_enabled {
                    board.unresolved_attention_for_panel(panel.id).cloned()
                } else {
                    None
                };
                let profile = claude_profile_for(panel.kind, &panel.env, presets);
                let inputs = LabelInputs {
                    title: &panel.title,
                    terminal_title: &panel.terminal_title,
                    custom_name: panel.has_custom_name(),
                    kind: panel.kind,
                    profile,
                    launch_command: panel.launch_command.as_deref(),
                };
                let (icon, seven_day) = match profile {
                    ProfileRef::Named(name) => (
                        RowIcon::Claude(profile_accent(hub, name)),
                        hub.usage.usage(name).and_then(|usage| usage.seven_day.percent),
                    ),
                    ProfileRef::Default => (RowIcon::Claude(claude_default_accent()), None),
                    ProfileRef::None => (RowIcon::Kind(panel.kind), None),
                };
                let label = hub.sidebar_labels.resolve(panel.id, &inputs, seven_day);
                (
                    panel.id,
                    SidebarPanelEntry {
                        id: panel.id,
                        label,
                        icon,
                        kind: panel.kind,
                        is_focused: board.focused == Some(panel.id),
                        attention,
                    },
                )
            })
            .collect();
        hub.sidebar_labels.end_frame();
        entries
    }

    fn paint_sidebar_frame(ui: &mut egui::Ui, sidebar_origin: Pos2, sidebar_size: Vec2, sidebar_width: f32) {
        ui.set_min_size(sidebar_size);
        ui.set_max_size(sidebar_size);
        ui.painter().rect_filled(
            Rect::from_min_size(sidebar_origin, sidebar_size),
            CornerRadius::ZERO,
            theme::BG_ELEVATED(),
        );
        ui.painter().line_segment(
            [
                Pos2::new(sidebar_origin.x + sidebar_width, sidebar_origin.y),
                Pos2::new(sidebar_origin.x + sidebar_width, sidebar_origin.y + sidebar_size.y),
            ],
            Stroke::new(1.0_f32, theme::BORDER_SUBTLE()),
        );
    }

    fn render_sidebar_contents(
        &mut self,
        ui: &mut egui::Ui,
        workspace_data: &[WorkspaceSidebarEntry],
        actions: &mut SidebarActions,
    ) {
        self.render_sidebar_header(ui, actions);
        ui.add_space(10.0);

        let available = ui.available_height();
        let mut drag_state = SidebarWorkspaceDragState::default();
        egui::ScrollArea::vertical()
            .max_height(available)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                // Rows carry their own height; the theme's 8 px row gap would double them.
                ui.spacing_mut().item_spacing.y = 0.0;

                for workspace in workspace_data {
                    self.render_sidebar_workspace(ui, workspace, workspace_data, actions, &mut drag_state);
                }
            });

        if drag_state.drop_requested {
            actions.workspace_drop = drag_state.drop_action;
            self.sidebar_drag_workspace = None;
        } else if !drag_state.active_this_frame && !ui.ctx().input(|input| input.pointer.primary_down()) {
            self.sidebar_drag_workspace = None;
        }

        ui.add_space(8.0);
    }

    fn render_sidebar_header(&mut self, ui: &mut egui::Ui, actions: &mut SidebarActions) {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            ui.label(
                egui::RichText::new("WORKSPACES")
                    .color(theme::FG_DIM())
                    .size(10.5)
                    .strong(),
            );
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                let fit = ui
                    .add_enabled(
                        self.has_attached_workspace(),
                        util::chrome_button("Fit").min_size(Vec2::new(42.0, 24.0)),
                    )
                    .on_hover_text(
                        self.shortcuts
                            .fit_active_workspace
                            .display_label(util::primary_shortcut_label()),
                    );
                if fit.clicked() {
                    actions.fit_active_workspace = true;
                }

                let new_workspace = ui
                    .add(util::chrome_button("New").min_size(Vec2::new(46.0, 24.0)))
                    .on_hover_text("Create a new workspace.");
                if new_workspace.clicked() {
                    actions.create_workspace = true;
                }
            });
        });
    }

    fn render_sidebar_workspace(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &WorkspaceSidebarEntry,
        workspace_data: &[WorkspaceSidebarEntry],
        actions: &mut SidebarActions,
        drag_state: &mut SidebarWorkspaceDragState,
    ) {
        let row_rect = ui.allocate_space(Vec2::new(ui.available_width(), HEADER_HEIGHT)).1;
        if self.workspace_rename_in_sidebar(workspace.id) {
            paint_workspace_row_bg(ui, row_rect, workspace.color, workspace.is_active, false, false);
            self.render_sidebar_rename_editor(ui, row_rect);
            self.render_sidebar_workspace_panels(ui, workspace, workspace_data, actions, false);
            return;
        }
        let mut click_target_hovered = ui.rect_contains_pointer(row_rect);
        let mut row_clicked = false;
        let mut caret_rect = Rect::NOTHING;
        let mut name_right = row_rect.min.x;
        let mut count_left = None;
        paint_workspace_row_bg(
            ui,
            row_rect,
            workspace.color,
            workspace.is_active,
            click_target_hovered,
            self.sidebar_drag_workspace == Some(workspace.id),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(row_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                let row = render_sidebar_workspace_header_row(ui, workspace);
                click_target_hovered |= row.hovered;
                row_clicked |= row.row_clicked;
                caret_rect = row.caret_rect;
                name_right = row.name_right;
                count_left = row.count_left;
            },
        );
        if workspace.sidebar_collapsed {
            let dot_x = count_left.map_or(row_rect.max.x - 18.0, |left| left - 10.0);
            git_overview::paint_collapsed_dirty_dot(ui, &self.hub.git, workspace.id, row_rect, dot_x);
        }

        let row_response = ui.interact(
            row_rect,
            ui.make_persistent_id(("sidebar_ws_click", workspace.id.0)),
            Sense::click_and_drag(),
        );
        click_target_hovered |= row_response.hovered();
        row_clicked |= row_response.clicked();
        self.handle_sidebar_workspace_drag(ui, workspace, row_rect, &row_response, drag_state, click_target_hovered);

        // Interact with the caret AFTER the full-row interaction so it sits on
        // top and wins overlapping clicks. Expand the hit-rect for comfort.
        let caret_hit_rect = caret_rect.expand(4.0);
        let caret_response = ui.interact(
            caret_hit_rect,
            ui.make_persistent_id(("sidebar_ws_caret", workspace.id.0)),
            Sense::click(),
        );
        let caret_hit = caret_response.clicked();
        if caret_response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if caret_hit {
            actions.toggle_sidebar_collapse = Some(workspace.id);
        }

        if should_focus_workspace_on_row_click(row_clicked, caret_hit) {
            if workspace.panels.len() == 1 {
                actions.focus_panel = Some(workspace.panels[0].id);
                actions.pan_to_panel = Some(workspace.panels[0].id);
            } else {
                actions.pan_to_workspace = Some(workspace.id);
            }
        }
        let menu_env = WorkspaceMenuEnv {
            presets: &self.presets,
            template_target: self.workspace_template_target(workspace.id),
            can_delete: self.board.workspaces.len() > 1,
        };
        Self::show_workspace_context_menu(&row_response, workspace, &menu_env, actions);

        // The branch chip sits on the header row when it fits next to the name,
        // otherwise it stays the first child row. Painted after the row
        // interaction so its hover card is not shadowed by the row.
        let chip_in_header = !workspace.sidebar_collapsed
            && git_chip_placement(
                row_rect,
                name_right,
                git_overview::header_chip_width(ui, &self.hub.git, workspace.id),
            ) == GitChipPlacement::Header;
        if chip_in_header {
            git_overview::paint_header_chip(ui, &self.hub.git, workspace.id, row_rect);
        }

        self.render_sidebar_workspace_panels(ui, workspace, workspace_data, actions, chip_in_header);
    }

    fn render_sidebar_workspace_panels(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &WorkspaceSidebarEntry,
        workspace_data: &[WorkspaceSidebarEntry],
        actions: &mut SidebarActions,
        git_line_in_header: bool,
    ) {
        if !workspace.sidebar_collapsed {
            if let Some(action) =
                git_overview::render_git_section(ui, &mut self.hub.git, &self.presets, workspace.id, git_line_in_header)
            {
                actions.git_action = Some(action);
            }
            for panel in &workspace.panels {
                self.render_sidebar_panel(ui, workspace, workspace_data, panel, actions);
            }
        }
        ui.add_space(GROUP_GAP);
    }

    fn handle_sidebar_workspace_drag(
        &mut self,
        ui: &egui::Ui,
        workspace: &WorkspaceSidebarEntry,
        row_rect: Rect,
        row_response: &egui::Response,
        drag_state: &mut SidebarWorkspaceDragState,
        click_target_hovered: bool,
    ) {
        if row_response.drag_started() || row_response.dragged() {
            self.sidebar_drag_workspace = Some(workspace.id);
            drag_state.active_this_frame = true;
        }
        if self.sidebar_drag_workspace == Some(workspace.id) && row_response.drag_stopped() {
            drag_state.active_this_frame = true;
            drag_state.drop_requested = true;
        }

        if sidebar_workspace_drop_should_dock(workspace.detached)
            && let Some(dragged_workspace_id) = self.sidebar_drag_workspace.filter(|id| *id != workspace.id)
            && let Some(pointer_pos) = ui.ctx().pointer_interact_pos()
            && row_rect.expand2(Vec2::new(0.0, 4.0)).contains(pointer_pos)
        {
            let insert = if pointer_pos.y <= row_rect.center().y {
                SidebarWorkspaceInsert::Before
            } else {
                SidebarWorkspaceInsert::After
            };
            drag_state.drop_action = Some(SidebarWorkspaceDropAction {
                dragged_workspace_id,
                target_workspace_id: workspace.id,
                insert,
            });
            paint_workspace_drop_indicator(ui, row_rect, insert, workspace.color);
        }

        if self.sidebar_drag_workspace == Some(workspace.id) && row_response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if click_target_hovered {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
    }

    fn show_workspace_context_menu(
        response: &egui::Response,
        workspace: &WorkspaceSidebarEntry,
        menu_env: &WorkspaceMenuEnv<'_>,
        actions: &mut SidebarActions,
    ) {
        menu::context_menu_above_sidebar_explicit_close(response, |ui| {
            ui.set_min_width(160.0);
            if let Some(action) = workspace_menu::show_items(ui, workspace.id, menu_env) {
                if matches!(action, WorkspaceMenuAction::Rename(_)) {
                    // The inline title editor lives on the canvas label: bring it into view.
                    actions.pan_to_workspace = Some(workspace.id);
                }
                actions.workspace_menu = Some(action);
                ui.close();
            }
            ui.separator();
            ui.label(egui::RichText::new("Arrange Panels").size(11.0).color(theme::FG_DIM()));
            if ui.add(sidebar_menu_item("Default", theme::FG_SOFT())).clicked() {
                actions.clear_layout = Some(workspace.id);
                ui.close();
            }
            for layout in WorkspaceLayout::ALL {
                if ui.add(sidebar_menu_item(layout.label(), theme::FG_SOFT())).clicked() {
                    actions.arrange_layout = Some((workspace.id, layout));
                    ui.close();
                }
            }

            ui.separator();
            let detach_label = if workspace.detached {
                "Move to Main Window"
            } else {
                "Open in New Window"
            };
            if ui.add(sidebar_menu_item(detach_label, theme::FG_SOFT())).clicked() {
                if workspace.detached {
                    actions.reattach_workspace = Some(workspace.id);
                } else {
                    actions.detach_workspace = Some(workspace.id);
                }
                ui.close();
            }

            ui.separator();
            if ui
                .add(sidebar_menu_item("Close All Panels", theme::PALETTE_RED()))
                .clicked()
            {
                actions.close_all_in_workspace = Some(workspace.id);
                ui.close();
            }
        });
    }

    fn show_sidebar_panel_context_menu(
        &mut self,
        response: &egui::Response,
        workspace: &WorkspaceSidebarEntry,
        workspace_data: &[WorkspaceSidebarEntry],
        panel_id: PanelId,
        kind: horizon_core::PanelKind,
        actions: &mut SidebarActions,
    ) {
        menu::context_menu_above_sidebar(response, |ui| {
            ui.set_min_width(160.0);
            ui.label(
                egui::RichText::new("Move to Workspace")
                    .size(11.0)
                    .color(theme::FG_DIM()),
            );
            for other_workspace in workspace_data {
                if other_workspace.id == workspace.id {
                    continue;
                }
                let text = egui::RichText::new(&other_workspace.name)
                    .size(12.0)
                    .color(theme::FG_SOFT());
                if ui.add(Button::new(text).frame(false)).clicked() {
                    self.board.assign_panel_to_workspace(panel_id, other_workspace.id);
                    self.mark_runtime_dirty();
                    ui.close();
                }
            }

            ui.separator();
            if (kind.is_agent()
                || matches!(
                    kind,
                    horizon_core::PanelKind::Ssh | horizon_core::PanelKind::Shell | horizon_core::PanelKind::Command
                ))
                && ui
                    .add(
                        Button::new(
                            egui::RichText::new(if kind == horizon_core::PanelKind::Ssh {
                                "Reconnect"
                            } else {
                                "Restart"
                            })
                            .size(12.0)
                            .color(theme::FG_SOFT()),
                        )
                        .frame(false),
                    )
                    .clicked()
            {
                self.panels_to_restart.push(panel_id);
                ui.close();
            }
            if ui
                .add(Button::new(egui::RichText::new("Close").size(12.0).color(theme::PALETTE_RED())).frame(false))
                .clicked()
            {
                actions.close_panel = Some(panel_id);
                ui.close();
            }
        });
    }

    fn apply_sidebar_actions(&mut self, ctx: &Context, actions: &SidebarActions) {
        if actions.create_workspace {
            let name = format!("Workspace {}", self.board.workspaces.len() + 1);
            self.create_workspace_visible(ctx, &name);
        }
        if actions.fit_active_workspace {
            let _ = self.fit_active_workspace(ctx);
        }
        let workspace_collision_ids = self.workspace_collision_scope(None);
        let workspace_drop_target = actions.workspace_drop.map(|drop| {
            let moved = if sidebar_workspace_drop_should_dock(self.workspace_is_detached(drop.target_workspace_id)) {
                self.board.move_workspace_beside_in_scope(
                    drop.dragged_workspace_id,
                    drop.target_workspace_id,
                    sidebar_workspace_insert_dock_side(drop.insert),
                    &workspace_collision_ids,
                )
            } else {
                false
            };
            let reordered = match drop.insert {
                SidebarWorkspaceInsert::Before => self
                    .board
                    .move_workspace_before(drop.dragged_workspace_id, drop.target_workspace_id),
                SidebarWorkspaceInsert::After => self
                    .board
                    .move_workspace_after(drop.dragged_workspace_id, drop.target_workspace_id),
            };
            if moved || reordered {
                self.mark_runtime_dirty();
            }
            drop.dragged_workspace_id
        });
        if let Some(panel_id) = actions.focus_panel {
            self.board.focus(panel_id);
        }

        let pan_workspace_id = if workspace_drop_target.is_some() {
            workspace_drop_target
        } else if let Some(panel_id) = actions.pan_to_panel {
            self.board.panel(panel_id).map(|panel| panel.workspace_id)
        } else {
            actions.pan_to_workspace
        };
        if let Some(workspace_id) = pan_workspace_id {
            if actions.pan_to_panel.is_none() {
                self.board.focus_workspace(workspace_id);
            }
            if self.focus_workspace_window(ctx, workspace_id) {
                if let Some(panel_id) = actions.focus_panel {
                    self.board.focus(panel_id);
                }
            } else if let Some((min, max)) = self.board.workspace_bounds(workspace_id) {
                let pos = Pos2::new(min[0] - WS_BG_PAD, min[1] - WS_BG_PAD - WS_TITLE_HEIGHT);
                let size = Vec2::new(
                    max[0] - min[0] + 2.0 * WS_BG_PAD,
                    max[1] - min[1] + 2.0 * WS_BG_PAD + WS_TITLE_HEIGHT,
                );
                self.pan_to_canvas_pos_aligned(ctx, pos, size, true);
            }
        }

        if let Some(workspace_id) = actions.detach_workspace {
            self.detach_workspace(workspace_id);
        }
        if let Some(workspace_id) = actions.reattach_workspace {
            self.reattach_workspace(workspace_id);
        }

        if let Some(panel_id) = actions.close_panel {
            self.close_panel(panel_id);
            self.panel_screen_rects.remove(&panel_id);
            self.terminal_body_screen_rects.remove(&panel_id);
        }
        if let Some(workspace_id) = actions.close_all_in_workspace {
            self.close_workspace_panels(workspace_id);
        }
        if let Some(workspace_id) = actions.clear_layout
            && self.board.clear_workspace_layout(workspace_id)
        {
            self.mark_runtime_dirty();
        }
        if let Some((workspace_id, layout)) = actions.arrange_layout {
            self.board.arrange_workspace(workspace_id, layout);
            self.mark_runtime_dirty();
        }
        if let Some(workspace_id) = actions.toggle_sidebar_collapse
            && let Some(workspace) = self.board.workspace_mut(workspace_id)
        {
            workspace.sidebar_collapsed = !workspace.sidebar_collapsed;
            self.mark_runtime_dirty();
        }
        if let Some(action) = actions.workspace_menu {
            self.apply_workspace_menu_action(ctx, action);
        }
        if let Some(action) = &actions.git_action {
            self.apply_git_action(ctx, action);
        }
    }
}

fn paint_workspace_drop_indicator(
    ui: &egui::Ui,
    workspace_rect: Rect,
    insert: SidebarWorkspaceInsert,
    workspace_color: Color32,
) {
    let y = match insert {
        SidebarWorkspaceInsert::Before => workspace_rect.min.y + 1.0,
        SidebarWorkspaceInsert::After => workspace_rect.max.y - 1.0,
    };
    let left = workspace_rect.min.x + 12.0;
    let right = workspace_rect.max.x - 12.0;
    ui.painter().line_segment(
        [Pos2::new(left, y), Pos2::new(right, y)],
        Stroke::new(2.0_f32, theme::alpha(workspace_color, 220)),
    );
}

fn sidebar_attention_tag(severity: AttentionSeverity) -> (&'static str, Color32) {
    match severity {
        AttentionSeverity::High => ("NEEDS INPUT", theme::PALETTE_RED()),
        AttentionSeverity::Medium => ("DONE", theme::PALETTE_GREEN()),
        AttentionSeverity::Low => ("INFO", theme::ACCENT()),
    }
}

/// A sidebar context-menu row button: flat at rest with a subtle warm
/// hover/active fill (from the theme widget visuals) so the item reads as
/// interactive. Spans the full menu width so the hover fill covers the row.
fn sidebar_menu_item(label: &str, color: Color32) -> Button<'_> {
    Button::new(egui::RichText::new(label).size(12.0).color(color))
        .fill(Color32::TRANSPARENT)
        .corner_radius(6)
        .min_size(Vec2::new(160.0, 0.0))
}

/// Accent of a Claude profile: the slot its usage chip uses, so the sidebar
/// asterisk and the chip dot match.
fn profile_accent(hub: &super::hub_state::HubState, name: &str) -> Color32 {
    let slot = hub
        .usage
        .entries()
        .iter()
        .position(|entry| entry.name == name)
        .and_then(|index| hub.usage.cache.chips.get(index))
        .map_or_else(|| super::hub_ui::accent_index(name, &[]), |chip| chip.accent_slot);
    super::hub_ui::accent_color(slot)
}

/// Accent of Claude panels on the default profile.
fn claude_default_accent() -> Color32 {
    horizon_core::agent_definition(PanelKind::Claude).map_or_else(theme::ACCENT, |definition| {
        let [r, g, b] = definition.accent_rgb;
        Color32::from_rgb(r, g, b)
    })
}
