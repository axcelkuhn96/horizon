use std::collections::HashSet;
use std::path::{Path, PathBuf};

use git2::{BranchType, Oid, Repository, RepositoryInitOptions, Signature};
use horizon_core::git_overview::{
    Blocker, CleanupCandidate, CleanupReason, CreateRequest, RemoveStatus, WorktreeError, classify_cleanup,
    create_worktree, plan_worktree_path, remove_worktrees, validate_branch_name,
};
use tempfile::TempDir;

/// `<tmp>/repo` with one commit on `main`; worktrees are siblings under `<tmp>`.
struct Fixture {
    tmp: TempDir,
    root: PathBuf,
    repo: Repository,
}

impl Fixture {
    fn new() -> Self {
        Self::with_head("main")
    }

    fn with_head(head: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).expect("repo dir");
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head(head);
        let repo = Repository::init_opts(&root, &opts).expect("init");
        commit_file(&repo, "a.txt", "one\n");
        Self { tmp, root, repo }
    }

    fn head_oid(&self) -> Oid {
        self.repo.head().expect("head").peel_to_commit().expect("commit").id()
    }

    fn branch_at_head(&self, name: &str) {
        let commit = self.repo.head().expect("head").peel_to_commit().expect("commit");
        self.repo.branch(name, &commit, false).expect("branch");
    }

    fn branch_exists(&self, name: &str) -> bool {
        self.repo.find_branch(name, BranchType::Local).is_ok()
    }

    /// Creates a worktree for `branch` (created at HEAD when missing) at the planned path.
    fn worktree(&self, branch: &str) -> PathBuf {
        let path = plan_worktree_path(&self.root, branch, Path::exists);
        create_worktree(
            &self.root,
            &CreateRequest {
                branch: branch.to_owned(),
                base: None,
                path: path.clone(),
            },
        )
        .expect("create worktree");
        path
    }

    fn classify(&self) -> Vec<CleanupCandidate> {
        classify_cleanup(&self.root, &[]).expect("classify")
    }

    fn candidate(&self, path: &Path) -> CleanupCandidate {
        self.classify()
            .into_iter()
            .find(|candidate| candidate.path == path)
            .expect("candidate listed")
    }
}

fn commit_file(repo: &Repository, name: &str, content: &str) -> Oid {
    let workdir = repo.workdir().expect("workdir");
    std::fs::write(workdir.join(name), content).expect("write");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new(name)).expect("add");
    index.write().expect("index write");
    let tree = repo.find_tree(index.write_tree().expect("tree")).expect("find tree");
    let sig = Signature::now("t", "t@example.com").expect("sig");
    let parents: Vec<_> = repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .into_iter()
        .collect();
    let parent_refs: Vec<_> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &parent_refs)
        .expect("commit")
}

fn remove(fixture: &Fixture, items: &[&Path], delete_branches: bool) -> Vec<RemoveStatus> {
    let items: Vec<PathBuf> = items.iter().map(|path| path.to_path_buf()).collect();
    remove_worktrees(&fixture.root, &items, delete_branches, &[])
        .into_iter()
        .map(|outcome| outcome.status)
        .collect()
}

// --- planning and validation -------------------------------------------------

#[test]
fn plan_path_is_sibling_with_slugged_branch() {
    let path = plan_worktree_path(Path::new("/work/api-core"), "feat/new-thing", |_| false);
    assert_eq!(path, PathBuf::from("/work/api-core-feat-new-thing"));
    // A trailing slash on the repository root changes nothing.
    let path = plan_worktree_path(Path::new("/work/api-core/"), "fix", |_| false);
    assert_eq!(path, PathBuf::from("/work/api-core-fix"));
}

#[test]
fn plan_path_appends_suffix_when_taken() {
    let taken: HashSet<PathBuf> = ["/work/api-fix", "/work/api-fix-2"].iter().map(PathBuf::from).collect();
    let path = plan_worktree_path(Path::new("/work/api"), "fix", |candidate| taken.contains(candidate));
    assert_eq!(path, PathBuf::from("/work/api-fix-3"));
}

#[test]
fn validate_branch_rejects_bad_names() {
    let too_long = "x".repeat(201);
    for bad in [
        "",
        "   ",
        "-x",
        "a..b",
        "a b",
        "a\u{7}b",
        "HEAD",
        "feat/",
        "a~b",
        "a:b",
        "x.lock",
        too_long.as_str(),
    ] {
        assert!(validate_branch_name(bad).is_err(), "{bad:?} must be rejected");
    }
    for good in ["main", "feat/new-thing", "fix_login-2", "release/2.1"] {
        assert!(validate_branch_name(good).is_ok(), "{good:?} must be accepted");
    }
}

// --- create -----------------------------------------------------------------------

#[test]
fn create_checks_out_existing_branch() {
    let fixture = Fixture::new();
    fixture.branch_at_head("existing");
    let path = plan_worktree_path(&fixture.root, "existing", Path::exists);

    let created = create_worktree(
        &fixture.root,
        &CreateRequest {
            branch: "existing".to_owned(),
            base: Some("ignored-for-existing".to_owned()),
            path: path.clone(),
        },
    )
    .expect("create");

    assert!(!created.created_branch);
    assert!(path.join("a.txt").is_file());
    let checkout = Repository::open(&path).expect("open worktree");
    assert_eq!(checkout.head().expect("head").shorthand(), Some("existing"));
}

#[test]
fn create_new_branch_from_base() {
    let fixture = Fixture::new();
    let first = fixture.head_oid();
    commit_file(&fixture.repo, "b.txt", "two\n");
    let path = plan_worktree_path(&fixture.root, "feature/new", Path::exists);

    let created = create_worktree(
        &fixture.root,
        &CreateRequest {
            branch: "feature/new".to_owned(),
            base: Some(first.to_string()),
            path: path.clone(),
        },
    )
    .expect("create");

    assert!(created.created_branch);
    let tip = fixture
        .repo
        .find_branch("feature/new", BranchType::Local)
        .expect("branch")
        .get()
        .peel_to_commit()
        .expect("commit")
        .id();
    assert_eq!(tip, first);
    assert!(path.join("a.txt").is_file());
    assert!(!path.join("b.txt").exists(), "checked out from the older base");
}

#[test]
fn create_existing_branch_checked_out_elsewhere_errors() {
    let fixture = Fixture::new();
    let first = fixture.worktree("taken");

    let again = create_worktree(
        &fixture.root,
        &CreateRequest {
            branch: "taken".to_owned(),
            base: None,
            path: fixture.tmp.path().join("second"),
        },
    );
    match again {
        Err(WorktreeError::BranchCheckedOut { branch, path }) => {
            assert_eq!(branch, "taken");
            assert_eq!(
                path.canonicalize().expect("canon"),
                first.canonicalize().expect("canon")
            );
        }
        other => panic!("expected BranchCheckedOut, got {other:?}"),
    }
    assert!(!fixture.tmp.path().join("second").exists());

    let main_branch = create_worktree(
        &fixture.root,
        &CreateRequest {
            branch: "main".to_owned(),
            base: None,
            path: fixture.tmp.path().join("third"),
        },
    );
    assert!(matches!(main_branch, Err(WorktreeError::BranchCheckedOut { .. })));
}

#[test]
fn create_rejects_taken_or_unusable_destinations() {
    let fixture = Fixture::new();
    let taken = fixture.tmp.path().join("taken");
    std::fs::create_dir(&taken).expect("dir");
    let request = |path: PathBuf| CreateRequest {
        branch: "new".to_owned(),
        base: None,
        path,
    };

    assert!(matches!(
        create_worktree(&fixture.root, &request(taken)),
        Err(WorktreeError::DestinationExists(_))
    ));
    assert!(matches!(
        create_worktree(&fixture.root, &request(fixture.tmp.path().join("no/such/parent"))),
        Err(WorktreeError::DestinationParentMissing(_))
    ));
    assert!(matches!(
        create_worktree(&fixture.root, &request(PathBuf::from("relative/dir"))),
        Err(WorktreeError::InvalidDestination(_))
    ));
    assert!(
        !fixture.branch_exists("new"),
        "no branch is created for a rejected destination"
    );
}

#[test]
fn create_failure_rolls_back_created_branch() {
    let fixture = Fixture::new();
    // A file where libgit2 expects the `worktrees` directory makes the add fail
    // after the branch was already created.
    std::fs::write(fixture.root.join(".git").join("worktrees"), "not a directory").expect("block");
    let path = fixture.tmp.path().join("repo-doomed");

    let result = create_worktree(
        &fixture.root,
        &CreateRequest {
            branch: "doomed".to_owned(),
            base: None,
            path: path.clone(),
        },
    );

    assert!(result.is_err());
    assert!(
        !fixture.branch_exists("doomed"),
        "the branch made for the attempt is removed"
    );
    assert!(!path.exists(), "no directory is left behind");
}

// --- classification -----------------------------------------------------------------

#[test]
fn classify_marks_merged_branch() {
    let fixture = Fixture::new();
    let path = fixture.worktree("done");

    let candidate = fixture.candidate(&path);

    assert_eq!(candidate.reason, CleanupReason::Merged);
    assert_eq!(candidate.branch.as_deref(), Some("done"));
    assert!(candidate.safe(), "blockers: {:?}", candidate.blockers);
}

#[test]
fn classify_unmerged_branch_not_candidate() {
    let fixture = Fixture::new();
    let path = fixture.worktree("wip");
    let checkout = Repository::open(&path).expect("open");
    commit_file(&checkout, "wip.txt", "work\n");

    assert!(fixture.classify().iter().all(|candidate| candidate.path != path));
}

#[test]
fn classify_blocks_dirty_including_untracked() {
    let fixture = Fixture::new();
    let untracked = fixture.worktree("untracked");
    let modified = fixture.worktree("modified");
    let clean = fixture.worktree("clean");
    std::fs::write(untracked.join("new.txt"), "x").expect("write");
    std::fs::write(modified.join("a.txt"), "changed\n").expect("write");

    assert_eq!(fixture.candidate(&untracked).blockers, [Blocker::Dirty]);
    assert_eq!(fixture.candidate(&modified).blockers, [Blocker::Dirty]);
    assert!(fixture.candidate(&clean).safe());
}

#[test]
fn classify_blocks_locked() {
    let fixture = Fixture::new();
    let path = fixture.worktree("pinned");
    let names = fixture.repo.worktrees().expect("worktrees");
    let name = names.get(0).expect("get").expect("name").to_owned();
    fixture
        .repo
        .find_worktree(&name)
        .expect("find")
        .lock(Some("keep"))
        .expect("lock");

    assert_eq!(fixture.candidate(&path).blockers, [Blocker::Locked]);
}

#[test]
fn classify_never_lists_main_worktree() {
    let fixture = Fixture::new();
    fixture.worktree("done");

    let root = fixture.root.canonicalize().expect("canon");
    assert!(
        fixture
            .classify()
            .iter()
            .all(|candidate| candidate.path.canonicalize().ok().as_deref() != Some(root.as_path()))
    );
}

#[test]
fn classify_blocks_protected_by_open_workspace() {
    let fixture = Fixture::new();
    let path = fixture.worktree("open");
    let inside = path.join("sub");
    std::fs::create_dir(&inside).expect("dir");

    let candidates = classify_cleanup(&fixture.root, &[inside]).expect("classify");

    let candidate = candidates
        .iter()
        .find(|candidate| candidate.path == path)
        .expect("listed");
    assert_eq!(candidate.blockers, [Blocker::ProtectedByWorkspace]);
}

#[test]
fn classify_blocks_unreachable_detached_commits() {
    let fixture = Fixture::new();
    let path = fixture.worktree("scratch");
    let checkout = Repository::open(&path).expect("open");
    let lost = commit_file(&checkout, "lost.txt", "only here\n");
    checkout.set_head_detached(lost).expect("detach");
    fixture
        .repo
        .find_branch("scratch", BranchType::Local)
        .expect("branch")
        .delete()
        .expect("delete branch");

    let candidate = fixture.candidate(&path);

    assert_eq!(candidate.reason, CleanupReason::Orphan);
    assert!(candidate.blockers.contains(&Blocker::UnreachableCommits));
    assert!(!candidate.safe());
}

#[test]
fn classify_prunable_when_dir_missing() {
    let fixture = Fixture::new();
    let path = fixture.worktree("gone");
    std::fs::remove_dir_all(&path).expect("remove dir");

    let candidate = fixture.candidate(&path);

    assert_eq!(candidate.reason, CleanupReason::Prunable);
    assert!(candidate.safe());
}

#[test]
fn classify_default_branch_unknown_blocks_merged_reason() {
    let fixture = Fixture::with_head("trunk");
    let path = fixture.worktree("done");
    // Merged only through its upstream: no `main`, `master` or `origin/HEAD` exists.
    fixture
        .repo
        .remote("origin", "https://example.invalid/x.git")
        .expect("remote");
    fixture
        .repo
        .reference("refs/remotes/origin/done", fixture.head_oid(), true, "test")
        .expect("remote ref");
    let mut config = fixture.repo.config().expect("config");
    config.set_str("branch.done.remote", "origin").expect("remote cfg");
    config
        .set_str("branch.done.merge", "refs/heads/done")
        .expect("merge cfg");

    let candidate = fixture.candidate(&path);

    assert_eq!(candidate.reason, CleanupReason::Merged);
    assert_eq!(candidate.blockers, [Blocker::DefaultBranchUnknown]);
}

// --- remove -------------------------------------------------------------------------

#[test]
fn remove_deletes_clean_merged_worktree_and_metadata() {
    let fixture = Fixture::new();
    let path = fixture.worktree("done");

    let statuses = remove(&fixture, &[&path], false);

    assert_eq!(statuses, [RemoveStatus::Removed { branch_deleted: false }]);
    assert!(!path.exists());
    assert_eq!(fixture.repo.worktrees().expect("worktrees").len(), 0);
}

#[test]
fn remove_rechecks_dirty_before_removing() {
    let fixture = Fixture::new();
    let path = fixture.worktree("done");
    assert!(fixture.candidate(&path).safe());
    // Becomes dirty after the dialog classified it.
    std::fs::write(path.join("late.txt"), "late").expect("write");

    let statuses = remove(&fixture, &[&path], true);

    assert_eq!(statuses, [RemoveStatus::Kept(vec![Blocker::Dirty])]);
    assert!(path.join("late.txt").is_file());
    assert!(fixture.branch_exists("done"));
}

#[test]
fn remove_never_deletes_locked_or_in_use() {
    let fixture = Fixture::new();
    let locked = fixture.worktree("locked");
    let in_use = fixture.worktree("in-use");
    let names = fixture.repo.worktrees().expect("worktrees");
    for name in names.iter().flatten().flatten() {
        let worktree = fixture.repo.find_worktree(name).expect("find");
        if worktree.path() == locked {
            worktree.lock(None).expect("lock");
        }
    }

    let outcomes = remove_worktrees(
        &fixture.root,
        &[locked.clone(), in_use.clone()],
        true,
        &[in_use.clone()],
    );

    assert_eq!(outcomes[0].status, RemoveStatus::Kept(vec![Blocker::Locked]));
    assert_eq!(
        outcomes[1].status,
        RemoveStatus::Kept(vec![Blocker::ProtectedByWorkspace])
    );
    assert!(locked.exists() && in_use.exists());
}

#[test]
fn remove_refuses_unregistered_path_and_traversal() {
    let fixture = Fixture::new();
    let path = fixture.worktree("done");
    let outside = fixture.tmp.path().join("outside");
    std::fs::create_dir(&outside).expect("dir");
    std::fs::write(outside.join("keep.txt"), "keep").expect("write");
    let traversal = path.join("..").join("outside");
    let mut items = vec![outside.clone(), traversal, fixture.root.clone()];
    #[cfg(unix)]
    {
        let link = fixture.tmp.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        items.push(link);
    }
    let items: Vec<&Path> = items.iter().map(PathBuf::as_path).collect();

    let statuses = remove(&fixture, &items, true);

    assert!(
        statuses.iter().all(|status| *status == RemoveStatus::Refused),
        "{statuses:?}"
    );
    assert!(outside.join("keep.txt").is_file());
    assert!(fixture.root.join("a.txt").is_file());
    assert!(path.exists());
}

#[test]
fn remove_keeps_unmerged_branch() {
    let fixture = Fixture::new();
    let path = fixture.worktree("wip");
    let checkout = Repository::open(&path).expect("open");
    commit_file(&checkout, "wip.txt", "work\n");

    let statuses = remove(&fixture, &[&path], true);

    assert_eq!(statuses, [RemoveStatus::NotCandidate]);
    assert!(path.join("wip.txt").is_file());
    assert!(fixture.branch_exists("wip"));
}

#[test]
fn remove_deletes_merged_branch_only_when_flag() {
    let fixture = Fixture::new();
    let keep = fixture.worktree("keep-branch");
    let drop = fixture.worktree("drop-branch");

    let first = remove(&fixture, &[&keep], false);
    let second = remove(&fixture, &[&drop], true);

    assert_eq!(first, [RemoveStatus::Removed { branch_deleted: false }]);
    assert_eq!(second, [RemoveStatus::Removed { branch_deleted: true }]);
    assert!(fixture.branch_exists("keep-branch"));
    assert!(!fixture.branch_exists("drop-branch"));
    assert!(fixture.branch_exists("main"));
}

#[test]
fn remove_partial_failure_continues_others() {
    let fixture = Fixture::new();
    let first = fixture.worktree("first");
    let bogus = fixture.tmp.path().join("not-a-worktree");
    let second = fixture.worktree("second");

    let statuses = remove(&fixture, &[&first, &bogus, &second], false);

    assert_eq!(statuses[0], RemoveStatus::Removed { branch_deleted: false });
    assert_eq!(statuses[1], RemoveStatus::Refused);
    assert_eq!(statuses[2], RemoveStatus::Removed { branch_deleted: false });
    assert!(!first.exists() && !second.exists());
}

#[test]
fn remove_prunes_metadata_of_missing_directory() {
    let fixture = Fixture::new();
    let path = fixture.worktree("gone");
    std::fs::remove_dir_all(&path).expect("remove dir");

    let statuses = remove(&fixture, &[&path], false);

    assert_eq!(statuses, [RemoveStatus::Removed { branch_deleted: false }]);
    assert_eq!(fixture.repo.worktrees().expect("worktrees").len(), 0);
}
