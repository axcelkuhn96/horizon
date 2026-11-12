//! Inline workspace rename that falls back to the sidebar row.
//!
//! Rename normally edits the workspace label on the canvas. When that label
//! cannot be drawn (hidden behind an overlay, culled off-screen), the editor
//! moves to the sidebar row instead of leaving `renaming_workspace` stuck.

use std::time::{Duration, Instant};

use egui::{Context, CornerRadius, FontId, Rect, Ui, Vec2};
use horizon_core::WorkspaceId;

use crate::app::panels::show_inline_rename_editor;
use crate::app::{HorizonApp, RenameEditAction};
use crate::theme;

/// How long the canvas label may stay missing before the sidebar takes over
/// (the pan started by "Rename" needs a moment to bring the label into view).
const LABEL_WAIT: Duration = Duration::from_millis(400);

/// Where the renamed workspace's canvas label stands this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameLabel {
    /// Drawn on the canvas (or edited in a detached window).
    Visible,
    /// Hidden only by the revealed auto-hide sidebar. It collapses by itself once the pointer
    /// leaves, then the canvas editor shows, so this must not pin the sidebar open.
    UnderSidebar,
    /// Off-screen or covered by another overlay: the sidebar row has to host the editor.
    Missing,
}

/// Classify the label from what the canvas pass knows about it. `visual` is `None` when the
/// workspace was culled off-screen; otherwise `(hidden_by_any_overlay, hidden_besides_sidebar)`.
fn classify_rename_label(detached: bool, visual: Option<(bool, bool)>) -> RenameLabel {
    if detached {
        return RenameLabel::Visible;
    }
    match visual {
        None | Some((_, true)) => RenameLabel::Missing,
        Some((true, false)) => RenameLabel::UnderSidebar,
        Some((false, false)) => RenameLabel::Visible,
    }
}

/// Next value of the "label missing since" marker.
pub(in crate::app) fn update_label_missing_since(
    missing_since: Option<Instant>,
    label_visible: bool,
    now: Instant,
) -> Option<Instant> {
    if label_visible {
        None
    } else {
        Some(missing_since.unwrap_or(now))
    }
}

/// Whether the label has been missing long enough to edit in the sidebar.
pub(in crate::app) fn label_wait_elapsed(missing_since: Option<Instant>, now: Instant) -> bool {
    missing_since.is_some_and(|since| now.saturating_duration_since(since) >= LABEL_WAIT)
}

impl HorizonApp {
    /// Called once per frame by the main canvas pass. `visual` is the renamed workspace's
    /// `(label hidden by any overlay, hidden besides the sidebar)`, `None` when it was culled.
    pub(in crate::app) fn track_workspace_rename_label(&mut self, ctx: &Context, visual: Option<(bool, bool)>) {
        let now = Instant::now();
        let label = self
            .renaming_workspace
            .map(|workspace| classify_rename_label(self.workspace_is_detached(workspace), visual));
        let Some(label) = label else {
            self.hub.rename_label_missing_since = None;
            self.sidebar_auto_hide.rename_label_under_sidebar = false;
            return;
        };
        self.sidebar_auto_hide.rename_label_under_sidebar = label == RenameLabel::UnderSidebar;
        let visible = label != RenameLabel::Missing;
        let missing = update_label_missing_since(self.hub.rename_label_missing_since, visible, now);
        self.hub.rename_label_missing_since = missing;
        let Some(since) = missing else {
            return;
        };
        if label_wait_elapsed(missing, now) {
            // No canvas label and no sidebar to edit in: nothing can finish
            // the rename, so cancel instead of leaving it stuck.
            if !self.sidebar_visible {
                self.clear_workspace_rename();
                self.hub.rename_label_missing_since = None;
            }
        } else {
            ctx.request_repaint_after(LABEL_WAIT.saturating_sub(now.saturating_duration_since(since)));
        }
    }

    /// The sidebar row of `workspace` shows the rename editor.
    pub(super) fn workspace_rename_in_sidebar(&self, workspace: WorkspaceId) -> bool {
        self.sidebar_visible
            && self.renaming_workspace == Some(workspace)
            && (self.sidebar_auto_hide.rename_label_under_sidebar
                || label_wait_elapsed(self.hub.rename_label_missing_since, Instant::now()))
    }

    /// Keep the sidebar open while it hosts the rename editor because the label is missing from
    /// the canvas. A label merely covered by the sidebar itself does not pin it.
    pub(super) fn workspace_rename_needs_sidebar(&self) -> bool {
        self.renaming_workspace.is_some_and(|workspace| {
            self.workspace_rename_in_sidebar(workspace) && !self.sidebar_auto_hide.rename_label_under_sidebar
        })
    }

    /// Auto-hide is on and the sidebar is collapsed to its edge strip.
    pub(in crate::app) fn sidebar_collapsed_to_strip(&self) -> bool {
        self.template_config.overlays.sidebar_auto_hide && !self.sidebar_auto_hide.revealed
    }

    pub(super) fn render_sidebar_rename_editor(&mut self, ui: &mut Ui, row_rect: Rect) {
        let edit_rect = Rect::from_min_max(row_rect.min + Vec2::new(30.0, 5.0), row_rect.max - Vec2::new(8.0, 5.0));
        ui.painter().rect_filled(
            edit_rect.expand2(Vec2::new(6.0, 0.0)),
            CornerRadius::same(6),
            theme::PANEL_BG_ALT(),
        );
        let action = show_inline_rename_editor(ui, edit_rect, &mut self.rename_buffer, FontId::proportional(13.0));
        self.apply_workspace_rename_action(action);
    }

    fn apply_workspace_rename_action(&mut self, action: RenameEditAction) {
        match action {
            RenameEditAction::Commit => {
                if let Some(workspace_id) = self.renaming_workspace {
                    let name = self.rename_buffer.trim().to_string();
                    if !name.is_empty() && self.board.rename_workspace(workspace_id, &name) {
                        self.mark_runtime_dirty();
                    }
                    self.clear_workspace_rename();
                }
            }
            RenameEditAction::Cancel => self.clear_workspace_rename(),
            RenameEditAction::None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{RenameLabel, classify_rename_label, label_wait_elapsed, update_label_missing_since};

    #[test]
    fn rename_label_covered_only_by_the_sidebar_does_not_count_as_missing() {
        assert_eq!(
            classify_rename_label(false, Some((true, false))),
            RenameLabel::UnderSidebar
        );
    }

    #[test]
    fn rename_label_off_screen_or_under_another_overlay_is_missing() {
        assert_eq!(classify_rename_label(false, None), RenameLabel::Missing);
        assert_eq!(classify_rename_label(false, Some((true, true))), RenameLabel::Missing);
    }

    #[test]
    fn rename_label_drawn_or_detached_is_visible() {
        assert_eq!(classify_rename_label(false, Some((false, false))), RenameLabel::Visible);
        assert_eq!(classify_rename_label(true, None), RenameLabel::Visible);
    }

    #[test]
    fn workspace_menu_rename_falls_back_to_sidebar_after_label_stays_missing() {
        let start = Instant::now();
        let missing = update_label_missing_since(None, false, start);
        assert_eq!(missing, Some(start));
        assert_eq!(
            update_label_missing_since(missing, false, start + Duration::from_millis(100)),
            Some(start),
            "marker keeps the first missing time"
        );
        assert!(!label_wait_elapsed(missing, start + Duration::from_millis(399)));
        assert!(label_wait_elapsed(missing, start + Duration::from_millis(400)));
    }

    #[test]
    fn workspace_menu_rename_visible_label_resets_the_fallback() {
        let start = Instant::now();
        assert_eq!(update_label_missing_since(Some(start), true, start), None);
        assert!(!label_wait_elapsed(None, start + Duration::from_secs(5)));
    }
}
