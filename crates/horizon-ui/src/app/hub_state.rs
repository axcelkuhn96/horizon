//! State owned by the claude-hub features; later tasks add their fields here.

use horizon_core::WorkspaceId;

#[derive(Default)]
pub(super) struct HubState {
    #[expect(dead_code, reason = "wired in Task 12")]
    pub(super) pending_delete_workspace: Option<WorkspaceId>,
}
