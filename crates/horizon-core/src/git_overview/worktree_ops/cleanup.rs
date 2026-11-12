//! Classifying and removing worktrees that are done with.
//!
//! Classification lists registered linked worktrees whose branch is merged, whose
//! directory is gone, or whose branch was deleted, each with the reasons it
//! must not be removed. Removal re-classifies every item right before touching
//! it, only ever acts on paths libgit2 registers for the repository, and never
//! forces anything: dirty, locked, in-use and main worktrees are kept.

use std::path::{Component, Path, PathBuf};

use git2::{BranchType, ErrorCode, Oid, Repository, WorktreeLockStatus, WorktreePruneOptions};

use super::{WorktreeError, open_main};
use crate::git_overview::compute::{count_dirty, linked_branch};

/// Refs scanned when looking for a ref that still holds a detached commit.
const MAX_REFS_SCANNED: usize = 5000;
/// Branch names never deleted, whatever the repository says is its default.
const PROTECTED_BRANCHES: [&str; 5] = ["main", "master", "develop", "dev", "trunk"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupReason {
    /// The branch tip is contained in the default branch or the upstream.
    Merged,
    /// The directory or the metadata is gone or invalid.
    Prunable,
    /// Registered, but its branch was deleted (or HEAD is detached on nothing).
    Orphan,
}

impl CleanupReason {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Prunable => "prunable",
            Self::Orphan => "orphan",
        }
    }
}

/// Why a candidate may not be removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    Dirty,
    Locked,
    Main,
    ProtectedByWorkspace,
    UnreachableCommits,
    DefaultBranchUnknown,
    NestedWorktree,
}

impl Blocker {
    /// Short pill text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dirty => "dirty",
            Self::Locked => "locked",
            Self::Main => "main",
            Self::ProtectedByWorkspace => "in use",
            Self::UnreachableCommits => "unreachable commits",
            Self::DefaultBranchUnknown => "no default branch",
            Self::NestedWorktree => "contains worktree",
        }
    }

    /// One-sentence explanation for a tooltip.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Dirty => "It has uncommitted or untracked changes.",
            Self::Locked => "It is locked (git worktree lock).",
            Self::Main => "It is the main worktree.",
            Self::ProtectedByWorkspace => "A workspace or terminal is open inside it.",
            Self::UnreachableCommits => "Its HEAD has commits that no branch or tag contains.",
            Self::DefaultBranchUnknown => "The default branch cannot be determined, so \"merged\" cannot be verified.",
            Self::NestedWorktree => "Another worktree lives inside it.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub reason: CleanupReason,
    pub blockers: Vec<Blocker>,
}

impl CleanupCandidate {
    /// Nothing stands in the way of removing it.
    #[must_use]
    pub fn safe(&self) -> bool {
        self.blockers.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoveStatus {
    Removed {
        branch_deleted: bool,
    },
    /// Re-checked right before removing and blocked.
    Kept(Vec<Blocker>),
    /// No longer merged, prunable or orphaned.
    NotCandidate,
    /// Not a linked worktree registered for this repository (or it is the main one).
    Refused,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub path: PathBuf,
    pub status: RemoveStatus,
}

/// A registered linked worktree as libgit2 reports it right now.
struct Linked {
    name: String,
    path: PathBuf,
    canonical: Option<PathBuf>,
    locked: bool,
    valid: bool,
    branch: Option<String>,
}

fn read_linked(main: &Repository, name: &str) -> Option<Linked> {
    let worktree = main.find_worktree(name).ok()?;
    let path = worktree.path().to_path_buf();
    Some(Linked {
        name: name.to_owned(),
        canonical: path.canonicalize().ok(),
        // Fail closed: a lock state that cannot be read counts as locked.
        locked: !matches!(worktree.is_locked(), Ok(WorktreeLockStatus::Unlocked)),
        valid: worktree.validate().is_ok(),
        branch: linked_branch(main.commondir(), name),
        path,
    })
}

fn list_linked(main: &Repository) -> Vec<Linked> {
    let Ok(names) = main.worktrees() else {
        return Vec::new();
    };
    names
        .iter()
        .flatten()
        .flatten()
        .filter_map(|name| read_linked(main, name))
        .collect()
}

/// Everything classification needs besides the entry itself.
struct Context<'r> {
    main: &'r Repository,
    main_canonical: Option<PathBuf>,
    common_canonical: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Canonical paths of the main worktree and every linked one.
    all_canonical: Vec<PathBuf>,
    default_tips: Vec<Oid>,
    default_name: Option<String>,
    protected: Vec<PathBuf>,
}

impl<'r> Context<'r> {
    fn new(main: &'r Repository, linked: &[Linked], protected_paths: &[PathBuf]) -> Self {
        let main_canonical = main.workdir().and_then(|dir| dir.canonicalize().ok());
        let all_canonical = main_canonical
            .iter()
            .cloned()
            .chain(linked.iter().filter_map(|entry| entry.canonical.clone()))
            .collect();
        Self {
            main,
            main_canonical,
            common_canonical: main.commondir().canonicalize().ok(),
            home: std::env::var_os("HOME").and_then(|home| PathBuf::from(home).canonicalize().ok()),
            all_canonical,
            default_tips: default_branch_tips(main),
            default_name: default_branch_name(main),
            protected: protected_paths
                .iter()
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
                .collect(),
        }
    }

    fn is_merged(&self, tip: Oid, upstream: Option<Oid>) -> bool {
        self.default_tips
            .iter()
            .copied()
            .chain(upstream)
            .any(|base| contains(self.main, base, tip))
    }

    fn is_protected(&self, entry: &Linked) -> bool {
        self.protected.iter().any(|used| {
            entry
                .canonical
                .as_ref()
                .is_some_and(|canonical| used.starts_with(canonical))
                || used.starts_with(&entry.path)
        })
    }

    fn is_protected_branch(&self, name: &str) -> bool {
        PROTECTED_BRANCHES.contains(&name) || self.default_name.as_deref() == Some(name)
    }

    /// Locations that must never be deleted wholesale: the root, a parent of
    /// the main checkout or of the git directory, or the home folder.
    fn is_dangerous_target(&self, canonical: &Path) -> bool {
        canonical.parent().is_none()
            || [&self.main_canonical, &self.common_canonical, &self.home]
                .into_iter()
                .flatten()
                .any(|other| other.starts_with(canonical))
    }
}

/// `base` contains `tip` (a commit contains itself).
fn contains(repo: &Repository, base: Oid, tip: Oid) -> bool {
    base == tip || repo.graph_descendant_of(base, tip).unwrap_or(false)
}

/// Tips of the branches counted as "the default branch": `origin/HEAD` and its
/// local namesake, or `main`/`master` (local and `origin`) when there is none.
fn default_branch_tips(repo: &Repository) -> Vec<Oid> {
    let mut tips = Vec::new();
    let mut add = |name: &str| {
        if let Ok(oid) = repo.refname_to_id(name) {
            tips.push(oid);
        }
    };
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target)) = reference.symbolic_target()
    {
        add(target);
        if let Some(short) = target.strip_prefix("refs/remotes/origin/") {
            add(&format!("refs/heads/{short}"));
        }
    } else {
        for name in ["main", "master"] {
            add(&format!("refs/heads/{name}"));
            add(&format!("refs/remotes/origin/{name}"));
        }
    }
    tips
}

/// Name of the default branch, if the repository tells.
pub(super) fn default_branch_name(repo: &Repository) -> Option<String> {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target)) = reference.symbolic_target()
        && let Some(short) = target.strip_prefix("refs/remotes/origin/")
    {
        return Some(short.to_owned());
    }
    ["main", "master"]
        .into_iter()
        .find(|name| repo.find_branch(name, BranchType::Local).is_ok())
        .map(str::to_owned)
}

/// Some branch, remote branch or tag contains `oid`. An inconclusive scan
/// (too many refs) answers `false`, which keeps the worktree.
fn reachable_from_refs(repo: &Repository, oid: Oid) -> bool {
    let mut scanned = 0;
    for glob in ["refs/heads/*", "refs/remotes/*", "refs/tags/*"] {
        let Ok(references) = repo.references_glob(glob) else {
            continue;
        };
        for reference in references.flatten() {
            scanned += 1;
            if scanned > MAX_REFS_SCANNED {
                return false;
            }
            if let Ok(commit) = reference.peel_to_commit()
                && contains(repo, commit.id(), oid)
            {
                return true;
            }
        }
    }
    false
}

/// Lists the linked worktrees of the repository at `repo_root` that cleanup
/// may offer, each with what blocks its removal. The main worktree is never
/// listed. `protected_paths` are directories in use (workspace and terminal
/// working directories): a worktree containing one is blocked.
///
/// # Errors
///
/// Returns an error when `repo_root` is not a non-bare repository.
pub fn classify_cleanup(repo_root: &Path, protected_paths: &[PathBuf]) -> Result<Vec<CleanupCandidate>, WorktreeError> {
    let main = open_main(repo_root)?;
    let linked = list_linked(&main);
    let ctx = Context::new(&main, &linked, protected_paths);
    let mut candidates: Vec<CleanupCandidate> = linked.iter().filter_map(|entry| classify_entry(&ctx, entry)).collect();
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(candidates)
}

fn classify_entry(ctx: &Context<'_>, entry: &Linked) -> Option<CleanupCandidate> {
    let mut blockers = Vec::new();
    if entry.locked {
        blockers.push(Blocker::Locked);
    }
    if entry.canonical.is_some() && entry.canonical == ctx.main_canonical {
        blockers.push(Blocker::Main);
    }
    if ctx.is_protected(entry) {
        blockers.push(Blocker::ProtectedByWorkspace);
    }
    let reason = if entry.valid {
        classify_valid(ctx, entry, &mut blockers)?
    } else {
        CleanupReason::Prunable
    };
    if reason == CleanupReason::Merged && ctx.default_tips.is_empty() {
        blockers.push(Blocker::DefaultBranchUnknown);
    }
    Some(CleanupCandidate {
        path: entry.path.clone(),
        branch: entry.branch.clone(),
        reason,
        blockers,
    })
}

/// Reason for a worktree whose directory exists; `None` when it is not a
/// cleanup candidate (unmerged work) or cannot be inspected.
fn classify_valid(ctx: &Context<'_>, entry: &Linked, blockers: &mut Vec<Blocker>) -> Option<CleanupReason> {
    let canonical = entry.canonical.as_ref()?;
    let repo = Repository::open(&entry.path).ok()?;
    let reason = match &entry.branch {
        Some(name) => match ctx.main.find_branch(name, BranchType::Local) {
            Ok(branch) => {
                let tip = branch.get().peel_to_commit().ok()?.id();
                let upstream = branch
                    .upstream()
                    .ok()
                    .and_then(|upstream| upstream.get().peel_to_commit().ok())
                    .map(|commit| commit.id());
                if !ctx.is_merged(tip, upstream) {
                    return None;
                }
                CleanupReason::Merged
            }
            Err(error) if error.code() == ErrorCode::NotFound => CleanupReason::Orphan,
            Err(_) => return None,
        },
        None => {
            let head = repo.refname_to_id("HEAD").ok()?;
            if ctx.is_merged(head, None) {
                CleanupReason::Merged
            } else if reachable_from_refs(ctx.main, head) {
                return None;
            } else {
                blockers.push(Blocker::UnreachableCommits);
                CleanupReason::Orphan
            }
        }
    };
    let nested: Vec<PathBuf> = ctx
        .all_canonical
        .iter()
        .filter(|other| other.as_path() != canonical && other.starts_with(canonical))
        .cloned()
        .collect();
    if !nested.is_empty() {
        blockers.push(Blocker::NestedWorktree);
    }
    // A scan that fails counts as dirty.
    if !matches!(count_dirty(&repo, true, &nested, None), Ok(0)) {
        blockers.push(Blocker::Dirty);
    }
    Some(reason)
}

/// Removes the worktrees at `items` that are still safe to remove, one at a
/// time. Each item is looked up among the repository's registered worktrees
/// and re-classified immediately before removal; a worktree that turned dirty,
/// locked or in use in the meantime is kept. A failure on one item does not
/// stop the others. With `delete_merged_branches`, the branch of a removed
/// *merged* worktree is deleted too (never a default branch).
///
/// Directories are only deleted when the worktree is valid, its `.git` link
/// points back at the repository's metadata for it, and the location is not
/// the root, the home folder or a parent of the repository.
#[must_use]
pub fn remove_worktrees(
    repo_root: &Path,
    items: &[PathBuf],
    delete_merged_branches: bool,
    protected_paths: &[PathBuf],
) -> Vec<RemoveOutcome> {
    let main = match open_main(repo_root) {
        Ok(main) => main,
        Err(error) => {
            return items
                .iter()
                .map(|item| RemoveOutcome {
                    path: item.clone(),
                    status: RemoveStatus::Failed(error.to_string()),
                })
                .collect();
        }
    };
    items
        .iter()
        .map(|item| RemoveOutcome {
            path: item.clone(),
            status: remove_one(&main, item, delete_merged_branches, protected_paths),
        })
        .collect()
}

fn remove_one(main: &Repository, item: &Path, delete_branch: bool, protected: &[PathBuf]) -> RemoveStatus {
    if item.components().any(|part| matches!(part, Component::ParentDir)) {
        return RemoveStatus::Refused;
    }
    // Looked up fresh for every item so lock and validity are current.
    let linked = list_linked(main);
    let item_canonical = item.canonicalize().ok();
    let Some(entry) = linked
        .iter()
        .find(|entry| entry.path.as_path() == item || (item_canonical.is_some() && entry.canonical == item_canonical))
    else {
        return RemoveStatus::Refused;
    };
    let ctx = Context::new(main, &linked, protected);
    if entry.canonical.is_some() && entry.canonical == ctx.main_canonical {
        return RemoveStatus::Refused;
    }
    let Some(candidate) = classify_entry(&ctx, entry) else {
        return RemoveStatus::NotCandidate;
    };
    if !candidate.safe() {
        return RemoveStatus::Kept(candidate.blockers);
    }
    match delete(&ctx, entry) {
        Ok(()) => RemoveStatus::Removed {
            branch_deleted: delete_branch && delete_merged_branch(&ctx, &candidate),
        },
        Err(message) => RemoveStatus::Failed(message),
    }
}

/// Deletes the directory (only for a valid worktree) and the metadata.
fn delete(ctx: &Context<'_>, entry: &Linked) -> Result<(), String> {
    if entry.valid {
        let canonical = entry.canonical.as_ref().ok_or("the worktree path cannot be resolved")?;
        if ctx.is_dangerous_target(canonical) {
            return Err("refusing to delete this location".to_owned());
        }
        let gitdir = ctx.main.commondir().join("worktrees").join(&entry.name);
        if !owns_gitlink(canonical, &gitdir) {
            return Err("its .git link does not point back at this repository".to_owned());
        }
        std::fs::remove_dir_all(canonical)
            .map_err(|error| format!("could not delete {}: {error}", canonical.display()))?;
    }
    let worktree = ctx
        .main
        .find_worktree(&entry.name)
        .map_err(|error| error.message().to_owned())?;
    // No `valid`/`locked` flags: the directory is gone by now and it is unlocked.
    worktree
        .prune(Some(&mut WorktreePruneOptions::new()))
        .map_err(|error| format!("directory removed but metadata remained: {}", error.message()))
}

/// The `.git` file of `dir` is a gitlink to `gitdir` (the repository's metadata for it).
fn owns_gitlink(dir: &Path, gitdir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(".git")) else {
        return false;
    };
    let Some(target) = text.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let linked = dir.join(target.trim()).canonicalize().ok();
    linked.is_some() && linked == gitdir.canonicalize().ok()
}

fn delete_merged_branch(ctx: &Context<'_>, candidate: &CleanupCandidate) -> bool {
    if candidate.reason != CleanupReason::Merged {
        return false;
    }
    let Some(name) = candidate.branch.as_deref() else {
        return false;
    };
    if ctx.is_protected_branch(name) {
        return false;
    }
    let Ok(mut branch) = ctx.main.find_branch(name, BranchType::Local) else {
        return false;
    };
    match branch.delete() {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(branch = name, "could not delete the merged branch: {error}");
            false
        }
    }
}
