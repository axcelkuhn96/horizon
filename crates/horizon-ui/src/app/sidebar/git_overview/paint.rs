//! Painting of the sidebar git line, the worktrees toggle and the worktree
//! rows (spec section 7). Everything reads strings prepared by `build_cache`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Galley, Id, Margin, Painter, Pos2, Rect, RichText, Sense,
    Shape, Stroke, Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::git_overview::OverviewPoller;
use horizon_core::{PresetConfig, WorkspaceId};

use super::{
    COPIED_FEEDBACK, Copied, GitAction, GitActionKind, GitLineCache, GitSidebarState, TooltipData, VISIBLE_ROWS,
    WorktreeRow, row_menu, visible_rows,
};
use crate::app::hub_ui;
use crate::app::sidebar::rows::{BG_INSET, CHILD_X, HEADER_RIGHT_PAD, LABEL_OFFSET};
use crate::theme;

const GIT_LINE_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 22.0;
/// Left indent of the git line and the section toggles: the sidebar-wide child indent.
const LINE_INDENT: f32 = CHILD_X;
/// Left indent of the text of a worktree row.
const ROW_INDENT: f32 = CHILD_X + 12.0;
/// Left edge of a row's highlight, inside the indent so rows read as nested.
const ROW_FILL_INDENT: f32 = CHILD_X + 2.0;
/// Space between the parts of the branch chip (glyph, branch, counters).
const CHIP_PART_GAP: f32 = 6.0;
const CHIP_INNER_GAP: f32 = 3.0;
const ROW_RIGHT_INSET: f32 = 6.0;
const ROW_RADIUS: u8 = 6;
const TOOLTIP_WIDTH: f32 = 300.0;
const TOOLTIP_MARGIN: i8 = 12;
/// Margin egui's popup frame already adds around a tooltip body.
const POPUP_FRAME_MARGIN: i8 = 6;
const AGENT_TOGGLE_EXTRA_INDENT: f32 = 12.0;

/// Everything a worktree row (and its menu) needs besides the row itself.
pub(super) struct RowEnv<'a> {
    pub(super) workspace: WorkspaceId,
    pub(super) presets: &'a [PresetConfig],
    pub(super) poller: Option<&'a OverviewPoller<WorkspaceId>>,
    pub(super) copied: Option<Copied>,
    pub(super) changed_at: Instant,
}

/// Dirty dot on a collapsed workspace header (the git line is hidden then).
pub(in crate::app::sidebar) fn paint_collapsed_dirty_dot(
    ui: &Ui,
    state: &GitSidebarState,
    workspace: WorkspaceId,
    row_rect: Rect,
    center_x: f32,
) {
    if state
        .caches
        .get(&workspace)
        .is_some_and(|cache| cache.error.is_none() && cache.dirty_count > 0)
    {
        ui.painter()
            .circle_filled(Pos2::new(center_x, row_rect.center().y), 3.0, theme::PALETTE_YELLOW());
    }
}

/// Git line plus the expandable worktrees list of one workspace. Draws nothing
/// (zero height) for workspaces without a repository. Returns the action the
/// user picked, if any.
pub(in crate::app::sidebar) fn render_git_section(
    ui: &mut Ui,
    state: &mut GitSidebarState,
    presets: &[PresetConfig],
    workspace: WorkspaceId,
    line_in_header: bool,
) -> Option<GitAction> {
    let GitSidebarState {
        caches,
        poller,
        expanded,
        agents_open,
        show_all,
        watch_stale,
        copied,
        ..
    } = state;
    let cache = caches.get(&workspace)?;
    if cache.error.is_some() {
        paint_error_line(ui, cache, workspace);
        return None;
    }
    if !line_in_header {
        paint_git_line(ui, cache, workspace);
    }
    if cache.worktree_rows.len() < 2 {
        return None;
    }

    let open = expanded.contains(&workspace);
    let toggle_id = ui.make_persistent_id(("git_wt_toggle", workspace.0));
    if toggle_row(
        ui,
        toggle_id,
        LINE_INDENT,
        "worktrees",
        &cache.worktrees_count_label,
        open,
    ) {
        if open {
            expanded.remove(&workspace);
        } else {
            expanded.insert(workspace);
        }
        *watch_stale = true;
        return None;
    }
    if !open {
        return None;
    }

    if copied.is_some_and(|copied| copied.at.elapsed() >= COPIED_FEEDBACK) {
        *copied = None;
    }
    let env = RowEnv {
        workspace,
        presets,
        poller: poller.as_ref(),
        copied: copied.filter(|copied| copied.workspace == workspace),
        changed_at: cache.changed_at,
    };
    if let Some(copied) = env.copied {
        ui.ctx()
            .request_repaint_after(COPIED_FEEDBACK.saturating_sub(copied.at.elapsed()));
    }

    let all = show_all.contains(&workspace);
    let agents_open_now = agents_open.contains(&workspace);
    let list = paint_worktree_list(ui, &env, cache, all, agents_open_now);
    if list.toggled_all {
        flip(show_all, workspace);
        *watch_stale = true;
    }
    if list.toggled_agents {
        flip(agents_open, workspace);
        *watch_stale = true;
    }
    list.action
}

/// What painting the open worktrees list produced this frame.
struct ListOutcome {
    action: Option<GitAction>,
    toggled_all: bool,
    toggled_agents: bool,
}

/// Primary worktree rows, then the collapsible agents group, each capped at
/// `VISIBLE_ROWS` unless `all`.
fn paint_worktree_list(
    ui: &mut Ui,
    env: &RowEnv<'_>,
    cache: &GitLineCache,
    all: bool,
    agents_open_now: bool,
) -> ListOutcome {
    let workspace = env.workspace;
    let mut action = None;
    let mut toggled_all = false;
    let mut toggled_agents = false;

    let primary = visible_rows(cache.primary_count, all);
    for index in 0..primary {
        action = action.or(paint_worktree_row(ui, env, &cache.worktree_rows, index));
    }
    if cache.primary_count > VISIBLE_ROWS {
        toggled_all |= show_all_row(ui, workspace, "primary", all, &cache.primary_show_all_label);
    }

    if cache.agent_count() > 0 {
        let agents_id = ui.make_persistent_id(("git_wt_agents", workspace.0));
        toggled_agents = toggle_row(
            ui,
            agents_id,
            LINE_INDENT + AGENT_TOGGLE_EXTRA_INDENT,
            &cache.agents_label,
            "",
            agents_open_now,
        );
        if agents_open_now {
            let shown = visible_rows(cache.agent_count(), all);
            for index in cache.primary_count..cache.primary_count + shown {
                action = action.or(paint_worktree_row(ui, env, &cache.worktree_rows, index));
            }
            if cache.agent_count() > VISIBLE_ROWS {
                toggled_all |= show_all_row(ui, workspace, "agents", all, &cache.agents_show_all_label);
            }
        }
    }
    ListOutcome {
        action,
        toggled_all,
        toggled_agents,
    }
}

fn flip(set: &mut HashSet<WorkspaceId>, workspace: WorkspaceId) {
    if !set.remove(&workspace) {
        set.insert(workspace);
    }
}

// ---------------------------------------------------------------------------
// Git line
// ---------------------------------------------------------------------------

fn paint_error_line(ui: &mut Ui, cache: &GitLineCache, workspace: WorkspaceId) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GIT_LINE_HEIGHT), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.min.x + LINE_INDENT, rect.center().y),
        Align2::LEFT_CENTER,
        "git?",
        FontId::proportional(10.5),
        theme::FG_DIM(),
    );
    let response = ui.interact(rect, ui.make_persistent_id(("git_line", workspace.0)), Sense::hover());
    response.on_hover_text(&cache.tooltip.path);
}

/// Branch glyph: a stem, a side branch and a node (1.2 px strokes).
fn paint_branch_glyph(painter: &Painter, area: Rect, color: Color32) {
    let stroke = Stroke::new(1.2_f32, color);
    let at = |x: f32, y: f32| Pos2::new(area.min.x + x, area.min.y + y);
    painter.line_segment([at(3.0, 1.5), at(3.0, 10.5)], stroke);
    painter.line_segment([at(3.0, 8.0), at(8.5, 5.5)], stroke);
    painter.circle_stroke(at(9.0, 4.0), 2.0, stroke);
}

/// Small filled triangle (5 wide, 7 tall) pointing up or down, centred on `center`.
fn paint_arrow(painter: &Painter, center: Pos2, up: bool, color: Color32) {
    let (half_w, half_h) = (2.5_f32, 3.5_f32);
    let points = if up {
        vec![
            Pos2::new(center.x, center.y - half_h),
            Pos2::new(center.x - half_w, center.y + half_h),
            Pos2::new(center.x + half_w, center.y + half_h),
        ]
    } else {
        vec![
            Pos2::new(center.x, center.y + half_h),
            Pos2::new(center.x - half_w, center.y - half_h),
            Pos2::new(center.x + half_w, center.y - half_h),
        ]
    };
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

/// Galleys of the branch chip, laid out once per pixels-per-point. Colours are
/// applied when painting (the galleys carry `Color32::PLACEHOLDER`).
#[derive(Clone, Debug)]
pub(super) struct ChipLayout {
    ppp_bits: u32,
    width: f32,
    branch: Arc<Galley>,
    ahead: Option<Arc<Galley>>,
    behind: Option<Arc<Galley>>,
    dirty: Option<Arc<Galley>>,
}

fn build_chip_layout(painter: &Painter, cache: &GitLineCache, ppp: f32) -> ChipLayout {
    let lay = |text: &str, font: FontId, italics: bool| {
        let mut format = TextFormat::simple(font, Color32::PLACEHOLDER);
        format.italics = italics;
        painter.layout_job(LayoutJob::single_section(text.to_owned(), format))
    };
    let counter = |text: &str| (!text.is_empty()).then(|| lay(text, FontId::monospace(10.0), false));
    let mut layout = ChipLayout {
        ppp_bits: ppp.to_bits(),
        width: 0.0,
        branch: lay(&cache.branch_label, FontId::proportional(10.5), cache.detached),
        ahead: counter(&cache.ahead_label),
        behind: counter(&cache.behind_label),
        dirty: counter(&cache.dirty_label),
    };
    layout.width = walk_chip(painter, &layout, cache.detached, 0.0, 0.0, false);
    layout
}

/// Chip galleys of `cache`, laid out again only when `ppp` moved.
fn chip_layout(painter: &Painter, cache: &GitLineCache, ppp: f32) -> ChipLayout {
    let bits = ppp.to_bits();
    if let Some(layout) = cache.chip.borrow().as_ref().filter(|layout| layout.ppp_bits == bits) {
        return layout.clone();
    }
    let layout = build_chip_layout(painter, cache, ppp);
    *cache.chip.borrow_mut() = Some(layout.clone());
    layout
}

/// Walks the branch chip (glyph, branch, ahead/behind, dirty) from `left`,
/// painting it when `draw`. Returns the x after the last part, so a dry run
/// from 0 measures its width. Lays out nothing.
fn walk_chip(painter: &Painter, layout: &ChipLayout, detached: bool, left: f32, cy: f32, draw: bool) -> f32 {
    let text = |x: f32, galley: &Arc<Galley>, color: Color32| {
        if draw {
            painter.galley(Pos2::new(x, cy - galley.size().y / 2.0), Arc::clone(galley), color);
        }
        x + galley.size().x
    };
    let mut x = left;
    if draw {
        let area = Rect::from_min_size(Pos2::new(x, cy - 6.0), Vec2::splat(12.0));
        paint_branch_glyph(painter, area, theme::FG_DIM());
    }
    x += 12.0 + 4.0;
    let branch_color = if detached { theme::FG_DIM() } else { theme::FG_SOFT() };
    x = text(x, &layout.branch, branch_color);
    for (galley, up, color) in [
        (&layout.ahead, true, theme::PALETTE_GREEN()),
        (&layout.behind, false, theme::PALETTE_YELLOW()),
    ] {
        let Some(galley) = galley else {
            continue;
        };
        x += CHIP_PART_GAP;
        if draw {
            paint_arrow(painter, Pos2::new(x + 2.5, cy), up, color);
        }
        x += 5.0 + CHIP_INNER_GAP;
        x = text(x, galley, color);
    }
    if let Some(galley) = &layout.dirty {
        x += CHIP_PART_GAP;
        if draw {
            painter.circle_filled(Pos2::new(x + 3.0, cy), 3.0, theme::PALETTE_YELLOW());
        }
        x += 6.0 + CHIP_INNER_GAP;
        x = text(x, galley, theme::FG_SOFT());
    }
    x
}

/// Width of the branch chip of `workspace`; `None` without a healthy repository.
/// Laid out once per overview and scale, and kept on the cache.
pub(in crate::app::sidebar) fn header_chip_width(
    ui: &Ui,
    state: &GitSidebarState,
    workspace: WorkspaceId,
) -> Option<f32> {
    let cache = state.caches.get(&workspace).filter(|cache| cache.error.is_none())?;
    Some(chip_layout(ui.painter(), cache, ui.ctx().pixels_per_point()).width)
}

/// Branch chip right-aligned on a workspace header row, with its hover card.
pub(in crate::app::sidebar) fn paint_header_chip(
    ui: &mut Ui,
    state: &GitSidebarState,
    workspace: WorkspaceId,
    row_rect: Rect,
) {
    let Some(cache) = state.caches.get(&workspace).filter(|cache| cache.error.is_none()) else {
        return;
    };
    let layout = chip_layout(ui.painter(), cache, ui.ctx().pixels_per_point());
    let left = row_rect.max.x - HEADER_RIGHT_PAD - layout.width;
    let cy = row_rect.center().y;
    walk_chip(ui.painter(), &layout, cache.detached, left, cy, true);
    let hit = Rect::from_min_max(
        Pos2::new(left - 4.0, cy - 10.0),
        Pos2::new(row_rect.max.x - BG_INSET, cy + 10.0),
    );
    let response = ui.interact(hit, ui.make_persistent_id(("git_line", workspace.0)), Sense::hover());
    response.on_hover_ui_at_pointer(|ui| show_tooltip(ui, &cache.tooltip, cache.changed_at));
}

/// Branch chip as the first child row (when it does not fit on the header).
fn paint_git_line(ui: &mut Ui, cache: &GitLineCache, workspace: WorkspaceId) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GIT_LINE_HEIGHT), Sense::hover());
    let layout = chip_layout(ui.painter(), cache, ui.ctx().pixels_per_point());
    walk_chip(
        ui.painter(),
        &layout,
        cache.detached,
        rect.min.x + LINE_INDENT,
        rect.center().y,
        true,
    );
    let response = ui.interact(rect, ui.make_persistent_id(("git_line", workspace.0)), Sense::hover());
    response.on_hover_ui_at_pointer(|ui| show_tooltip(ui, &cache.tooltip, cache.changed_at));
}

// ---------------------------------------------------------------------------
// Toggles and show-all rows
// ---------------------------------------------------------------------------

fn inset_rect(rect: Rect, left: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.min.x + left, rect.min.y),
        Pos2::new(rect.max.x - ROW_RIGHT_INSET, rect.max.y),
    )
}

/// Expand/collapse row (`▸`/`▾` painted as a triangle). Returns true on click.
fn toggle_row(ui: &mut Ui, id: Id, indent: f32, label: &str, count: &str, open: bool) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GIT_LINE_HEIGHT), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::CollapsingHeader, true, label));
    let hovered = response.hovered();
    let painter = ui.painter();
    let bg = inset_rect(rect, BG_INSET);
    if hovered {
        painter.rect_filled(
            bg,
            CornerRadius::same(ROW_RADIUS),
            theme::alpha(theme::PANEL_BG_ALT(), 160),
        );
    }
    let color = if hovered { theme::FG_SOFT() } else { theme::FG_DIM() };
    let (x, cy) = (rect.min.x + indent, rect.center().y);
    let points = if open {
        vec![
            Pos2::new(x, cy - 2.0),
            Pos2::new(x + 7.0, cy - 2.0),
            Pos2::new(x + 3.5, cy + 2.5),
        ]
    } else {
        vec![
            Pos2::new(x + 1.5, cy - 3.5),
            Pos2::new(x + 1.5, cy + 3.5),
            Pos2::new(x + 6.0, cy),
        ]
    };
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    let text = painter.text(
        Pos2::new(x + LABEL_OFFSET, cy),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(10.5),
        color,
    );
    if !count.is_empty() {
        painter.text(
            Pos2::new(text.right() + 6.0, cy),
            Align2::LEFT_CENTER,
            count,
            FontId::monospace(10.0),
            theme::FG_DIM(),
        );
    }
    hub_ui::paint_focus_ring(painter, &response, bg, ROW_RADIUS);
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.clicked()
}

/// "Show all (N)" / "Show less" row ending a truncated group.
fn show_all_row(ui: &mut Ui, workspace: WorkspaceId, group: &str, all: bool, label: &str) -> bool {
    let id = ui.make_persistent_id(("git_wt_show_all", workspace.0, group));
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GIT_LINE_HEIGHT), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    let text = if all { "Show less" } else { label };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    let hovered = response.hovered();
    let painter = ui.painter();
    let bg = inset_rect(rect, ROW_FILL_INDENT);
    if hovered {
        painter.rect_filled(
            bg,
            CornerRadius::same(ROW_RADIUS),
            theme::alpha(theme::PANEL_BG_ALT(), 160),
        );
    }
    painter.text(
        Pos2::new(rect.min.x + ROW_INDENT, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(10.0),
        if hovered { theme::FG_SOFT() } else { theme::FG_DIM() },
    );
    hub_ui::paint_focus_ring(painter, &response, bg, ROW_RADIUS);
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.clicked()
}

// ---------------------------------------------------------------------------
// Worktree rows
// ---------------------------------------------------------------------------

/// Padlock: filled body with a polyline shackle. `right` is its right edge.
fn paint_lock(painter: &Painter, right: f32, cy: f32, color: Color32) {
    let left = right - 7.0;
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(left, cy - 0.5), Pos2::new(right, cy + 4.5)),
        CornerRadius::same(1),
        color,
    );
    painter.add(Shape::line(
        vec![
            Pos2::new(left + 1.5, cy - 0.5),
            Pos2::new(left + 1.5, cy - 3.0),
            Pos2::new(right - 1.5, cy - 3.0),
            Pos2::new(right - 1.5, cy - 0.5),
        ],
        Stroke::new(1.2_f32, color),
    ));
}

fn paint_worktree_row(ui: &mut Ui, env: &RowEnv<'_>, rows: &[WorktreeRow], index: usize) -> Option<GitAction> {
    let row = rows.get(index)?;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::hover());
    // Rows scrolled out of the sidebar are not painted or interacted with.
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let id = ui.make_persistent_id(("git_wt_row", env.workspace.0, row.key));
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, !row.prunable, &row.label));
    let hovered = response.hovered();
    let clickable = !row.prunable;
    let bg = inset_rect(rect, ROW_FILL_INDENT);
    let cy = rect.center().y;
    let fade = |color: Color32| if row.prunable { theme::alpha(color, 140) } else { color };

    let painter = ui.painter();
    let fill = match (row.is_current, hovered) {
        (true, true) => Some(theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.22)),
        (true, false) => Some(theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.14)),
        (false, true) => Some(theme::PANEL_BG_ALT()),
        (false, false) => None,
    };
    if let Some(fill) = fill {
        painter.rect_filled(bg, CornerRadius::same(ROW_RADIUS), fill);
    }
    if row.is_current {
        let bar = Rect::from_min_size(Pos2::new(bg.min.x, bg.min.y + 3.0), Vec2::new(3.0, bg.height() - 6.0));
        painter.rect_filled(bar, CornerRadius::same(2), theme::ACCENT());
    }

    // Right-aligned markers: dirty dot, lock, prunable pill.
    let mut right = bg.max.x - 8.0;
    if row.dirty {
        painter.circle_filled(Pos2::new(right - 3.0, cy), 3.0, fade(theme::PALETTE_YELLOW()));
        right -= 12.0;
    }
    if row.locked {
        paint_lock(painter, right, cy, fade(theme::FG_DIM()));
        right -= 14.0;
    }
    if row.prunable {
        let width = hub_ui::pill_width(painter, "prunable");
        let pill = hub_ui::pill_rect(Pos2::new(right - width, cy - 9.0), width);
        hub_ui::pill(painter, pill, "prunable", theme::FG_DIM());
        right = pill.min.x - 6.0;
    }

    let clip = Rect::from_min_max(bg.min, Pos2::new(right, rect.max.y));
    let clipped = painter.with_clip_rect(clip);
    let label_color = if row.detached { theme::FG_DIM() } else { theme::FG() };
    let label = clipped.text(
        Pos2::new(rect.min.x + ROW_INDENT, cy),
        Align2::LEFT_CENTER,
        &row.label,
        FontId::proportional(10.5),
        fade(label_color),
    );
    let path_x = label.right() + 8.0;
    if path_x < right {
        if env.copied.is_some_and(|copied| copied.key == row.key) {
            clipped.text(
                Pos2::new(path_x, cy),
                Align2::LEFT_CENTER,
                "Copied",
                FontId::monospace(10.0),
                theme::PALETTE_GREEN(),
            );
        } else {
            clipped.text(
                Pos2::new(path_x, cy),
                Align2::LEFT_CENTER,
                &row.short_path,
                FontId::monospace(10.0),
                fade(theme::FG_DIM()),
            );
        }
    }
    hub_ui::paint_focus_ring(painter, &response, bg, ROW_RADIUS);
    if hovered && clickable {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let mut action = None;
    if response.clicked() && clickable {
        action = Some(GitAction {
            workspace: env.workspace,
            path: row.path.clone(),
            kind: GitActionKind::OpenTerminal,
        });
    }
    if let Some(picked) = row_menu::show(&response, env, row) {
        action = Some(picked);
    }
    response
        .clone()
        .on_hover_ui_at_pointer(|ui| show_tooltip(ui, &row.tooltip, env.changed_at));
    action
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

/// "Changed 12 s ago" style text for the hover card. The poller only delivers
/// snapshots that differ from the last one, so this is the time of the last change.
fn changed_text(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=1 => "Changed just now".to_string(),
        2..=59 => format!("Changed {secs} s ago"),
        60..=3599 => format!("Changed {} min ago", secs / 60),
        _ => format!("Changed {} h ago", secs / 3600),
    }
}

fn badge_color(badge: &str) -> Color32 {
    match badge {
        "current" => theme::ACCENT(),
        "locked" => theme::PALETTE_YELLOW(),
        "prunable" => theme::FG_DIM(),
        _ => theme::FG_SOFT(),
    }
}

fn badge_pill(ui: &mut Ui, text: &str) {
    let width = hub_ui::pill_width(ui.painter(), text);
    let (rect, _) = ui.allocate_exact_size(hub_ui::pill_rect(Pos2::ZERO, width).size(), Sense::hover());
    hub_ui::pill(ui.painter(), rect, text, badge_color(text));
}

fn show_tooltip(ui: &mut Ui, tip: &TooltipData, changed_at: Instant) {
    Frame::new()
        .inner_margin(Margin::same(TOOLTIP_MARGIN - POPUP_FRAME_MARGIN))
        .show(ui, |ui| {
            ui.set_width(TOOLTIP_WIDTH - f32::from(TOOLTIP_MARGIN) * 2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&tip.title).size(13.0).strong().color(theme::FG()));
                for badge in &tip.badges {
                    badge_pill(ui, badge);
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&tip.path).monospace().size(10.5).color(theme::FG_DIM()));
            if tip.upstream.is_some() || tip.changes.is_some() {
                ui.add_space(4.0);
            }
            for line in [&tip.upstream, &tip.changes].into_iter().flatten() {
                ui.label(RichText::new(line).size(11.0).color(theme::FG_SOFT()));
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(changed_text(changed_at.elapsed()))
                    .size(10.5)
                    .color(theme::FG_DIM()),
            );
            ui.label(
                RichText::new("Click: open terminal \u{b7} Right-click: more")
                    .size(10.0)
                    .color(theme::FG_DIM()),
            );
        });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::changed_text;

    #[test]
    fn changed_text_scales_units() {
        assert_eq!(changed_text(Duration::from_secs(0)), "Changed just now");
        assert_eq!(changed_text(Duration::from_secs(12)), "Changed 12 s ago");
        assert_eq!(changed_text(Duration::from_secs(180)), "Changed 3 min ago");
        assert_eq!(changed_text(Duration::from_secs(7300)), "Changed 2 h ago");
    }
}
