//! Inline workspace rename that falls back to the sidebar row.
//!
//! Rename normally edits the workspace label on the canvas. When that label
//! cannot be drawn (hidden behind an overlay, culled off-screen), the editor
//! moves to the sidebar row instead of leaving `renaming_workspace` stuck.

use std::time::{Duration, Instant};

use egui::{Context, CornerRadius, FontId, Rect, Ui, Vec2};
use horizon_core::WorkspaceId;

use super::auto_hide::{RENAME_PIN_IDLE, RenamePin, rename_pin_step};
use crate::app::panels::show_inline_rename_editor;
use crate::app::util;
use crate::app::{HorizonApp, RenameEditAction};
use crate::theme;

/// How long the canvas label may stay missing before the sidebar takes over
/// (the pan started by "Rename" needs a moment to bring the label into view).
const LABEL_WAIT: Duration = Duration::from_millis(400);

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
    /// Called once per frame by the main canvas pass. `label_visible` is
    /// `None` when no workspace is being renamed.
    pub(in crate::app) fn track_workspace_rename_label(&mut self, ctx: &Context, label_visible: Option<bool>) {
        let now = Instant::now();
        let Some(visible) = label_visible else {
            self.hub.rename_label_missing_since = None;
            self.sidebar_auto_hide.rename_pin_anchor = None;
            return;
        };
        let missing = update_label_missing_since(self.hub.rename_label_missing_since, visible, now);
        self.hub.rename_label_missing_since = missing;
        let Some(since) = missing else {
            self.sidebar_auto_hide.rename_pin_anchor = None;
            return;
        };
        if label_wait_elapsed(missing, now) {
            // No canvas label and no sidebar to edit in: nothing can finish
            // the rename, so cancel instead of leaving it stuck.
            if !self.sidebar_visible {
                self.clear_workspace_rename();
                self.hub.rename_label_missing_since = None;
            } else if self.template_config.overlays.sidebar_auto_hide {
                // Without auto-hide the sidebar is always open: nothing to unpin.
                self.limit_sidebar_rename_pin(ctx, now);
            }
        } else {
            ctx.request_repaint_after(LABEL_WAIT.saturating_sub(now.saturating_duration_since(since)));
        }
    }

    /// The sidebar hosts the rename and keeps itself open for it; that pin ends with the rename
    /// or, if nobody finishes it, after [`RENAME_PIN_IDLE`] without pointer or typing activity.
    fn limit_sidebar_rename_pin(&mut self, ctx: &Context, now: Instant) {
        let sidebar_edge = self.reserved_sidebar_width(util::viewport_local_rect(ctx).width());
        let activity = ctx.input(|input| {
            let pointer_on_sidebar = input.pointer.interact_pos().is_some_and(|pos| pos.x <= sidebar_edge);
            let typing = input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)));
            pointer_on_sidebar || typing
        });
        match rename_pin_step(self.sidebar_auto_hide.rename_pin_anchor, activity, now) {
            RenamePin::Hold(anchor) => {
                self.sidebar_auto_hide.rename_pin_anchor = Some(anchor);
                // No input arrives while idle: wake up when the limit would be reached.
                ctx.request_repaint_after(RENAME_PIN_IDLE.saturating_sub(now.saturating_duration_since(anchor)));
            }
            RenamePin::Abandon => {
                self.clear_workspace_rename();
                self.sidebar_auto_hide.rename_pin_anchor = None;
                self.hub.rename_label_missing_since = None;
            }
        }
    }

    /// The sidebar row of `workspace` shows the rename editor.
    pub(super) fn workspace_rename_in_sidebar(&self, workspace: WorkspaceId) -> bool {
        self.sidebar_visible
            && self.renaming_workspace == Some(workspace)
            && label_wait_elapsed(self.hub.rename_label_missing_since, Instant::now())
    }

    /// Keep the sidebar open while it hosts the rename editor.
    pub(super) fn workspace_rename_needs_sidebar(&self) -> bool {
        self.renaming_workspace
            .is_some_and(|workspace| self.workspace_rename_in_sidebar(workspace))
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

    use super::{label_wait_elapsed, update_label_missing_since};

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
