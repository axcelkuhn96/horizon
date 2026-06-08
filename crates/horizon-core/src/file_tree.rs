//! Lazy, gitignore-aware project file tree backing the `FileExplorer` panel.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

use crate::error::Result;
use crate::file_search::FileSearchOptions;
use crate::file_search_runner::{SearchRunner, SearchState};
use crate::git_status::{FileStatus, GitStatus, compute_status};

/// Quiescence window the content-search must observe before dispatching a new
/// query to the background runner. The runner does NOT debounce itself (see its
/// module docs), so the caller must: we only `start` after this much input
/// silence to avoid spawning one full directory walk per keystroke.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

/// Decides whether the content-search should dispatch `query` to the background
/// runner right now. Pure and deterministic (no clock): the caller passes the
/// elapsed time since the last edit so tests stay timing-free.
///
/// Returns `true` only when ALL hold:
/// - `query` is non-empty after trimming (empty/whitespace never searches);
/// - `query` differs from `last_dispatched` (no redundant re-dispatch of the
///   same query);
/// - the input has been quiescent for at least `debounce` (that is,
///   `elapsed_since_edit >= debounce`), satisfying the runner's contract.
#[must_use]
pub fn should_dispatch(
    query: &str,
    last_dispatched: &str,
    elapsed_since_edit: Duration,
    debounce: Duration,
) -> bool {
    if query.trim().is_empty() {
        return false;
    }
    if query == last_dispatched {
        return false;
    }
    elapsed_since_edit >= debounce
}

/// Throttle interval for the File Explorer's automatic git-status refresh.
///
/// The shared [`crate::git_watcher::GitWatcher`] only recomputes status when the
/// `.git/index` mtime changes (i.e. on `git add` / `git commit`); a plain
/// working-tree edit or a new untracked file never touches the index, so the
/// green/changed decorations would otherwise go stale until a commit. The
/// explorer therefore recomputes its own snapshot on this cadence while visible.
/// `git2` status on a normal repo is fast; 1.5s keeps it cheap.
pub const GIT_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);

/// Decide whether to recompute the explorer's git status this frame.
///
/// Pure and clock-free (the caller passes `elapsed_since_last`), so the trigger
/// logic stays unit-testable. Returns `true` when ANY of:
/// - `focus_regained` — the panel just became focused this frame (immediate
///   refresh so the user sees fresh decorations on tab-back);
/// - `elapsed_since_last` is `None` — status has never been refreshed yet;
/// - `elapsed_since_last >= interval` — the throttle window has elapsed.
#[must_use]
pub fn should_refresh_git(
    elapsed_since_last: Option<Duration>,
    interval: Duration,
    focus_regained: bool,
) -> bool {
    if focus_regained {
        return true;
    }
    match elapsed_since_last {
        None => true,
        Some(elapsed) => elapsed >= interval,
    }
}

/// One visible explorer row's screen-space hit box, recorded each frame so the
/// app-level OS-file-drop handler can map a global pointer position to the tree
/// node under the cursor without depending on `egui` types.
///
/// `rect` is `(min_x, min_y, max_x, max_y)` in screen (global) coordinates. The
/// geometry test lives in [`row_hit_at`], kept pure (plain floats) so it can be
/// unit-tested without a renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct RowHit {
    /// Screen-space rect as `(min_x, min_y, max_x, max_y)`.
    pub rect: (f32, f32, f32, f32),
    /// Absolute path of the node this row paints.
    pub path: PathBuf,
    /// `true` for directory rows, `false` for files.
    pub is_dir: bool,
}

/// Returns the topmost recorded [`RowHit`] whose screen rect contains `(x, y)`,
/// or `None` when the point hits no row. Rows are tested in reverse
/// (last-painted-first) so a later row wins when rects overlap.
///
/// Pure geometry over plain floats — no `egui`. Callers that need the full row
/// (path + `is_dir` + rect for a highlight) use this to avoid a second scan;
/// [`row_hit_at`] is the thin predicate wrapper returning just `(path, is_dir)`.
#[must_use]
pub fn row_hit_entry_at(rows: &[RowHit], x: f32, y: f32) -> Option<&RowHit> {
    rows.iter().rev().find(|row| {
        let (min_x, min_y, max_x, max_y) = row.rect;
        x >= min_x && x <= max_x && y >= min_y && y <= max_y
    })
}

/// Returns the path + `is_dir` of the topmost recorded row whose screen rect
/// contains `(x, y)`, or `None` when the point hits no row. Thin wrapper over
/// [`row_hit_entry_at`] for callers that only need the destination identity.
///
/// Pure geometry over plain floats — no `egui`, so it is directly unit-tested.
#[must_use]
pub fn row_hit_at(rows: &[RowHit], x: f32, y: f32) -> Option<(&Path, bool)> {
    row_hit_entry_at(rows, x, y).map(|row| (row.path.as_path(), row.is_dir))
}

/// Resolve the destination directory for an OS-file drop, given the explorer row
/// under the cursor (`path` + `is_dir`) and the explorer `root`.
///
/// - A folder hit copies INTO that folder.
/// - A file hit copies into the file's parent directory (falling back to `root`
///   if the file somehow has no parent).
/// - No hit (empty area / panel background) copies into `root`.
#[must_use]
pub fn drop_target_dir(hit: Option<(&Path, bool)>, root: &Path) -> PathBuf {
    match hit {
        Some((path, true)) => path.to_path_buf(),
        Some((path, false)) => path.parent().map_or_else(|| root.to_path_buf(), Path::to_path_buf),
        None => root.to_path_buf(),
    }
}

/// One node in the file tree. `children == None` means "directory not yet
/// scanned" (lazy). `children == Some(_)` means scanned (possibly empty).
#[derive(Clone, Debug)]
pub struct FileNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Option<Vec<FileNode>>,
    /// `true` when this entry is matched by a `.gitignore` rule. The entry is
    /// still listed (so the UI can dim it) rather than hidden. Entries under
    /// [`HARD_SKIP`] are never produced and so never reach this flag.
    pub ignored: bool,
}

/// Directories we never descend into regardless of .gitignore.
const HARD_SKIP: [&str; 3] = [".git", "node_modules", "target"];

/// A stack of per-directory `.gitignore` matchers, ordered nearest-first
/// (the scanned dir's own `.gitignore`, then each ancestor up to the repo root).
/// Each matcher is rooted at the directory whose `.gitignore` it came from, so
/// anchored patterns like `dist/` resolve relative to that directory.
struct IgnoreMatchers {
    /// Nearest-first: `matchers[0]` is `dir`'s own `.gitignore`.
    matchers: Vec<Gitignore>,
}

impl IgnoreMatchers {
    /// Returns `true` if `path` is ignored. The nearest matcher with a decisive
    /// rule wins, mirroring git: a closer `.gitignore` can whitelist (`!foo`)
    /// something an ancestor ignored, so we stop at the first non-`None` match.
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.matchers {
            let m = matcher.matched(path, is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        false
    }
}

/// Build the ancestor-aware ignore matcher for `dir`: collect `dir/.gitignore`
/// and every ancestor `.gitignore` walking UP to (and including) the repo root
/// — the first ancestor containing a `.git` entry — or the filesystem root if
/// no repo is found. Matchers are returned nearest-first so closer rules win.
///
/// Errors (missing files, bad globs) are swallowed: a missing `.gitignore` or a
/// failed build simply contributes no rules, never a panic. `.git` /
/// `node_modules` / `target` are still removed by [`HARD_SKIP`] in the walk,
/// independent of this matcher.
fn build_ignore_matcher(dir: &Path) -> IgnoreMatchers {
    let mut matchers = Vec::new();
    let mut current = Some(dir);
    while let Some(d) = current {
        let mut builder = GitignoreBuilder::new(d);
        // `add` returns Some(err) on failure (the file is absent, or present
        // but unreadable / permission-denied); either way this directory
        // contributes no ignore rules.
        if builder.add(d.join(".gitignore")).is_none()
            && let Ok(gi) = builder.build()
        {
            matchers.push(gi);
        }
        // Stop after the repo root (the dir holding `.git`); patterns above the
        // repo do not apply to paths inside it.
        if d.join(".git").exists() {
            break;
        }
        current = d.parent();
    }
    IgnoreMatchers { matchers }
}

/// Scan a single directory level. Dirs first, then files, each alphabetical
/// (case-insensitive). Gitignored entries are still listed but tagged
/// [`FileNode::ignored`] so the UI can dim them; [`HARD_SKIP`] dirs are always
/// omitted entirely.
///
/// # Errors
///
/// Currently infallible: individual walk errors (e.g. permission denied) are
/// skipped rather than propagated, so this never returns `Err`. The `Result`
/// return type is retained so future stricter scanning can surface failures.
pub fn scan_dir(dir: &Path) -> Result<Vec<FileNode>> {
    let mut entries: Vec<FileNode> = Vec::new();
    let matcher = build_ignore_matcher(dir);

    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1)) // only this level
        .hidden(false) // show dotfiles like .gitignore
        // Yield gitignored entries so they can be shown (dimmed); we classify
        // them ourselves via `matcher` below.
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .filter_entry(|entry| entry.file_name().to_str().is_none_or(|name| !HARD_SKIP.contains(&name)))
        .build();

    for result in walker {
        // permission denied etc: skip, never panic
        let Ok(entry) = result else {
            continue;
        };
        // max_depth(1) yields the root itself first; skip it.
        if entry.path() == dir {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        let ignored = matcher.is_ignored(&path, is_dir);
        entries.push(FileNode {
            name,
            path,
            is_dir,
            children: None,
            ignored,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir) // dirs first
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Look up the git status for an absolute path against a [`GitStatus`] snapshot.
#[must_use]
pub fn status_for_path(status: &GitStatus, abs_path: &Path) -> Option<FileStatus> {
    let rel = abs_path.strip_prefix(&status.repo_root).ok()?;
    let rel = rel.to_str()?;
    status.changes.iter().find(|c| c.path == rel).map(|c| c.status)
}

/// Returns `true` when any pending change in `status` lives strictly inside
/// `abs_dir` (at any depth). Used to tint a folder green in the normal tree when
/// it contains uncommitted work, like `VSCode`'s changed-folder propagation.
///
/// Matching is **component-wise**, not string-prefix: a change under `src/` does
/// not count for a directory named `src2` (and vice versa). `abs_dir` equal to
/// the repo root is "inside" for every change, so the root lights up whenever
/// any change exists.
#[must_use]
pub fn dir_contains_changes(status: &GitStatus, abs_dir: &Path) -> bool {
    // The directory must itself be within (or equal to) the repo root, else no
    // repo-relative change can be inside it.
    let Ok(dir_rel) = abs_dir.strip_prefix(&status.repo_root) else {
        return false;
    };
    let dir_components: Vec<_> = dir_rel.components().collect();
    status.changes.iter().any(|change| {
        let change_path = Path::new(&change.path);
        let mut change_components = change_path.components();
        // Every component of the directory must be a leading component of the
        // change path; the change must then have at least one more component
        // (the file itself) so the directory is a strict ancestor.
        for dir_comp in &dir_components {
            match change_components.next() {
                Some(c) if c == *dir_comp => {}
                _ => return false,
            }
        }
        change_components.next().is_some()
    })
}

/// One row of the "only uncommitted files" filtered view: an absolute path,
/// its git status, and the repo-relative display path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFileRow {
    /// Absolute path (repo root joined with the change's relative path).
    pub abs_path: PathBuf,
    /// Repo-relative path as reported by git (used for display).
    pub rel_path: String,
    pub status: FileStatus,
}

/// Builds the flat list of rows for the File Explorer "show only uncommitted
/// files" filter from a [`GitStatus`] snapshot. Returns one row per pending
/// change, preserving git's ordering. Includes every tracked change kind
/// (Modified, Added, Untracked, Deleted, Renamed).
#[must_use]
pub fn changed_file_rows(status: &GitStatus) -> Vec<ChangedFileRow> {
    status
        .changes
        .iter()
        .map(|change| ChangedFileRow {
            abs_path: status.repo_root.join(&change.path),
            rel_path: change.path.clone(),
            status: change.status,
        })
        .collect()
}

/// One node of the grouped "only uncommitted files" tree: directories carry
/// their changed descendants in `children`; files carry their git `status`.
/// Single-child directory chains are compacted VSCode-style, so `name` may be
/// a joined path segment like `"src/app/views"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedTreeNode {
    /// Display name. For compacted directory chains this is `"a/b/c"`.
    pub name: String,
    /// Absolute path (repo root joined with the relative path).
    pub abs_path: PathBuf,
    pub is_dir: bool,
    /// `Some` for files, `None` for directories.
    pub status: Option<FileStatus>,
    pub children: Vec<ChangedTreeNode>,
}

/// Builds the grouped tree for the File Explorer "show only uncommitted files"
/// filter: changed files nested under their parent directories (dirs first,
/// case-insensitive alphabetical), with single-child directory chains
/// compacted into one node (`a/b/c`) like `VSCode`'s compact folders.
#[must_use]
pub fn changed_file_tree(status: &GitStatus) -> Vec<ChangedTreeNode> {
    let mut roots: Vec<ChangedTreeNode> = Vec::new();

    for change in &status.changes {
        let mut components: Vec<&str> = change.path.split('/').filter(|c| !c.is_empty()).collect();
        let Some(file_name) = components.pop() else {
            continue;
        };

        let mut abs = status.repo_root.clone();
        let mut current = &mut roots;
        for dir in components {
            abs.push(dir);
            let idx = current
                .iter()
                .position(|n| n.is_dir && n.abs_path == abs)
                .unwrap_or_else(|| {
                    current.push(ChangedTreeNode {
                        name: dir.to_string(),
                        abs_path: abs.clone(),
                        is_dir: true,
                        status: None,
                        children: Vec::new(),
                    });
                    current.len() - 1
                });
            current = &mut current[idx].children;
        }

        abs.push(file_name);
        current.push(ChangedTreeNode {
            name: file_name.to_string(),
            abs_path: abs,
            is_dir: false,
            status: Some(change.status),
            children: Vec::new(),
        });
    }

    sort_changed_nodes(&mut roots);
    compact_dir_chains(&mut roots);
    roots
}

/// Recursively sorts: directories first, then case-insensitive alphabetical.
fn sort_changed_nodes(nodes: &mut [ChangedTreeNode]) {
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for node in nodes {
        sort_changed_nodes(&mut node.children);
    }
}

/// Merges single-child directory chains (`a > b > c`) into one `a/b/c` node,
/// recursively, mirroring `VSCode`'s "compact folders" behavior.
fn compact_dir_chains(nodes: &mut [ChangedTreeNode]) {
    for node in nodes.iter_mut() {
        while node.is_dir && node.children.len() == 1 && node.children[0].is_dir {
            let child = node.children.remove(0);
            node.name = format!("{}/{}", node.name, child.name);
            node.abs_path = child.abs_path;
            node.children = child.children;
        }
        compact_dir_chains(&mut node.children);
    }
}

/// Per-panel file-explorer state. Lives inside `PanelContent::FileExplorer`.
///
/// Not `Clone`/`Debug`: it owns a [`SearchRunner`], which holds a thread
/// [`std::sync::mpsc::Receiver`] (neither `Clone` nor `Debug`). It is never
/// cloned — each panel constructs its own with [`FileTreeState::new`].
pub struct FileTreeState {
    pub root: PathBuf,
    pub roots: Vec<FileNode>,
    pub loaded: bool,
    pub git_status: Option<Arc<GitStatus>>,
    /// Set true when a `code` launch failed because the binary was missing.
    pub code_missing: bool,
    /// When true, the view filters to a flat list of only the uncommitted
    /// (pending) files from `git_status` instead of the full project tree.
    pub show_only_changes: bool,
    /// Directories of the filtered (uncommitted) tree the user expanded,
    /// keyed by absolute path. Empty = fully collapsed (the default).
    /// Cleared whenever the filter toggle changes value.
    pub changed_expanded: HashSet<PathBuf>,
    /// Content-search ("search in files") panel state, scoped to `root`.
    pub search: SearchPanelState,
    /// Instant of the last automatic git-status recompute. `None` until the
    /// first refresh. Drives the [`GIT_REFRESH_INTERVAL`] throttle so we never
    /// run `git2` status every frame.
    last_git_refresh: Option<Instant>,
    /// `is_focused` value observed on the previous frame, so the show path can
    /// detect a focus-regain edge (`!prev && now`) and refresh immediately.
    prev_focus: bool,
    /// Screen-space hit boxes for the visible tree rows, rebuilt every frame by
    /// the widget's paint pass (cleared at the start of `show`, pushed per row).
    /// The OS-file-drop handler reads these to map a global pointer position to
    /// the drop target directory; see [`row_hit_at`] / [`drop_target_dir`].
    pub row_hits: Vec<RowHit>,
}

/// State for the File Explorer's content-search panel (the VSCode-style "search
/// in files" UI bound to Ctrl+Shift+F). Owns the background [`SearchRunner`] so
/// it survives across frames, plus the debounce bookkeeping the runner requires.
#[derive(Default)]
pub struct SearchPanelState {
    /// Whether the search box is showing above the tree.
    pub active: bool,
    /// Current text in the query input.
    pub query: String,
    /// Set when the panel was just opened so the view can grab keyboard focus
    /// for the input on the next frame, then clear it.
    pub focus_requested: bool,
    /// The query string last handed to `runner.start`. Guards against
    /// re-dispatching an unchanged query.
    last_dispatched: String,
    /// Wall-clock instant of the most recent query edit, used to measure input
    /// quiescence for the debounce. `None` until the first edit.
    last_edit: Option<Instant>,
    /// Background runner; detached worker threads, non-blocking poll.
    runner: SearchRunner,
}

impl SearchPanelState {
    /// Record that the query was just edited (resets the debounce clock). Called
    /// by the view whenever the text input reports a change.
    pub fn mark_edited(&mut self) {
        self.last_edit = Some(Instant::now());
    }

    /// The runner's latest observed state, for the view to render.
    #[must_use]
    pub fn state(&self) -> &SearchState {
        self.runner.state()
    }

    /// The query string that was last dispatched to the runner (for tests and
    /// for the view to confirm which query a result belongs to).
    #[must_use]
    pub fn last_dispatched(&self) -> &str {
        &self.last_dispatched
    }
}

impl FileTreeState {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            roots: Vec::new(),
            loaded: false,
            git_status: None,
            code_missing: false,
            show_only_changes: false,
            changed_expanded: HashSet::new(),
            search: SearchPanelState::default(),
            last_git_refresh: None,
            prev_focus: false,
            row_hits: Vec::new(),
        }
    }

    /// Resolve the OS-file-drop destination directory for a global pointer
    /// position, using the row hit-map recorded by the widget this frame. Folder
    /// row -> that folder; file row -> its parent; no row (empty area / panel
    /// background) -> the explorer `root`. See [`row_hit_at`] / [`drop_target_dir`].
    #[must_use]
    pub fn drop_target_for(&self, x: f32, y: f32) -> PathBuf {
        drop_target_dir(row_hit_at(&self.row_hits, x, y), &self.root)
    }

    /// Open (or re-focus) the content-search panel. Requests input focus on the
    /// next frame; never clears an in-progress query, so re-pressing the
    /// shortcut while open just re-focuses the box.
    pub fn open_search(&mut self) {
        self.search.active = true;
        self.search.focus_requested = true;
    }

    /// Close the content-search panel and reset the runner to idle. The query
    /// text is preserved so re-opening shows the previous search; results are
    /// dropped (the runner is cleared).
    pub fn close_search(&mut self) {
        self.search.active = false;
        self.search.focus_requested = false;
        self.search.runner.clear();
        self.search.last_dispatched.clear();
        self.search.last_edit = None;
    }

    /// Per-frame search pump. When the panel is active, applies the debounce
    /// decision (dispatching a fresh search once the query has been quiescent
    /// for [`SEARCH_DEBOUNCE`]) and drains any finished results.
    ///
    /// Quiescence is measured from the `last_edit` instant the view records via
    /// [`SearchPanelState::mark_edited`]; the pure decision lives in
    /// [`should_dispatch`] (clock-free, separately tested). Returns `true` while
    /// a search is in flight, signalling the view to request a repaint.
    pub fn tick_search(&mut self) -> bool {
        if !self.search.active {
            return false;
        }
        // Time since the last edit; if there has been no edit yet, treat it as
        // long-quiescent so a pre-filled query (re-opened panel) can dispatch.
        let elapsed = self
            .search
            .last_edit
            .map_or(SEARCH_DEBOUNCE, |t| t.elapsed());
        if should_dispatch(
            &self.search.query,
            &self.search.last_dispatched,
            elapsed,
            SEARCH_DEBOUNCE,
        ) {
            self.search.last_dispatched = self.search.query.clone();
            self.search
                .runner
                .start(self.root.clone(), self.search.query.clone(), FileSearchOptions::default());
        }
        matches!(self.search.runner.poll(), SearchState::Searching)
    }

    /// (Re)scan the root level. Safe to call repeatedly (refresh button).
    pub fn reload_root(&mut self) {
        self.roots = scan_dir(&self.root).unwrap_or_default();
        self.loaded = true;
    }

    /// Lazily scan a directory node's children (called on first expand).
    pub fn ensure_children(node: &mut FileNode) {
        if node.is_dir && node.children.is_none() {
            node.children = Some(scan_dir(&node.path).unwrap_or_default());
        }
    }

    /// Re-scan the on-disk children of `dir` so newly-created entries appear.
    ///
    /// Used after an OS-file drop copies files into `dir`. When `dir` is the
    /// explorer root the whole root level is reloaded; otherwise the matching
    /// directory node is found and its (already-expanded) children are re-scanned
    /// in place so the copied files show immediately. A collapsed or absent
    /// target is left untouched — re-expanding it will lazily pick up the new
    /// files. Never panics.
    pub fn refresh_dir(&mut self, dir: &Path) {
        if dir == self.root {
            self.reload_root();
            return;
        }
        if let Some(node) = find_node_mut(&mut self.roots, dir)
            && node.is_dir
            && node.children.is_some()
        {
            node.children = Some(scan_dir(dir).unwrap_or_default());
        }
    }

    pub fn set_git_status(&mut self, status: Arc<GitStatus>) {
        self.git_status = Some(status);
    }

    /// Per-frame git-status refresh driven from the explorer show path.
    ///
    /// `is_focused` is the panel's current focus state this frame. A focus-regain
    /// edge (`!prev_focus && is_focused`) forces an immediate refresh; otherwise
    /// the [`GIT_REFRESH_INTERVAL`] throttle applies (see [`should_refresh_git`]).
    /// When a refresh is due we recompute the snapshot with the SAME
    /// [`compute_status`] used by the initial load / watcher, stamp
    /// `last_git_refresh`, and keep the previous snapshot on error (transient
    /// git failures must not blank out the decorations). Always records
    /// `is_focused` as the new previous-focus before returning.
    pub fn maybe_refresh_git_status(&mut self, is_focused: bool) {
        let focus_regained = is_focused && !self.prev_focus;
        self.prev_focus = is_focused;

        let elapsed = self.last_git_refresh.map(|t| t.elapsed());
        if !should_refresh_git(elapsed, GIT_REFRESH_INTERVAL, focus_regained) {
            return;
        }

        self.last_git_refresh = Some(Instant::now());
        // Reuse the shared computation; on failure keep the existing snapshot.
        if let Ok(status) = compute_status(&self.root) {
            self.git_status = Some(Arc::new(status));
        }
    }

    /// Flip the uncommitted-files filter. Any change of value resets the
    /// filtered tree's expansion so re-enabling always starts collapsed.
    /// Re-asserting the current value (every frame) is a no-op.
    pub fn set_show_only_changes(&mut self, on: bool) {
        if self.show_only_changes != on {
            self.show_only_changes = on;
            self.changed_expanded.clear();
        }
    }

    #[must_use]
    pub fn is_changed_expanded(&self, path: &Path) -> bool {
        self.changed_expanded.contains(path)
    }

    pub fn expand_changed(&mut self, path: PathBuf) {
        self.changed_expanded.insert(path);
    }

    pub fn collapse_changed(&mut self, path: &Path) {
        self.changed_expanded.remove(path);
    }
}

/// Depth-first search for the (unique) loaded node whose `path` matches.
fn find_node_mut<'n>(nodes: &'n mut [FileNode], path: &Path) -> Option<&'n mut FileNode> {
    for node in nodes {
        if node.path == path {
            return Some(node);
        }
        if let Some(children) = &mut node.children
            && let Some(found) = find_node_mut(children, path)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(nodes: &[FileNode]) -> Vec<String> {
        nodes.iter().map(|n| n.name.clone()).collect()
    }

    #[test]
    fn scan_dir_lists_entries_dirs_first_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("src")).expect("mkdir src");
        fs::write(dir.path().join("b.txt"), b"").expect("b");
        fs::write(dir.path().join("a.txt"), b"").expect("a");

        let nodes = scan_dir(dir.path()).expect("scan");
        // dirs first (src), then files alphabetical (a.txt, b.txt)
        assert_eq!(names(&nodes), vec!["src", "a.txt", "b.txt"]);
        assert!(nodes[0].is_dir);
        assert!(!nodes[1].is_dir);
    }

    #[test]
    fn scan_dir_skips_git_node_modules_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        for skipped in [".git", "node_modules", "target"] {
            fs::create_dir(dir.path().join(skipped)).expect("mkdir skip");
        }
        fs::write(dir.path().join("keep.txt"), b"").expect("keep");

        let nodes = scan_dir(dir.path()).expect("scan");
        assert_eq!(names(&nodes), vec!["keep.txt"]);
    }

    #[test]
    fn scan_dir_shows_gitignored_files_tagged_not_hidden() {
        // New contract: gitignored entries are SHOWN (so the UI can dim them),
        // tagged `ignored == true`, rather than omitted from the listing.
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        fs::write(dir.path().join(".gitignore"), b"ignored.txt\n").expect("gitignore");
        fs::write(dir.path().join("ignored.txt"), b"").expect("ignored");
        fs::write(dir.path().join("visible.txt"), b"").expect("visible");

        let nodes = scan_dir(dir.path()).expect("scan");
        let listed = names(&nodes);
        assert!(listed.contains(&"visible.txt".to_string()));
        assert!(listed.contains(&".gitignore".to_string()));
        assert!(listed.contains(&"ignored.txt".to_string()), "gitignored file must be shown");
        assert!(find(&nodes, "ignored.txt").expect("ignored.txt").ignored);
        assert!(!find(&nodes, "visible.txt").expect("visible.txt").ignored);
    }

    #[test]
    fn scan_dir_is_single_level_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        fs::write(dir.path().join("sub").join("deep.txt"), b"").expect("deep");

        let nodes = scan_dir(dir.path()).expect("scan");
        let sub = nodes.iter().find(|n| n.name == "sub").expect("sub present");
        // children are not eagerly loaded
        assert!(sub.children.is_none());
    }

    #[test]
    fn status_for_path_matches_relative_change() {
        use crate::git_status::{FileChange, FileStatus, GitStatus};
        use std::collections::HashMap;
        use std::time::Instant;

        let root = std::path::PathBuf::from("/repo");
        let status = GitStatus {
            repo_root: root.clone(),
            branch: None,
            changes: vec![FileChange {
                path: "src/main.rs".to_string(),
                status: FileStatus::Modified,
                insertions: 1,
                deletions: 0,
            }],
            diffs: HashMap::new(),
            total_insertions: 1,
            total_deletions: 0,
            timestamp: Instant::now(),
        };
        let found = status_for_path(&status, &root.join("src").join("main.rs"));
        assert_eq!(found, Some(FileStatus::Modified));
        assert_eq!(status_for_path(&status, &root.join("other.rs")), None);
    }

    #[test]
    fn changed_file_rows_includes_every_pending_change() {
        use crate::git_status::{FileChange, FileStatus, GitStatus};
        use std::collections::HashMap;
        use std::time::Instant;

        let root = std::path::PathBuf::from("/repo");
        let kinds = [
            ("a.rs", FileStatus::Modified),
            ("b.rs", FileStatus::Added),
            ("c.rs", FileStatus::Untracked),
            ("d.rs", FileStatus::Deleted),
            ("e.rs", FileStatus::Renamed),
        ];
        let changes = kinds
            .iter()
            .map(|(path, status)| FileChange {
                path: (*path).to_string(),
                status: *status,
                insertions: 0,
                deletions: 0,
            })
            .collect();
        let status = GitStatus {
            repo_root: root.clone(),
            branch: None,
            changes,
            diffs: HashMap::new(),
            total_insertions: 0,
            total_deletions: 0,
            timestamp: Instant::now(),
        };

        let rows = changed_file_rows(&status);
        // Every change kind is represented, ordering preserved.
        assert_eq!(rows.len(), kinds.len());
        for (row, (path, kind)) in rows.iter().zip(kinds.iter()) {
            assert_eq!(row.rel_path, *path);
            assert_eq!(row.status, *kind);
            assert_eq!(row.abs_path, root.join(path));
        }
    }

    fn status_with(paths: &[(&str, FileStatus)]) -> GitStatus {
        use crate::git_status::FileChange;
        use std::collections::HashMap;
        use std::time::Instant;

        GitStatus {
            repo_root: std::path::PathBuf::from("/repo"),
            branch: None,
            changes: paths
                .iter()
                .map(|(path, status)| FileChange {
                    path: (*path).to_string(),
                    status: *status,
                    insertions: 0,
                    deletions: 0,
                })
                .collect(),
            diffs: HashMap::new(),
            total_insertions: 0,
            total_deletions: 0,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn changed_file_tree_groups_files_under_directories() {
        let status = status_with(&[
            ("src/main.rs", FileStatus::Modified),
            ("src/lib.rs", FileStatus::Untracked),
            ("README.md", FileStatus::Modified),
        ]);

        let tree = changed_file_tree(&status);
        // dirs first: src, then the root-level file
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].name, "src");
        assert!(tree[0].is_dir);
        assert_eq!(tree[0].abs_path, std::path::Path::new("/repo/src"));
        let children: Vec<&str> = tree[0].children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(children, vec!["lib.rs", "main.rs"]);
        assert_eq!(tree[0].children[1].status, Some(FileStatus::Modified));
        assert_eq!(tree[1].name, "README.md");
        assert!(!tree[1].is_dir);
        assert_eq!(tree[1].status, Some(FileStatus::Modified));
    }

    #[test]
    fn changed_file_tree_compacts_single_child_dir_chains() {
        let status = status_with(&[("a/b/c/deep.rs", FileStatus::Untracked)]);

        let tree = changed_file_tree(&status);
        assert_eq!(tree.len(), 1);
        // a > b > c collapses into one "a/b/c" node
        assert_eq!(tree[0].name, "a/b/c");
        assert_eq!(tree[0].abs_path, std::path::Path::new("/repo/a/b/c"));
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].name, "deep.rs");
        assert_eq!(
            tree[0].children[0].abs_path,
            std::path::Path::new("/repo/a/b/c/deep.rs")
        );
    }

    #[test]
    fn changed_file_tree_does_not_compact_branching_dirs() {
        let status = status_with(&[
            ("a/b/one.rs", FileStatus::Modified),
            ("a/c/two.rs", FileStatus::Modified),
        ]);

        let tree = changed_file_tree(&status);
        // "a" branches into b and c, so it must stay its own node
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].name, "a");
        let subdirs: Vec<&str> = tree[0].children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(subdirs, vec!["b", "c"]);
    }

    #[test]
    fn changed_file_tree_empty_when_no_changes() {
        let status = status_with(&[]);
        assert!(changed_file_tree(&status).is_empty());
    }

    #[test]
    fn changed_file_rows_empty_when_no_changes() {
        use crate::git_status::GitStatus;
        use std::collections::HashMap;
        use std::time::Instant;

        let status = GitStatus {
            repo_root: std::path::PathBuf::from("/repo"),
            branch: None,
            changes: Vec::new(),
            diffs: HashMap::new(),
            total_insertions: 0,
            total_deletions: 0,
            timestamp: Instant::now(),
        };
        assert!(changed_file_rows(&status).is_empty());
    }

    #[test]
    fn dir_contains_changes_true_for_direct_and_nested_children() {
        let status = status_with(&[("src/app/main.rs", FileStatus::Modified)]);
        // direct ancestor
        assert!(dir_contains_changes(&status, Path::new("/repo/src/app")));
        // grand-ancestor (nested levels up)
        assert!(dir_contains_changes(&status, Path::new("/repo/src")));
    }

    #[test]
    fn dir_contains_changes_false_for_sibling_dir() {
        let status = status_with(&[("src/main.rs", FileStatus::Modified)]);
        assert!(!dir_contains_changes(&status, Path::new("/repo/docs")));
    }

    #[test]
    fn dir_contains_changes_rejects_string_prefix_trap() {
        // "src2" must NOT match a change under "src" (component-wise, not string prefix).
        let status = status_with(&[("src/main.rs", FileStatus::Modified)]);
        assert!(!dir_contains_changes(&status, Path::new("/repo/src2")));
        // and the reverse: a change in src2 must not light up src.
        let status2 = status_with(&[("src2/main.rs", FileStatus::Modified)]);
        assert!(!dir_contains_changes(&status2, Path::new("/repo/src")));
    }

    #[test]
    fn dir_contains_changes_true_for_repo_root_when_any_change() {
        let status = status_with(&[("a/b.rs", FileStatus::Modified)]);
        assert!(dir_contains_changes(&status, Path::new("/repo")));
    }

    #[test]
    fn dir_contains_changes_false_when_no_changes() {
        let status = status_with(&[]);
        assert!(!dir_contains_changes(&status, Path::new("/repo")));
        assert!(!dir_contains_changes(&status, Path::new("/repo/src")));
    }

    #[test]
    fn dir_contains_changes_false_for_dir_outside_repo() {
        let status = status_with(&[("src/main.rs", FileStatus::Modified)]);
        assert!(!dir_contains_changes(&status, Path::new("/other/src")));
    }

    fn find<'a>(nodes: &'a [FileNode], name: &str) -> Option<&'a FileNode> {
        nodes.iter().find(|n| n.name == name)
    }

    #[test]
    fn scan_dir_shows_gitignored_entries_tagged_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("keep.txt"), b"").expect("keep");
        fs::write(dir.path().join(".gitignore"), b"tmp/\ntemp/\n").expect("gitignore");
        for d in ["tmp", "temp", "node_modules", "src"] {
            fs::create_dir(dir.path().join(d)).expect("mkdir");
            fs::write(dir.path().join(d).join("inside.txt"), b"").expect("inside");
        }

        let nodes = scan_dir(dir.path()).expect("scan");
        let listed = names(&nodes);

        // gitignored dirs are SHOWN but tagged ignored=true
        assert!(listed.contains(&"tmp".to_string()), "tmp must be shown: {listed:?}");
        assert!(listed.contains(&"temp".to_string()), "temp must be shown: {listed:?}");
        assert!(find(&nodes, "tmp").expect("tmp").ignored, "tmp must be ignored");
        assert!(find(&nodes, "temp").expect("temp").ignored, "temp must be ignored");

        // normal entries shown and not ignored
        assert!(!find(&nodes, "keep.txt").expect("keep.txt").ignored);
        assert!(!find(&nodes, "src").expect("src").ignored);

        // HARD_SKIP still hidden entirely
        assert!(!listed.contains(&"node_modules".to_string()), "node_modules must stay hidden");
    }

    #[test]
    fn scan_dir_gitignored_dir_flagged_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(".gitignore"), b"build/\n").expect("gitignore");
        fs::create_dir(dir.path().join("build")).expect("mkdir build");
        fs::write(dir.path().join("build").join("x"), b"").expect("x");

        let nodes = scan_dir(dir.path()).expect("scan");
        assert!(find(&nodes, "build").expect("build").ignored);
    }

    #[test]
    fn scan_dir_normal_file_not_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(".gitignore"), b"build/\n").expect("gitignore");
        fs::write(dir.path().join("main.rs"), b"").expect("main");

        let nodes = scan_dir(dir.path()).expect("scan");
        assert!(!find(&nodes, "main.rs").expect("main.rs").ignored);
    }

    #[test]
    fn scan_dir_honors_ancestor_gitignore() {
        // root/.gitignore ignores `dist/`. Scanning root/app/ (fresh scan_dir,
        // as lazy subdir expansion does) must still flag app/dist as ignored
        // because the rule lives in an ANCESTOR .gitignore.
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join(".gitignore"), b"dist/\n").expect("root gitignore");
        let app = root.path().join("app");
        fs::create_dir(&app).expect("mkdir app");
        fs::create_dir(app.join("dist")).expect("mkdir dist");
        fs::write(app.join("dist").join("bundle.js"), b"").expect("bundle");
        fs::write(app.join("keep.ts"), b"").expect("keep");

        let nodes = scan_dir(&app).expect("scan");
        assert!(find(&nodes, "dist").expect("dist").ignored, "ancestor rule must apply");
        assert!(!find(&nodes, "keep.ts").expect("keep.ts").ignored);
    }

    #[test]
    fn scan_dir_nearer_whitelist_overrides_ancestor_ignore() {
        // Ancestor ignores all *.log; the scanned dir's own .gitignore
        // whitelists keep.log via `!keep.log`. The nearer rule must win, so
        // keep.log is NOT ignored while other.log still is.
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join(".gitignore"), b"*.log\n").expect("root gitignore");
        let app = root.path().join("app");
        fs::create_dir(&app).expect("mkdir app");
        fs::write(app.join(".gitignore"), b"!keep.log\n").expect("app gitignore");
        fs::write(app.join("keep.log"), b"").expect("keep");
        fs::write(app.join("other.log"), b"").expect("other");

        let nodes = scan_dir(&app).expect("scan");
        assert!(
            !find(&nodes, "keep.log").expect("keep.log").ignored,
            "nearer !keep.log must un-ignore it"
        );
        assert!(find(&nodes, "other.log").expect("other.log").ignored);
    }

    #[test]
    fn scan_dir_without_gitignore_marks_nothing_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.txt"), b"").expect("a");
        fs::create_dir(dir.path().join("d")).expect("mkdir d");

        let nodes = scan_dir(dir.path()).expect("scan");
        assert!(!nodes.is_empty());
        assert!(nodes.iter().all(|n| !n.ignored), "no .gitignore => nothing ignored");
    }

    #[test]
    fn changed_expansion_starts_fully_collapsed() {
        let state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        assert!(!state.is_changed_expanded(std::path::Path::new("/repo/src")));
    }

    #[test]
    fn expand_and_collapse_changed_dirs_roundtrip() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        let dir = std::path::PathBuf::from("/repo/src/app");

        state.expand_changed(dir.clone());
        assert!(state.is_changed_expanded(&dir));
        // unrelated paths stay collapsed
        assert!(!state.is_changed_expanded(std::path::Path::new("/repo/docs")));

        state.collapse_changed(&dir);
        assert!(!state.is_changed_expanded(&dir));
    }

    #[test]
    fn should_dispatch_true_when_changed_and_quiescent() {
        // A new, non-empty query that differs from the last dispatched one and
        // has been quiescent long enough must dispatch.
        assert!(should_dispatch(
            "needle",
            "",
            SEARCH_DEBOUNCE,
            SEARCH_DEBOUNCE
        ));
        assert!(should_dispatch(
            "needle",
            "old",
            SEARCH_DEBOUNCE + Duration::from_millis(50),
            SEARCH_DEBOUNCE,
        ));
    }

    #[test]
    fn should_dispatch_false_when_unchanged() {
        // Same query already dispatched: no redundant re-dispatch even once quiescent.
        assert!(!should_dispatch(
            "needle",
            "needle",
            SEARCH_DEBOUNCE,
            SEARCH_DEBOUNCE
        ));
    }

    #[test]
    fn should_dispatch_false_when_empty_or_whitespace() {
        assert!(!should_dispatch("", "", SEARCH_DEBOUNCE, SEARCH_DEBOUNCE));
        assert!(!should_dispatch(
            "   ",
            "",
            SEARCH_DEBOUNCE,
            SEARCH_DEBOUNCE
        ));
    }

    #[test]
    fn should_dispatch_false_when_changed_but_not_yet_quiescent() {
        // Query changed but the input is still settling (elapsed < debounce).
        assert!(!should_dispatch(
            "needle",
            "old",
            SEARCH_DEBOUNCE - Duration::from_millis(1),
            SEARCH_DEBOUNCE,
        ));
    }

    #[test]
    fn open_search_activates_and_requests_focus() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        assert!(!state.search.active);
        state.open_search();
        assert!(state.search.active);
        assert!(state.search.focus_requested);
    }

    #[test]
    fn close_search_deactivates_and_resets_runner() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        state.open_search();
        state.search.query = "needle".to_string();
        state.search.last_dispatched = "needle".to_string();
        state.close_search();
        assert!(!state.search.active);
        assert!(!state.search.focus_requested);
        assert!(state.search.last_dispatched().is_empty());
        assert!(matches!(state.search.state(), SearchState::Idle));
    }

    #[test]
    fn tick_search_no_dispatch_while_inactive() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        state.search.query = "needle".to_string();
        // Not active: must not dispatch regardless of elapsed time.
        assert!(!state.tick_search());
        assert!(state.search.last_dispatched().is_empty());
    }

    #[test]
    fn tick_search_dispatches_query_once_quiescent() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        state.open_search();
        state.search.query = "needle".to_string();
        // No edit recorded yet => treated as long-quiescent, so it dispatches.
        let _ = state.tick_search();
        assert_eq!(state.search.last_dispatched(), "needle");
        // Calling again with the same query does not re-dispatch (idempotent).
        let _ = state.tick_search();
        assert_eq!(state.search.last_dispatched(), "needle");
    }

    #[test]
    fn tick_search_waits_for_quiescence_after_edit() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        state.open_search();
        state.search.query = "needle".to_string();
        // A fresh edit just happened: not yet quiescent, so no dispatch.
        state.search.mark_edited();
        let _ = state.tick_search();
        assert!(
            state.search.last_dispatched().is_empty(),
            "must wait out the debounce after an edit"
        );
    }

    #[test]
    fn should_refresh_git_true_on_focus_regain_even_if_just_refreshed() {
        // focus_regained wins even when the throttle window has NOT elapsed.
        assert!(should_refresh_git(
            Some(Duration::from_millis(0)),
            GIT_REFRESH_INTERVAL,
            true,
        ));
    }

    #[test]
    fn should_refresh_git_true_when_never_refreshed() {
        // None == never refreshed => always refresh (initial load), no focus edge.
        assert!(should_refresh_git(None, GIT_REFRESH_INTERVAL, false));
    }

    #[test]
    fn should_refresh_git_true_when_interval_elapsed() {
        assert!(should_refresh_git(
            Some(GIT_REFRESH_INTERVAL),
            GIT_REFRESH_INTERVAL,
            false,
        ));
        assert!(should_refresh_git(
            Some(GIT_REFRESH_INTERVAL + Duration::from_millis(1)),
            GIT_REFRESH_INTERVAL,
            false,
        ));
    }

    #[test]
    fn should_refresh_git_false_when_recent_and_no_focus_change() {
        // Within the throttle window and no focus regain => skip.
        assert!(!should_refresh_git(
            Some(GIT_REFRESH_INTERVAL - Duration::from_millis(1)),
            GIT_REFRESH_INTERVAL,
            false,
        ));
    }

    #[test]
    fn maybe_refresh_git_status_populates_for_modified_file_in_temp_repo() {
        // Reuse of compute_status via the explorer entry point: a temp repo with
        // an untracked file must yield a non-empty snapshot after a refresh.
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("new.txt"), b"hello").expect("write file");

        let mut state = FileTreeState::new(dir.path().to_path_buf());
        assert!(state.git_status.is_none());
        // First call: never refreshed => recomputes (focus irrelevant here).
        state.maybe_refresh_git_status(false);
        let status = state.git_status.as_ref().expect("status populated");
        assert!(
            status.changes.iter().any(|c| c.path == "new.txt"),
            "untracked file must appear in refreshed status"
        );
    }

    #[test]
    fn drop_target_dir_folder_hit_targets_the_folder() {
        let root = std::path::PathBuf::from("/repo");
        let folder = std::path::PathBuf::from("/repo/src");
        assert_eq!(drop_target_dir(Some((folder.as_path(), true)), &root), folder);
    }

    #[test]
    fn drop_target_dir_file_hit_targets_parent() {
        let root = std::path::PathBuf::from("/repo");
        let file = std::path::PathBuf::from("/repo/src/main.rs");
        assert_eq!(
            drop_target_dir(Some((file.as_path(), false)), &root),
            std::path::PathBuf::from("/repo/src")
        );
    }

    #[test]
    fn drop_target_dir_none_targets_root() {
        let root = std::path::PathBuf::from("/repo");
        assert_eq!(drop_target_dir(None, &root), root);
    }

    #[test]
    fn drop_target_dir_file_at_root_targets_root() {
        let root = std::path::PathBuf::from("/repo");
        let file = std::path::PathBuf::from("/repo/top.txt");
        assert_eq!(drop_target_dir(Some((file.as_path(), false)), &root), root);
    }

    #[test]
    fn row_hit_at_returns_topmost_row_containing_point() {
        let rows = vec![
            RowHit {
                rect: (0.0, 0.0, 100.0, 20.0),
                path: std::path::PathBuf::from("/repo/a"),
                is_dir: true,
            },
            RowHit {
                rect: (0.0, 20.0, 100.0, 40.0),
                path: std::path::PathBuf::from("/repo/b.txt"),
                is_dir: false,
            },
        ];
        // Point in the first row.
        assert_eq!(
            row_hit_at(&rows, 10.0, 10.0),
            Some((std::path::Path::new("/repo/a"), true))
        );
        // Point in the second row.
        assert_eq!(
            row_hit_at(&rows, 10.0, 30.0),
            Some((std::path::Path::new("/repo/b.txt"), false))
        );
    }

    #[test]
    fn row_hit_at_returns_none_outside_all_rows() {
        let rows = vec![RowHit {
            rect: (0.0, 0.0, 100.0, 20.0),
            path: std::path::PathBuf::from("/repo/a"),
            is_dir: true,
        }];
        assert_eq!(row_hit_at(&rows, 200.0, 200.0), None);
        assert_eq!(row_hit_at(&[], 10.0, 10.0), None);
    }

    #[test]
    fn row_hit_entry_at_returns_full_row_with_rect() {
        // The entry variant exposes the rect (used for the hover highlight) so
        // callers avoid a second scan to recover it.
        let rows = vec![RowHit {
            rect: (5.0, 6.0, 95.0, 26.0),
            path: std::path::PathBuf::from("/repo/a"),
            is_dir: true,
        }];
        let hit = row_hit_entry_at(&rows, 10.0, 10.0).expect("row under point");
        assert_eq!(hit.rect, (5.0, 6.0, 95.0, 26.0));
        assert_eq!(hit.path, std::path::PathBuf::from("/repo/a"));
        assert!(hit.is_dir);
        assert!(row_hit_entry_at(&rows, 500.0, 500.0).is_none());
    }

    #[test]
    fn row_hit_at_prefers_later_row_when_rects_overlap() {
        let rows = vec![
            RowHit {
                rect: (0.0, 0.0, 100.0, 40.0),
                path: std::path::PathBuf::from("/repo/under"),
                is_dir: true,
            },
            RowHit {
                rect: (0.0, 0.0, 100.0, 40.0),
                path: std::path::PathBuf::from("/repo/over"),
                is_dir: true,
            },
        ];
        assert_eq!(
            row_hit_at(&rows, 10.0, 10.0),
            Some((std::path::Path::new("/repo/over"), true))
        );
    }

    #[test]
    fn refresh_dir_reloads_root_and_expanded_subdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("sub")).expect("mkdir sub");

        let mut state = FileTreeState::new(dir.path().to_path_buf());
        state.reload_root();
        assert!(state.roots.iter().any(|n| n.name == "sub"));

        // A new root-level file appears after refresh_dir(root).
        fs::write(dir.path().join("new.txt"), b"x").expect("write new");
        state.refresh_dir(dir.path());
        assert!(state.roots.iter().any(|n| n.name == "new.txt"));

        // Expand the subdir, then a file created inside it shows after refresh.
        let sub = state.roots.iter_mut().find(|n| n.name == "sub").expect("sub node");
        FileTreeState::ensure_children(sub);
        let sub_path = dir.path().join("sub");
        fs::write(sub_path.join("inner.txt"), b"y").expect("write inner");
        state.refresh_dir(&sub_path);
        let sub = state.roots.iter().find(|n| n.name == "sub").expect("sub node");
        let children = sub.children.as_ref().expect("loaded children");
        assert!(children.iter().any(|n| n.name == "inner.txt"));
    }

    #[test]
    fn toggling_filter_resets_changed_expansion() {
        let mut state = FileTreeState::new(std::path::PathBuf::from("/repo"));
        let dir = std::path::PathBuf::from("/repo/src");

        state.set_show_only_changes(true);
        state.expand_changed(dir.clone());
        assert!(state.is_changed_expanded(&dir));

        // re-asserting the same value must NOT clear (happens every frame)
        state.set_show_only_changes(true);
        assert!(state.is_changed_expanded(&dir));

        // turning the filter off clears; turning it back on starts collapsed
        state.set_show_only_changes(false);
        state.set_show_only_changes(true);
        assert!(!state.is_changed_expanded(&dir));
    }
}
