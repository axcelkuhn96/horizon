//! Creating worktrees and cleaning up merged, prunable and orphaned ones.
//!
//! Everything goes through libgit2 and the filesystem; nothing here shells out
//! to `git`. The calls block, so callers run them on a worker thread. Removal
//! is deliberately conservative: a worktree is only deleted after it was
//! re-classified moments before, it is registered in the repository, and it is
//! clean, unlocked and not in use. Nothing is ever forced.

mod cleanup;

use std::path::{Path, PathBuf};

use git2::{Branch, BranchType, Commit, ErrorCode, Reference, Repository, WorktreeAddOptions, WorktreePruneOptions};

use super::compute::{head_branch, linked_branch};

pub use cleanup::{
    Blocker, CleanupCandidate, CleanupReason, RemoveOutcome, RemoveStatus, classify_cleanup, remove_worktrees,
};

const MAX_BRANCH_CHARS: usize = 200;
/// Highest `-N` suffix tried when the planned directory is taken.
const MAX_PATH_SUFFIX: usize = 99;
const MAX_LISTED_BRANCHES: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("{0}")]
    InvalidBranchName(&'static str),
    #[error("invalid base revision: {0}")]
    InvalidBase(String),
    #[error("{branch} is already checked out in {}", .path.display())]
    BranchCheckedOut { branch: String, path: PathBuf },
    #[error("destination already exists: {}", .0.display())]
    DestinationExists(PathBuf),
    #[error("parent folder does not exist: {}", .0.display())]
    DestinationParentMissing(PathBuf),
    #[error("invalid destination: {0}")]
    InvalidDestination(&'static str),
    #[error("the repository has no commits yet")]
    NoCommits,
    #[error("not a git repository: {}", .0.display())]
    NotARepository(PathBuf),
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// What to create: `branch` is checked out when it exists and is free, and
/// created from `base` (default `HEAD`) otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRequest {
    pub branch: String,
    pub base: Option<String>,
    /// Absolute directory of the new worktree; must not exist yet.
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub path: PathBuf,
    pub branch: String,
    /// The branch did not exist and was created for this worktree.
    pub created_branch: bool,
}

/// Local branches of a repository, for the "new worktree" form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchInfo {
    /// Alphabetical, capped.
    pub local: Vec<String>,
    /// `origin/HEAD` target, else `main` or `master` when present.
    pub default_branch: Option<String>,
    /// Branch checked out in the main worktree; `None` when detached.
    pub current: Option<String>,
}

/// Directory name for the worktree of `branch`: a sibling of the main
/// checkout named `<repo>-<branch with / as ->`, with `-2`, `-3`, ... added
/// while `exists` says the directory is taken.
#[must_use]
pub fn plan_worktree_path(main_root: &Path, branch: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let parent = main_root.parent().unwrap_or(main_root);
    let repo_name = main_root
        .file_name()
        .map_or_else(|| "repo".to_owned(), |name| name.to_string_lossy().into_owned());
    let slug: String = branch
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    let base = format!("{repo_name}-{slug}");
    let first = parent.join(&base);
    if !exists(&first) {
        return first;
    }
    for suffix in 2..=MAX_PATH_SUFFIX {
        let candidate = parent.join(format!("{base}-{suffix}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    parent.join(format!("{base}-{MAX_PATH_SUFFIX}"))
}

/// Checks `name` as a branch name a user may type.
///
/// # Errors
///
/// Returns [`WorktreeError::InvalidBranchName`] with the reason, or a git
/// error when libgit2 itself fails.
pub fn validate_branch_name(name: &str) -> Result<(), WorktreeError> {
    use WorktreeError::InvalidBranchName as Bad;
    if name.trim().is_empty() {
        return Err(Bad("branch name is empty"));
    }
    if name.chars().count() > MAX_BRANCH_CHARS {
        return Err(Bad("branch name is too long"));
    }
    if name.starts_with('-') {
        return Err(Bad("branch name cannot start with '-'"));
    }
    if name.contains("..") {
        return Err(Bad("branch name cannot contain '..'"));
    }
    if name.chars().any(char::is_control) {
        return Err(Bad("branch name cannot contain control characters"));
    }
    if name == "HEAD" {
        return Err(Bad("HEAD is not a valid branch name"));
    }
    if Branch::name_is_valid(name)? {
        Ok(())
    } else {
        Err(Bad("not a valid git branch name"))
    }
}

/// Local branches and the default branch of the repository at `repo_root`.
///
/// # Errors
///
/// Returns an error when `repo_root` is not a non-bare repository or libgit2
/// fails reading the branches.
pub fn list_branches(repo_root: &Path) -> Result<BranchInfo, WorktreeError> {
    let main = open_main(repo_root)?;
    let mut local = Vec::new();
    for entry in main.branches(Some(BranchType::Local))? {
        let (branch, _) = entry?;
        if let Ok(Some(name)) = branch.name() {
            local.push(name.to_owned());
        }
        if local.len() >= MAX_LISTED_BRANCHES {
            break;
        }
    }
    local.sort();
    let (current, _) = head_branch(&main);
    Ok(BranchInfo {
        local,
        default_branch: cleanup::default_branch_name(&main),
        current,
    })
}

/// Creates the worktree described by `req` in the repository at `repo_root`.
///
/// A failure after the branch was created removes what this call made (the
/// directory, the worktree metadata and the new branch), so no litter stays.
///
/// # Errors
///
/// Returns a [`WorktreeError`]: invalid branch name or base, branch already
/// checked out elsewhere, destination taken or unusable, or a git/io failure.
pub fn create_worktree(repo_root: &Path, req: &CreateRequest) -> Result<Created, WorktreeError> {
    validate_branch_name(&req.branch)?;
    let main = open_main(repo_root)?;
    check_destination(&main, &req.path)?;
    let (reference, created_branch) = resolve_branch(&main, req)?;
    let name = unique_worktree_name(&main, &req.path);
    let mut options = WorktreeAddOptions::new();
    options.reference(Some(&reference));
    match main.worktree(&name, &req.path, Some(&options)) {
        Ok(_) => Ok(Created {
            path: req.path.clone(),
            branch: req.branch.clone(),
            created_branch,
        }),
        Err(error) => {
            rollback(&main, &name, &req.path, created_branch.then_some(req.branch.as_str()));
            Err(error.into())
        }
    }
}

/// Opens the main repository for `repo_root`, which may itself be a linked worktree.
fn open_main(repo_root: &Path) -> Result<Repository, WorktreeError> {
    let repo = match Repository::open(repo_root) {
        Ok(repo) => repo,
        Err(error) if error.code() == ErrorCode::NotFound => {
            return Err(WorktreeError::NotARepository(repo_root.to_path_buf()));
        }
        Err(error) => return Err(error.into()),
    };
    let main = if repo.is_worktree() {
        Repository::open(repo.commondir())?
    } else {
        repo
    };
    if main.is_bare() {
        return Err(WorktreeError::NotARepository(repo_root.to_path_buf()));
    }
    Ok(main)
}

fn check_destination(main: &Repository, path: &Path) -> Result<(), WorktreeError> {
    if !path.is_absolute() {
        return Err(WorktreeError::InvalidDestination("the path must be absolute"));
    }
    if path.starts_with(main.commondir()) {
        return Err(WorktreeError::InvalidDestination("inside the repository's .git folder"));
    }
    // `symlink_metadata` so a dangling symlink counts as taken.
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(WorktreeError::DestinationExists(path.to_path_buf()));
    }
    match path.parent() {
        Some(parent) if parent.is_dir() => Ok(()),
        Some(parent) => Err(WorktreeError::DestinationParentMissing(parent.to_path_buf())),
        None => Err(WorktreeError::InvalidDestination("the path has no parent folder")),
    }
}

/// Reference of the branch to check out, plus whether this call created it.
fn resolve_branch<'r>(main: &'r Repository, req: &CreateRequest) -> Result<(Reference<'r>, bool), WorktreeError> {
    match main.find_branch(&req.branch, BranchType::Local) {
        Ok(branch) => {
            if let Some(path) = branch_occupant(main, &req.branch) {
                return Err(WorktreeError::BranchCheckedOut {
                    branch: req.branch.clone(),
                    path,
                });
            }
            Ok((branch.into_reference(), false))
        }
        Err(error) if error.code() == ErrorCode::NotFound => {
            let commit = base_commit(main, req.base.as_deref())?;
            let branch = main.branch(&req.branch, &commit, false)?;
            Ok((branch.into_reference(), true))
        }
        Err(error) => Err(error.into()),
    }
}

fn base_commit<'r>(main: &'r Repository, base: Option<&str>) -> Result<Commit<'r>, WorktreeError> {
    match base.map(str::trim).filter(|spec| !spec.is_empty()) {
        None => match main.head() {
            Ok(head) => Ok(head.peel_to_commit()?),
            Err(error) if error.code() == ErrorCode::UnbornBranch => Err(WorktreeError::NoCommits),
            Err(error) => Err(error.into()),
        },
        Some(spec) if spec.starts_with('-') => Err(WorktreeError::InvalidBase(spec.to_owned())),
        Some(spec) => main
            .revparse_single(spec)
            .and_then(|object| object.peel_to_commit())
            .map_err(|_| WorktreeError::InvalidBase(spec.to_owned())),
    }
}

/// Where `branch` is checked out right now (main or a linked worktree), if anywhere.
fn branch_occupant(main: &Repository, branch: &str) -> Option<PathBuf> {
    let (main_branch, _) = head_branch(main);
    if main_branch.as_deref() == Some(branch) {
        return main.workdir().map(Path::to_path_buf);
    }
    let names = main.worktrees().ok()?;
    for name in names.iter().flatten().flatten() {
        if linked_branch(main.commondir(), name).as_deref() == Some(branch) {
            return main
                .find_worktree(name)
                .ok()
                .map(|worktree| worktree.path().to_path_buf());
        }
    }
    None
}

/// Metadata name for the worktree: the directory name made path-safe, with a
/// number added while a worktree of that name is already registered.
fn unique_worktree_name(main: &Repository, path: &Path) -> String {
    let base: String = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = if base.trim_matches('.').is_empty() {
        "worktree".to_owned()
    } else {
        base
    };
    let taken = |name: &str| main.commondir().join("worktrees").join(name).exists();
    if !taken(&base) {
        return base;
    }
    let mut suffix = 2_usize;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !taken(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Undoes a failed `create_worktree`. `check_destination` guaranteed `path`
/// was free, so anything there now was made by the failed attempt.
fn rollback(main: &Repository, name: &str, path: &Path, created_branch: Option<&str>) {
    if std::fs::symlink_metadata(path).is_ok()
        && let Err(error) = std::fs::remove_dir_all(path)
    {
        tracing::warn!(path = %path.display(), "could not remove the partial worktree directory: {error}");
    }
    if let Ok(worktree) = main.find_worktree(name)
        && let Err(error) = worktree.prune(Some(&mut WorktreePruneOptions::new()))
    {
        tracing::warn!(name, "could not prune the partial worktree: {error}");
    }
    if let Some(branch) = created_branch
        && let Ok(mut branch) = main.find_branch(branch, BranchType::Local)
        && let Err(error) = branch.delete()
    {
        tracing::warn!("could not delete the branch created for the failed worktree: {error}");
    }
}
