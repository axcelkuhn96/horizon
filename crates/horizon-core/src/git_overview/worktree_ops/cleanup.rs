//! Classifying and removing worktrees that are done with.
//!
//! Classification lists registered linked worktrees whose branch is merged, whose
//! directory is gone, or whose branch was deleted, each with the reasons it
//! must not be removed. Removal re-classifies every item right before touching
//! it, only ever acts on paths libgit2 registers for the repository, and never
//! forces anything: dirty, locked, in-use and main worktrees are kept.

use std::path::{Component, Path, PathBuf};

use git2::{BranchType, ErrorCode, Oid, Repository, StatusOptions, WorktreeLockStatus, WorktreePruneOptions};

use super::{WorktreeError, open_main};
use crate::git_overview::compute::{count_dirty, linked_branch};

/// Refs scanned when looking for a ref that still holds a detached commit.
const MAX_REFS_SCANNED: usize = 5000;
/// Ignored entries named in the dialog.
const MAX_IGNORED_EXAMPLES: usize = 3;
/// Ignored entries counted per worktree before the count is reported as "at least".
const MAX_IGNORED_COUNTED: usize = 2000;
/// Branch names never deleted, whatever the repository says is its default.
const PROTECTED_BRANCHES: [&str; 5] = ["main", "master", "develop", "dev", "trunk"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupReason {
    /// The branch tip is contained in a default branch (local or `origin`'s), and
    /// is not simply the default branch's own tip.
    Merged,
    /// The branch tip is contained only in its own upstream: pushed, but not
    /// merged into the default branch. Never pre-ticked and its branch is never
    /// deleted, so the work stays reachable on the branch.
    Pushed,
    /// The branch tip is exactly the default branch's tip: a branch with no
    /// commits of its own yet (or one merged by fast-forward). Never offered
    /// pre-ticked.
    Fresh,
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
            Self::Pushed => "pushed, not merged",
            Self::Fresh => "no commits yet",
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
    /// The worktree has initialized submodules (`git worktree remove` refuses too).
    Submodules,
    /// Git considers the worktree broken but its directory is still on disk.
    StillOnDisk,
    /// The directory is gone and so is (or is empty) its parent folder: it may
    /// be on a drive that is not mounted.
    ParentMissing,
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
            Self::Submodules => "submodules",
            Self::StillOnDisk => "still on disk",
            Self::ParentMissing => "folder unavailable",
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
            Self::Submodules => "It has initialized submodules, which would be lost.",
            Self::StillOnDisk => {
                "Git considers it broken, but its directory is still on disk. Its metadata is kept so nothing is lost."
            }
            Self::ParentMissing => {
                "Its folder is gone and so is (or is empty) the parent folder: is a drive not mounted? Nothing is removed."
            }
        }
    }
}

/// Ignored files (`.env`, `node_modules/`, build output, local databases) that
/// "clean" does not cover but deleting the directory would destroy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IgnoredFiles {
    /// Ignored entries found (a wholly ignored directory counts as one).
    pub count: usize,
    /// A few of them, as paths relative to the worktree.
    pub examples: Vec<String>,
    /// The scan stopped early: there are at least `count`.
    pub capped: bool,
    /// The scan failed, so ignored files cannot be ruled out.
    pub unreadable: bool,
}

impl IgnoredFiles {
    /// Ignored data may exist.
    #[must_use]
    pub fn any(&self) -> bool {
        self.count > 0 || self.unreadable
    }

    /// `3 ignored items, e.g. .env, node_modules/`.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.count == 0 {
            return "ignored files could not be read".to_owned();
        }
        let plus = if self.capped { "+" } else { "" };
        let noun = if self.count == 1 && !self.capped {
            "item"
        } else {
            "items"
        };
        let mut text = format!("{}{plus} ignored {noun}", self.count);
        if !self.examples.is_empty() {
            text.push_str(", e.g. ");
            text.push_str(&self.examples.join(", "));
        }
        text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub reason: CleanupReason,
    pub blockers: Vec<Blocker>,
    /// Ignored data that removing the directory would delete.
    pub ignored: IgnoredFiles,
}

impl CleanupCandidate {
    /// Nothing stands in the way of removing it.
    #[must_use]
    pub fn safe(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Whether a dialog may tick it by default: only merged worktrees with
    /// nothing ignored in them. Everything else needs a deliberate choice.
    #[must_use]
    pub fn preselect(&self) -> bool {
        self.safe() && self.reason == CleanupReason::Merged && !self.ignored.any()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoveStatus {
    Removed {
        branch_deleted: bool,
    },
    /// Re-checked right before removing and blocked.
    Kept(Vec<Blocker>),
    /// Ignored files appeared (or were never confirmed): kept untouched.
    IgnoredNotConfirmed(IgnoredFiles),
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

    /// `tip` is contained in a default branch. A branch's own upstream does not count
    /// (pushed is not merged); an upstream that is the default branch is already among
    /// the default tips.
    fn is_merged(&self, tip: Oid) -> bool {
        self.default_tips.iter().any(|base| contains(self.main, *base, tip))
    }

    /// `tip` is the very tip of a default branch.
    fn at_default_tip(&self, tip: Oid) -> bool {
        self.default_tips.contains(&tip)
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
    if has_submodules(ctx.main.commondir(), &entry.name) {
        blockers.push(Blocker::Submodules);
    }
    let (reason, ignored) = if entry.valid {
        classify_valid(ctx, entry, &mut blockers)?
    } else {
        broken_blockers(entry, &mut blockers);
        (CleanupReason::Prunable, IgnoredFiles::default())
    };
    if reason == CleanupReason::Merged && ctx.default_tips.is_empty() {
        blockers.push(Blocker::DefaultBranchUnknown);
    }
    Some(CleanupCandidate {
        path: entry.path.clone(),
        branch: entry.branch.clone(),
        reason,
        blockers,
        ignored,
    })
}

/// `<commondir>/worktrees/<name>/modules` holds the clones of initialized
/// submodules; pruning the metadata would delete them. Unreadable counts as present.
fn has_submodules(commondir: &Path, name: &str) -> bool {
    match std::fs::read_dir(commondir.join("worktrees").join(name).join("modules")) {
        Ok(mut entries) => entries.next().is_some(),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Blockers for a worktree git calls invalid. Only one whose directory is
/// verifiably gone, with an existing non-empty parent folder, may be pruned: a
/// missing parent (or an empty mount point) looks like an unmounted drive, and a
/// directory that is still there must keep its metadata.
fn broken_blockers(entry: &Linked, blockers: &mut Vec<Blocker>) {
    match std::fs::symlink_metadata(&entry.path) {
        Ok(_) => blockers.push(Blocker::StillOnDisk),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent_present = entry
                .path
                .parent()
                .and_then(|parent| std::fs::read_dir(parent).ok())
                .is_some_and(|mut siblings| siblings.next().is_some());
            if !parent_present {
                blockers.push(Blocker::ParentMissing);
            }
        }
        // Cannot tell whether it is gone: keep it.
        Err(_) => blockers.push(Blocker::ParentMissing),
    }
}

/// Reason for a worktree whose directory exists; `None` when it is not a
/// cleanup candidate (unmerged work) or cannot be inspected.
fn classify_valid(
    ctx: &Context<'_>,
    entry: &Linked,
    blockers: &mut Vec<Blocker>,
) -> Option<(CleanupReason, IgnoredFiles)> {
    let canonical = entry.canonical.as_ref()?;
    let repo = Repository::open(&entry.path).ok()?;
    let reason = if let Some(name) = &entry.branch {
        match ctx.main.find_branch(name, BranchType::Local) {
            Ok(branch) => {
                let tip = branch.get().peel_to_commit().ok()?.id();
                let upstream = branch
                    .upstream()
                    .ok()
                    .and_then(|upstream| upstream.get().peel_to_commit().ok())
                    .map(|commit| commit.id());
                if ctx.is_merged(tip) {
                    if ctx.at_default_tip(tip) {
                        CleanupReason::Fresh
                    } else {
                        CleanupReason::Merged
                    }
                } else if upstream.is_some_and(|base| contains(ctx.main, base, tip)) {
                    CleanupReason::Pushed
                } else {
                    return None;
                }
            }
            Err(error) if error.code() == ErrorCode::NotFound => CleanupReason::Orphan,
            Err(_) => return None,
        }
    } else {
        let head = repo.refname_to_id("HEAD").ok()?;
        if ctx.is_merged(head) {
            if ctx.at_default_tip(head) {
                CleanupReason::Fresh
            } else {
                CleanupReason::Merged
            }
        } else if reachable_from_refs(ctx.main, head) {
            return None;
        } else {
            blockers.push(Blocker::UnreachableCommits);
            CleanupReason::Orphan
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
    Some((reason, scan_ignored(&repo, &nested)))
}

/// Ignored entries of `repo`'s working tree (`nested` canonical paths skipped).
/// An ignored directory counts once, so the scan stays cheap; a failing scan
/// reports `unreadable`, which counts as "ignored data may exist".
fn scan_ignored(repo: &Repository, nested: &[PathBuf]) -> IgnoredFiles {
    let mut found = IgnoredFiles::default();
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(true)
        .recurse_ignored_dirs(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        found.unreadable = true;
        return found;
    };
    let workdir = repo.workdir().and_then(|dir| dir.canonicalize().ok());
    for entry in statuses.iter() {
        if !entry.status().is_ignored() {
            continue;
        }
        if found.count >= MAX_IGNORED_COUNTED {
            found.capped = true;
            break;
        }
        let relative = entry.path().ok();
        if let (Some(relative), Some(workdir)) = (relative, &workdir)
            && nested
                .iter()
                .any(|path| workdir.join(relative.trim_end_matches('/')).starts_with(path))
        {
            continue;
        }
        found.count += 1;
        if let Some(relative) = relative
            && found.examples.len() < MAX_IGNORED_EXAMPLES
        {
            found.examples.push(relative.to_owned());
        }
    }
    found
}

/// Removes the worktrees at `items` that are still safe to remove, one at a
/// time. Each item is looked up among the repository's registered worktrees
/// and re-classified immediately before removal; a worktree that turned dirty,
/// locked or in use in the meantime is kept. A failure on one item does not
/// stop the others. With `delete_merged_branches`, the branch of a removed
/// *merged* worktree is deleted too (never a default branch).
///
/// A worktree with ignored files (see [`IgnoredFiles`]) is only removed when
/// its path is in `confirmed_ignored`; otherwise it is kept untouched.
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
    confirmed_ignored: &[PathBuf],
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
            status: remove_one(&main, item, delete_merged_branches, protected_paths, confirmed_ignored),
        })
        .collect()
}

fn remove_one(
    main: &Repository,
    item: &Path,
    delete_branch: bool,
    protected: &[PathBuf],
    confirmed_ignored: &[PathBuf],
) -> RemoveStatus {
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
    if candidate.ignored.any()
        && !confirmed_ignored
            .iter()
            .any(|confirmed| confirmed == item || confirmed == &entry.path)
    {
        return RemoveStatus::IgnoredNotConfirmed(candidate.ignored);
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
    } else if std::fs::symlink_metadata(&entry.path).is_ok() {
        // Classification already blocks this; never prune the metadata of a directory that exists.
        return Err("the directory still exists, so its metadata is kept".to_owned());
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
pub(super) fn owns_gitlink(dir: &Path, gitdir: &Path) -> bool {
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
    // The branch may have moved since classification (a commit made while the
    // dialog was open): check again, with fresh default tips, right before deleting.
    if !branch_still_merged(ctx.main, &branch) {
        tracing::info!(branch = name, "kept the branch: it is no longer merged");
        return false;
    }
    match branch.delete() {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(branch = name, "could not delete the merged branch: {error}");
            false
        }
    }
}

/// The branch tip is contained in a default branch, read now (its own upstream does not count).
fn branch_still_merged(main: &Repository, branch: &git2::Branch<'_>) -> bool {
    let Ok(tip) = branch.get().peel_to_commit().map(|commit| commit.id()) else {
        return false;
    };
    default_branch_tips(main)
        .into_iter()
        .any(|base| contains(main, base, tip))
}

#[cfg(test)]
mod tests {
    use git2::{BranchType, Repository, Signature};

    use super::branch_still_merged;

    fn commit(repo: &Repository, reference: &str, parent: Option<git2::Oid>, message: &str) -> git2::Oid {
        let sig = Signature::now("t", "t@example.com").expect("sig");
        let tree = repo
            .find_tree(repo.treebuilder(None).expect("builder").write().expect("tree"))
            .expect("find tree");
        let parent = parent.map(|oid| repo.find_commit(oid).expect("parent"));
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(Some(reference), &sig, &sig, message, &tree, &parents)
            .expect("commit")
    }

    #[test]
    fn branch_that_gained_a_commit_is_no_longer_merged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(tmp.path()).expect("init");
        let base = commit(&repo, "refs/heads/main", None, "base");
        repo.reference("refs/heads/topic", base, true, "test").expect("topic");
        commit(&repo, "refs/heads/main", Some(base), "main moves on");

        let topic = repo.find_branch("topic", BranchType::Local).expect("topic");
        assert!(branch_still_merged(&repo, &topic), "an ancestor of main is merged");

        // A commit lands on the branch after classification.
        commit(&repo, "refs/heads/topic", Some(base), "late work");
        let topic = repo.find_branch("topic", BranchType::Local).expect("topic");
        assert!(!branch_still_merged(&repo, &topic), "the new commit is not in main");
    }

    #[test]
    fn a_pushed_but_unmerged_branch_is_not_merged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(tmp.path()).expect("init");
        let base = commit(&repo, "refs/heads/main", None, "base");
        let work = commit(&repo, "refs/heads/feature", Some(base), "feature work");
        repo.reference("refs/remotes/origin/feature", work, true, "pushed")
            .expect("remote ref");
        repo.remote("origin", "https://example.invalid/x.git").expect("remote");
        let mut config = repo.config().expect("config");
        config.set_str("branch.feature.remote", "origin").expect("remote");
        config
            .set_str("branch.feature.merge", "refs/heads/feature")
            .expect("merge");

        let feature = repo.find_branch("feature", BranchType::Local).expect("feature");
        assert!(feature.upstream().is_ok(), "the fixture has an upstream");
        assert!(!branch_still_merged(&repo, &feature), "pushed is not merged");
    }
}
