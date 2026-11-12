//! State owned by the claude-hub features; later tasks add their fields here.

use std::time::Instant;

use horizon_core::WorkspaceId;

#[derive(Default)]
pub(super) struct HubState {
    pub(super) pending_delete_workspace: Option<WorkspaceId>,
    /// Since when the canvas label of the workspace being renamed could not be
    /// drawn; drives the sidebar rename fallback.
    pub(super) rename_label_missing_since: Option<Instant>,
}
