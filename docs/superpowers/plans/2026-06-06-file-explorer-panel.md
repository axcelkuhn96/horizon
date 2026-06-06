# File Explorer Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a VSCode-style `FileExplorer` panel that lists workspace files as a lazy tree with icon-font glyphs, live git status decorations, and double-click-to-open-in-VS-Code.

**Architecture:** Mirror the existing `GitChanges` panel pattern. New `PanelKind::FileExplorer` carries a `PanelContent::FileExplorer(FileTreeState)`. `FileTreeState` (in `horizon-core/src/file_tree.rs`) holds the root path, a lazily-populated node tree (built via the `ignore` crate so `.gitignore`/`.git`/`node_modules`/`target` are skipped), the set of expanded dirs, and the latest `Arc<GitStatus>`. Rendering lives in `horizon-ui/src/file_tree_widget.rs` (`FileExplorerView`), reusing the `GitWatcher` already polled in `lifecycle.rs`. Icons come from a Symbols Nerd Font added to the egui font fallback stack.

**Tech Stack:** Rust, egui/eframe, `git2` (existing), `ignore` (new dep), Symbols Nerd Font (new asset).

**Domain note:** UI tasks render with egui (immediate-mode Rust GUI), NOT web frontend — the `frontend` web rulebook (React/Vue/CSS/design-systems) does NOT apply. UI tasks must instead mirror the existing `GitChangesView`/`DiffViewer` patterns and are validated by live screenshot.

---

## File Structure

- `crates/horizon-core/src/git_status.rs` — MODIFY: add `FileStatus::Untracked`, map `Delta::Untracked` to it.
- `crates/horizon-core/src/file_tree.rs` — CREATE: `FileNode`, `FileTreeState`, lazy `scan_dir`, status lookup. Pure core (no egui).
- `crates/horizon-core/src/editor.rs` — MODIFY: add `PanelContent::FileExplorer(FileTreeState)` variant + accessors; fix exhaustive matches.
- `crates/horizon-core/src/panel.rs` — MODIFY: add `PanelKind::FileExplorer` + `display_name` arm.
- `crates/horizon-core/src/panel/spawn.rs` — MODIFY: add spawn arm + `spawn_file_explorer`.
- `crates/horizon-core/src/config.rs` — MODIFY: add a default "Files" preset.
- `crates/horizon-core/src/lib.rs` — MODIFY: `pub mod file_tree;` + re-exports.
- `crates/horizon-core/Cargo.toml` — MODIFY: add `ignore` dependency.
- `crates/horizon-ui/assets/fonts/SymbolsNerdFont-Regular.ttf` — CREATE (asset) + `assets/fonts/LICENSE-nerd-font` note.
- `crates/horizon-ui/src/app/mod.rs` — MODIFY: register the Nerd Font in `configure_fonts`.
- `crates/horizon-ui/src/file_tree_widget.rs` — CREATE: `FileExplorerView` + `file_type_icon`.
- `crates/horizon-ui/src/app/panels.rs` — MODIFY: dispatch arm for `FileExplorer`.
- `crates/horizon-ui/src/app/panel_chrome.rs` — MODIFY: `panel_kind_icon` arm.
- `crates/horizon-ui/src/app/lifecycle.rs` — MODIFY: include `FileExplorer` in git-watcher gate + push status to file-explorer panels.
- `crates/horizon-ui/src/app/settings/presets.rs` — MODIFY: extend `ALL_KINDS` to 13.
- `crates/horizon-ui/src/lib.rs` — MODIFY: `mod file_tree_widget;` + export `FileExplorerView`.

---

## Task 1: Add `FileStatus::Untracked` to git_status

**Files:**
- Modify: `crates/horizon-core/src/git_status.rs:9-15` (enum) and `:138-144` (mapping)
- Modify: `crates/horizon-ui/src/git_changes_widget.rs` (exhaustive `match FileStatus` arms — search for them)
- Test: inline `#[cfg(test)]` in `git_status.rs`

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)]` module in `git_status.rs` (create the module if absent). Uses `git2` to init a temp repo with one untracked file.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn untracked_file_maps_to_untracked_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");
        let _ = repo; // repo created; leave file untracked
        fs::write(dir.path().join("new.txt"), b"hello").expect("write file");

        let status = compute_status(dir.path()).expect("compute status");
        let change = status
            .changes
            .iter()
            .find(|c| c.path == "new.txt")
            .expect("new.txt present in changes");
        assert_eq!(change.status, FileStatus::Untracked);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-core untracked_file_maps_to_untracked_status`
Expected: FAIL — `FileStatus::Untracked` does not exist (compile error) or maps to `Added`.

- [ ] **Step 3: Add the enum variant** — `git_status.rs:9-15`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
    Renamed,
}
```

- [ ] **Step 4: Update the Delta mapping** — `git_status.rs:138-144`. Split `Untracked` out of the `Added` arm:

```rust
match delta.status() {
    Delta::Added => FileStatus::Added,
    Delta::Untracked => FileStatus::Untracked,
    Delta::Deleted => FileStatus::Deleted,
    Delta::Modified => FileStatus::Modified,
    Delta::Renamed | Delta::Copied => FileStatus::Renamed,
    _ => continue,
}
```

- [ ] **Step 5: Fix exhaustive matches in the UI** — in `crates/horizon-ui/src/git_changes_widget.rs`, find every `match` on `FileStatus` (status letter + color helpers). Add an `Untracked` arm wherever `Added` is handled (letter `"U"`, same green as `Added`). Example shape (adapt to actual code):

```rust
FileStatus::Added => ("A", theme::PALETTE_GREEN()),
FileStatus::Untracked => ("U", theme::PALETTE_GREEN()),
```

Run `cargo build -p horizon-ui` and fix any non-exhaustive-match compile errors that surface in other files.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p horizon-core untracked_file_maps_to_untracked_status`
Expected: PASS

- [ ] **Step 7: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy -p horizon-core -p horizon-ui --all-targets
git add crates/horizon-core/src/git_status.rs crates/horizon-ui/src/git_changes_widget.rs
git commit -m "feat(git): distinguish untracked from added in FileStatus"
```

> Note: confirm `tempfile` is a dev-dependency of `horizon-core`; if not, add `tempfile` under `[dev-dependencies]` in `crates/horizon-core/Cargo.toml`.

---

## Task 2: `file_tree.rs` core — lazy scan + status lookup

**Files:**
- Create: `crates/horizon-core/src/file_tree.rs`
- Modify: `crates/horizon-core/src/lib.rs` (add `pub mod file_tree;` and re-export `FileTreeState`, `FileNode`)
- Modify: `crates/horizon-core/Cargo.toml` (`ignore = "0.4"`)
- Test: inline `#[cfg(test)]` in `file_tree.rs`

- [ ] **Step 1: Add the `ignore` dependency** — in `crates/horizon-core/Cargo.toml` under `[dependencies]`:

```toml
ignore = "0.4"
```

Run: `cargo build -p horizon-core` (downloads the crate; expect success with no code yet using it).

- [ ] **Step 2: Write the failing tests** — create `crates/horizon-core/src/file_tree.rs` with ONLY the test module first (so it fails to compile against missing types), then implement in Step 3.

```rust
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
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p horizon-core file_tree`
Expected: FAIL — `scan_dir`, `FileNode`, `status_for_path` undefined.

- [ ] **Step 4: Implement the module** — prepend this above the test module in `file_tree.rs`:

```rust
//! Lazy, gitignore-aware project file tree backing the FileExplorer panel.

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
pub fn scan_dir(dir: &Path) -> Result<Vec<FileNode>> {
    let mut entries: Vec<FileNode> = Vec::new();

    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1)) // only this level
        .hidden(false) // show dotfiles like .gitignore
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !HARD_SKIP.contains(&name))
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // permission denied etc: skip, never panic
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
    status
        .changes
        .iter()
        .find(|c| c.path == rel)
        .map(|c| c.status)
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
```

- [ ] **Step 5: Export from lib.rs** — in `crates/horizon-core/src/lib.rs` add `pub mod file_tree;` near the other `pub mod` lines, and re-export: `pub use file_tree::{FileNode, FileTreeState};` next to the existing `GitStatus` re-export.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p horizon-core file_tree`
Expected: PASS (all 5)

- [ ] **Step 7: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy -p horizon-core --all-targets
git add crates/horizon-core/src/file_tree.rs crates/horizon-core/src/lib.rs crates/horizon-core/Cargo.toml
git commit -m "feat(file-tree): lazy gitignore-aware directory scan core"
```

---

## Task 3: Wire `PanelKind::FileExplorer` + `PanelContent::FileExplorer` + spawn

**Files:**
- Modify: `crates/horizon-core/src/panel.rs:36-50` (enum) and `:69-79` (display_name)
- Modify: `crates/horizon-core/src/editor.rs:80-85` (PanelContent) + all accessor matches `:87-...`
- Modify: `crates/horizon-core/src/panel/spawn.rs:143-187` (arm) + new `spawn_file_explorer` near `:587`
- Test: inline test in `spawn.rs` (or panel.rs)

- [ ] **Step 1: Write the failing test** — append to `spawn.rs` tests (or create the module). Verify spawning a FileExplorer panel yields the right kind/content and inherits cwd.

```rust
#[cfg(test)]
mod file_explorer_spawn_tests {
    use super::*;

    #[test]
    fn spawn_file_explorer_sets_kind_and_root() {
        let opts = PanelOptions {
            kind: PanelKind::FileExplorer,
            cwd: Some(std::path::PathBuf::from("/tmp/proj")),
            ..test_panel_options() // reuse existing test helper if present; else build minimal opts
        };
        let panel = spawn_panel(PanelId(1), WorkspaceId(1), opts).expect("spawn");
        assert_eq!(panel.kind, PanelKind::FileExplorer);
        let state = panel.content.file_explorer().expect("file explorer content");
        assert_eq!(state.root, std::path::PathBuf::from("/tmp/proj"));
    }
}
```

> If no `test_panel_options()` helper exists, construct `PanelOptions` fully (mirror the `Default`/existing test setup used by other spawn tests in this file).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-core spawn_file_explorer_sets_kind_and_root`
Expected: FAIL — variant/accessor undefined.

- [ ] **Step 3: Add the `PanelKind` variant** — `panel.rs:36-50`, insert after `GitChanges`:

```rust
    GitChanges,
    FileExplorer,
    Usage,
```

And the `display_name` arm (`panel.rs:69-79`):

```rust
            Self::GitChanges => "Git Changes",
            Self::FileExplorer => "Files",
            Self::Usage => "Usage",
```

- [ ] **Step 4: Add the `PanelContent` variant + accessors** — `editor.rs:80-85`:

```rust
pub enum PanelContent {
    Terminal(Terminal),
    Editor(MarkdownEditor),
    GitChanges(DiffViewer),
    FileExplorer(crate::file_tree::FileTreeState),
    Usage(UsageDashboard),
}
```

Then update EVERY existing accessor match in this `impl PanelContent` block (the `terminal`/`editor`/`git_changes`/... methods) to include the new variant in their "None" arms, and add two new accessors:

```rust
    #[must_use]
    pub fn file_explorer(&self) -> Option<&crate::file_tree::FileTreeState> {
        match self {
            Self::FileExplorer(s) => Some(s),
            _ => None,
        }
    }

    pub fn file_explorer_mut(&mut self) -> Option<&mut crate::file_tree::FileTreeState> {
        match self {
            Self::FileExplorer(s) => Some(s),
            _ => None,
        }
    }
```

> Where existing accessors spell out each variant instead of using `_`, add `Self::FileExplorer(_)` to those arms explicitly to keep matches exhaustive and clippy-clean.

- [ ] **Step 5: Add the spawn arm + function** — `spawn.rs`, add an arm after `GitChanges` (mirror it):

```rust
        PanelKind::FileExplorer => {
            let PanelOptions {
                name,
                position,
                size,
                template,
                cwd,
                collapsed,
                ..
            } = opts;
            let seed = StaticPanelSeed::new(id, workspace_id, local_id, name, position, size, template)
                .with_collapsed(collapsed);
            Ok(spawn_file_explorer(seed, cwd))
        }
```

And near `spawn_git_changes` (`:587`):

```rust
fn spawn_file_explorer(mut seed: StaticPanelSeed, cwd: Option<PathBuf>) -> Panel {
    let (title, has_custom_name) = seed.take_title(|| "Files".to_string());
    tracing::info!("created file explorer panel '{}' (id={})", title, seed.id.0);

    let root = cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    seed.into_panel(
        title,
        PanelKind::FileExplorer,
        PanelContent::FileExplorer(horizon_core_file_tree_state(root)),
        None,
        cwd,
        has_custom_name,
    )
}

fn horizon_core_file_tree_state(root: PathBuf) -> crate::file_tree::FileTreeState {
    crate::file_tree::FileTreeState::new(root)
}
```

> If `crate::file_tree::FileTreeState` is already imported at top of `spawn.rs`, drop the helper and inline `FileTreeState::new(root)`.

- [ ] **Step 6: Build the whole core, fix exhaustiveness** — `cargo build -p horizon-core`. Resolve any remaining non-exhaustive `match panel.kind`/`PanelContent` errors the new variant surfaces.

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p horizon-core spawn_file_explorer_sets_kind_and_root`
Expected: PASS

- [ ] **Step 8: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy -p horizon-core --all-targets
git add crates/horizon-core/src/panel.rs crates/horizon-core/src/editor.rs crates/horizon-core/src/panel/spawn.rs
git commit -m "feat(panel): add FileExplorer panel kind, content, and spawn"
```

---

## Task 4: Presets, config default, chrome icon

**Files:**
- Modify: `crates/horizon-ui/src/app/settings/presets.rs:6-19` (`ALL_KINDS`)
- Modify: `crates/horizon-core/src/config.rs` (`default_presets`, the `presets.extend([...])` block)
- Modify: `crates/horizon-ui/src/app/panel_chrome.rs:86-100` (`panel_kind_icon`)

- [ ] **Step 1: Extend `ALL_KINDS`** — `presets.rs:6`, change array length `12` → `13` and insert `PanelKind::FileExplorer` after `GitChanges`:

```rust
const ALL_KINDS: [PanelKind; 13] = [
    PanelKind::Shell,
    PanelKind::Ssh,
    PanelKind::Codex,
    PanelKind::Claude,
    PanelKind::OpenCode,
    PanelKind::Gemini,
    PanelKind::KiloCode,
    PanelKind::Pi,
    PanelKind::Command,
    PanelKind::Editor,
    PanelKind::GitChanges,
    PanelKind::FileExplorer,
    PanelKind::Usage,
];
```

- [ ] **Step 2: Add the default preset** — in `config.rs` `default_presets()`, inside the `presets.extend([...])` block (after the "Git Changes" entry):

```rust
        PresetConfig {
            name: "Files".to_string(),
            alias: Some("fx".to_string()),
            kind: PanelKind::FileExplorer,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
        },
```

- [ ] **Step 3: Add the chrome icon arm** — `panel_chrome.rs:86-100`, add after the `GitChanges` arm:

```rust
        PanelKind::FileExplorer => ("FX", panel_kind_label_color(theme::PALETTE_CYAN(), focused)),
```

- [ ] **Step 4: Build + verify exhaustiveness**

Run: `cargo build -p horizon-ui`
Expected: success (any remaining non-exhaustive `match kind` errors get a `FileExplorer` arm; the dispatch one is Task 7).

- [ ] **Step 5: fmt + commit**

```bash
cargo fmt
git add crates/horizon-ui/src/app/settings/presets.rs crates/horizon-core/src/config.rs crates/horizon-ui/src/app/panel_chrome.rs
git commit -m "feat(panel): register Files preset and chrome icon for FileExplorer"
```

---

## Task 5: GitWatcher feeds FileExplorer panels

**Files:**
- Modify: `crates/horizon-ui/src/app/lifecycle.rs:168-213` (`poll_git_watchers`)

- [ ] **Step 1: Broaden the "needs watcher" predicate** — `lifecycle.rs:172-180`. Replace `panel.kind == PanelKind::GitChanges` with a helper that also matches FileExplorer:

```rust
        for panel in &self.board.panels {
            if matches!(panel.kind, PanelKind::GitChanges | PanelKind::FileExplorer) {
                let cwd = panel
                    .launch_cwd
                    .clone()
                    .or_else(|| self.board.workspace(panel.workspace_id).and_then(|ws| ws.cwd.clone()));
                workspaces_needing_watchers.entry(panel.workspace_id).or_insert(cwd);
            }
        }
```

- [ ] **Step 2: Push status to FileExplorer panels too** — `lifecycle.rs:199-208`. After the existing GitChanges update loop, in the same `for (workspace_id, status)` block add:

```rust
        for (workspace_id, status) in updates {
            for panel in &mut self.board.panels {
                if panel.workspace_id != workspace_id {
                    continue;
                }
                if panel.kind == PanelKind::GitChanges {
                    if let Some(viewer) = panel.content.git_changes_mut() {
                        viewer.update(std::sync::Arc::clone(&status));
                    }
                } else if panel.kind == PanelKind::FileExplorer {
                    if let Some(state) = panel.content.file_explorer_mut() {
                        state.set_git_status(std::sync::Arc::clone(&status));
                    }
                }
            }
        }
```

- [ ] **Step 3: Update the retain comment + verify** — the `retain` at `:211` already keys off `workspaces_needing_watchers`, which now includes FileExplorer workspaces; no change needed. Build:

Run: `cargo build -p horizon-ui`
Expected: success.

- [ ] **Step 4: commit**

```bash
cargo fmt
git add crates/horizon-ui/src/app/lifecycle.rs
git commit -m "feat(lifecycle): drive git watcher for FileExplorer panels"
```

> No unit test here (egui app state needs a running board); covered by the live screenshot in Task 8.

---

## Task 6: Nerd Font asset + `file_type_icon`

**Files:**
- Create: `crates/horizon-ui/assets/fonts/SymbolsNerdFont-Regular.ttf` (download) + `crates/horizon-ui/assets/fonts/LICENSE-SymbolsNerdFont.txt`
- Modify: `crates/horizon-ui/src/app/mod.rs:474-511` (`configure_fonts`) + the `FONT_*` const block
- Create: the `file_type_icon` fn in `crates/horizon-ui/src/file_tree_widget.rs` (file created here, view in Task 7)
- Test: inline `#[cfg(test)]` in `file_tree_widget.rs`

- [ ] **Step 1: Add the font asset** — download **Symbols Nerd Font** (or "Symbols Nerd Font Mono"), license **OFL-1.1**, from the official Nerd Fonts release (`https://github.com/ryanoasis/nerd-fonts/releases` → `NerdFontsSymbolsOnly`). Save the `.ttf` to `crates/horizon-ui/assets/fonts/SymbolsNerdFont-Regular.ttf` and copy its `LICENSE` to `assets/fonts/LICENSE-SymbolsNerdFont.txt`.

```bash
mkdir -p crates/horizon-ui/assets/fonts
# place SymbolsNerdFont-Regular.ttf and LICENSE-SymbolsNerdFont.txt here
ls -la crates/horizon-ui/assets/fonts/SymbolsNerdFont-Regular.ttf
```

> If downloading is not possible in this environment, STOP and report — do NOT fall back to emoji silently. The agreed fallback is the `egui-phosphor` crate (document the switch in the final report and adjust Steps 2–4 to register Phosphor instead).

- [ ] **Step 2: Register the font** — `app/mod.rs`. Add a const beside the other `FONT_*` consts:

```rust
const FONT_NERD: &str = "SymbolsNerdFont";
```

In `configure_fonts()` after the existing `insert_font_data(...)` calls:

```rust
    insert_font_data(
        &mut fonts,
        FONT_NERD,
        include_bytes!("../../assets/fonts/SymbolsNerdFont-Regular.ttf"),
    );
```

And append it to BOTH fallback stacks (so Nerd glyphs — which live in the Private Use Area and never collide with text — resolve everywhere):

```rust
    proportional.insert(3, FONT_NERD.to_owned());
    // ...
    monospace.insert(3, FONT_NERD.to_owned());
```

- [ ] **Step 3: Write the failing test** — create `crates/horizon-ui/src/file_tree_widget.rs` with the icon mapping test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn known_extensions_get_distinct_icons() {
        let rs = file_type_icon(Path::new("main.rs"), false);
        let json = file_type_icon(Path::new("pkg.json"), false);
        let generic = file_type_icon(Path::new("data.unknownext"), false);
        assert_ne!(rs, generic);
        assert_ne!(json, generic);
        assert_ne!(rs, json);
    }

    #[test]
    fn directories_use_folder_icons() {
        let closed = file_type_icon(Path::new("src"), true);
        let file = file_type_icon(Path::new("src.rs"), false);
        assert_ne!(closed, file);
    }

    #[test]
    fn extensionless_file_falls_back_to_generic() {
        let dockerfile = file_type_icon(Path::new("Dockerfile"), false);
        let generic = file_type_icon(Path::new("noext"), false);
        // both resolve to *some* glyph without panicking
        assert!(!dockerfile.is_empty());
        assert!(!generic.is_empty());
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p horizon-ui known_extensions_get_distinct_icons`
Expected: FAIL — `file_type_icon` undefined.

- [ ] **Step 5: Implement `file_type_icon`** — at the top of `file_tree_widget.rs`. Returns a `&'static str` containing the Nerd Font glyph (Private Use Area codepoints). Use the Seti/devicon codepoints below (these are stable Nerd Font v3 codepoints):

```rust
use std::path::Path;

/// Returns a Nerd Font glyph (Private Use Area) for a path. `is_dir` selects a
/// folder glyph. Unknown extensions fall back to a generic file glyph.
#[must_use]
pub fn file_type_icon(path: &Path, is_dir: bool) -> &'static str {
    if is_dir {
        return "\u{f07b}"; // nf-fa-folder
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // special-cased filenames
    match name {
        ".gitignore" | ".gitattributes" => return "\u{e702}", // nf-dev-git
        "Cargo.toml" | "Cargo.lock" => return "\u{e7a8}",      // nf-dev-rust
        "Dockerfile" => return "\u{f308}",                     // nf-linux-docker
        _ => {}
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => "\u{e7a8}",                       // rust
        "toml" => "\u{e615}",                     // settings/seti
        "lock" => "\u{f023}",                     // lock
        "md" | "markdown" => "\u{f48a}",          // markdown
        "json" => "\u{e60b}",                     // json
        "yaml" | "yml" => "\u{e615}",             // yaml/settings
        "js" => "\u{e74e}",                       // js
        "ts" => "\u{e628}",                       // ts
        "tsx" | "jsx" => "\u{e7ba}",              // react
        "py" => "\u{e606}",                       // python
        "sh" | "bash" | "zsh" => "\u{f489}",      // terminal
        "html" | "htm" => "\u{e736}",             // html5
        "css" | "scss" | "sass" => "\u{e749}",    // css3
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "\u{f1c5}", // image
        "svg" => "\u{f1c5}",                      // image
        "txt" => "\u{f0f6}",                      // file-text
        _ => "\u{f15b}",                          // generic file
    }
}
```

> The exact glyph per extension is cosmetic; the test only asserts distinctness + non-panic. The agent may adjust codepoints to match the installed Nerd Font version if a glyph renders as tofu — verify visually in Task 8.

- [ ] **Step 6: Register the module** — `crates/horizon-ui/src/lib.rs`: add `mod file_tree_widget;` and `pub use file_tree_widget::FileExplorerView;` (the view type lands in Task 7; if Task 7 not yet done, temporarily export just the module or add a stub `pub struct FileExplorerView` — but prefer doing Task 7 before building the bin).

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p horizon-ui file_type_icon` (and the other two)
Expected: PASS

- [ ] **Step 8: fmt + commit**

```bash
cargo fmt
git add crates/horizon-ui/assets/fonts crates/horizon-ui/src/app/mod.rs crates/horizon-ui/src/file_tree_widget.rs crates/horizon-ui/src/lib.rs
git commit -m "feat(ui): bundle Symbols Nerd Font and add file_type_icon mapping"
```

---

## Task 7: `FileExplorerView` rendering + dispatch

**Files:**
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (add `FileExplorerView`)
- Modify: `crates/horizon-ui/src/app/panels.rs:133-153` (dispatch arm)

- [ ] **Step 1: Implement `FileExplorerView`** — add to `file_tree_widget.rs`, mirroring `GitChangesView` structure (`new(panel)` + `show(ui, is_focused) -> bool`). Behavior:
  - On first `show`, if `!state.loaded`, call `state.reload_root()`.
  - Header row: panel root name + a refresh button (⟳) that calls `reload_root()`.
  - Recursively render `roots` with indentation per depth.
  - Folder row: clicking toggles expansion; on first expand call `FileTreeState::ensure_children` for that node. Track expansion by mutating `node.children` (None = collapsed/unloaded, Some = expanded). Use a separate `expanded: HashSet<PathBuf>` if you prefer to keep children cached across collapse — simplest correct approach: keep `children: Some` cached and track open/closed in a `HashSet<PathBuf>` field added to `FileTreeState` (add it in core if needed; update Task 2 struct + its `Clone`/`Debug`).
  - File row: icon (`file_type_icon`) + name. Color the name by git status via `horizon_core::file_tree::status_for_path(status, &node.path)`:
    - `Untracked | Added` → `theme::PALETTE_GREEN()`, letter `U`/`A`
    - `Modified | Renamed` → `theme::PALETTE_YELLOW()`, letter `M`/`R`
    - `Deleted` → `theme::PALETTE_RED()`, letter `D`
    - clean/None → `theme::FG()` (neutral), no letter
  - Status letter right-aligned (mirror `git_changes_widget::render_file_list`).
  - Double-click on a file row → call `open_in_vscode(&node.path, &mut state.code_missing)` (Task 8).
  - Footer: if `state.code_missing`, show a dim warning line "VS Code (`code`) não encontrado no PATH".
  - Return `true` if the panel rect contains the pointer (focus tracking), same as `GitChangesView`.

Use `egui::ScrollArea::vertical()` around the tree. Keep this file focused; if it approaches ~600 lines, extract row rendering into a private submodule `file_tree_widget/row.rs`.

> Mirror the concrete egui calls (`RichText`, `FontId`, `Layout`, `ScrollArea`, colors, row sizing constants) from `git_changes_widget.rs:1-13,229-312` so styling matches the rest of the app.

- [ ] **Step 2: Add the dispatch arm** — `panels.rs:139`, after the `GitChanges` arm:

```rust
        PanelKind::FileExplorer => FileExplorerView::new(panel).show(ui, is_focused),
```

Add the import at the top of `panels.rs` (next to `GitChangesView`): `use crate::FileExplorerView;` (or the existing path style used for `GitChangesView`).

- [ ] **Step 3: Build the binary**

Run: `cargo build -p horizon-ui`
Expected: success.

- [ ] **Step 4: Manual smoke (will be fully validated in Task 8)** — `cargo run` is exercised in Task 8.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy -p horizon-ui --all-targets
git add crates/horizon-ui/src/file_tree_widget.rs crates/horizon-ui/src/app/panels.rs
git commit -m "feat(ui): render FileExplorer tree with icons and git decorations"
```

---

## Task 8: Double-click opens VS Code (with missing-binary fallback) + live validation

**Files:**
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (`open_in_vscode`)
- Test: inline `#[cfg(test)]` in `file_tree_widget.rs`

- [ ] **Step 1: Write the failing test** — the missing-binary path must set the flag, not panic. Make the launcher testable by injecting the program name:

```rust
#[cfg(test)]
mod vscode_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn missing_binary_sets_flag_and_does_not_panic() {
        let mut code_missing = false;
        // a program name that certainly does not exist on PATH
        let ok = try_launch_editor("definitely-not-a-real-binary-xyz", Path::new("/tmp/x"));
        if !ok {
            code_missing = true;
        }
        assert!(!ok);
        assert!(code_missing);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-ui missing_binary_sets_flag_and_does_not_panic`
Expected: FAIL — `try_launch_editor` undefined.

- [ ] **Step 3: Implement the launcher** — in `file_tree_widget.rs`. Vectorized args (no shell → no injection):

```rust
use std::process::Command;

/// Spawn `program <path>` detached. Returns `false` if the program could not be
/// launched (e.g. not on PATH). Never panics; never inherits our stdio.
fn try_launch_editor(program: &str, path: &Path) -> bool {
    Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Open a file in VS Code. Sets `code_missing` when the `code` binary is absent.
pub(crate) fn open_in_vscode(path: &Path, code_missing: &mut bool) {
    if try_launch_editor("code", path) {
        *code_missing = false;
    } else {
        *code_missing = true;
        tracing::warn!("failed to launch `code` for file open");
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horizon-ui missing_binary_sets_flag_and_does_not_panic`
Expected: PASS

- [ ] **Step 5: Full workspace gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
./scripts/check-maintainability.sh
```

Expected: all green, no new warnings.

- [ ] **Step 6: Live UI validation (REQUIRED — AGENTS.md)** — run the app and verify visually:

Run: `cargo run` (or the project's run skill). In a workspace whose `cwd` is this repo:
  1. Create a "Files" panel (command palette → `fx`/"Files").
  2. Confirm the tree shows files with distinct icons (no tofu boxes).
  3. Modify a tracked file + create a new untracked file; confirm `M` (yellow) and `U`/`A` (green) decorations appear within a couple seconds (GitWatcher).
  4. Expand a folder (lazy load) and collapse it.
  5. Double-click a file → VS Code opens it.
  6. Click the refresh button → tree re-scans.

Capture a screenshot showing the tree with icons + git decorations. Save under `docs/` or attach to the report.

- [ ] **Step 7: Commit**

```bash
git add crates/horizon-ui/src/file_tree_widget.rs
git commit -m "feat(ui): double-click opens file in VS Code with missing-binary fallback"
```

---

## Task 9 (DEFERRED — optional polish, do NOT implement in v1 unless time permits)

- Aggregate folder decoration (a folder containing changed files shows a dot).
- Persist expanded-node set across restarts (extend runtime.yaml panel serde).
- Automatic filesystem watching (notify crate) instead of manual refresh.

Log these as follow-ups in the final report; they are explicitly out of v1 scope.

---

## Self-Review

- **Spec coverage:** AC1 → Tasks 3,4. AC2 → Tasks 6,7. AC3 → Tasks 2,7. AC4 → Tasks 1,5,7. AC5 → Task 8. AC6 → Task 7 (refresh). AC7 → gates in Tasks 1–8 + Task 8 Step 5. AC8 → Task 8 Step 6. ✓ All covered.
- **Open implementation detail for the implementer to resolve in Task 7:** open/closed tracking — recommended to add `expanded: std::collections::HashSet<PathBuf>` to `FileTreeState` in Task 2 (update its `Clone`/`Debug` derive — already derived) and key expansion off it while caching `children`. If the implementer chooses the simpler "children: Some = open" model, that is acceptable as long as collapse re-hides children.
- **Type consistency:** `FileTreeState` fields (`root`, `roots`, `loaded`, `git_status`, `code_missing`) used consistently across Tasks 2,5,7,8. `file_type_icon(path, is_dir)` signature consistent Tasks 6,7. `status_for_path(&GitStatus, &Path)` consistent Tasks 2,7. ✓
- **Placeholder scan:** font download (Task 6 Step 1) is an external fetch, not a code placeholder — explicit STOP-and-report instruction given if it fails. Glyph codepoints flagged as visually-verifiable. ✓
