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
            info.dirty_count = Some(match canonical {
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
                    .unwrap_or(0)
                }
                None => 0,
            });
        }
    }

    Ok(Some(RepoOverview {
        worktrees,
        dirty_count: current_dirty,
        repo_root,
        branch,
        detached,
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
                linked.push(linked_info(&worktree, main.commondir(), name));
            }
        }
    }
    linked.sort_by(|a, b| (a.branch.is_none(), &a.branch, &a.path).cmp(&(b.branch.is_none(), &b.branch, &b.path)));

    let mut infos = Vec::with_capacity(linked.len() + 1);
    infos.push(WorktreeInfo {
        path: repo_root.to_path_buf(),
        branch: main_branch,
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

fn linked_info(worktree: &Worktree, commondir: &Path, name: &str) -> WorktreeInfo {
    let locked = matches!(worktree.is_locked(), Ok(WorktreeLockStatus::Locked(_)));
    let path = worktree.path().to_path_buf();
    WorktreeInfo {
        branch: linked_branch(commondir, name),
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
pub(super) mod test_repo {
    use std::path::{Path, PathBuf};

    use git2::{Oid, Repository, RepositoryInitOptions, Signature};
    use tempfile::TempDir;

    pub struct Fixture {
        pub dir: TempDir,
        pub repo: Repository,
    }

    impl Fixture {
        /// Repository with HEAD on an unborn `main`.
        pub fn unborn() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut opts = RepositoryInitOptions::new();
            opts.initial_head("main");
            let repo = Repository::init_opts(dir.path(), &opts).expect("init");
            Self { dir, repo }
        }

        pub fn with_commit() -> Self {
            let fixture = Self::unborn();
            fixture.commit_file("a.txt", "one\n");
            fixture
        }

        pub fn root(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }

        pub fn commit_file(&self, name: &str, content: &str) -> Oid {
            std::fs::write(self.dir.path().join(name), content).expect("write");
            let mut index = self.repo.index().expect("index");
            index.add_path(Path::new(name)).expect("add");
            index.write().expect("index write");
            let tree = self
                .repo
                .find_tree(index.write_tree().expect("tree"))
                .expect("find tree");
            let sig = Signature::now("t", "t@example.com").expect("sig");
            let parents: Vec<_> = self
                .repo
                .head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .into_iter()
                .collect();
            let parent_refs: Vec<_> = parents.iter().collect();
            self.repo
                .commit(Some("HEAD"), &sig, &sig, "c", &tree, &parent_refs)
                .expect("commit")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    use super::test_repo::Fixture;
    use super::*;

    fn canon(path: &Path) -> std::path::PathBuf {
        path.canonicalize().expect("canonicalize")
    }

    fn overview(path: &Path) -> RepoOverview {
        compute_repo_overview(path).expect("ok").expect("some")
    }

    fn overview_with(path: &Path, requested: &[&Path]) -> RepoOverview {
        let requested: HashSet<PathBuf> = requested.iter().map(|p| p.to_path_buf()).collect();
        compute_cached(path, &mut CycleCache::default(), &requested, &AtomicBool::new(false))
            .expect("ok")
            .expect("some")
    }

    #[test]
    fn non_repo_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(compute_repo_overview(dir.path()).expect("ok").is_none());
    }

    #[test]
    fn bare_repo_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init_bare(dir.path()).expect("init bare");
        assert!(compute_repo_overview(dir.path()).expect("ok").is_none());
    }

    #[test]
    fn plain_repo_reports_branch_and_clean() {
        let fx = Fixture::with_commit();
        let o = overview(&fx.root());
        assert_eq!(o.branch.as_deref(), Some("main"));
        assert!(!o.detached);
        assert_eq!(o.dirty_count, 0);
        assert_eq!(canon(&o.repo_root), canon(&fx.root()));
        assert_eq!(o.worktrees.len(), 1);
        assert!(o.worktrees[0].is_main && o.worktrees[0].is_current);
        assert_eq!(o.worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(o.worktrees[0].dirty_count, Some(0));
        assert!(!o.worktrees[0].is_agent);
    }

    #[test]
    fn dirty_count_counts_modified_and_untracked_not_ignored() {
        let fx = Fixture::unborn();
        fx.commit_file(".gitignore", "ignored.log\n");
        fx.commit_file("a.txt", "one\n");
        std::fs::write(fx.root().join("a.txt"), "changed\n").expect("modify");
        std::fs::write(fx.root().join("new.txt"), "x\n").expect("untracked");
        std::fs::write(fx.root().join("ignored.log"), "x\n").expect("ignored");
        assert_eq!(overview(&fx.root()).dirty_count, 2);
    }

    #[test]
    fn detached_head_sets_detached_and_no_branch() {
        let fx = Fixture::with_commit();
        let oid = fx.repo.head().expect("head").target().expect("oid");
        fx.repo.set_head_detached(oid).expect("detach");
        let o = overview(&fx.root());
        assert!(o.detached);
        assert_eq!(o.branch, None);
        assert_eq!(o.upstream, None);
        assert_eq!(o.worktrees[0].branch, None);
    }

    #[test]
    fn unborn_head_reports_branch_without_error() {
        let fx = Fixture::unborn();
        let o = overview(&fx.root());
        assert_eq!(o.branch.as_deref(), Some("main"));
        assert!(!o.detached);
        assert_eq!(o.upstream, None);
        assert_eq!((o.ahead, o.behind), (0, 0));
    }

    #[test]
    fn upstream_ahead_behind_counts() {
        let fx = Fixture::with_commit();
        let base = fx.repo.head().expect("head").target().expect("oid");
        // Upstream gets one commit the local branch lacks.
        let sig = git2::Signature::now("t", "t@example.com").expect("sig");
        let base_commit = fx.repo.find_commit(base).expect("commit");
        let upstream_oid = fx
            .repo
            .commit(
                None,
                &sig,
                &sig,
                "u",
                &base_commit.tree().expect("tree"),
                &[&base_commit],
            )
            .expect("upstream commit");
        fx.repo
            .remote("origin", "https://example.invalid/r.git")
            .expect("remote");
        fx.repo
            .reference("refs/remotes/origin/main", upstream_oid, true, "fixture")
            .expect("remote ref");
        let mut config = fx.repo.config().expect("config");
        config.set_str("branch.main.remote", "origin").expect("remote cfg");
        config
            .set_str("branch.main.merge", "refs/heads/main")
            .expect("merge cfg");
        // Local gets two commits upstream lacks.
        fx.commit_file("b.txt", "b\n");
        fx.commit_file("c.txt", "c\n");

        let o = overview(&fx.root());
        assert_eq!(o.upstream.as_deref(), Some("origin/main"));
        assert_eq!((o.ahead, o.behind), (2, 1));
    }

    #[test]
    fn no_upstream_gives_zero_and_none() {
        let fx = Fixture::with_commit();
        let o = overview(&fx.root());
        assert_eq!(o.upstream, None);
        assert_eq!((o.ahead, o.behind), (0, 0));
    }

    fn add_worktree(fx: &Fixture, name: &str) -> std::path::PathBuf {
        let path = fx.dir.path().join("wts").join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fx.repo.worktree(name, &path, None).expect("worktree");
        path
    }

    #[test]
    fn linked_worktree_listed_with_branch_and_dirty() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "feature");
        std::fs::write(wt.join("scratch.txt"), "x\n").expect("untracked");

        let o = overview_with(&fx.root(), &[&wt]);
        assert_eq!(o.worktrees.len(), 2);
        assert!(o.worktrees[0].is_main);
        assert_eq!(o.worktrees[0].dirty_count, Some(0));
        let linked = &o.worktrees[1];
        assert!(!linked.is_main);
        assert_eq!(canon(&linked.path), canon(&wt));
        assert_eq!(linked.branch.as_deref(), Some("feature"));
        assert_eq!(linked.dirty_count, Some(1));
        assert!(!linked.locked && !linked.prunable);
    }

    #[test]
    fn linked_dirty_is_none_until_requested() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "feature");
        std::fs::write(wt.join("scratch.txt"), "x\n").expect("untracked");
        assert_eq!(overview(&fx.root()).worktrees[1].dirty_count, None);
        let requested = overview_with(&fx.root(), &[&wt]);
        assert_eq!(requested.worktrees[1].dirty_count, Some(1));
    }

    #[test]
    fn current_linked_dirty_reuses_overview_count() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "feature");
        std::fs::write(wt.join("scratch.txt"), "x\n").expect("untracked");
        let mut cache = CycleCache::default();
        let o = compute_cached(&wt, &mut cache, &HashSet::new(), &AtomicBool::new(false))
            .expect("ok")
            .expect("some");
        assert_eq!(o.dirty_count, 1);
        assert_eq!(o.worktrees[1].dirty_count, Some(1));
        assert_eq!(o.worktrees[0].dirty_count, Some(0));
        // current (recursive) + main (non-recursive) -> exactly two scans.
        assert_eq!(cache.scans, 2);
    }

    #[test]
    fn workspaces_sharing_a_repo_share_one_listing_and_scan() {
        let fx = Fixture::with_commit();
        add_worktree(&fx, "feature");
        let sub = fx.root().join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir");
        let mut cache = CycleCache::default();
        let none = HashSet::new();
        let stop = AtomicBool::new(false);
        compute_cached(&fx.root(), &mut cache, &none, &stop).expect("a");
        compute_cached(&sub, &mut cache, &none, &stop).expect("b");
        assert_eq!(cache.listings_built, 1);
        assert_eq!(cache.scans, 1);
    }

    #[test]
    fn agent_worktree_is_flagged() {
        let fx = Fixture::with_commit();
        let agent = fx.dir.path().join(".claude/worktrees/agent-1");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        fx.repo.worktree("agent-1", &agent, None).expect("worktree");
        add_worktree(&fx, "feature");
        let o = overview(&fx.root());
        let by_branch = |name: &str| {
            o.worktrees
                .iter()
                .find(|w| w.branch.as_deref() == Some(name))
                .expect("worktree")
        };
        assert!(by_branch("agent-1").is_agent);
        assert!(!by_branch("feature").is_agent);
        assert!(!by_branch("main").is_agent);
    }

    #[test]
    fn nested_worktrees_are_not_untracked_in_main() {
        let fx = Fixture::with_commit();
        let agent = fx.dir.path().join(".claude/worktrees/agent-1");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        fx.repo.worktree("agent-1", &agent, None).expect("worktree");
        // Main as a non-current tree (non-recursive scan).
        let outside = overview_with(&agent, &[]);
        assert_eq!(outside.worktrees[0].dirty_count, Some(0));
        // Main as the current tree (recursive scan).
        let inside = overview(&fx.root());
        assert_eq!(inside.dirty_count, 0);
        assert_eq!(inside.worktrees[0].dirty_count, Some(0));
        // Real untracked content next to the worktrees still counts.
        std::fs::write(fx.root().join(".claude/note.txt"), "x\n").expect("note");
        assert_eq!(overview(&fx.root()).dirty_count, 1);
        assert_eq!(overview_with(&agent, &[]).worktrees[0].dirty_count, Some(1));
    }

    #[test]
    fn worktrees_sorted_main_then_branch_then_detached() {
        let fx = Fixture::with_commit();
        add_worktree(&fx, "b");
        add_worktree(&fx, "a");
        let detached = add_worktree(&fx, "z");
        let oid = fx.repo.head().expect("head").target().expect("oid");
        Repository::open(&detached)
            .expect("open")
            .set_head_detached(oid)
            .expect("detach");
        let o = overview(&fx.root());
        let branches: Vec<_> = o.worktrees.iter().map(|w| w.branch.as_deref()).collect();
        assert_eq!(branches, [Some("main"), Some("a"), Some("b"), None]);
        assert!(o.worktrees[0].is_main);
    }

    #[test]
    fn cwd_inside_linked_worktree_marks_it_current() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "feature");
        let sub = wt.join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir");

        let o = overview(&sub);
        assert_eq!(o.branch.as_deref(), Some("feature"));
        assert_eq!(canon(&o.repo_root), canon(&fx.root()));
        assert!(
            !o.worktrees[0].is_current,
            "main contains the linked dir but is not the match"
        );
        assert!(o.worktrees[1].is_current);
    }

    #[test]
    fn locked_worktree_flagged() {
        let fx = Fixture::with_commit();
        add_worktree(&fx, "feature");
        fx.repo
            .find_worktree("feature")
            .expect("find")
            .lock(Some("busy"))
            .expect("lock");
        let o = overview(&fx.root());
        assert!(o.worktrees[1].locked);
    }

    #[test]
    fn locked_missing_worktree_is_not_prunable() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "gone");
        fx.repo
            .find_worktree("gone")
            .expect("find")
            .lock(Some("on a removable drive"))
            .expect("lock");
        std::fs::remove_dir_all(&wt).expect("remove");
        let o = overview(&fx.root());
        assert!(o.worktrees[1].locked);
        assert!(!o.worktrees[1].prunable);
    }

    #[test]
    fn missing_worktree_dir_is_prunable_and_does_not_fail() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "gone");
        std::fs::remove_dir_all(&wt).expect("remove");
        let o = overview(&fx.root());
        assert_eq!(o.worktrees.len(), 2);
        assert!(o.worktrees[1].prunable);
        assert_eq!(o.worktrees[1].dirty_count, None);
        assert_eq!(o.worktrees[1].branch.as_deref(), Some("gone"));
        let requested = overview_with(&fx.root(), &[&wt]);
        assert_eq!(requested.worktrees[1].dirty_count, Some(0));
        assert!(o.worktrees[0].is_main);
    }
}
