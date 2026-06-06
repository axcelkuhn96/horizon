//! Pure routing/decision helpers for the sidebar. Kept free of egui so they can
//! be unit-tested directly and to keep `sidebar.rs` under the maintainability
//! line limit.

use horizon_core::WorkspaceDockSide;

use super::SidebarWorkspaceInsert;

pub(super) fn sidebar_workspace_insert_dock_side(insert: SidebarWorkspaceInsert) -> WorkspaceDockSide {
    match insert {
        SidebarWorkspaceInsert::Before => WorkspaceDockSide::Left,
        SidebarWorkspaceInsert::After => WorkspaceDockSide::Right,
    }
}

pub(super) fn sidebar_workspace_drop_should_dock(target_detached: bool) -> bool {
    !target_detached
}

/// Decide whether a click on the workspace header row should focus/pan the
/// workspace. A click that landed on the collapse caret (`caret_hit`) toggles
/// the sidebar fold and must not also focus the workspace.
pub(super) fn should_focus_workspace_on_row_click(row_clicked: bool, caret_hit: bool) -> bool {
    row_clicked && !caret_hit
}

#[cfg(test)]
mod tests {
    use super::{
        SidebarWorkspaceInsert, should_focus_workspace_on_row_click, sidebar_workspace_drop_should_dock,
        sidebar_workspace_insert_dock_side,
    };
    use horizon_core::WorkspaceDockSide;

    #[test]
    fn row_click_focuses_workspace_when_caret_not_hit() {
        assert!(should_focus_workspace_on_row_click(true, false));
    }

    #[test]
    fn caret_hit_suppresses_workspace_focus() {
        // The caret click toggles collapse and must NOT focus/pan the workspace.
        assert!(!should_focus_workspace_on_row_click(true, true));
    }

    #[test]
    fn no_row_click_never_focuses_workspace() {
        assert!(!should_focus_workspace_on_row_click(false, false));
        assert!(!should_focus_workspace_on_row_click(false, true));
    }

    #[test]
    fn sidebar_drop_docks_attached_workspace_against_attached_target() {
        assert!(sidebar_workspace_drop_should_dock(false));
    }

    #[test]
    fn sidebar_drop_preserves_detached_workspace_reposition_against_attached_target() {
        assert!(sidebar_workspace_drop_should_dock(false));
    }

    #[test]
    fn sidebar_drop_skips_board_docking_when_target_workspace_is_detached() {
        assert!(!sidebar_workspace_drop_should_dock(true));
    }

    #[test]
    fn sidebar_insert_side_maps_to_expected_dock_side() {
        assert_eq!(
            sidebar_workspace_insert_dock_side(SidebarWorkspaceInsert::Before),
            WorkspaceDockSide::Left
        );
        assert_eq!(
            sidebar_workspace_insert_dock_side(SidebarWorkspaceInsert::After),
            WorkspaceDockSide::Right
        );
    }
}
