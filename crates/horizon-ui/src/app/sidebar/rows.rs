//! Sidebar panel rows: layout math, painted icons and the row itself.
//!
//! One metric set for every row of the sidebar: 30 px workspace header, 26 px
//! panel rows, a 6 px gap between workspace groups and a single 20 px indent for
//! all child rows (git line, worktrees toggle, panels).

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Painter, Pos2, Rect, Sense, Stroke, Vec2, WidgetInfo,
    WidgetType,
};
use horizon_core::agent_status::AgentStatus;
use horizon_core::{AttentionItem, PanelId, PanelKind};

use super::labels::ResolvedLabel;
use super::{HorizonApp, SidebarActions, WorkspaceSidebarEntry, sidebar_attention_tag};
use crate::app::agent_status_ui::{SIDEBAR_DOT_RADIUS, SIDEBAR_STATUS_COLUMN, paint_status_dot, status_label};
use crate::app::hub_ui;
use crate::app::panels::panel_kind_icon;
use crate::theme;

pub(super) const HEADER_HEIGHT: f32 = 30.0;
pub(super) const ROW_HEIGHT: f32 = 26.0;
/// Second line of a row that carries an attention item.
const ATTENTION_EXTRA: f32 = 16.0;
pub(super) const GROUP_GAP: f32 = 6.0;
/// Horizontal inset of every row highlight inside the sidebar.
pub(super) const BG_INSET: f32 = 6.0;
/// Indent of all child rows, measured from the left edge of the highlight.
const CHILD_INDENT: f32 = 20.0;
/// Left edge of child row content, measured from the row rect.
pub(super) const CHILD_X: f32 = BG_INSET + CHILD_INDENT;
pub(super) const ICON_WIDTH: f32 = 14.0;
const ICON_GAP: f32 = 4.0;
/// Distance from the child edge to the label text (icon box plus gap).
pub(super) const LABEL_OFFSET: f32 = ICON_WIDTH + ICON_GAP;
const CLOSE_SIZE: f32 = 18.0;
const ROW_RIGHT_PAD: f32 = 6.0;
const HINT_GAP: f32 = 8.0;
const MIN_LABEL_WIDTH: f32 = 24.0;
const ROW_RADIUS: u8 = 8;
/// Distance of the right-aligned header content (branch chip, panel count)
/// from the right edge of the row.
pub(super) const HEADER_RIGHT_PAD: f32 = 14.0;
/// Space kept between the workspace name and a branch chip.
const CHIP_GAP: f32 = 10.0;

// ---------------------------------------------------------------------------
// Pure layout
// ---------------------------------------------------------------------------

/// Where the git branch chip goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitChipPlacement {
    /// Right side of the workspace header row.
    Header,
    /// First child row, at the child indent.
    ChildRow,
}

/// Header placement when the chip fits between the end of the workspace name
/// (`name_right`) and the right padding of the header row; first child row otherwise.
#[must_use]
pub(super) fn git_chip_placement(row: Rect, name_right: f32, chip_width: Option<f32>) -> GitChipPlacement {
    let room = row.max.x - HEADER_RIGHT_PAD - name_right - CHIP_GAP;
    match chip_width {
        Some(width) if width <= room => GitChipPlacement::Header,
        _ => GitChipPlacement::ChildRow,
    }
}

/// `(4)` shown on a collapsed workspace header.
#[must_use]
pub(super) fn collapsed_count_label(panels: usize) -> String {
    format!("({panels})")
}

/// Where the parts of a panel row sit.
#[derive(Clone, Copy, Debug)]
pub(super) struct PanelRowLayout {
    pub(super) icon: Rect,
    pub(super) label_left: f32,
    pub(super) label_width: f32,
    /// Right edge of the dim hint text.
    pub(super) hint_right: f32,
    pub(super) close: Rect,
    /// Fixed column of the agent status dot, left of the close slot; zero width when the
    /// workspace has no agent status.
    pub(super) status: Rect,
}

#[must_use]
pub(super) fn panel_row_layout(row: Rect, hint_width: f32, status_width: f32) -> PanelRowLayout {
    let bg_right = row.max.x - BG_INSET;
    let center_y = row.min.y + ROW_HEIGHT / 2.0;
    let close = Rect::from_center_size(
        Pos2::new(bg_right - ROW_RIGHT_PAD - CLOSE_SIZE / 2.0, center_y),
        Vec2::splat(CLOSE_SIZE),
    );
    let icon = Rect::from_min_size(
        Pos2::new(row.min.x + CHILD_X, row.min.y),
        Vec2::new(ICON_WIDTH, ROW_HEIGHT),
    );
    let label_left = icon.max.x + ICON_GAP;
    let status = Rect::from_min_size(
        Pos2::new(close.min.x - 4.0 - status_width, row.min.y),
        Vec2::new(status_width, ROW_HEIGHT),
    );
    let hint_right = if status_width > 0.0 {
        status.min.x
    } else {
        close.min.x - 4.0
    };
    let label_right = if hint_width > 0.0 {
        hint_right - hint_width - HINT_GAP
    } else {
        hint_right
    };
    PanelRowLayout {
        icon,
        label_left,
        label_width: (label_right - label_left).max(MIN_LABEL_WIDTH),
        hint_right,
        close,
        status,
    }
}

/// Row height: the 26 px line, plus the attention line when there is one.
#[must_use]
pub(super) fn panel_row_height(has_attention: bool) -> f32 {
    if has_attention {
        ROW_HEIGHT + ATTENTION_EXTRA
    } else {
        ROW_HEIGHT
    }
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

/// Icon of a panel row.
#[derive(Clone, Copy)]
pub(super) enum RowIcon {
    Kind(PanelKind),
    /// Claude panel; the asterisk takes the profile accent.
    Claude(Color32),
}

fn paint_shell_icon(painter: &Painter, center: Pos2, color: Color32) {
    let stroke = Stroke::new(1.4_f32, color);
    painter.line_segment(
        [
            Pos2::new(center.x - 5.0, center.y - 3.5),
            Pos2::new(center.x - 1.5, center.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - 1.5, center.y),
            Pos2::new(center.x - 5.0, center.y + 3.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x + 0.5, center.y + 3.5),
            Pos2::new(center.x + 5.5, center.y + 3.5),
        ],
        stroke,
    );
}

fn paint_files_icon(painter: &Painter, center: Pos2, color: Color32) {
    let body = Rect::from_center_size(center, Vec2::new(11.0, 10.0));
    painter.rect_stroke(
        body,
        CornerRadius::same(2),
        Stroke::new(1.2_f32, color),
        egui::StrokeKind::Inside,
    );
    for dy in [-1.5_f32, 1.8_f32] {
        painter.line_segment(
            [
                Pos2::new(body.min.x + 2.5, center.y + dy),
                Pos2::new(body.max.x - 2.5, center.y + dy),
            ],
            Stroke::new(1.0_f32, color),
        );
    }
}

/// Eight-spoke asterisk.
fn paint_claude_icon(painter: &Painter, center: Pos2, color: Color32) {
    let stroke = Stroke::new(1.4_f32, color);
    let radius = 5.0_f32;
    for step in 0..4_u8 {
        let angle = std::f32::consts::FRAC_PI_4 * f32::from(step);
        let spoke = Vec2::angled(angle) * radius;
        painter.line_segment([center - spoke, center + spoke], stroke);
    }
}

fn paint_text_icon(painter: &Painter, center: Pos2, label: &str, color: Color32) {
    let size = if label.chars().count() <= 2 { 10.0 } else { 8.0 };
    painter.text(center, Align2::CENTER_CENTER, label, FontId::monospace(size), color);
}

fn paint_row_icon(painter: &Painter, icon: Rect, row_icon: RowIcon, workspace_color: Color32, focused: bool) {
    let center = icon.center();
    match row_icon {
        RowIcon::Claude(accent) => {
            paint_claude_icon(painter, center, theme::alpha(accent, if focused { 255 } else { 205 }));
        }
        RowIcon::Kind(kind) => {
            let (label, color) = panel_kind_icon(kind, workspace_color, focused);
            match kind {
                PanelKind::Shell | PanelKind::Command => paint_shell_icon(painter, center, color),
                PanelKind::FileExplorer => paint_files_icon(painter, center, color),
                _ => paint_text_icon(painter, center, label, color),
            }
        }
    }
}

fn paint_close_cross(painter: &Painter, rect: Rect, color: Color32) {
    let half = 4.0_f32;
    let center = rect.center();
    let stroke = Stroke::new(1.5_f32, color);
    painter.line_segment(
        [
            Pos2::new(center.x - half, center.y - half),
            Pos2::new(center.x + half, center.y + half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - half, center.y + half),
            Pos2::new(center.x + half, center.y - half),
        ],
        stroke,
    );
}

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

/// A panel row as the sidebar draws it.
#[derive(Clone)]
pub(super) struct SidebarPanelEntry {
    pub(super) id: PanelId,
    pub(super) label: ResolvedLabel,
    pub(super) icon: RowIcon,
    pub(super) kind: PanelKind,
    pub(super) is_focused: bool,
    pub(super) attention: Option<AttentionItem>,
}

fn paint_panel_row_bg(painter: &Painter, row: Rect, workspace_color: Color32, is_focused: bool, hovered: bool) {
    let bg_rect = Rect::from_min_max(
        Pos2::new(row.min.x + BG_INSET, row.min.y + 1.0),
        Pos2::new(row.max.x - BG_INSET, row.max.y - 1.0),
    );
    if is_focused {
        painter.rect_filled(
            bg_rect,
            CornerRadius::same(ROW_RADIUS),
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), workspace_color, 0.22), 200),
        );
        let edge = Rect::from_min_size(
            Pos2::new(bg_rect.min.x, bg_rect.min.y + 4.0),
            Vec2::new(2.0, bg_rect.height() - 8.0),
        );
        painter.rect_filled(edge, CornerRadius::same(1), workspace_color);
    } else if hovered {
        painter.rect_filled(
            bg_rect,
            CornerRadius::same(ROW_RADIUS),
            theme::alpha(theme::PANEL_BG_ALT(), 180),
        );
    }
}

/// Paints the label and the dim hint of a row. Returns whether the label was
/// cut to fit (the full text then goes on the hover tooltip).
fn paint_row_texts(
    painter: &Painter,
    row: Rect,
    layout: &PanelRowLayout,
    panel: &SidebarPanelEntry,
    label: Option<Arc<Galley>>,
    hint: Option<Arc<Galley>>,
) -> bool {
    let center_y = row.min.y + ROW_HEIGHT / 2.0;
    let mut truncated = false;
    if let Some(galley) = label {
        truncated = galley.elided;
        let color = if panel.is_focused {
            theme::FG()
        } else {
            theme::FG_SOFT()
        };
        let pos = Pos2::new(layout.label_left, center_y - galley.size().y / 2.0);
        painter.galley(pos, galley, color);
    }
    if let Some(galley) = hint {
        let pos = Pos2::new(layout.hint_right - galley.size().x, center_y - galley.size().y / 2.0);
        let color = if panel.is_focused {
            theme::FG_SOFT()
        } else {
            theme::FG_DIM()
        };
        painter.galley(pos, galley, color);
    }
    truncated
}

fn paint_close_button(painter: &Painter, rect: Rect, hovered: bool) {
    if hovered {
        painter.rect_filled(rect, CornerRadius::same(6), theme::alpha(theme::PANEL_BG_ALT(), 220));
    }
    paint_close_cross(
        painter,
        rect,
        if hovered { theme::BTN_CLOSE() } else { theme::FG_DIM() },
    );
}

impl HorizonApp {
    pub(super) fn render_sidebar_panel(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &WorkspaceSidebarEntry,
        workspace_data: &[WorkspaceSidebarEntry],
        panel: &SidebarPanelEntry,
        actions: &mut SidebarActions,
    ) {
        let row_rect = ui
            .allocate_space(Vec2::new(
                ui.available_width(),
                panel_row_height(panel.attention.is_some()),
            ))
            .1;
        let hovered = ui.rect_contains_pointer(row_rect);
        let painter = ui.painter_at(row_rect);
        let ppp = ui.ctx().pixels_per_point();

        let (status, status_seen) = self
            .board
            .panel(panel.id)
            .map_or((AgentStatus::Unknown, true), |entry| {
                (entry.agent_status(), entry.agent_status_seen())
            });
        // The column is reserved on every row of a workspace that has an agent status, so rows
        // do not shift when a state changes.
        let status_width = if self.workspace_shows_status(workspace) {
            SIDEBAR_STATUS_COLUMN
        } else {
            0.0
        };

        let labels = &mut self.hub.sidebar_labels;
        let hint_galley = labels.hint_galley(panel.id, &painter, ppp);
        let hint_width = hint_galley.as_ref().map_or(0.0, |galley| galley.size().x);
        let layout = panel_row_layout(row_rect, hint_width, status_width);
        let label_galley = labels.label_galley(panel.id, &painter, ppp, layout.label_width);

        // Row interaction first; the close button is added after it so it wins overlapping clicks.
        let row_response = ui.interact(
            row_rect,
            ui.make_persistent_id(("sidebar_panel_click", panel.id.0)),
            Sense::click(),
        );
        let hovered = hovered || row_response.hovered();
        let show_close = hovered || panel.is_focused;
        let close_response = show_close.then(|| {
            ui.interact(
                layout.close,
                ui.make_persistent_id(("sidebar_panel_close", panel.id.0)),
                Sense::click(),
            )
        });
        let close_hovered = close_response.as_ref().is_some_and(egui::Response::hovered);

        paint_panel_row_bg(&painter, row_rect, workspace.color, panel.is_focused, hovered);
        paint_row_icon(&painter, layout.icon, panel.icon, workspace.color, panel.is_focused);
        let label_truncated = paint_row_texts(&painter, row_rect, &layout, panel, label_galley, hint_galley);
        paint_status_dot(
            &painter,
            layout.status.center(),
            SIDEBAR_DOT_RADIUS,
            status,
            status_seen,
        );
        if status != AgentStatus::Unknown && status_width > 0.0 {
            // Hover-only, added after the row so it sits on top without taking its clicks.
            let status_response = ui.interact(
                layout.status,
                ui.make_persistent_id(("sidebar_panel_status", panel.id.0)),
                Sense::hover(),
            );
            status_response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, status_label(status)));
            status_response.on_hover_text(status_label(status));
        }
        if show_close {
            paint_close_button(&painter, layout.close, close_hovered);
        }
        if let Some(attention_item) = &panel.attention {
            paint_attention_line(&painter, row_rect, layout.label_left, attention_item);
        }

        row_response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &*panel.label.label));
        // The ring is painted outside the row; the row painter would clip it.
        hub_ui::paint_focus_ring(
            ui.painter(),
            &row_response,
            Rect::from_min_max(
                Pos2::new(row_rect.min.x + BG_INSET, row_rect.min.y + 1.0),
                Pos2::new(row_rect.max.x - BG_INSET, row_rect.max.y - 1.0),
            ),
            ROW_RADIUS,
        );
        if let Some(close) = &close_response {
            close.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close panel"));
            hub_ui::paint_focus_ring(ui.painter(), close, layout.close, 6);
        }
        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        let close_clicked = close_response.as_ref().is_some_and(egui::Response::clicked);
        if close_clicked {
            actions.close_panel = Some(panel.id);
        } else if row_response.clicked() {
            actions.focus_panel = Some(panel.id);
            actions.pan_to_panel = Some(panel.id);
        }

        let row_response = if label_truncated && !close_hovered {
            row_response.on_hover_text(&*panel.label.label)
        } else {
            row_response
        };
        self.show_sidebar_panel_context_menu(&row_response, workspace, workspace_data, panel.id, panel.kind, actions);
    }
}

impl HorizonApp {
    /// `true` when any panel of `workspace` has an agent status to show.
    fn workspace_shows_status(&self, workspace: &WorkspaceSidebarEntry) -> bool {
        workspace.panels.iter().any(|entry| {
            self.board
                .panel(entry.id)
                .is_some_and(|panel| panel.agent_status() != AgentStatus::Unknown)
        })
    }
}

fn paint_attention_line(painter: &Painter, row: Rect, left: f32, item: &AttentionItem) {
    let (label, color) = sidebar_attention_tag(item.severity);
    let center_y = row.min.y + ROW_HEIGHT + ATTENTION_EXTRA / 2.0 - 2.0;
    let tag = painter.text(
        Pos2::new(left, center_y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(8.5),
        color,
    );
    let summary_left = tag.max.x + 4.0;
    let width = (row.max.x - BG_INSET - ROW_RIGHT_PAD - summary_left).max(MIN_LABEL_WIDTH);
    let galley = hub_ui::elided(painter, &item.summary, FontId::proportional(9.0), width);
    painter.galley(
        Pos2::new(summary_left, center_y - galley.size().y / 2.0),
        galley,
        theme::alpha(color, 180),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(width: f32) -> Rect {
        Rect::from_min_size(Pos2::new(0.0, 100.0), Vec2::new(width, ROW_HEIGHT))
    }

    #[test]
    fn row_metrics_follow_the_spec() {
        assert!((HEADER_HEIGHT - 30.0).abs() < f32::EPSILON);
        assert!((ROW_HEIGHT - 26.0).abs() < f32::EPSILON);
        assert!((GROUP_GAP - 6.0).abs() < f32::EPSILON);
        assert!((CHILD_INDENT - 20.0).abs() < f32::EPSILON);
        assert!((panel_row_height(false) - 26.0).abs() < f32::EPSILON);
        assert!(panel_row_height(true) > panel_row_height(false));
    }

    #[test]
    fn layout_keeps_close_slot_at_the_right_and_icon_at_the_child_indent() {
        let layout = panel_row_layout(row(260.0), 0.0, 0.0);
        assert!((layout.icon.min.x - CHILD_X).abs() < f32::EPSILON);
        assert!((layout.label_left - (CHILD_X + LABEL_OFFSET)).abs() < f32::EPSILON);
        assert!(layout.close.max.x <= 260.0 - BG_INSET);
        assert!((layout.close.center().y - 113.0).abs() < f32::EPSILON);
        assert!(layout.label_left + layout.label_width <= layout.close.min.x);
    }

    #[test]
    fn hint_takes_width_from_the_label_not_from_the_close_slot() {
        let without = panel_row_layout(row(260.0), 0.0, 0.0);
        let with = panel_row_layout(row(260.0), 40.0, 0.0);
        assert!((without.label_width - with.label_width - (40.0 + HINT_GAP)).abs() < 0.01);
        assert!((with.hint_right - without.hint_right).abs() < f32::EPSILON);
        assert!(with.hint_right <= with.close.min.x);
    }

    #[test]
    fn status_column_sits_between_the_hint_and_the_close_slot() {
        let without = panel_row_layout(row(260.0), 30.0, 0.0);
        let with = panel_row_layout(row(260.0), 30.0, SIDEBAR_STATUS_COLUMN);

        assert!(without.status.width().abs() < f32::EPSILON);
        assert!((with.status.width() - SIDEBAR_STATUS_COLUMN).abs() < f32::EPSILON);
        assert!(with.status.max.x <= with.close.min.x);
        assert!((with.hint_right - with.status.min.x).abs() < f32::EPSILON);
        assert!((without.label_width - with.label_width - SIDEBAR_STATUS_COLUMN).abs() < 0.01);
        assert!((with.status.center().y - 113.0).abs() < f32::EPSILON);
    }

    #[test]
    fn label_width_never_collapses() {
        let layout = panel_row_layout(row(80.0), 60.0, 0.0);
        assert!(layout.label_width >= MIN_LABEL_WIDTH);
    }

    #[test]
    fn git_chip_goes_to_the_header_only_when_it_fits() {
        let header = Rect::from_min_size(Pos2::ZERO, Vec2::new(260.0, HEADER_HEIGHT));
        // Name ends at 90: room = 260 - 14 - 90 - 10 = 146.
        assert_eq!(git_chip_placement(header, 90.0, Some(146.0)), GitChipPlacement::Header);
        assert_eq!(
            git_chip_placement(header, 90.0, Some(147.0)),
            GitChipPlacement::ChildRow
        );
        assert_eq!(git_chip_placement(header, 90.0, None), GitChipPlacement::ChildRow);
        assert_eq!(
            git_chip_placement(header, 300.0, Some(10.0)),
            GitChipPlacement::ChildRow
        );
    }

    #[test]
    fn collapsed_count_label_wraps_the_number() {
        assert_eq!(collapsed_count_label(4), "(4)");
        assert_eq!(collapsed_count_label(0), "(0)");
    }
}
