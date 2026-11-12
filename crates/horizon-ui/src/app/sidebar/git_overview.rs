//! Git line and worktrees list under each sidebar workspace row.
//!
//! Visual spec: `docs/design/claude-hub-ui.md` section 7. All git work happens
//! in the core `OverviewPoller` thread; this module only drains its results,
//! turns each overview into display strings once ([`build_cache`]) and paints
//! them. Nothing here touches git or builds strings per frame.
//!
//! The poller carries a repaint waker, so an idle UI draws a new result as soon
//! as it is delivered; nothing here polls on a timer.

mod actions;
mod paint;
mod row_menu;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Context;
use horizon_core::git_overview::{DEFAULT_INTERVAL, GitOverviewError, OverviewPoller, RepoOverview, WorktreeInfo};
use horizon_core::{Waker, WorkspaceId};

use crate::app::HorizonApp;

pub(super) use paint::{header_chip_width, paint_collapsed_dirty_dot, paint_header_chip, render_git_section};

/// Branch names longer than this are cut in the middle.
const BRANCH_MAX_CHARS: usize = 22;
/// Rows listed per group before the "Show all" row.
const VISIBLE_ROWS: usize = 8;
/// Results drained per frame.
const MAX_RESULTS_PER_FRAME: usize = 16;
/// How long the "Copied" feedback replaces the path on a row.
const COPIED_FEEDBACK: Duration = Duration::from_millis(1500);
const DETACHED_FALLBACK: &str = "HEAD (detached)";
const NO_COMMITS: &str = "(no commits)";

type OverviewResult = Result<Option<RepoOverview>, GitOverviewError>;

// ---------------------------------------------------------------------------
// Cache types (built once per overview)
// ---------------------------------------------------------------------------

/// Pieces of the hover card of a git line or worktree row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TooltipData {
    title: String,
    badges: Vec<&'static str>,
    path: String,
    upstream: Option<String>,
    changes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorktreeRow {
    path: PathBuf,
    /// Hash of `path`: stable widget/menu identity that survives reordering.
    key: u64,
    /// Branch label, middle-cut for the row.
    label: String,
    /// Name for a workspace created from this worktree (full branch or dir name).
    workspace_name: String,
    short_path: String,
    tooltip: TooltipData,
    is_current: bool,
    is_main: bool,
    /// Detached HEAD (or unreadable): the label is dimmed.
    detached: bool,
    /// Listed in the "agent worktrees" subgroup.
    agent_group: bool,
    dirty: bool,
    locked: bool,
    prunable: bool,
}

/// Display data of one workspace's repository.
#[derive(Clone, Debug)]
pub(in crate::app) struct GitLineCache {
    /// What this cache was built from; `None` for an error entry.
    source: Option<RepoOverview>,
    error: Option<String>,
    branch_label: String,
    detached: bool,
    dirty_count: usize,
    ahead_label: String,
    behind_label: String,
    dirty_label: String,
    tooltip: TooltipData,
    changed_at: Instant,
    /// Width of the branch chip on the workspace header, measured on first paint.
    chip_width: Cell<Option<f32>>,
    /// Primary rows first (`..primary_count`), then the agent subgroup.
    worktree_rows: Vec<WorktreeRow>,
    primary_count: usize,
    worktrees_count_label: String,
    agents_label: String,
    primary_show_all_label: String,
    agents_show_all_label: String,
}

impl GitLineCache {
    fn agent_count(&self) -> usize {
        self.worktree_rows.len() - self.primary_count
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Cuts `text` in the middle with an ellipsis so it has at most `max` chars.
pub(in crate::app::sidebar) fn middle_ellipsis(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail = keep / 2;
    let head = keep - tail;
    let mut out: String = text.chars().take(head).collect();
    out.push('\u{2026}');
    out.extend(text.chars().skip(count - tail));
    out
}

/// Stable identity of a worktree row: a hash of its path.
fn path_key(path: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

/// Full (uncut) branch text: the name (`<name> (no commits)` on an unborn
/// branch), or `HEAD @ <sha>` when detached.
fn branch_text(branch: Option<&str>, detached: bool, head_short: Option<&str>) -> String {
    match branch {
        Some(name) if !detached => match head_short {
            Some(_) => name.to_string(),
            None => format!("{name} {NO_COMMITS}"),
        },
        _ => match head_short {
            Some(sha) => format!("HEAD @ {}", sha.chars().take(7).collect::<String>()),
            None => DETACHED_FALLBACK.to_string(),
        },
    }
}

/// Branch label for a row: [`branch_text`] cut to 22 chars in the middle.
fn branch_label(branch: Option<&str>, detached: bool, head_short: Option<&str>) -> String {
    middle_ellipsis(&branch_text(branch, detached, head_short), BRANCH_MAX_CHARS)
}

/// Short path of worktree `wt`: `.` for the main one, relative to the main
/// root when nested inside it, `../name` for siblings of the main root, and
/// `~/...` or absolute for anything else.
fn short_worktree_path(main_root: &Path, wt: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    short_path_with_home(main_root, wt, home.as_deref())
}

fn short_path_with_home(main_root: &Path, wt: &Path, home: Option<&Path>) -> String {
    if wt == main_root {
        return ".".to_string();
    }
    if let Ok(rel) = wt.strip_prefix(main_root) {
        return rel.display().to_string();
    }
    if let Some(parent) = main_root.parent()
        && let Ok(rel) = wt.strip_prefix(parent)
    {
        return format!("../{}", rel.display());
    }
    if let Some(home) = home
        && let Ok(rel) = wt.strip_prefix(home)
    {
        return format!("~/{}", rel.display());
    }
    wt.display().to_string()
}

fn changes_text(count: Option<usize>) -> Option<String> {
    match count? {
        0 => Some("Working tree clean".to_string()),
        1 => Some("1 changed file".to_string()),
        n => Some(format!("{n} changed files")),
    }
}

fn upstream_text(overview: &RepoOverview) -> Option<String> {
    let upstream = overview.upstream.as_deref()?;
    let mut text = format!("upstream {upstream}");
    if overview.ahead > 0 {
        let _ = write!(text, " \u{b7} {} ahead", overview.ahead);
    }
    if overview.behind > 0 {
        let _ = write!(text, " \u{b7} {} behind", overview.behind);
    }
    Some(text)
}

fn counter_label(count: usize) -> String {
    if count == 0 { String::new() } else { count.to_string() }
}

fn build_row(overview: &RepoOverview, wt: &WorktreeInfo) -> WorktreeRow {
    let detached = wt.branch.is_none();
    let head_short = wt.head_short.as_deref();
    let title = branch_text(wt.branch.as_deref(), detached, head_short);
    let dir_name = wt.path.file_name().map_or_else(
        || wt.path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut badges = Vec::new();
    if wt.is_main {
        badges.push("main");
    }
    if wt.is_current {
        badges.push("current");
    }
    if wt.locked {
        badges.push("locked");
    }
    if wt.prunable {
        badges.push("prunable");
    }
    // Upstream and the live dirty count describe the checkout holding the cwd.
    let (upstream, dirty) = if wt.is_current {
        (upstream_text(overview), Some(overview.dirty_count))
    } else {
        (None, wt.dirty_count)
    };
    WorktreeRow {
        label: branch_label(wt.branch.as_deref(), detached, head_short),
        workspace_name: wt.branch.clone().unwrap_or(dir_name),
        short_path: short_worktree_path(&overview.repo_root, &wt.path),
        tooltip: TooltipData {
            title,
            badges,
            path: wt.path.display().to_string(),
            upstream,
            changes: changes_text(dirty),
        },
        key: path_key(&wt.path),
        path: wt.path.clone(),
        is_current: wt.is_current,
        is_main: wt.is_main,
        detached,
        agent_group: wt.is_agent && !wt.is_current,
        dirty: dirty.is_some_and(|count| count > 0),
        locked: wt.locked,
        prunable: wt.prunable,
    }
}

/// Primary rows first (main, current, then by name), agent subgroup after.
fn build_rows(overview: &RepoOverview) -> Vec<WorktreeRow> {
    let mut rows: Vec<WorktreeRow> = overview.worktrees.iter().map(|wt| build_row(overview, wt)).collect();
    rows.sort_by_cached_key(|row| {
        (
            row.agent_group,
            !row.is_main,
            !row.is_current,
            row.workspace_name.to_lowercase(),
        )
    });
    rows
}

/// Builds every display string of `overview` once.
fn build_cache(overview: &RepoOverview) -> GitLineCache {
    let worktree_rows = build_rows(overview);
    let primary_count = worktree_rows.iter().filter(|row| !row.agent_group).count();
    let agent_count = worktree_rows.len() - primary_count;
    let tooltip = worktree_rows.iter().find(|row| row.is_current).map_or_else(
        || TooltipData {
            title: branch_text(
                overview.branch.as_deref(),
                overview.detached,
                overview.head_short.as_deref(),
            ),
            badges: Vec::new(),
            path: overview.repo_root.display().to_string(),
            upstream: upstream_text(overview),
            changes: changes_text(Some(overview.dirty_count)),
        },
        |row| row.tooltip.clone(),
    );
    GitLineCache {
        source: Some(overview.clone()),
        error: None,
        branch_label: branch_label(
            overview.branch.as_deref(),
            overview.detached,
            overview.head_short.as_deref(),
        ),
        detached: overview.detached || overview.branch.is_none(),
        dirty_count: overview.dirty_count,
        ahead_label: counter_label(overview.ahead),
        behind_label: counter_label(overview.behind),
        dirty_label: counter_label(overview.dirty_count),
        tooltip,
        changed_at: Instant::now(),
        chip_width: Cell::new(None),
        worktrees_count_label: worktree_rows.len().to_string(),
        agents_label: format!("agent worktrees ({agent_count})"),
        primary_show_all_label: format!("Show all ({primary_count})"),
        agents_show_all_label: format!("Show all ({agent_count})"),
        worktree_rows,
        primary_count,
    }
}

fn error_cache(message: String) -> GitLineCache {
    GitLineCache {
        source: None,
        tooltip: TooltipData {
            title: "git?".to_string(),
            badges: Vec::new(),
            path: message.clone(),
            upstream: None,
            changes: None,
        },
        error: Some(message),
        branch_label: String::new(),
        detached: false,
        dirty_count: 0,
        ahead_label: String::new(),
        behind_label: String::new(),
        dirty_label: String::new(),
        changed_at: Instant::now(),
        chip_width: Cell::new(None),
        worktree_rows: Vec::new(),
        primary_count: 0,
        worktrees_count_label: String::new(),
        agents_label: String::new(),
        primary_show_all_label: String::new(),
        agents_show_all_label: String::new(),
    }
}

/// How many of `total` rows of a group are drawn.
fn visible_rows(total: usize, show_all: bool) -> usize {
    if show_all { total } else { total.min(VISIBLE_ROWS) }
}

/// `true` when the polled `(workspace, cwd)` set already equals `current`.
fn cwds_unchanged<'a>(
    synced: &[(WorkspaceId, PathBuf)],
    current: impl Iterator<Item = (WorkspaceId, &'a PathBuf)>,
) -> bool {
    let mut synced = synced.iter();
    for (id, cwd) in current {
        match synced.next() {
            Some((synced_id, synced_cwd)) if *synced_id == id && synced_cwd == cwd => {}
            _ => return false,
        }
    }
    synced.next().is_none()
}

fn sync_poller_cwds(workspaces: &[(WorkspaceId, PathBuf)], poller: &OverviewPoller<WorkspaceId>) {
    poller.set_cwds(workspaces.to_vec());
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which row's path was just copied, and when.
#[derive(Clone, Copy)]
struct Copied {
    workspace: WorkspaceId,
    /// `WorktreeRow::key` of the copied row.
    key: u64,
    at: Instant,
}

/// What a row or its menu asked for; applied by the app after the sidebar
/// frame. Rows are identified by worktree path, not position, so a reorder
/// between the click and its application cannot retarget the action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct GitAction {
    workspace: WorkspaceId,
    /// Worktree the action was picked on; re-checked against the current overview.
    path: PathBuf,
    kind: GitActionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitActionKind {
    OpenTerminal,
    OpenClaude { preset: usize },
    NewWorkspace,
    CopyPath,
}

/// Sidebar git state, owned by `HubState`. UI state (what is expanded) is not
/// persisted.
#[derive(Default)]
pub(in crate::app) struct GitSidebarState {
    caches: HashMap<WorkspaceId, GitLineCache>,
    poller: Option<OverviewPoller<WorkspaceId>>,
    expanded: HashSet<WorkspaceId>,
    agents_open: HashSet<WorkspaceId>,
    show_all: HashSet<WorkspaceId>,
    version: u64,
    synced_cwds: Vec<(WorkspaceId, PathBuf)>,
    /// Worktree paths last sent to `set_dirty_watch` (sorted).
    watch_sent: Vec<PathBuf>,
    /// Expansion or rows changed: recompute the dirty watch set.
    watch_stale: bool,
    /// `None` until the first frame, so startup does not count as a focus gain.
    window_focused: Option<bool>,
    copied: Option<Copied>,
}

impl GitSidebarState {
    /// Applies one poller result. Returns whether anything visible changed.
    fn apply_result(&mut self, key: WorkspaceId, result: OverviewResult) -> bool {
        match result {
            Ok(None) => {
                let removed = self.caches.remove(&key).is_some();
                if removed {
                    self.version = self.version.wrapping_add(1);
                    self.watch_stale = true;
                }
                removed
            }
            Ok(Some(overview)) => {
                let unchanged = self
                    .caches
                    .get(&key)
                    .and_then(|cache| cache.source.as_ref())
                    .is_some_and(|source| *source == overview);
                if unchanged {
                    return false;
                }
                self.version = self.version.wrapping_add(1);
                self.caches.insert(key, build_cache(&overview));
                self.watch_stale = true;
                true
            }
            Err(error) => {
                let message = error.to_string();
                let unchanged = self
                    .caches
                    .get(&key)
                    .is_some_and(|cache| cache.error.as_deref() == Some(message.as_str()));
                if unchanged {
                    return false;
                }
                self.version = self.version.wrapping_add(1);
                self.caches.insert(key, error_cache(message));
                self.watch_stale = true;
                true
            }
        }
    }

    /// Points the poller at a new `(workspace, cwd)` set and drops caches of
    /// workspaces that are gone.
    fn apply_cwds(&mut self, cwds: Vec<(WorkspaceId, PathBuf)>, ctx: &Context) {
        self.caches.retain(|id, _| cwds.iter().any(|(key, _)| key == id));
        self.expanded.retain(|id| cwds.iter().any(|(key, _)| key == id));
        match &self.poller {
            Some(poller) => sync_poller_cwds(&cwds, poller),
            None if !cwds.is_empty() => {
                let waker: Waker = {
                    let ctx = ctx.clone();
                    Arc::new(move || ctx.request_repaint())
                };
                self.poller = Some(OverviewPoller::start_with_waker(
                    cwds.clone(),
                    DEFAULT_INTERVAL,
                    Some(waker),
                ));
            }
            None => {}
        }
        self.synced_cwds = cwds;
        self.watch_stale = true;
    }

    /// Paths of the worktrees whose dirty count is visible right now.
    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for workspace in &self.expanded {
            let Some(cache) = self.caches.get(workspace) else {
                continue;
            };
            let show_all = self.show_all.contains(workspace);
            let primary = visible_rows(cache.primary_count, show_all);
            paths.extend(cache.worktree_rows.iter().take(primary).map(|row| row.path.clone()));
            if self.agents_open.contains(workspace) {
                let agents = visible_rows(cache.agent_count(), show_all);
                paths.extend(
                    cache
                        .worktree_rows
                        .iter()
                        .skip(cache.primary_count)
                        .take(agents)
                        .map(|row| row.path.clone()),
                );
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }

    /// The row for worktree `path` in the workspace's current overview.
    fn row_for_path(&self, workspace: WorkspaceId, path: &Path) -> Option<&WorktreeRow> {
        self.caches
            .get(&workspace)?
            .worktree_rows
            .iter()
            .find(|row| row.path == path)
    }
}

impl HorizonApp {
    /// Per-frame, allocation-free unless something changed: keeps the poller's
    /// cwd set in sync with the workspaces, drains results and refreshes the
    /// dirty watch set.
    pub(in crate::app) fn poll_git_overviews(&mut self, ctx: &Context) {
        let git = &mut self.hub.git;
        let current = self
            .board
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.cwd.as_ref().map(|cwd| (workspace.id, cwd)));
        if !cwds_unchanged(&git.synced_cwds, current) {
            let cwds: Vec<(WorkspaceId, PathBuf)> = self
                .board
                .workspaces
                .iter()
                .filter_map(|workspace| workspace.cwd.clone().map(|cwd| (workspace.id, cwd)))
                .collect();
            git.apply_cwds(cwds, ctx);
        }
        // Taken out while draining so `git` stays freely borrowable; put back below.
        let Some(poller) = git.poller.take() else {
            return;
        };

        let focused = ctx.input(|input| input.viewport().focused).unwrap_or(true);
        if focused && git.window_focused == Some(false) {
            poller.refresh_now();
        }
        git.window_focused = Some(focused);

        let results: Vec<(WorkspaceId, OverviewResult)> = std::iter::from_fn(|| poller.try_recv())
            .take(MAX_RESULTS_PER_FRAME)
            .collect();
        let drained_cap = results.len() == MAX_RESULTS_PER_FRAME;
        for (key, result) in results {
            git.apply_result(key, result);
        }

        if git.watch_stale {
            git.watch_stale = false;
            let paths = git.watch_paths();
            if paths != git.watch_sent {
                poller.set_dirty_watch(paths.clone());
                git.watch_sent = paths;
            }
        }

        git.poller = Some(poller);
        // This runs before the sidebar paints, so a change is drawn this frame;
        // only a capped drain needs another frame to pick up the rest.
        if drained_cap {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use horizon_core::WorkspaceId;
    use horizon_core::git_overview::{GitOverviewError, RepoOverview, WorktreeInfo};

    use super::{
        GitSidebarState, branch_label, build_cache, cwds_unchanged, middle_ellipsis, path_key, short_path_with_home,
        visible_rows,
    };

    fn worktree(path: &str, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            branch: branch.map(str::to_string),
            head_short: Some("abc1234".to_string()),
            is_main: false,
            is_current: false,
            locked: false,
            prunable: false,
            dirty_count: None,
            is_agent: false,
        }
    }

    fn overview(worktrees: Vec<WorktreeInfo>) -> RepoOverview {
        RepoOverview {
            repo_root: PathBuf::from("/work/api"),
            branch: Some("main".to_string()),
            detached: false,
            head_short: Some("abc1234".to_string()),
            dirty_count: 0,
            upstream: None,
            ahead: 0,
            behind: 0,
            worktrees,
        }
    }

    #[test]
    fn branch_label_detached_shows_short_sha() {
        assert_eq!(branch_label(None, true, Some("abc1234def")), "HEAD @ abc1234");
        assert_eq!(branch_label(None, true, None), "HEAD (detached)");
    }

    #[test]
    fn branch_label_unborn_branch_shows_no_commits() {
        assert_eq!(branch_label(Some("main"), false, None), "main (no commits)");
        assert_eq!(branch_label(Some("main"), false, Some("abc1234")), "main");
    }

    #[test]
    fn detached_overview_shows_head_sha_on_line_and_row() {
        let mut detached = worktree("/work/api-det", None);
        detached.head_short = Some("0123456789".to_string());
        let mut ov = overview(vec![detached]);
        ov.branch = None;
        ov.detached = true;
        ov.head_short = Some("0123456".to_string());
        let cache = build_cache(&ov);
        assert_eq!(cache.branch_label, "HEAD @ 0123456");
        assert_eq!(cache.worktree_rows[0].label, "HEAD @ 0123456");
        assert!(cache.worktree_rows[0].detached);
    }

    #[test]
    fn unborn_overview_shows_no_commits() {
        let mut unborn = worktree("/work/api", Some("main"));
        unborn.head_short = None;
        let mut ov = overview(vec![unborn]);
        ov.head_short = None;
        let cache = build_cache(&ov);
        assert_eq!(cache.branch_label, "main (no commits)");
        assert_eq!(cache.worktree_rows[0].label, "main (no commits)");
        assert_eq!(cache.worktree_rows[0].workspace_name, "main");
    }

    #[test]
    fn branch_label_middle_ellipsis_over_22() {
        assert_eq!(branch_label(Some("feat/short"), false, Some("abc1234")), "feat/short");
        let long = "feature/very-long-branch-name-here";
        let label = branch_label(Some(long), false, Some("abc1234"));
        assert_eq!(label.chars().count(), 22);
        assert!(label.starts_with("feature/ve"));
        assert!(label.ends_with("e-here"));
        assert!(label.contains('\u{2026}'));
        assert_eq!(
            middle_ellipsis("exactly-twenty-two-chrs", 23),
            "exactly-twenty-two-chrs"
        );
    }

    #[test]
    fn short_path_main_is_dot() {
        let root = Path::new("/work/api");
        assert_eq!(short_path_with_home(root, root, None), ".");
    }

    #[test]
    fn short_path_sibling_is_relative_to_repo_parent() {
        let root = Path::new("/work/api");
        assert_eq!(
            short_path_with_home(root, Path::new("/work/api-fix"), None),
            "../api-fix"
        );
        assert_eq!(
            short_path_with_home(root, Path::new("/work/api/.claude/worktrees/a1"), None),
            ".claude/worktrees/a1"
        );
    }

    #[test]
    fn short_path_outside_uses_tilde_or_absolute() {
        let root = Path::new("/work/api");
        let home = Path::new("/home/me");
        assert_eq!(
            short_path_with_home(root, Path::new("/home/me/trees/x"), Some(home)),
            "~/trees/x"
        );
        assert_eq!(short_path_with_home(root, Path::new("/srv/x"), Some(home)), "/srv/x");
        assert_eq!(short_path_with_home(root, Path::new("/srv/x"), None), "/srv/x");
    }

    #[test]
    fn build_cache_orders_main_first_and_marks_current() {
        let mut main = worktree("/work/api", Some("main"));
        main.is_main = true;
        let mut current = worktree("/work/api-zed", Some("zed"));
        current.is_current = true;
        let alpha = worktree("/work/api-alpha", Some("alpha"));
        let mut agent = worktree("/work/api/.claude/worktrees/a1", Some("agent-a1"));
        agent.is_agent = true;
        let cache = build_cache(&overview(vec![agent, alpha, current, main]));
        let names: Vec<&str> = cache
            .worktree_rows
            .iter()
            .map(|row| row.workspace_name.as_str())
            .collect();
        assert_eq!(names, ["main", "zed", "alpha", "agent-a1"]);
        assert!(cache.worktree_rows[1].is_current && !cache.worktree_rows[0].is_current);
        assert_eq!(cache.primary_count, 3);
        assert_eq!(cache.agent_count(), 1);
        assert_eq!(cache.agents_label, "agent worktrees (1)");
    }

    #[test]
    fn rows_are_found_by_path_after_a_reorder() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        let alpha = worktree("/work/api-alpha", Some("alpha"));
        let beta = worktree("/work/api-beta", Some("beta"));
        state.apply_result(key, Ok(Some(overview(vec![alpha.clone(), beta.clone()]))));
        let before = state.row_for_path(key, &beta.path).expect("beta").key;
        // A new branch sorts before beta: beta's index moves, its identity must not.
        let aaa = worktree("/work/api-aaa", Some("aaa"));
        state.apply_result(key, Ok(Some(overview(vec![alpha, beta.clone(), aaa]))));
        let row = state.row_for_path(key, &beta.path).expect("beta");
        assert_eq!(row.key, before);
        assert_eq!(row.workspace_name, "beta");
        assert_eq!(state.caches[&key].worktree_rows[2].path, beta.path);
    }

    #[test]
    fn action_on_a_removed_worktree_finds_no_row() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        let alpha = worktree("/work/api-alpha", Some("alpha"));
        let beta = worktree("/work/api-beta", Some("beta"));
        state.apply_result(key, Ok(Some(overview(vec![alpha.clone(), beta.clone()]))));
        state.apply_result(key, Ok(Some(overview(vec![alpha]))));
        assert!(state.row_for_path(key, &beta.path).is_none());
        assert!(
            state
                .row_for_path(WorkspaceId(2), Path::new("/work/api-alpha"))
                .is_none()
        );
    }

    #[test]
    fn row_keys_differ_per_path() {
        assert_ne!(path_key(Path::new("/a")), path_key(Path::new("/b")));
        assert_eq!(path_key(Path::new("/a")), path_key(Path::new("/a")));
    }

    #[test]
    fn current_agent_worktree_stays_in_primary_group() {
        let mut agent = worktree("/work/api/.claude/worktrees/a1", Some("agent-a1"));
        agent.is_agent = true;
        agent.is_current = true;
        let cache = build_cache(&overview(vec![agent]));
        assert_eq!(cache.primary_count, 1);
    }

    #[test]
    fn build_cache_hides_ahead_behind_when_zero() {
        let mut ov = overview(vec![worktree("/work/api", Some("main"))]);
        let cache = build_cache(&ov);
        assert!(cache.ahead_label.is_empty() && cache.behind_label.is_empty() && cache.dirty_label.is_empty());
        ov.upstream = Some("origin/main".to_string());
        ov.ahead = 2;
        ov.dirty_count = 5;
        let cache = build_cache(&ov);
        assert_eq!(cache.ahead_label, "2");
        assert!(cache.behind_label.is_empty());
        assert_eq!(cache.dirty_label, "5");
        assert_eq!(
            cache.tooltip.upstream.as_deref(),
            Some("upstream origin/main \u{b7} 2 ahead")
        );
    }

    #[test]
    fn no_cache_entry_for_non_repo_workspace() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        assert!(state.apply_result(key, Ok(Some(overview(vec![])))));
        assert!(state.caches.contains_key(&key));
        assert!(state.apply_result(key, Ok(None)));
        assert!(!state.caches.contains_key(&key));
        assert!(!state.apply_result(key, Ok(None)));
    }

    #[test]
    fn identical_overview_does_not_bump_version() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        assert!(state.apply_result(key, Ok(Some(overview(vec![])))));
        let version = state.version;
        assert!(!state.apply_result(key, Ok(Some(overview(vec![])))));
        assert_eq!(state.version, version);
        let mut changed = overview(vec![]);
        changed.dirty_count = 1;
        assert!(state.apply_result(key, Ok(Some(changed))));
        assert_ne!(state.version, version);
    }

    #[test]
    fn error_result_is_shown_once() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        let error = || Err(GitOverviewError::Io(std::io::Error::other("boom")));
        assert!(state.apply_result(key, error()));
        assert!(state.caches[&key].error.is_some());
        assert!(!state.apply_result(key, error()));
    }

    #[test]
    fn cwds_unchanged_detects_added_removed_and_moved() {
        let a = (WorkspaceId(1), PathBuf::from("/a"));
        let b = (WorkspaceId(2), PathBuf::from("/b"));
        let synced = vec![a.clone(), b.clone()];
        assert!(cwds_unchanged(&synced, [(a.0, &a.1), (b.0, &b.1)].into_iter()));
        assert!(!cwds_unchanged(&synced, [(a.0, &a.1)].into_iter()));
        assert!(!cwds_unchanged(&synced[..1], [(a.0, &a.1), (b.0, &b.1)].into_iter()));
        let moved = PathBuf::from("/c");
        assert!(!cwds_unchanged(&synced, [(a.0, &a.1), (b.0, &moved)].into_iter()));
    }

    #[test]
    fn visible_rows_caps_at_eight_until_show_all() {
        assert_eq!(visible_rows(5, false), 5);
        assert_eq!(visible_rows(42, false), 8);
        assert_eq!(visible_rows(42, true), 42);
    }

    #[test]
    fn watch_paths_only_cover_visible_rows() {
        let mut state = GitSidebarState::default();
        let key = WorkspaceId(1);
        let mut trees: Vec<WorktreeInfo> = (0..10)
            .map(|index| worktree(&format!("/work/api-{index:02}"), Some(&format!("b{index:02}"))))
            .collect();
        let mut agent = worktree("/work/api/.claude/worktrees/a1", Some("agent"));
        agent.is_agent = true;
        trees.push(agent);
        state.apply_result(key, Ok(Some(overview(trees))));
        assert!(state.watch_paths().is_empty(), "collapsed workspaces are not watched");
        state.expanded.insert(key);
        assert_eq!(state.watch_paths().len(), 8);
        state.agents_open.insert(key);
        assert_eq!(state.watch_paths().len(), 9);
        state.show_all.insert(key);
        assert_eq!(state.watch_paths().len(), 11);
    }
}
