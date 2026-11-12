//! State owned by the claude-hub features; later tasks add their fields here.

use std::collections::HashMap;
use std::time::Instant;

use horizon_core::WorkspaceId;

use super::claude_launcher::ClaudeLauncherState;
use super::hub_ui;
use super::link::LinkUiState;
use super::minimap::MinimapAutoHide;
use super::sidebar::{GitSidebarState, PanelLabelCache};
use super::usage_chips::ClaudeUsageState;

/// Accent slots for the visible profile list, recomputed only when the list changes.
#[derive(Default)]
struct AccentCache {
    names: Vec<String>,
    slots: Vec<usize>,
}

#[derive(Default)]
pub(super) struct HubState {
    pub(super) pending_delete_workspace: Option<WorkspaceId>,
    /// Since when the canvas label of the workspace being renamed could not be
    /// drawn; drives the sidebar rename fallback.
    pub(super) rename_label_missing_since: Option<Instant>,
    /// Open Claude launcher overlay, if any.
    pub(super) claude_launcher: Option<ClaudeLauncherState>,
    /// Profile launched last; the launcher preselects it on the next open.
    pub(super) last_launched_profile: Option<String>,
    accents: AccentCache,
    /// Bumped when the profile list or usage data behind the launcher rows
    /// changes; the open launcher rebuilds its rows when it moves.
    pub(super) rows_version: u64,
    /// Claude usage snapshot, poller and chip layout cache.
    pub(super) usage: ClaudeUsageState,
    /// Git line and worktrees list state of the sidebar.
    pub(super) git: GitSidebarState,
    /// Sidebar panel labels and galleys, rebuilt only when a panel's label inputs change.
    pub(super) sidebar_labels: PanelLabelCache,
    /// Panel-link server, receive badges and the send-to-panel dialog.
    pub(super) link: LinkUiState,
    /// Reveal state of each auto-hiding minimap overlay (root canvas and detached viewports).
    pub(super) minimap_auto_hide: HashMap<egui::Id, MinimapAutoHide>,
}

impl HubState {
    /// Marks the launcher rows stale (profiles or usage changed).
    pub(super) fn invalidate_launcher_rows(&mut self) {
        self.rows_version = self.rows_version.wrapping_add(1);
    }

    /// Accent slot per profile name (list order), cached by the name list.
    pub(super) fn accent_slots(&mut self, names: &[String]) -> &[usize] {
        if self.accents.names != names {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.accents.slots = hub_ui::assign_accent_indices(&refs);
            self.accents.names = names.to_vec();
        }
        &self.accents.slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_slots_follow_name_list_and_are_distinct() {
        let mut hub = HubState::default();
        let names = vec!["conta1".to_owned(), "conta2".to_owned(), "pessoal".to_owned()];

        let slots = hub.accent_slots(&names).to_vec();

        assert_eq!(slots.len(), 3);
        assert_ne!(slots[0], slots[1]);
        assert_ne!(slots[1], slots[2]);
        assert_ne!(slots[0], slots[2]);
    }

    #[test]
    fn invalidating_launcher_rows_moves_the_version() {
        let mut hub = HubState::default();
        let before = hub.rows_version;

        hub.invalidate_launcher_rows();

        assert_ne!(hub.rows_version, before);
    }

    #[test]
    fn accent_slots_recompute_when_the_list_changes() {
        let mut hub = HubState::default();
        let first = vec!["a".to_owned()];
        let second = vec!["a".to_owned(), "b".to_owned()];

        assert_eq!(hub.accent_slots(&first).len(), 1);
        assert_eq!(hub.accent_slots(&second).len(), 2);
        assert_eq!(hub.accents.names, second);
    }
}
