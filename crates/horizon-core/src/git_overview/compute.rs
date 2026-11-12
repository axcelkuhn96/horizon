use std::path::{Path, PathBuf};

use git2::{BranchType, ErrorCode, Repository, StatusOptions, Worktree, WorktreeLockStatus};

use super::model::{GitOverviewError, RepoOverview, WorktreeInfo};

/// Compute the git overview for the repository containing `cwd`.
///
/// Returns `Ok(None)` when `cwd` is not inside a repository, or the
/// repository is bare. Branch, dirty count and upstream describe the working
/// tree containing `cwd`; `repo_root` is the main working tree.
///
/// # Errors
///
/// Returns an error when libgit2 fails on the repository itself. Failures on
/// individual linked worktrees only degrade that entry.
pub fn compute_repo_overview(cwd: &Path) -> Result<Option<RepoOverview>, GitOverviewError> {
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

    Ok(Some(RepoOverview {
        worktrees: list_worktrees(&main, &repo_root, &cwd_canonical),
        dirty_count: dirty_count(&current)?,
        repo_root,
        branch,
        detached,
        upstream,
        ahead,
        behind,
    }))
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

fn dirty_count(repo: &Repository) -> Result<usize, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    Ok(repo.statuses(Some(&mut opts))?.len())
}

fn list_worktrees(main: &Repository, repo_root: &Path, cwd: &Path) -> Vec<WorktreeInfo> {
    let (main_branch, _) = head_branch(main);
    let mut linked = Vec::new();
    if let Ok(names) = main.worktrees() {
        for name in names.iter().flatten().flatten() {
            if let Ok(worktree) = main.find_worktree(name) {
                linked.push(linked_info(&worktree));
            }
        }
    }
    linked.sort_by(|a, b| (a.branch.is_none(), &a.branch, &a.path).cmp(&(b.branch.is_none(), &b.branch, &b.path)));

    let mut all = Vec::with_capacity(linked.len() + 1);
    all.push(WorktreeInfo {
        path: repo_root.to_path_buf(),
        branch: main_branch,
        is_main: true,
        is_current: false,
        locked: false,
        prunable: false,
        dirty_count: dirty_count(main).unwrap_or(0),
    });
    all.extend(linked);
    mark_current(&mut all, cwd);
    all
}

fn linked_info(worktree: &Worktree) -> WorktreeInfo {
    let opened = Repository::open_from_worktree(worktree).ok();
    WorktreeInfo {
        path: worktree.path().to_path_buf(),
        branch: opened.as_ref().and_then(|repo| head_branch(repo).0),
        is_main: false,
        is_current: false,
        locked: matches!(worktree.is_locked(), Ok(WorktreeLockStatus::Locked(_))),
        prunable: worktree.validate().is_err(),
        dirty_count: opened.as_ref().and_then(|repo| dirty_count(repo).ok()).unwrap_or(0),
    }
}

/// Flag the worktree whose canonical path is the longest prefix of `cwd`.
fn mark_current(worktrees: &mut [WorktreeInfo], cwd: &Path) {
    let best = worktrees
        .iter()
        .enumerate()
        .filter_map(|(index, info)| {
            let canonical: PathBuf = info.path.canonicalize().ok()?;
            cwd.starts_with(&canonical)
                .then(|| (canonical.components().count(), index))
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
    use std::path::Path;

    use super::test_repo::Fixture;
    use super::*;

    fn canon(path: &Path) -> std::path::PathBuf {
        path.canonicalize().expect("canonicalize")
    }

    fn overview(path: &Path) -> RepoOverview {
        compute_repo_overview(path).expect("ok").expect("some")
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

        let o = overview(&fx.root());
        assert_eq!(o.worktrees.len(), 2);
        assert!(o.worktrees[0].is_main);
        let linked = &o.worktrees[1];
        assert!(!linked.is_main);
        assert_eq!(canon(&linked.path), canon(&wt));
        assert_eq!(linked.branch.as_deref(), Some("feature"));
        assert_eq!(linked.dirty_count, 1);
        assert!(!linked.locked && !linked.prunable);
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
    fn missing_worktree_dir_is_prunable_and_does_not_fail() {
        let fx = Fixture::with_commit();
        let wt = add_worktree(&fx, "gone");
        std::fs::remove_dir_all(&wt).expect("remove");
        let o = overview(&fx.root());
        assert_eq!(o.worktrees.len(), 2);
        assert!(o.worktrees[1].prunable);
        assert_eq!(o.worktrees[1].dirty_count, 0);
        assert!(o.worktrees[0].is_main);
    }
}
