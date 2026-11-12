//! Git overview (branch, dirty, ahead/behind, worktrees) for a working
//! directory, a background poller, and worktree create/cleanup operations.

mod compute;
mod model;
mod poller;
mod worktree_ops;

pub use compute::compute_repo_overview;
pub use model::{GitOverviewError, RepoOverview, WorktreeInfo};
pub use poller::{DEFAULT_INTERVAL, OverviewPoller};
pub use worktree_ops::{
    Blocker, BranchInfo, CleanupCandidate, CleanupReason, CreateRequest, Created, IgnoredFiles, RemoveOutcome,
    RemoveStatus, WorktreeError, classify_cleanup, create_worktree, list_branches, plan_worktree_path,
    remove_worktrees, validate_branch_name,
};
