//! Lazy, gitignore-aware project file tree backing the `FileExplorer` panel.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

use crate::error::Result;
use crate::git_status::{FileStatus, GitStatus};

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

/// Build a gitignore matcher rooted at `dir`, honoring `dir/.gitignore` plus any
/// parent/global ignore files. Errors (missing files, bad globs) are swallowed:
/// a partially-built — or empty — matcher just means "fewer things flagged
/// ignored", never a panic. `.git`/`node_modules`/`target` are still removed by
/// [`HARD_SKIP`] in the walk, independent of this matcher.
fn build_ignore_matcher(dir: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(dir);
    // The directory's own .gitignore (the common case). `add` returns Some(err)
    // on failure (e.g. file absent); treat that as "no rules from this file".
    let _ = builder.add(dir.join(".gitignore"));
    // Parent + global ignores, best-effort. add_line never touches disk; the
    // global config path may not exist, so ignore any returned error.
    match builder.build() {
        Ok(gi) => gi,
        Err(_) => Gitignore::empty(),
    }
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
        let ignored = matcher.matched(&path, is_dir).is_ignore();
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
#[derive(Clone, Debug)]
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
        }
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

    pub fn set_git_status(&mut self, status: Arc<GitStatus>) {
        self.git_status = Some(status);
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
