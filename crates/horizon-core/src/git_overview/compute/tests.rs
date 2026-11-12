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
    assert_eq!(o.head_short.as_deref(), Some(&oid.to_string()[..7]));
    assert_eq!(o.worktrees[0].head_short, o.head_short);
}

#[test]
fn head_short_is_seven_chars_on_a_branch_and_none_when_unborn() {
    let fx = Fixture::unborn();
    let unborn = overview(&fx.root());
    assert_eq!(unborn.head_short, None);
    assert_eq!(unborn.worktrees[0].head_short, None);
    let oid = fx.commit_file("a.txt", "one\n");
    let o = overview(&fx.root());
    assert_eq!(o.head_short.as_deref(), Some(&oid.to_string()[..7]));
    assert_eq!(o.worktrees[0].head_short, o.head_short);
}

#[test]
fn linked_worktree_head_short_for_branch_and_detached() {
    let fx = Fixture::with_commit();
    let oid = fx.repo.head().expect("head").target().expect("oid");
    let expected = oid.to_string()[..7].to_string();
    add_worktree(&fx, "feature");
    let detached = add_worktree(&fx, "z");
    Repository::open(&detached)
        .expect("open")
        .set_head_detached(oid)
        .expect("detach");
    let o = overview(&fx.root());
    assert_eq!(o.worktrees.len(), 3);
    assert!(
        o.worktrees
            .iter()
            .all(|w| w.head_short.as_deref() == Some(expected.as_str()))
    );
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

#[test]
fn failed_scan_of_a_requested_worktree_is_none_not_clean() {
    let fx = Fixture::with_commit();
    let wt = add_worktree(&fx, "broken");
    std::fs::write(wt.join(".git"), "not a gitdir pointer\n").expect("corrupt");
    let o = overview_with(&fx.root(), &[&wt]);
    assert_eq!(o.worktrees[1].dirty_count, None);
}
