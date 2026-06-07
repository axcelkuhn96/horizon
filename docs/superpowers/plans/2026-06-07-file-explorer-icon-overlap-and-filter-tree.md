# File Explorer Icon Overlap Fix + Collapsible Filter Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix file/folder icons painting over filename text in the Files panel, and turn the uncommitted-files git filter into a collapsible tree with green, collapsed-by-default folders.

**Architecture:** Two surgical changes. (1) Stop relying on the icon glyph's font advance: allocate a fixed-width icon column and paint the glyph centered in it with the painter, clipped to the column — robust against Nerd-Font/emoji-fallback metric mismatches. (2) Add a `changed_expanded: HashSet<PathBuf>` to `FileTreeState` (horizon-core) driving a caret-based collapsible render of the existing `ChangedTreeNode` tree, reusing the deferred `TreeAction` pattern; any filter toggle clears the set so re-enabling starts fully collapsed.

**Tech Stack:** Rust workspace, egui/eframe 0.33 (`horizon-ui`), plain std (`horizon-core`). Story: `docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md`.

**Branch:** `fix/file-explorer-icons-filter-tree` (already created from `main`). Push ONLY to remote `fork` — `origin` is third-party upstream.

**Repo conventions:** tests inline under `#[cfg(test)]`; conventional commits; gates = `cargo test --workspace`, `cargo clippy --workspace --all-targets` (6 pre-existing f32 warnings in `sidebar/auto_hide.rs` are NOT yours), `cargo fmt --all -- --check` (hard gate). No new dependencies. Do not touch sidebar/panels/terminal_widget.

---

### Task 1: Fixed-width icon column (fixes icon/text overlap in both views)

**Files:**
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (constants block :18-31, `render_row` :308-378, `render_changed_row` :414-470)

**Background for the implementer (zero context assumed):** Each row is a manual `Rect` + `scope_builder` left-to-right layout: indent → caret (dirs only, `\u{25bc}`/`\u{25b6}` 7pt + 4px) or 11px alignment space (files) → **icon via `ui.label(RichText … FontId::proportional(13.0))`** → `ui.add_space(6.0)` → `set_max_width(row width − LETTER_RESERVE|PLAIN_RESERVE)` → truncated name label. The status letter is painted absolutely at the right edge and is NOT part of the problem. The icon glyphs are Nerd Font Private-Use-Area codepoints (`file_type_icon`, :39-69). Symptom (user screenshots): the icon paints ON TOP of the middle of the name in both views.

- [ ] **Step 1: Diagnose — confirm the root cause before touching code (systematic debugging, do not guess)**

Run these and record what you find in the story's Dev Agent Record (`docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md`):

```bash
# Is a Nerd Font actually registered? If nothing matches, the PUA glyphs are
# falling back to egui's bundled emoji-icon-font, whose paint extents exceed
# their advance width — which would explain glyphs painting over later text.
grep -rn -i "nerd\|add_font\|FontDefinitions\|font_data\|insert.*Font" crates/horizon-ui/src --include="*.rs" | grep -v test
ls crates/horizon-ui/assets 2>/dev/null; find crates -iname "*.ttf" -o -iname "*.otf" | head
```

Interpretation guide: if a Symbols Nerd Font IS registered in the proportional fallback stack, the bug is its glyph metrics (paint extent > advance). If it is NOT registered (or the file/registration is broken), egui's `emoji-icon-font` fallback serves these PUA codepoints with known-bad metrics — same class of bug. **Either way the fix below applies** (it removes all dependence on glyph metrics), but write down which case it is. Only if you find a trivially broken registration (e.g. font bytes present but never inserted into the fallback stack) fix that too — anything bigger (shipping a new font) is OUT of scope; note it as a follow-up.

- [ ] **Step 2: Add the icon-column constant and painter helper**

In the constants block (after `PLAIN_RESERVE`, :31) add:

```rust
/// Fixed width of the icon column. Icons are painted centered inside this
/// column (clipped to it) instead of flowing as a label, so glyphs with
/// paint extents wider than their font advance (Nerd Font / emoji fallback)
/// can never bleed over the filename that follows.
const ICON_COL_WIDTH: f32 = 18.0;
```

After `paint_status_letter` (:482) add:

```rust
/// Allocates a fixed-width icon column and paints `icon` centered in it,
/// clipped to the column rect. Replaces `ui.label(icon)` in row rendering:
/// a label's width follows the glyph's font advance, which for Nerd Font /
/// emoji-fallback glyphs is narrower than the painted outline — the next
/// widget then starts under the glyph. A fixed column sidesteps the metrics
/// entirely.
fn paint_icon_column(ui: &mut egui::Ui, icon: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ICON_COL_WIDTH, ROW_HEIGHT), egui::Sense::hover());
    ui.painter().with_clip_rect(rect).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::proportional(ICON_FONT_SIZE),
        color,
    );
}
```

- [ ] **Step 3: Use it in `render_row`**

Replace (current :343-348):

```rust
            ui.label(
                RichText::new(file_type_icon(&node.path, node.is_dir))
                    .font(FontId::proportional(ICON_FONT_SIZE))
                    .color(if node.is_dir { theme::ACCENT() } else { name_color }),
            );
            ui.add_space(6.0);
```

with:

```rust
            paint_icon_column(
                ui,
                file_type_icon(&node.path, node.is_dir),
                if node.is_dir { theme::ACCENT() } else { name_color },
            );
            ui.add_space(2.0);
```

(6px gap shrinks to 2px because the 18px column already includes breathing room around the ~13px glyph.)

- [ ] **Step 4: Use it in `render_changed_row`**

Replace (current :441-446):

```rust
            ui.label(
                RichText::new(icon)
                    .font(FontId::proportional(ICON_FONT_SIZE))
                    .color(icon_color),
            );
            ui.add_space(6.0);
```

with:

```rust
            paint_icon_column(ui, icon, icon_color);
            ui.add_space(2.0);
```

- [ ] **Step 5: Run the gates**

```bash
cargo test -p horizon-ui && cargo clippy -p horizon-ui --all-targets && cargo fmt --all -- --check
```

Expected: all existing tests PASS (no behavior under test changed), clippy clean, fmt clean. If fmt fails, run `cargo fmt --all` and re-check.

- [ ] **Step 6: Update story Dev Agent Record (root cause + File List) and commit**

```bash
git add crates/horizon-ui/src/file_tree_widget.rs docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md
git commit -m "fix(ui): paint file-tree icons in a fixed-width column to stop text overlap"
```

---

### Task 2: Core expansion state for the changed-file filter tree (TDD)

**Files:**
- Modify: `crates/horizon-core/src/file_tree.rs` (`FileTreeState` struct :204-214, `impl` :216-245, tests at end)

**Background:** `FileTreeState` is the per-panel explorer state. The filter view is rebuilt every frame from `changed_file_tree(&GitStatus)` into `ChangedTreeNode`s (dirs have `status: None`, files `Some`). There is no expansion state for it today — the UI renders everything. We add a `HashSet<PathBuf>` keyed by `ChangedTreeNode.abs_path` (works with compacted `a/b/c` nodes — their `abs_path` is the deepest dir). Default empty = all collapsed. ANY change of the filter toggle clears it, so off→on always starts collapsed (story AC 6).

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/horizon-core/src/file_tree.rs`:

```rust
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
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p horizon-core file_tree -- --nocapture
```

Expected: COMPILE ERROR — `is_changed_expanded`, `expand_changed`, `collapse_changed`, `set_show_only_changes` not found. That is the red.

- [ ] **Step 3: Implement the state**

Top of file, extend the imports (:3):

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};
```

Add field to `FileTreeState` (after `show_only_changes`, :213):

```rust
    /// Directories of the filtered (uncommitted) tree the user expanded,
    /// keyed by absolute path. Empty = fully collapsed (the default).
    /// Cleared whenever the filter toggle changes value.
    pub changed_expanded: HashSet<PathBuf>,
```

In `FileTreeState::new` add `changed_expanded: HashSet::new(),` to the struct literal.

Add methods to the `impl` block (after `set_git_status`):

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass (plus the whole crate)**

```bash
cargo test -p horizon-core
```

Expected: PASS, including the 3 new tests and all 11 pre-existing file_tree tests (grouping/compaction regressions).

- [ ] **Step 5: Gates and commit**

```bash
cargo clippy -p horizon-core --all-targets && cargo fmt --all -- --check
git add crates/horizon-core/src/file_tree.rs
git commit -m "feat(core): collapsed-by-default expansion state for the changed-file filter tree"
```

---

### Task 3: Collapsible green filter tree in the UI

**Files:**
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (`TreeAction` :120-127, `show()` :136-204, `render_changes_only` :385-398, `render_changed_nodes` :400-407, `render_changed_row` :414-470, tests)

**Background:** Task 2 added the core state. Now wire it: dir rows in the filter view get a caret (same glyphs as the normal tree: `\u{25bc}` open / `\u{25b6}` closed, 7pt, `theme::FG_DIM()`), folder icon+name in `theme::PALETTE_GREEN()` (the same green the `U`/`A` status letters use — story AC 3), children render only when expanded, click toggles via the deferred-action pattern. Files keep their current rendering (status-colored name + letter; double-click opens). Depends on Task 1 (`paint_icon_column`) and Task 2 (state methods).

- [ ] **Step 1: Write the failing test for the dir-row visuals helper**

Append inside `mod tests` in `file_tree_widget.rs`:

```rust
    #[test]
    fn changed_dir_visuals_follow_expansion_state() {
        let (closed_caret, closed_icon) = changed_dir_visuals(false);
        let (open_caret, open_icon) = changed_dir_visuals(true);
        // carets match the normal tree's glyphs
        assert_eq!(closed_caret, "\u{25b6}");
        assert_eq!(open_caret, "\u{25bc}");
        // closed vs open folder icons differ
        assert_ne!(closed_icon, open_icon);
        assert_eq!(closed_icon, "\u{f07b}"); // nf-fa-folder
        assert_eq!(open_icon, "\u{f07c}"); // nf-fa-folder_open
    }
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test -p horizon-ui changed_dir_visuals -- --nocapture
```

Expected: COMPILE ERROR — `changed_dir_visuals` not found.

- [ ] **Step 3: Implement the helper**

Add above `render_changed_row`:

```rust
/// Caret and folder glyph for a directory row of the filtered tree, by
/// expansion state. Carets mirror the normal tree (`\u{25bc}`/`\u{25b6}`).
fn changed_dir_visuals(is_open: bool) -> (&'static str, &'static str) {
    if is_open {
        ("\u{25bc}", "\u{f07c}") // nf-fa-folder_open
    } else {
        ("\u{25b6}", "\u{f07b}") // nf-fa-folder
    }
}
```

- [ ] **Step 4: Extend `TreeAction`**

Replace the enum (:120-127) with:

```rust
enum TreeAction {
    /// Lazily scan and expand the directory at this path.
    Expand(std::path::PathBuf),
    /// Re-hide (collapse) the directory at this path.
    Collapse(std::path::PathBuf),
    /// Open the file at this path in VS Code.
    Open(std::path::PathBuf),
    /// Expand a directory of the filtered (uncommitted) tree.
    ExpandChanged(std::path::PathBuf),
    /// Collapse a directory of the filtered (uncommitted) tree.
    CollapseChanged(std::path::PathBuf),
}
```

- [ ] **Step 5: Rewrite the filter render functions**

Add `use std::collections::HashSet;` and `use std::path::PathBuf;` to the imports if not present (`Path` is already imported at :8 — extend that line).

Replace `render_changes_only` (:385-398) with:

```rust
/// Renders the grouped "only uncommitted files" tree. With no git snapshot,
/// shows a dim "Sem reposit\u{f3}rio git" message; with an empty change set,
/// shows "Nada para commitar". Changed files are nested under their parent
/// folders (collapsed by default, single-child chains compacted VSCode-style);
/// folders render green with a caret and expand on click. Double-clicking a
/// file row collects a [`TreeAction::Open`].
fn render_changes_only(
    ui: &mut egui::Ui,
    status: Option<&GitStatus>,
    expanded: &HashSet<PathBuf>,
    action: &mut Option<TreeAction>,
) {
    let Some(status) = status else {
        render_centered_dim(ui, "Sem reposit\u{f3}rio git");
        return;
    };

    let tree = changed_file_tree(status);
    if tree.is_empty() {
        render_centered_dim(ui, "Nada para commitar");
        return;
    }

    render_changed_nodes(ui, &tree, 0, expanded, action);
}
```

Replace `render_changed_nodes` (:400-407) with:

```rust
fn render_changed_nodes(
    ui: &mut egui::Ui,
    nodes: &[ChangedTreeNode],
    depth: usize,
    expanded: &HashSet<PathBuf>,
    action: &mut Option<TreeAction>,
) {
    for node in nodes {
        if node.is_dir {
            let is_open = expanded.contains(&node.abs_path);
            if render_changed_row(ui, node, depth, is_open) && action.is_none() {
                *action = Some(if is_open {
                    TreeAction::CollapseChanged(node.abs_path.clone())
                } else {
                    TreeAction::ExpandChanged(node.abs_path.clone())
                });
            }
            if is_open {
                render_changed_nodes(ui, &node.children, depth + 1, expanded, action);
            }
        } else if render_changed_row(ui, node, depth, false) && action.is_none() {
            *action = Some(TreeAction::Open(node.abs_path.clone()));
        }
    }
}
```

Replace `render_changed_row` (:414-470) with:

```rust
/// Renders one row of the grouped uncommitted-files tree: directories show a
/// caret plus a green folder icon and green name (green marks "contains
/// uncommitted changes"); files show their type icon, the name colored by
/// status (truncated with `…` when too wide), and a fixed right-aligned
/// status letter. Returns `true` when the row should act — click for a
/// directory (toggle), double-click for a file (open in editor).
fn render_changed_row(ui: &mut egui::Ui, node: &ChangedTreeNode, depth: usize, is_open: bool) -> bool {
    let row_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), ROW_HEIGHT));
    let response = ui.allocate_rect(row_rect, egui::Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(row_rect, CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }

    let decoration = status_decoration(node.status);
    let name_color = decoration.map_or_else(theme::FG, |(_, color)| color);

    #[allow(clippy::cast_precision_loss)]
    let indent = BASE_INDENT + depth as f32 * INDENT_PER_DEPTH;

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(row_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(indent);

            let (icon, icon_color, text_color) = if node.is_dir {
                let (caret, folder_icon) = changed_dir_visuals(is_open);
                ui.label(RichText::new(caret).size(7.0).color(theme::FG_DIM()));
                ui.add_space(4.0);
                (folder_icon, theme::PALETTE_GREEN(), theme::PALETTE_GREEN())
            } else {
                // Align files with folders that have a caret in front.
                ui.add_space(11.0);
                (file_type_icon(&node.abs_path, false), name_color, name_color)
            };
            paint_icon_column(ui, icon, icon_color);
            ui.add_space(2.0);

            let reserve = if decoration.is_some() {
                LETTER_RESERVE
            } else {
                PLAIN_RESERVE
            };
            ui.set_max_width((row_rect.width() - reserve).max(20.0));
            ui.add(
                egui::Label::new(
                    RichText::new(&node.name)
                        .font(FontId::proportional(ROW_FONT_SIZE))
                        .color(text_color),
                )
                .truncate(),
            );
        },
    );

    if let Some((letter, color)) = decoration {
        paint_status_letter(ui, row_rect, letter, color);
    }

    if node.is_dir {
        response.clicked()
    } else {
        response.double_clicked()
    }
}
```

- [ ] **Step 6: Wire `show()`**

In `FileExplorerView::show` (:136-204):

Replace `state.show_only_changes = show_only;` (:153) with:

```rust
        state.set_show_only_changes(show_only);
```

Replace the filter call inside the scroll closure (:174) with:

```rust
                    render_changes_only(ui, status.as_deref(), &state.changed_expanded, &mut action);
```

Extend the final `match action` (:196-201) with the two new arms before `None => {}`:

```rust
            Some(TreeAction::ExpandChanged(path)) => state.expand_changed(path),
            Some(TreeAction::CollapseChanged(path)) => state.collapse_changed(&path),
```

- [ ] **Step 7: Run the full gates**

```bash
cargo test -p horizon-ui -p horizon-core && cargo clippy --workspace --all-targets && cargo fmt --all -- --check
```

Expected: PASS (new `changed_dir_visuals` test + all existing), clippy no new warnings, fmt clean.

- [ ] **Step 8: Update story (mark Tasks, File List) and commit**

```bash
git add crates/horizon-ui/src/file_tree_widget.rs docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md
git commit -m "feat(ui): collapsible green folder tree for the uncommitted-files filter"
```

---

### Task 4: Delivery — gates, release build, install, in-app validation, push

**Files:**
- Modify: `docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md` (status → Done, trade-offs)
- No source changes (fixes only if validation finds defects).

- [ ] **Step 1: Full workspace gates**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets && cargo fmt --all -- --check
```

Expected: all green (clippy: only the 6 pre-existing f32 warnings in `sidebar/auto_hide.rs`).

- [ ] **Step 2: Release build and install**

```bash
cargo build --release && cp target/release/horizon ~/.local/bin/horizon && ~/.local/bin/horizon --version 2>/dev/null; echo "installed: $(stat -c %y ~/.local/bin/horizon)"
```

Expected: build succeeds (~2m30s); installed timestamp is now.

- [ ] **Step 3: Autonomous in-app validation (X11)**

Launch the installed binary against a repo with uncommitted changes (the horizon-fork working tree qualifies). Capture screenshots with whatever is available (`scrot`, `import` from ImageMagick, `gnome-screenshot`, `spectacle -b`, or `flameshot full`); drive interaction with `xdotool` if needed.

```bash
cd ~/horizon-fork && nohup ~/.local/bin/horizon >/tmp/horizon-validation.log 2>&1 &
sleep 6
scrot /tmp/horizon-validate-1-tree.png || import -window root /tmp/horizon-validate-1-tree.png
```

Verify in order (screenshot each state; READ the screenshots — do not just take them):
1. Normal tree: icons in their own column, no glyph over any filename (AC 1).
2. Click the funnel filter icon in the Files panel header (xdotool click at its coordinates, found from screenshot 1): folders appear COLLAPSED and GREEN with carets; root-level changed files show status letters (AC 2, 3, 4).
3. Click a green folder: it expands showing children (files with letters); click again: collapses (AC 3, 4).
4. Toggle filter off and on: everything collapsed again (AC 6).
5. Kill the app: `pkill -f '.local/bin/horizon'`.

If the Files panel is not present in the restored workspace or interaction proves unreliable, report exactly what could and could not be verified and ask the user for manual validation — do NOT claim validated without evidence (AC 10).

- [ ] **Step 4: Finalize story and commit docs**

Mark all story tasks `[x]`, set Status → Done (or "Done — pending manual validation" if step 3 fell back), record trade-offs (e.g. 2px tighter icon gap; fixed 18px icon column clips oversized fallback glyphs).

```bash
git add docs/stories/story-file-explorer-icon-overlap-and-filter-tree.md docs/superpowers/plans/2026-06-07-file-explorer-icon-overlap-and-filter-tree.md
git commit -m "docs: finalize file-explorer icon/filter-tree story"
```

- [ ] **Step 5: Push to fork**

```bash
git push -u fork fix/file-explorer-icons-filter-tree
```

Expected: branch on `fork` remote (NEVER push to `origin`). Merging to `main` is the user's call — do not merge unprompted.
