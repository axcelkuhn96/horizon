//! Workspace context menu of the sidebar: Rename, New terminal, Save as
//! template and Delete (with a confirmation modal), plus the short feedback
//! notice shown after saving a template.
//!
//! Visual spec: `docs/design/claude-hub-ui.md` section 5. The egui `SubMenu`
//! popups open in `Order::Foreground`, which sits below the `Order::Tooltip`
//! sidebar, so the "New terminal" submenu is a small `Order::Tooltip` area
//! positioned next to its row instead.

mod submenu;

use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Layout, Margin, Order, Pos2, Response,
    RichText, Sense, Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::{PanelKind, PresetConfig, WorkspaceId};

use crate::app::HorizonApp;
use crate::app::actions::TemplateSaved;
use crate::app::hub_ui::{self, ModalPlacement};
use crate::app::util;
use crate::theme;
use submenu::show_presets_submenu;

const MENU_MIN_WIDTH: f32 = 220.0;
const SUBMENU_WIDTH: f32 = 240.0;
const ROW_HEIGHT: f32 = 28.0;
const HEADER_HEIGHT: f32 = 22.0;
const ROW_RADIUS: u8 = 6;
const ROW_PADDING_X: f32 = 8.0;
const LEAD_COLUMN_WIDTH: f32 = 16.0;
const DOT_RADIUS: f32 = 4.0;
const SUBMENU_OFFSET_X: f32 = 6.0;
const DELETE_MODAL_WIDTH: f32 = 380.0;
const NOTICE_DURATION: Duration = Duration::from_secs(3);
const NOTICE_MAX_WIDTH: f32 = 320.0;
const NOTICE_MARGIN: f32 = 12.0;
/// Prefix of the launcher presets derived from Claude profiles (`Claude · <profile>`).
const CLAUDE_PROFILE_PREFIX: &str = "Claude \u{b7} ";

/// What the user picked in the workspace context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceMenuAction {
    Rename(WorkspaceId),
    NewTerminal { workspace: WorkspaceId, preset: usize },
    NewWorktree(WorkspaceId),
    SendToAll(WorkspaceId),
    SaveAsTemplate(WorkspaceId),
    RequestDelete(WorkspaceId),
}

/// Read-only app data the menu needs; built by the sidebar on each frame.
pub(super) struct WorkspaceMenuEnv<'a> {
    pub(super) presets: &'a [PresetConfig],
    /// Saved template that "Save as template" would update, if any.
    pub(super) template_target: Option<&'a str>,
    pub(super) can_delete: bool,
    /// The workspace folder is inside a git repository.
    pub(super) has_repo: bool,
}

/// Launcher presets split into plain presets and Claude-profile presets, as
/// indices into the preset list.
#[derive(Debug, Default, PartialEq, Eq)]
struct PresetGroups {
    general: Vec<usize>,
    /// `(preset index, accent slot)`
    profiles: Vec<(usize, usize)>,
}

fn claude_profile_alias(preset: &PresetConfig) -> Option<&str> {
    let alias = preset.alias.as_deref()?;
    (preset.kind == PanelKind::Claude && preset.name.strip_prefix(CLAUDE_PROFILE_PREFIX) == Some(alias))
        .then_some(alias)
}

fn split_presets(presets: &[PresetConfig]) -> PresetGroups {
    let profile_aliases: Vec<(usize, &str)> = presets
        .iter()
        .enumerate()
        .filter_map(|(index, preset)| claude_profile_alias(preset).map(|alias| (index, alias)))
        .collect();
    let names: Vec<&str> = profile_aliases.iter().map(|(_, alias)| *alias).collect();
    let slots = hub_ui::assign_accent_indices(&names);
    PresetGroups {
        general: (0..presets.len())
            .filter(|index| !profile_aliases.iter().any(|(profile, _)| profile == index))
            .collect(),
        profiles: profile_aliases
            .iter()
            .zip(slots)
            .map(|((index, _), slot)| (*index, slot))
            .collect(),
    }
}

/// Label of the save item: it turns into an update when a template exists.
fn save_item_label(template_target: Option<&str>) -> String {
    match template_target {
        Some(name) => format!("Update template \"{name}\""),
        None => "Save as template".to_string(),
    }
}

fn saved_notice_text(saved: &TemplateSaved) -> String {
    let verb = if saved.updated { "Updated" } else { "Saved" };
    let name = &saved.name;
    if saved.pending_in_settings {
        format!("{verb} template \"{name}\" in Settings \u{2014} save Settings to keep it")
    } else {
        format!("{verb} template \"{name}\"")
    }
}

/// Body text of the delete confirmation. `live_panels` is the number of
/// terminals that will be closed.
pub(super) fn confirm_label(workspace_name: &str, live_panels: usize) -> String {
    match live_panels {
        0 => format!("Delete workspace \"{workspace_name}\"?"),
        1 => {
            format!("Delete workspace \"{workspace_name}\" and its 1 terminal? Running processes will be closed.")
        }
        count => {
            format!(
                "Delete workspace \"{workspace_name}\" and its {count} terminals? Running processes will be closed."
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Menu rows
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowTone {
    Normal,
    Danger,
}

enum RowLead<'a> {
    None,
    Icon(&'a str, Color32),
    Dot(Color32),
}

struct MenuRow<'a> {
    label: &'a str,
    tone: RowTone,
    enabled: bool,
    lead: RowLead<'a>,
    submenu_arrow: bool,
}

impl<'a> MenuRow<'a> {
    fn new(label: &'a str) -> Self {
        Self {
            label,
            tone: RowTone::Normal,
            enabled: true,
            lead: RowLead::None,
            submenu_arrow: false,
        }
    }
}

fn menu_row(ui: &mut Ui, row: &MenuRow<'_>) -> Response {
    let sense = if row.enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, row.enabled, row.label));
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let active = row.enabled && (response.hovered() || response.has_focus());
    let fill = match (active, row.tone) {
        (false, _) => Color32::TRANSPARENT,
        (true, RowTone::Danger) => theme::alpha(theme::BTN_CLOSE(), 40),
        (true, RowTone::Normal) => theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.22),
    };
    let text_color = match (row.enabled, row.tone) {
        (false, _) => theme::alpha(theme::FG_DIM(), 160),
        (true, RowTone::Danger) => theme::BTN_CLOSE(),
        (true, RowTone::Normal) => theme::FG(),
    };

    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::same(ROW_RADIUS), fill);
    let center_y = rect.center().y;
    let mut text_x = rect.left() + ROW_PADDING_X;
    match row.lead {
        RowLead::None => {}
        RowLead::Icon(icon, color) => {
            painter.text(
                Pos2::new(text_x + LEAD_COLUMN_WIDTH / 2.0, center_y),
                Align2::CENTER_CENTER,
                icon,
                FontId::monospace(10.5),
                color,
            );
            text_x += LEAD_COLUMN_WIDTH + ROW_PADDING_X;
        }
        RowLead::Dot(color) => {
            painter.circle_filled(Pos2::new(text_x + LEAD_COLUMN_WIDTH / 2.0, center_y), DOT_RADIUS, color);
            text_x += LEAD_COLUMN_WIDTH + ROW_PADDING_X;
        }
    }
    painter.text(
        Pos2::new(text_x, center_y),
        Align2::LEFT_CENTER,
        row.label,
        FontId::proportional(12.0),
        text_color,
    );
    if row.submenu_arrow {
        // Painted as a triangle so it does not depend on a font glyph.
        let tip = Pos2::new(rect.right() - ROW_PADDING_X - 2.0, center_y);
        painter.add(egui::Shape::convex_polygon(
            vec![tip, tip + Vec2::new(-4.0, -3.5), tip + Vec2::new(-4.0, 3.5)],
            theme::FG_DIM(),
            egui::Stroke::NONE,
        ));
    }
    hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
    response
}

fn section_header(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + ROW_PADDING_X, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(9.5),
        theme::FG_DIM(),
    );
}

/// Items of the workspace context menu. Returns the picked action, if any.
pub(super) fn show_items(
    ui: &mut Ui,
    workspace: WorkspaceId,
    env: &WorkspaceMenuEnv<'_>,
) -> Option<WorkspaceMenuAction> {
    ui.set_min_width(MENU_MIN_WIDTH);
    let mut chosen = None;

    if menu_row(ui, &MenuRow::new("Rename")).clicked() {
        chosen = Some(WorkspaceMenuAction::Rename(workspace));
    }

    let new_terminal = menu_row(
        ui,
        &MenuRow {
            submenu_arrow: true,
            ..MenuRow::new("New terminal")
        },
    );
    let submenu_id = ui.id().with(("workspace_menu_new_terminal", workspace.0));
    if let Some(preset) = show_presets_submenu(ui.ctx(), submenu_id, &new_terminal, env.presets) {
        chosen = Some(WorkspaceMenuAction::NewTerminal { workspace, preset });
    }

    if env.has_repo && menu_row(ui, &MenuRow::new("New worktree + Claude\u{2026}")).clicked() {
        chosen = Some(WorkspaceMenuAction::NewWorktree(workspace));
    }

    if menu_row(ui, &MenuRow::new("Send to all Claudes\u{2026}")).clicked() {
        chosen = Some(WorkspaceMenuAction::SendToAll(workspace));
    }

    if menu_row(ui, &MenuRow::new(&save_item_label(env.template_target))).clicked() {
        chosen = Some(WorkspaceMenuAction::SaveAsTemplate(workspace));
    }

    ui.separator();
    let delete = MenuRow {
        tone: RowTone::Danger,
        enabled: env.can_delete,
        ..MenuRow::new("Delete\u{2026}")
    };
    let delete_response = menu_row(ui, &delete);
    if !env.can_delete {
        delete_response.on_hover_text("The last workspace cannot be deleted");
    } else if delete_response.clicked() {
        chosen = Some(WorkspaceMenuAction::RequestDelete(workspace));
    }
    chosen
}

// ---------------------------------------------------------------------------
// Notice (save feedback) and delete confirmation
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Notice {
    text: String,
    error: bool,
    until: Instant,
}

fn notice_id() -> Id {
    Id::new("workspace_menu_notice")
}

fn show_notice(ctx: &Context, text: String, error: bool) {
    let notice = Notice {
        text,
        error,
        until: Instant::now() + NOTICE_DURATION,
    };
    ctx.data_mut(|data| data.insert_temp(notice_id(), notice));
    ctx.request_repaint();
}

/// `left` is the canvas-side edge of the sidebar: the notice sits to its right
/// so it never covers the sidebar's bottom controls.
fn render_notice(ctx: &Context, left: f32) {
    let Some(notice) = ctx.data(|data| data.get_temp::<Notice>(notice_id())) else {
        return;
    };
    let now = Instant::now();
    if now >= notice.until {
        ctx.data_mut(|data| data.remove::<Notice>(notice_id()));
        return;
    }
    let color = if notice.error {
        theme::PALETTE_RED()
    } else {
        theme::PALETTE_GREEN()
    };
    Area::new(notice_id().with("area"))
        .order(Order::Tooltip)
        .anchor(Align2::LEFT_BOTTOM, Vec2::new(left + NOTICE_MARGIN, -NOTICE_MARGIN))
        .interactable(false)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::PANEL_BG())
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER_SUBTLE()))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_max_width(NOTICE_MAX_WIDTH);
                    ui.label(RichText::new(&notice.text).size(11.0).color(color));
                });
        });
    ctx.request_repaint_after(notice.until.saturating_duration_since(now));
}

fn delete_confirm_id(workspace: WorkspaceId) -> Id {
    Id::new(("workspace_delete_confirm", workspace.0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteDecision {
    Pending,
    Cancel,
    Delete,
}

/// Destructive button of the confirmation modal: `BTN_CLOSE` fill, `BG` text.
fn danger_button(ui: &mut Ui, label: &str) -> Response {
    const RADIUS: u8 = 8;
    let font = FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new((text_width + 32.0).max(64.0), 30.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let fill = if response.is_pointer_button_down_on() {
            theme::blend(theme::BTN_CLOSE(), theme::BG(), 0.18)
        } else if response.hovered() {
            theme::blend(theme::BTN_CLOSE(), theme::FG(), 0.12)
        } else {
            theme::BTN_CLOSE()
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        painter.text(rect.center(), Align2::CENTER_CENTER, label, font, theme::BG());
        hub_ui::paint_focus_ring(painter, &response, rect, RADIUS);
    }
    response
}

impl HorizonApp {
    /// Overlays owned by the workspace menu; drawn even while the sidebar is hidden.
    pub(super) fn render_workspace_menu_overlays(&mut self, ctx: &Context) {
        let sidebar_edge = self.reserved_sidebar_width(util::viewport_local_rect(ctx).width());
        render_notice(ctx, sidebar_edge);
        self.render_delete_workspace_modal(ctx);
    }

    pub(super) fn apply_workspace_menu_action(&mut self, ctx: &Context, action: WorkspaceMenuAction) {
        match action {
            WorkspaceMenuAction::Rename(workspace) => {
                if let Some(name) = self.board.workspace(workspace).map(|ws| ws.name.clone()) {
                    self.clear_panel_rename();
                    self.renaming_workspace = Some(workspace);
                    self.rename_buffer = name;
                }
            }
            WorkspaceMenuAction::NewTerminal { workspace, preset } => {
                if let Some(preset) = self.presets.get(preset).cloned() {
                    self.add_panel_to_workspace(workspace, preset, None);
                }
            }
            WorkspaceMenuAction::NewWorktree(workspace) => self.open_new_worktree(ctx, workspace, None),
            WorkspaceMenuAction::SendToAll(workspace) => self.open_send_to_workspace(workspace),
            WorkspaceMenuAction::SaveAsTemplate(workspace) => match self.save_workspace_as_template(workspace) {
                Ok(saved) => {
                    show_notice(ctx, saved_notice_text(&saved), false);
                }
                Err(error) => {
                    tracing::warn!("failed to save workspace template: {error}");
                    show_notice(ctx, format!("Could not save template: {error}"), true);
                }
            },
            WorkspaceMenuAction::RequestDelete(workspace) => self.request_delete_workspace(ctx, workspace),
        }
    }

    /// Open the delete confirmation. The text (name and terminal count) is
    /// computed once here, not on every frame the modal is shown.
    fn request_delete_workspace(&mut self, ctx: &Context, workspace: WorkspaceId) {
        let Some(ws) = self.board.workspace(workspace) else {
            return;
        };
        let live_panels = ws
            .panels
            .iter()
            .filter_map(|panel_id| self.board.panel(*panel_id))
            .filter(|panel| !panel.process_exited())
            .count();
        let text = confirm_label(&ws.name, live_panels);
        ctx.data_mut(|data| data.insert_temp(delete_confirm_id(workspace), text));
        self.hub.pending_delete_workspace = Some(workspace);
        ctx.request_repaint();
    }

    fn render_delete_workspace_modal(&mut self, ctx: &Context) {
        let Some(workspace) = self.hub.pending_delete_workspace else {
            return;
        };
        if self.board.workspace(workspace).is_none() || self.board.workspaces.len() <= 1 {
            self.hub.pending_delete_workspace = None;
            return;
        }
        let text = ctx
            .data(|data| data.get_temp::<String>(delete_confirm_id(workspace)))
            .unwrap_or_default();

        let mut decision = DeleteDecision::Pending;
        let mut cancel_id = None;
        let outcome = hub_ui::show_modal(
            ctx,
            Id::new("workspace_delete_modal"),
            DELETE_MODAL_WIDTH,
            ModalPlacement::Center,
            None,
            |ui| {
                Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                    ui.label(RichText::new("Delete workspace").size(14.0).strong().color(theme::FG()));
                    ui.add_space(8.0);
                    ui.label(RichText::new(&text).size(12.0).color(theme::FG_SOFT()));
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if danger_button(ui, "Delete").clicked() {
                            decision = DeleteDecision::Delete;
                        }
                        cancel_id = Some(ui.next_auto_id());
                        if hub_ui::secondary_button(ui, "Cancel").clicked() {
                            decision = DeleteDecision::Cancel;
                        }
                    });
                });
            },
        );
        // Cancel is the safe default: focus it when the modal opens.
        if outcome.first_frame
            && let Some(id) = cancel_id
        {
            ctx.memory_mut(|memory| memory.request_focus(id));
        }

        if outcome.dismissed {
            decision = DeleteDecision::Cancel;
        }
        match decision {
            DeleteDecision::Pending => {}
            DeleteDecision::Cancel => self.hub.pending_delete_workspace = None,
            DeleteDecision::Delete => {
                self.hub.pending_delete_workspace = None;
                self.delete_workspace(workspace);
            }
        }
        if decision != DeleteDecision::Pending {
            ctx.data_mut(|data| data.remove::<String>(delete_confirm_id(workspace)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use horizon_core::{ClaudeProfile, PanelKind, PanelResume, PresetConfig, claude_profile_preset};

    use super::{PresetGroups, confirm_label, save_item_label, split_presets};

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

    fn profile(base: &PresetConfig, name: &str) -> PresetConfig {
        claude_profile_preset(
            base,
            &ClaudeProfile {
                name: name.to_string(),
                config_dir: format!("~/.claude-{name}"),
            },
            Path::new("/home/u"),
        )
    }

    #[test]
    fn workspace_menu_confirm_label_mentions_panel_count() {
        let many = confirm_label("api-core", 3);
        assert!(many.contains("\"api-core\""));
        assert!(many.contains("3 terminals"));
        assert!(many.contains("Running processes will be closed"));

        assert!(confirm_label("api-core", 1).contains("1 terminal?"));
        let none = confirm_label("api-core", 0);
        assert!(none.contains("\"api-core\""));
        assert!(!none.contains("terminal"));
    }

    #[test]
    fn workspace_menu_save_label_turns_into_update_when_template_exists() {
        assert_eq!(save_item_label(None), "Save as template");
        assert_eq!(save_item_label(Some("Backend")), "Update template \"Backend\"");
    }

    #[test]
    fn workspace_menu_splits_claude_profile_presets_from_the_rest() {
        let base = preset("Claude Code", PanelKind::Claude);
        let presets = vec![
            preset("Shell", PanelKind::Shell),
            base.clone(),
            profile(&base, "conta1"),
            preset("Codex", PanelKind::Codex),
            profile(&base, "conta2"),
        ];

        let groups = split_presets(&presets);

        assert_eq!(groups.general, vec![0, 1, 3]);
        assert_eq!(
            groups.profiles.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert_ne!(
            groups.profiles[0].1, groups.profiles[1].1,
            "profiles get distinct accents"
        );
    }

    #[test]
    fn workspace_menu_without_profiles_has_only_general_presets() {
        let presets = vec![
            preset("Shell", PanelKind::Shell),
            preset("Claude Code", PanelKind::Claude),
        ];

        assert_eq!(
            split_presets(&presets),
            PresetGroups {
                general: vec![0, 1],
                profiles: Vec::new()
            }
        );
    }
}
