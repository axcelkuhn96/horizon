//! Workspace header row of the sidebar: caret, accent bar, name, badge, and
//! the panel count of a collapsed workspace.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Vec2};

use super::rows::{BG_INSET, HEADER_RIGHT_PAD, collapsed_count_label};
use super::{WorkspaceSidebarEntry, theme};

pub(super) struct SidebarHeaderRowOutcome {
    pub(super) hovered: bool,
    pub(super) row_clicked: bool,
    /// Hit-rect of the collapse caret. The caret is painted (not interactive)
    /// here; the caller runs the click interaction on this rect *after* the
    /// full-row interaction so the caret sits on top and wins overlapping
    /// clicks instead of falling through to the workspace-focus handler.
    pub(super) caret_rect: Rect,
    /// Right edge of the name (and badge); the branch chip needs the room after it.
    pub(super) name_right: f32,
    /// Left edge of the panel count of a collapsed workspace.
    pub(super) count_left: Option<f32>,
}

impl Default for SidebarHeaderRowOutcome {
    fn default() -> Self {
        Self {
            hovered: false,
            row_clicked: false,
            caret_rect: Rect::NOTHING,
            name_right: 0.0,
            count_left: None,
        }
    }
}

/// Render the inner content of a workspace header row: the collapse caret, the
/// accent bar, the workspace name, the optional "NEW WINDOW" badge and, when
/// collapsed, the panel count. Returns which affordances were interacted with
/// so the caller can route actions.
pub(super) fn render_sidebar_workspace_header_row(
    ui: &mut egui::Ui,
    workspace: &WorkspaceSidebarEntry,
) -> SidebarHeaderRowOutcome {
    let mut outcome = SidebarHeaderRowOutcome::default();
    let row_rect = ui.max_rect();

    ui.add_space(8.0);
    let caret = if workspace.sidebar_collapsed {
        "\u{25B6}"
    } else {
        "\u{25BC}"
    };
    // Paint the caret glyph and remember its rect; the click hit-test runs in
    // the caller (on top of the row) so the row drag region cannot swallow it.
    // The glyph brightens on hover, mirroring the titlebar collapse caret.
    let caret_rect = ui.allocate_space(Vec2::new(11.0, 22.0)).1;
    let caret_color = if ui.rect_contains_pointer(caret_rect.expand(4.0)) || workspace.is_active {
        theme::FG_SOFT()
    } else {
        theme::alpha(theme::FG_DIM(), 200)
    };
    ui.painter().text(
        caret_rect.center(),
        Align2::CENTER_CENTER,
        caret,
        FontId::proportional(9.0),
        caret_color,
    );
    outcome.caret_rect = caret_rect;
    ui.add_space(4.0);

    let bar_color = if workspace.attention_count > 0 {
        theme::PALETTE_RED()
    } else {
        theme::alpha(workspace.color, if workspace.is_active { 240 } else { 110 })
    };
    let bar_rect = ui.allocate_space(Vec2::new(3.0, 22.0)).1;
    ui.painter().rect_filled(bar_rect, CornerRadius::same(2), bar_color);

    ui.add_space(8.0);
    let name_response = ui.add(
        egui::Label::new(
            egui::RichText::new(&workspace.name)
                .color(if workspace.is_active {
                    theme::FG()
                } else {
                    theme::FG_SOFT()
                })
                .size(13.0)
                .strong(),
        )
        .sense(Sense::click()),
    );
    outcome.hovered |= name_response.hovered();
    outcome.row_clicked |= name_response.clicked();
    outcome.name_right = name_response.rect.max.x;

    if workspace.detached {
        ui.add_space(4.0);
        let detached_response = ui.add(
            egui::Label::new(
                egui::RichText::new("NEW WINDOW")
                    .color(theme::FG_DIM())
                    .size(8.5)
                    .strong(),
            )
            .sense(Sense::click()),
        );
        outcome.hovered |= detached_response.hovered();
        outcome.row_clicked |= detached_response.clicked();
        outcome.name_right = detached_response.rect.max.x;
    }

    if workspace.sidebar_collapsed {
        let count = ui.painter().text(
            Pos2::new(row_rect.max.x - HEADER_RIGHT_PAD, row_rect.center().y),
            Align2::RIGHT_CENTER,
            collapsed_count_label(workspace.panels.len()),
            FontId::monospace(10.5),
            theme::FG_DIM(),
        );
        outcome.count_left = Some(count.min.x);
    }

    outcome
}

pub(super) fn paint_workspace_row_bg(
    ui: &mut egui::Ui,
    workspace_rect: Rect,
    workspace_color: Color32,
    is_active: bool,
    hovered: bool,
    dragging: bool,
) {
    let workspace_bg = Rect::from_min_max(
        Pos2::new(workspace_rect.min.x + BG_INSET, workspace_rect.min.y),
        Pos2::new(workspace_rect.max.x - BG_INSET, workspace_rect.max.y),
    );
    if dragging {
        ui.painter_at(workspace_bg).rect_filled(
            workspace_bg,
            CornerRadius::same(10),
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), workspace_color, 0.18), 180),
        );
    } else if is_active {
        ui.painter_at(workspace_bg).rect_filled(
            workspace_bg,
            CornerRadius::same(10),
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), workspace_color, 0.12), 140),
        );
    } else if hovered {
        ui.painter_at(workspace_bg).rect_filled(
            workspace_bg,
            CornerRadius::same(10),
            theme::alpha(theme::PANEL_BG_ALT(), 160),
        );
    }
}
