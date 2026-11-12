use std::path::PathBuf;

/// One working tree of a repository (the main one or a linked worktree).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// `None` means detached HEAD (or an unreadable worktree).
    pub branch: Option<String>,
    pub is_main: bool,
    /// The queried `cwd` lives inside this worktree.
    pub is_current: bool,
    pub locked: bool,
    /// The worktree directory or its metadata is gone or invalid.
    pub prunable: bool,
    pub dirty_count: usize,
}

/// Snapshot of the repository containing a working directory.
///
/// `branch`, `dirty_count` and the upstream fields describe the working tree
/// that contains the queried directory; `repo_root` is always the main one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoOverview {
    pub repo_root: PathBuf,
    pub branch: Option<String>,
    pub detached: bool,
    pub dirty_count: usize,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    /// Main worktree first, then by branch name.
    pub worktrees: Vec<WorktreeInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitOverviewError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
