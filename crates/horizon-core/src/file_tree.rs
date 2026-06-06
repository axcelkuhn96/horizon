//! Lazy, gitignore-aware project file tree backing the `FileExplorer` panel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
}

/// Directories we never descend into regardless of .gitignore.
const HARD_SKIP: [&str; 3] = [".git", "node_modules", "target"];

/// Scan a single directory level. Dirs first, then files, each alphabetical
/// (case-insensitive). Respects `.gitignore` and always skips [`HARD_SKIP`].
///
/// # Errors
///
/// Currently infallible: individual walk errors (e.g. permission denied) are
/// skipped rather than propagated, so this never returns `Err`. The `Result`
/// return type is retained so future stricter scanning can surface failures.
pub fn scan_dir(dir: &Path) -> Result<Vec<FileNode>> {
    let mut entries: Vec<FileNode> = Vec::new();

    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1)) // only this level
        .hidden(false) // show dotfiles like .gitignore
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
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
        entries.push(FileNode {
            name,
            path,
            is_dir,
            children: None,
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
    fn scan_dir_respects_gitignore() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        fs::write(dir.path().join(".gitignore"), b"ignored.txt\n").expect("gitignore");
        fs::write(dir.path().join("ignored.txt"), b"").expect("ignored");
        fs::write(dir.path().join("visible.txt"), b"").expect("visible");

        let nodes = scan_dir(dir.path()).expect("scan");
        let listed = names(&nodes);
        assert!(listed.contains(&"visible.txt".to_string()));
        assert!(listed.contains(&".gitignore".to_string()));
        assert!(!listed.contains(&"ignored.txt".to_string()));
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
}
