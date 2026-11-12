//! State owned by the claude-hub features; later tasks add their fields here.

use horizon_core::WorkspaceId;

#[derive(Default)]
pub(super) struct HubState {
    pub(super) pending_delete_workspace: Option<WorkspaceId>,
}
