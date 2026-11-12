//! Context menus of the worktree section (spec sections 7 and 11): per worktree
//! row, Open terminal here, Open Claude here, New workspace from worktree,
//! Remove worktree and Copy path; per repository, New worktree and Clean up.
//!
//! "Open Claude here" expands the Claude profiles inline instead of opening a
//! floating submenu: egui submenu popups paint below the `Order::Tooltip`
//! sidebar, and the workspace menu's custom submenu area is private to it.

use std::path::PathBuf;

use egui::{Align2, Color32, CornerRadius, FontId, Id, Pos2, Response, Sense, Ui, Vec2, WidgetInfo, WidgetType};
use horizon_core::{PanelKind, PresetConfig, WorkspaceId};

use super::paint::RowEnv;
use super::{GitAction, GitActionKind, WorktreeRow};
use crate::app::hub_ui;
use crate::theme;

use super::super::menu::context_menu_above_sidebar_explicit_close;

const MENU_MIN_WIDTH: f32 = 220.0;
const ITEM_HEIGHT: f32 = 26.0;
const HEADER_HEIGHT: f32 = 22.0;
const ITEM_RADIUS: u8 = 6;
const ITEM_PADDING_X: f32 = 8.0;
const LEAD_WIDTH: f32 = 16.0;
const DOT_RADIUS: f32 = 4.0;
/// Prefix of the launcher presets derived from Claude profiles.
const CLAUDE_PROFILE_PREFIX: &str = "Claude \u{b7} ";

/// A preset offered under "Open Claude here".
#[derive(Debug, PartialEq, Eq)]
struct ClaudeTarget {
    preset: usize,
    /// Accent slot of the profile; `None` for a plain Claude preset.
    slot: Option<usize>,
}

/// Derived `Claude · <profile>` presets; without any, every Claude preset.
fn claude_targets(presets: &[PresetConfig]) -> Vec<ClaudeTarget> {
    let profiles: Vec<(usize, &str)> = presets
        .iter()
        .enumerate()
        .filter(|(_, preset)| preset.kind == PanelKind::Claude)
        .filter_map(|(index, preset)| {
            let alias = preset.alias.as_deref()?;
            (preset.name.strip_prefix(CLAUDE_PROFILE_PREFIX) == Some(alias)).then_some((index, alias))
        })
        .collect();
    if profiles.is_empty() {
        return presets
            .iter()
            .enumerate()
            .filter(|(_, preset)| preset.kind == PanelKind::Claude)
            .map(|(preset, _)| ClaudeTarget { preset, slot: None })
            .collect();
    }
    let names: Vec<&str> = profiles.iter().map(|(_, alias)| *alias).collect();
    let slots = hub_ui::assign_accent_indices(&names);
    profiles
        .iter()
        .zip(slots)
        .map(|((preset, _), slot)| ClaudeTarget {
            preset: *preset,
            slot: Some(slot),
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arrow {
    None,
    Right,
    Down,
}

#[derive(Clone, Copy)]
enum Lead {
    None,
    Dot(Color32),
}

/// Keyed by worktree path hash so the open state follows the worktree when
/// rows reorder under an open menu.
fn claude_open_id(env: &RowEnv<'_>, row: &WorktreeRow) -> Id {
    Id::new(("git_wt_menu_claude_open", env.workspace.0, row.key))
}

/// Attaches the menu to `response`. Opening it (right-click) refreshes the
/// overview so dirty marks are current.
pub(super) fn show(response: &Response, env: &RowEnv<'_>, row: &WorktreeRow) -> Option<GitAction> {
    if response.secondary_clicked() {
        if let Some(poller) = env.poller {
            poller.refresh_now();
        }
        response
            .ctx
            .data_mut(|data| data.insert_temp(claude_open_id(env, row), false));
    }
    let mut picked = None;
    context_menu_above_sidebar_explicit_close(response, |ui| {
        picked = menu_items(ui, env, row);
    });
    picked
}

fn menu_items(ui: &mut Ui, env: &RowEnv<'_>, row: &WorktreeRow) -> Option<GitAction> {
    ui.set_min_width(MENU_MIN_WIDTH);
    ui.spacing_mut().item_spacing.y = 0.0;
    let action = |kind| GitAction {
        workspace: env.workspace,
        path: row.path.clone(),
        kind,
    };
    let enabled = !row.prunable;
    let mut picked = None;

    worktree_header(ui, row);

    let terminal = menu_item(ui, "Open terminal here", enabled, Lead::None, Arrow::None);
    if terminal.clicked() {
        picked = Some(action(GitActionKind::OpenTerminal));
    }

    let targets = claude_targets(env.presets);
    let claude_enabled = enabled && !targets.is_empty();
    let open_id = claude_open_id(env, row);
    let open = claude_enabled && ui.data(|data| data.get_temp::<bool>(open_id)).unwrap_or(false);
    let claude = menu_item(
        ui,
        "Open Claude here",
        claude_enabled,
        Lead::None,
        if open { Arrow::Down } else { Arrow::Right },
    );
    if enabled && targets.is_empty() {
        claude.clone().on_hover_text("No Claude presets configured");
    }
    if claude.clicked() {
        ui.data_mut(|data| data.insert_temp(open_id, !open));
    }
    if open {
        section_header(ui, "CLAUDE PROFILES");
        for target in &targets {
            let Some(preset) = env.presets.get(target.preset) else {
                continue;
            };
            let color = target.slot.map_or_else(theme::ACCENT, hub_ui::accent_color);
            if menu_item(ui, &preset.name, true, Lead::Dot(color), Arrow::None).clicked() {
                picked = Some(action(GitActionKind::OpenClaude { preset: target.preset }));
            }
        }
    }

    if menu_item(ui, "New workspace from worktree", enabled, Lead::None, Arrow::None).clicked() {
        picked = Some(action(GitActionKind::NewWorkspace));
    }
    ui.separator();
    let block = remove_block_reason(row);
    let remove = menu_item(ui, "Remove worktree\u{2026}", block.is_none(), Lead::None, Arrow::None);
    if let Some(reason) = block {
        remove.on_hover_text(reason);
    } else if remove.clicked() {
        picked = Some(action(GitActionKind::RemoveWorktree));
    }
    if menu_item(ui, "Copy path", true, Lead::None, Arrow::None).clicked() {
        picked = Some(action(GitActionKind::CopyPath));
    }

    if picked.is_some() {
        ui.close();
    }
    picked
}

/// Why "Remove worktree..." is unavailable for `row`, from what the sidebar
/// already knows. The cleanup dialog re-checks everything before removing.
fn remove_block_reason(row: &WorktreeRow) -> Option<&'static str> {
    if row.is_main {
        Some("The main worktree cannot be removed")
    } else if row.is_current {
        Some("This workspace is open in it")
    } else if row.locked {
        Some("It is locked")
    } else if row.dirty {
        Some("It has uncommitted changes")
    } else {
        None
    }
}

/// Context menu of the git line and the worktrees toggle: start a new worktree
/// (and, on the toggle, open the cleanup). Not tied to any row.
pub(super) fn show_repo_menu(response: &Response, workspace: WorkspaceId, with_cleanup: bool) -> Option<GitAction> {
    let mut picked = None;
    context_menu_above_sidebar_explicit_close(response, |ui| {
        ui.set_min_width(MENU_MIN_WIDTH);
        ui.spacing_mut().item_spacing.y = 0.0;
        let action = |kind| GitAction {
            workspace,
            path: PathBuf::new(),
            kind,
        };
        if menu_item(ui, "New worktree + Claude\u{2026}", true, Lead::None, Arrow::None).clicked() {
            picked = Some(action(GitActionKind::NewWorktree));
        }
        if with_cleanup && menu_item(ui, "Clean up worktrees\u{2026}", true, Lead::None, Arrow::None).clicked() {
            picked = Some(action(GitActionKind::CleanupWorktrees));
        }
        if picked.is_some() {
            ui.close();
        }
    });
    picked
}

/// Which worktree the menu acts on: branch label and short path.
fn worktree_header(ui: &mut Ui, row: &WorktreeRow) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    let label = painter.text(
        Pos2::new(rect.left() + ITEM_PADDING_X, rect.center().y),
        Align2::LEFT_CENTER,
        &row.label,
        FontId::proportional(11.0),
        theme::FG(),
    );
    painter.text(
        Pos2::new(label.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        &row.short_path,
        FontId::monospace(10.0),
        theme::FG_DIM(),
    );
    ui.separator();
}

fn section_header(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + ITEM_PADDING_X, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(9.5),
        theme::FG_DIM(),
    );
}

/// One 26 px menu row: 12 px label, hover fill, optional dot and arrow.
fn menu_item(ui: &mut Ui, label: &str, enabled: bool, lead: Lead, arrow: Arrow) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ITEM_HEIGHT), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let active = enabled && (response.hovered() || response.has_focus());
    let fill = if active {
        theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.22)
    } else {
        Color32::TRANSPARENT
    };
    let text_color = if enabled {
        theme::FG()
    } else {
        theme::alpha(theme::FG_DIM(), 160)
    };
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::same(ITEM_RADIUS), fill);
    let cy = rect.center().y;
    let mut text_x = rect.left() + ITEM_PADDING_X;
    if let Lead::Dot(color) = lead {
        painter.circle_filled(Pos2::new(text_x + LEAD_WIDTH / 2.0, cy), DOT_RADIUS, color);
        text_x += LEAD_WIDTH + ITEM_PADDING_X;
    }
    painter.text(
        Pos2::new(text_x, cy),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        text_color,
    );
    let tip = Pos2::new(rect.right() - ITEM_PADDING_X - 2.0, cy);
    let points = match arrow {
        Arrow::None => None,
        Arrow::Right => Some(vec![tip, tip + Vec2::new(-4.0, -3.5), tip + Vec2::new(-4.0, 3.5)]),
        Arrow::Down => Some(vec![
            tip + Vec2::new(-2.0, 2.0),
            tip + Vec2::new(-6.0, -1.5),
            tip + Vec2::new(2.0, -1.5),
        ]),
    };
    if let Some(points) = points {
        painter.add(egui::Shape::convex_polygon(points, theme::FG_DIM(), egui::Stroke::NONE));
    }
    hub_ui::paint_focus_ring(ui.painter(), &response, rect, ITEM_RADIUS);
    response
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use horizon_core::{ClaudeProfile, PanelKind, PanelResume, PresetConfig, claude_profile_preset};

    use super::{ClaudeTarget, claude_targets};

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
    fn claude_targets_prefer_derived_profile_presets() {
        let base = preset("Claude Code", PanelKind::Claude);
        let presets = vec![
            preset("Shell", PanelKind::Shell),
            base.clone(),
            profile(&base, "conta1"),
            profile(&base, "conta2"),
        ];
        let targets = claude_targets(&presets);
        assert_eq!(targets.iter().map(|t| t.preset).collect::<Vec<_>>(), [2, 3]);
        assert!(targets.iter().all(|target| target.slot.is_some()));
        assert_ne!(targets[0].slot, targets[1].slot);
    }

    #[test]
    fn claude_targets_fall_back_to_plain_claude_presets() {
        let presets = vec![
            preset("Shell", PanelKind::Shell),
            preset("Claude Code", PanelKind::Claude),
        ];
        assert_eq!(claude_targets(&presets), [ClaudeTarget { preset: 1, slot: None }]);
        assert!(claude_targets(&[preset("Shell", PanelKind::Shell)]).is_empty());
    }
}
