//! Read-only git overview (branch, dirty, ahead/behind, worktrees) for a
//! working directory, plus a background poller.

mod compute;
mod model;
mod poller;

pub use compute::compute_repo_overview;
pub use model::{GitOverviewError, RepoOverview, WorktreeInfo};
pub use poller::{DEFAULT_INTERVAL, OverviewPoller};
