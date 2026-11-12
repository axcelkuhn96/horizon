use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use git2::{BranchType, ErrorCode, Repository, StatusOptions, Worktree, WorktreeLockStatus};

use super::model::{GitOverviewError, RepoOverview, WorktreeInfo};

/// Compute the git overview for the repository containing `cwd`.
///
/// Returns `Ok(None)` when `cwd` is not inside a repository, or the
/// repository is bare. Branch, dirty count and upstream describe the working
/// tree containing `cwd`; `repo_root` is the main working tree. Per-worktree
/// dirty counts are only filled for the main worktree and the one containing
/// `cwd` (see [`WorktreeInfo::dirty_count`]).
///
/// # Errors
///
/// Returns an error when libgit2 fails on the repository itself. Failures on
/// individual linked worktrees only degrade that entry.
pub fn compute_repo_overview(cwd: &Path) -> Result<Option<RepoOverview>, GitOverviewError> {
    compute_cached(
        cwd,
        &mut CycleCache::default(),
        &HashSet::new(),
        &AtomicBool::new(false),
    )
}

/// Worktree listing of one repository, shared by every workspace in it.
struct Listing {
    infos: Vec<WorktreeInfo>,
    /// Canonical path per entry; `None` when the directory is missing.
    canonical: Vec<Option<PathBuf>>,
}

/// Work shared by all workspaces polled in one cycle: the worktree listing
/// per repository (keyed by commondir) and the dirty counts already scanned.
#[derive(Default)]
pub(super) struct CycleCache {
    listings: HashMap<PathBuf, Listing>,
    /// `(canonical worktree path, recursive untracked)` -> dirty count.
    dirty: HashMap<(PathBuf, bool), usize>,
    #[cfg(test)]
    pub scans: usize,
    #[cfg(test)]
    pub listings_built: usize,
}

/// [`compute_repo_overview`] with a per-cycle cache, extra worktrees whose
/// dirty count was requested, and a stop flag checked between worktrees (a
/// stopped computation returns a partial value the caller must discard).
pub(super) fn compute_cached(
    cwd: &Path,
    cache: &mut CycleCache,
    requested: &HashSet<PathBuf>,
    stop: &AtomicBool,
) -> Result<Option<RepoOverview>, GitOverviewError> {
    let current = match Repository::discover(cwd) {
        Ok(repo) => repo,
        Err(err) if err.code() == ErrorCode::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if current.is_bare() {
        return Ok(None);
    }
    let main = if current.is_worktree() {
        Repository::open(current.commondir())?
    } else {
        Repository::open(current.path())?
    };
    let Some(repo_root) = main.workdir().map(Path::to_path_buf) else {
        return Ok(None);
    };

    let (branch, detached) = head_branch(&current);
    let head_short = head_short_id(&current);
    let (upstream, ahead, behind) = upstream_state(&current, branch.as_deref());
    let cwd_canonical = cwd.canonicalize()?;

    let commondir = current.commondir().to_path_buf();
    if !cache.listings.contains_key(&commondir) {
        let listing = build_listing(&main, &repo_root, stop);
        #[cfg(test)]
        {
            cache.listings_built += 1;
        }
        cache.listings.insert(commondir.clone(), listing);
    }
    let Some(listing) = cache.listings.get(&commondir) else {
        return Ok(None);
    };
    let mut worktrees = listing.infos.clone();
    mark_current(&mut worktrees, &listing.canonical, &cwd_canonical);

    let current_canonical = current.workdir().and_then(|dir| dir.canonicalize().ok());
    let nested_of = |own: &Path| -> Vec<PathBuf> {
        listing
            .canonical
            .iter()
            .flatten()
            .filter(|path| path.as_path() != own && path.starts_with(own))
            .cloned()
            .collect()
    };

    let current_dirty = match &current_canonical {
        Some(canonical) => {
            let nested = nested_of(canonical);
            cached_dirty(&mut cache.dirty, canonical, true, || {
                #[cfg(test)]
                {
                    cache.scans += 1;
                }
                count_dirty(&current, true, &nested, None)
            })?
        }
        None => count_dirty(&current, true, &[], None)?,
    };

    for (index, info) in worktrees.iter_mut().enumerate() {
        let canonical = listing.canonical[index].as_ref();
        if canonical.is_some() && canonical == current_canonical.as_ref() {
            info.dirty_count = Some(current_dirty);
        } else if info.is_current || info.is_main || requested.contains(&info.path) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            info.dirty_count = match canonical {
                Some(canonical) => {
                    let nested = nested_of(canonical);
                    cached_dirty(&mut cache.dirty, canonical, false, || {
                        #[cfg(test)]
                        {
                            cache.scans += 1;
                        }
                        let repo = Repository::open(&info.path)?;
                        count_dirty(&repo, false, &nested, None)
                    })
                    .ok()
                }
                None => Some(0),
            };
        }
    }

    Ok(Some(RepoOverview {
        worktrees,
        dirty_count: current_dirty,
        repo_root,
        branch,
        detached,
        head_short,
        upstream,
        ahead,
        behind,
    }))
}

/// A recursive count also answers a non-recursive request (it is the more
/// precise one), so it is looked up first.
fn cached_dirty(
    cache: &mut HashMap<(PathBuf, bool), usize>,
    canonical: &Path,
    recursive: bool,
    scan: impl FnOnce() -> Result<usize, git2::Error>,
) -> Result<usize, git2::Error> {
    if let Some(count) = cache.get(&(canonical.to_path_buf(), true)) {
        return Ok(*count);
    }
    if !recursive && let Some(count) = cache.get(&(canonical.to_path_buf(), false)) {
        return Ok(*count);
    }
    let count = scan()?;
    cache.insert((canonical.to_path_buf(), recursive), count);
    Ok(count)
}

/// Branch name from the symbolic `HEAD` target, so an unborn branch still
/// reports its name. A direct `HEAD` is a detached checkout.
fn head_branch(repo: &Repository) -> (Option<String>, bool) {
    let Ok(head) = repo.find_reference("HEAD") else {
        return (None, false);
    };
    match head.symbolic_target() {
        Ok(Some(target)) => (
            Some(target.strip_prefix("refs/heads/").unwrap_or(target).to_string()),
            false,
        ),
        _ => (None, true),
    }
}

/// First 7 hex chars of the commit `HEAD` resolves to; `None` for an unborn branch.
fn head_short_id(repo: &Repository) -> Option<String> {
    short_id(repo.refname_to_id("HEAD").ok()?)
}

fn short_id(oid: git2::Oid) -> Option<String> {
    oid.to_string().get(..7).map(str::to_string)
}

/// Short HEAD id of a linked worktree from its `HEAD` file: a direct id when
/// detached, otherwise the id of the branch it names (refs are shared).
fn linked_head_short(main: &Repository, commondir: &Path, name: &str) -> Option<String> {
    let head = std::fs::read_to_string(commondir.join("worktrees").join(name).join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(target) => short_id(main.refname_to_id(target.trim()).ok()?),
        None => short_id(git2::Oid::from_str(head).ok()?),
    }
}

/// Branch of a linked worktree from `<commondir>/worktrees/<name>/HEAD`.
/// Works even when the worktree directory is gone; a detached HEAD is `None`.
fn linked_branch(commondir: &Path, name: &str) -> Option<String> {
    let head = std::fs::read_to_string(commondir.join("worktrees").join(name).join("HEAD")).ok()?;
    let target = head.trim().strip_prefix("ref:")?.trim();
    Some(target.strip_prefix("refs/heads/").unwrap_or(target).to_string())
}

fn upstream_state(repo: &Repository, branch: Option<&str>) -> (Option<String>, usize, usize) {
    let Some(branch) = branch else {
        return (None, 0, 0);
    };
    let resolved = || -> Result<(String, usize, usize), git2::Error> {
        let local = repo.find_branch(branch, BranchType::Local)?;
        let upstream = local.upstream()?;
        let name = upstream
            .name()?
            .map(str::to_string)
            .ok_or_else(|| git2::Error::from_str("unnamed upstream"))?;
        let local_oid = local.get().peel_to_commit()?.id();
        let upstream_oid = upstream.get().peel_to_commit()?.id();
        let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;
        Ok((name, ahead, behind))
    };
    match resolved() {
        Ok((name, ahead, behind)) => (Some(name), ahead, behind),
        Err(_) => (None, 0, 0),
    }
}

/// Count changed and untracked (not ignored) entries. Entries that are, or lie
/// inside, another worktree of the same repository (`nested`, canonical paths)
/// are skipped: a worktree nested in this one is not an untracked change. With
/// a non-recursive scan an untracked directory counts as one entry, except when
/// it contains such worktrees, in which case its other content is counted.
fn count_dirty(
    repo: &Repository,
    recursive: bool,
    nested: &[PathBuf],
    only: Option<&str>,
) -> Result<usize, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(recursive)
        .include_ignored(false);
    if let Some(dir) = only {
        opts.pathspec(dir);
    }
    let statuses = repo.statuses(Some(&mut opts))?;
    let workdir = repo.workdir().and_then(|dir| dir.canonicalize().ok());
    let mut count = 0;
    for entry in statuses.iter() {
        let (Ok(rel), Some(workdir)) = (entry.path(), &workdir) else {
            count += 1;
            continue;
        };
        let absolute = workdir.join(rel.trim_end_matches('/'));
        if nested.iter().any(|path| absolute.starts_with(path)) {
            continue;
        }
        let untracked_dir = rel.ends_with('/') && entry.status().is_wt_new();
        if !recursive && untracked_dir && nested.iter().any(|path| path.starts_with(&absolute)) {
            count += count_dirty(repo, true, nested, Some(rel))?;
            continue;
        }
        count += 1;
    }
    Ok(count)
}

fn build_listing(main: &Repository, repo_root: &Path, stop: &AtomicBool) -> Listing {
    let (main_branch, _) = head_branch(main);
    let mut linked = Vec::new();
    if let Ok(names) = main.worktrees() {
        for name in names.iter().flatten().flatten() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(worktree) = main.find_worktree(name) {
                linked.push(linked_info(&worktree, main, name));
            }
        }
    }
    linked.sort_by(|a, b| (a.branch.is_none(), &a.branch, &a.path).cmp(&(b.branch.is_none(), &b.branch, &b.path)));

    let mut infos = Vec::with_capacity(linked.len() + 1);
    infos.push(WorktreeInfo {
        path: repo_root.to_path_buf(),
        branch: main_branch,
        head_short: head_short_id(main),
        is_main: true,
        is_current: false,
        locked: false,
        prunable: false,
        dirty_count: None,
        is_agent: is_agent_path(repo_root),
    });
    infos.extend(linked);
    let canonical = infos.iter().map(|info| info.path.canonicalize().ok()).collect();
    Listing { infos, canonical }
}

fn linked_info(worktree: &Worktree, main: &Repository, name: &str) -> WorktreeInfo {
    let commondir = main.commondir();
    let locked = matches!(worktree.is_locked(), Ok(WorktreeLockStatus::Locked(_)));
    let path = worktree.path().to_path_buf();
    WorktreeInfo {
        branch: linked_branch(commondir, name),
        head_short: linked_head_short(main, commondir, name),
        is_main: false,
        is_current: false,
        locked,
        // A locked worktree (e.g. on a removable drive) is never pruned.
        prunable: !locked && worktree.validate().is_err(),
        dirty_count: None,
        is_agent: is_agent_path(&path),
        path,
    }
}

fn is_agent_path(path: &Path) -> bool {
    path.to_string_lossy().contains("/.claude/worktrees/")
}

/// Flag the worktree whose canonical path is the longest prefix of `cwd`.
fn mark_current(worktrees: &mut [WorktreeInfo], canonical: &[Option<PathBuf>], cwd: &Path) {
    let best = canonical
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let path = path.as_ref()?;
            cwd.starts_with(path).then(|| (path.components().count(), index))
        })
        .max_by_key(|(depth, _)| *depth);
    if let Some((_, index)) = best {
        worktrees[index].is_current = true;
    }
}

#[cfg(test)]
pub(super) mod test_repo;
#[cfg(test)]
mod tests;
