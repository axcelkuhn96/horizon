# File-search UX + alt-screen scrollbar — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three UI defects: (1) the file-explorer content-search panel can't be closed; (2) clicking a search result doesn't open the file in VS Code; (3) the terminal scrollbar shows a useless full-height pill while a full-screen TUI (claude code) is in alt-screen.

**Architecture:** All three are localized fixes in `horizon-ui`. Each extracts a small **pure** decision function (unit-testable) and wires it into the existing egui render path. No new state machines; reuse existing `SearchUiAction` / `close_search()` / `open_in_vscode()` / `render_scrollbar` plumbing.

**Tech Stack:** Rust, egui 0.33.3, alacritty_terminal 0.26.0. Source PRD: `docs/2026-06-09-file-search-ux-and-alt-screen-scrollbar.md`.

**Workspace:** Work on the **current branch** (`main`). **Do NOT create a git worktree.** No push.

**Gates (run before declaring any task done):**
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --lib --bins --examples -- -D warnings -D clippy::unwrap_used -D clippy::expect_used`
- `cargo test --workspace`

---

### Task 1: Close the search panel reliably (X button + unconditional Escape)

**Root cause:** Esc is only honored when the search `TextEdit` has egui focus (`file_search_widget.rs:252`), but the terminal steals focus back via `is_active_panel && !other_widget_has_focus` (`terminal_widget/mod.rs:112-116`), so the input loses focus and Esc never fires. There is no close button. Fix: a pure predicate + an always-visible X glyph + an Escape check that does not depend on the TextEdit holding focus (safe because `show_search_panel` only renders while `state.search.active`).

**Files:**
- Modify: `crates/horizon-ui/src/file_search_widget.rs` (input row ~226-256; add pure fn + test module)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `crates/horizon-ui/src/file_search_widget.rs`:

```rust
#[test]
fn close_requested_on_escape_or_button() {
    // Escape alone closes.
    assert!(search_close_requested(true, false));
    // The X button alone closes.
    assert!(search_close_requested(false, true));
    // Both at once still closes.
    assert!(search_close_requested(true, true));
    // Neither does nothing.
    assert!(!search_close_requested(false, false));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-ui close_requested_on_escape_or_button`
Expected: FAIL — `cannot find function search_close_requested`.

- [ ] **Step 3: Add the pure predicate**

Near the top-level fns of `file_search_widget.rs` (e.g. just above `show_search_panel`):

```rust
/// Whether the search panel should close this frame. Pure so the close logic is
/// unit-tested without an egui context. The panel closes if Escape was pressed
/// (the panel only renders while `search.active`, so consuming Escape here is
/// safe) OR the header close (X) button was clicked.
#[must_use]
pub fn search_close_requested(escape_pressed: bool, close_clicked: bool) -> bool {
    escape_pressed || close_clicked
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horizon-ui close_requested_on_escape_or_button`
Expected: PASS.

- [ ] **Step 5: Wire the X button + unconditional Escape into the input row**

In `show_search_panel_inner`, replace the focus-gated Escape block (currently `file_search_widget.rs:250-254`) and add a close button at the end of the input row. The input row is the `allocate_ui_with_layout` closure (~lines 220-256). After the `TextEdit` is added and `response.changed()/focus_requested` handled, replace:

```rust
            // Esc closes the panel (only honored while the input has focus so it
            // doesn't swallow a global Esc).
            if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                *action = Some(SearchUiAction::Close);
            }
```

with:

```rust
            // Close (X) button — always clickable regardless of which widget holds
            // egui focus (the terminal can steal focus from the TextEdit, which is
            // why Esc-while-focused alone was unreliable).
            let close = ui.add(
                egui::Button::new(RichText::new("\u{f00d}").size(12.0).color(theme::FG_DIM()))
                    .frame(false),
            );
            // Escape is checked unconditionally: `show_search_panel` only renders
            // while `state.search.active`, so consuming Escape here cannot swallow
            // an Escape meant for another context.
            let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if search_close_requested(escape_pressed, close.clicked()) {
                *action = Some(SearchUiAction::Close);
            }
```

(Keep the existing `response.request_focus()` / `response.changed()` handling above this block unchanged. The `\u{f00d}` glyph is nf-fa-times / the existing icon font's "✕".)

- [ ] **Step 6: Run the gates**

Run:
```
cargo fmt --all -- --check
cargo clippy --workspace --lib --bins --examples -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
cargo test -p horizon-ui
```
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/horizon-ui/src/file_search_widget.rs
git commit -m "fix(ui): file-search closes via X button and unconditional Esc"
```

**Acceptance:** With search open, clicking X closes it; pressing Esc closes it even when the TextEdit lost focus to the terminal; after closing, the tree renders again.

---

### Task 2: Open the clicked search result in VS Code (double-click)

**Root cause:** `SearchUiAction` has only `Close` and `Reveal`; result clicks emit `Reveal` (expand tree only) and never call `open_in_vscode`. Add an `Open(PathBuf)` variant emitted on **double-click** (single click keeps `Reveal`), handled in the caller exactly like `TreeAction::Open`.

**Files:**
- Modify: `crates/horizon-ui/src/file_search_widget.rs` (enum ~41-47; `render_file_header` ~371-398; `render_match_row` ~402-440; add pure fn + test)
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (search-action match ~198-204)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `file_search_widget.rs`:

```rust
#[test]
fn result_click_maps_to_reveal_double_click_to_open() {
    let p = Path::new("src/main.rs");
    // Double-click opens in the editor.
    assert!(matches!(
        result_row_action(p, false, true),
        Some(SearchUiAction::Open(ref q)) if q == p
    ));
    // Single click reveals in the tree.
    assert!(matches!(
        result_row_action(p, true, false),
        Some(SearchUiAction::Reveal(ref q)) if q == p
    ));
    // No interaction → no action.
    assert!(result_row_action(p, false, false).is_none());
    // Double-click takes precedence when both are reported the same frame.
    assert!(matches!(
        result_row_action(p, true, true),
        Some(SearchUiAction::Open(_))
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-ui result_click_maps_to_reveal_double_click_to_open`
Expected: FAIL — `cannot find function result_row_action` and `no variant Open`.

- [ ] **Step 3: Add the `Open` variant**

In the `SearchUiAction` enum (`file_search_widget.rs:41-47`), add:

```rust
pub enum SearchUiAction {
    /// User pressed Esc or the close button — close the search panel.
    Close,
    /// User clicked a match row — reveal this file in the tree (expand its
    /// ancestor directories and scroll it into view).
    Reveal(PathBuf),
    /// User double-clicked a result row — open the file in VS Code.
    Open(PathBuf),
}
```

- [ ] **Step 4: Add the pure mapping fn**

```rust
/// Maps a result-row interaction to its action. Pure so click routing is
/// unit-tested without an egui context. Double-click opens the file in the
/// editor; a single click reveals it in the tree. Double-click wins if both are
/// reported in the same frame.
#[must_use]
pub fn result_row_action(path: &Path, clicked: bool, double_clicked: bool) -> Option<SearchUiAction> {
    if double_clicked {
        Some(SearchUiAction::Open(path.to_path_buf()))
    } else if clicked {
        Some(SearchUiAction::Reveal(path.to_path_buf()))
    } else {
        None
    }
}
```

- [ ] **Step 5: Use it in both row renderers**

In `render_file_header` replace (`file_search_widget.rs:395-397`):

```rust
    if header.clicked() && action.is_none() {
        *action = Some(SearchUiAction::Reveal(path.to_path_buf()));
    }
```
with:
```rust
    if action.is_none()
        && let Some(a) = result_row_action(path, header.clicked(), header.double_clicked())
    {
        *action = Some(a);
    }
```

In `render_match_row` replace (`file_search_widget.rs:437-439`):

```rust
    if row.clicked() && action.is_none() {
        *action = Some(SearchUiAction::Reveal(path.to_path_buf()));
    }
```
with:
```rust
    if action.is_none()
        && let Some(a) = result_row_action(path, row.clicked(), row.double_clicked())
    {
        *action = Some(a);
    }
```

- [ ] **Step 6: Handle `Open` in the caller**

In `file_tree_widget.rs` search-action match (`198-204`), add the `Open` arm:

```rust
            match search_action {
                Some(crate::file_search_widget::SearchUiAction::Close) => state.close_search(),
                Some(crate::file_search_widget::SearchUiAction::Reveal(path)) => {
                    reveal_in_tree(&mut state.roots, &state.root, &path);
                }
                Some(crate::file_search_widget::SearchUiAction::Open(path)) => {
                    open_in_vscode(&path, &mut state.code_missing);
                }
                None => {}
            }
```

- [ ] **Step 7: Run test + gates**

Run:
```
cargo test -p horizon-ui result_click_maps_to_reveal_double_click_to_open
cargo fmt --all -- --check
cargo clippy --workspace --lib --bins --examples -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
cargo test --workspace
```
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/horizon-ui/src/file_search_widget.rs crates/horizon-ui/src/file_tree_widget.rs
git commit -m "feat(ui): double-click a search result to open it in VS Code"
```

**Acceptance:** double-clicking a result opens the file via `code <path>` (footer warning if `code` is missing, same UX as the tree); single click still reveals in the tree.

---

### Task 3: Hide the terminal scrollbar in alt-screen

**Root cause:** in alt-screen the alacritty grid has `max_scroll_limit=0` → `history_size()==0` → `render_scrollbar` draws a useless full-height pill. Gate the call on `TermMode::ALT_SCREEN` (exposed via `terminal.mode()` at `terminal/events.rs:58`). A normal shell with scrollback is unaffected.

**Files:**
- Modify: `crates/horizon-ui/src/terminal_widget/mod.rs` (imports; render block ~141-176; add test)

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod` (or extend an existing one) in `terminal_widget/mod.rs`:

```rust
#[cfg(test)]
mod scrollbar_visibility_tests {
    use super::terminal_scrollbar_visible;
    use alacritty_terminal::term::TermMode;

    #[test]
    fn scrollbar_hidden_in_alt_screen() {
        assert!(!terminal_scrollbar_visible(TermMode::ALT_SCREEN));
        // Alt-screen combined with other flags is still hidden.
        assert!(!terminal_scrollbar_visible(TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK));
    }

    #[test]
    fn scrollbar_shown_in_normal_screen() {
        assert!(terminal_scrollbar_visible(TermMode::NONE));
        assert!(terminal_scrollbar_visible(TermMode::APP_CURSOR));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horizon-ui scrollbar_visibility_tests`
Expected: FAIL — `cannot find function terminal_scrollbar_visible`.

- [ ] **Step 3: Add the import + pure predicate**

At the top of `terminal_widget/mod.rs`, add the import (if not present):

```rust
use alacritty_terminal::term::TermMode;
```

Add the pure fn at module scope:

```rust
/// Whether to draw the host scrollbar for a terminal in this mode. A full-screen
/// TUI in alt-screen (e.g. claude code) has no host scrollback — `history_size()`
/// is always 0 there — so the scrollbar would render as a useless full-height
/// pill. Hide it in alt-screen; a normal shell with scrollback keeps it.
#[must_use]
fn terminal_scrollbar_visible(mode: TermMode) -> bool {
    !mode.contains(TermMode::ALT_SCREEN)
}
```

- [ ] **Step 4: Gate the render call**

In the render block, capture the mode next to `history_size` (`mod.rs:144`) and wrap the `render_scrollbar` call (`mod.rs:166-173`):

```rust
            let history_size = terminal.history_size();
            let scrollbar_visible = terminal_scrollbar_visible(terminal.mode());
            let scrollbar_highlighted = interaction.scrollbar.hovered() || interaction.scrollbar.dragged();
```

then, inside the `with_renderable_content` closure, wrap only the scrollbar call:

```rust
                if scrollbar_visible {
                    render_scrollbar(
                        ui,
                        interaction.layout.scrollbar,
                        display_offset,
                        usize::from(new_rows),
                        history_size,
                        scrollbar_highlighted,
                    );
                }
```

(`render_grid` and `render_cursor` stay unchanged and unconditional.)

- [ ] **Step 5: Run test + gates**

Run:
```
cargo test -p horizon-ui scrollbar_visibility_tests
cargo fmt --all -- --check
cargo clippy --workspace --lib --bins --examples -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
cargo test --workspace
```
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/horizon-ui/src/terminal_widget/mod.rs
git commit -m "fix(ui): hide terminal scrollbar in alt-screen (claude code)"
```

**Acceptance:** inside claude (alt-screen) the scrollbar pill is gone; a normal shell with scrollback still shows and uses it.

---

### Task 4: Ctrl+click a file path in the terminal → open in VS Code (relative paths + line jump)

**Root cause / gap:** A Ctrl/Cmd+click handler already exists (`terminal_widget/input.rs:399-415`) and intercepts before PTY forwarding (works in alt-screen/mouse-mode). But `clickable_at_point` (`horizon-core terminal/content.rs:207` → `find_file_path_at_column` `terminal/support.rs:298`) only detects **absolute** paths (`/…`, `~/…`) and opens via `open_url` (xdg-open). Claude code prints **relative** paths like `crates/foo/bar.rs:12`, which aren't detected (no pointing-hand, click leaks to claude), and even absolute hits don't open in VS Code nor jump to the line.

**Decision (user):** open in VS Code at the line — `code --goto <path>:<line>` (falls back to `code <path>` when no line). URLs keep using `open_url`.

**Design:**
- New enum in horizon-core: `pub enum ClickTarget { Url(String), File { path: String, line: Option<u32> } }`.
- `clickable_at_point` returns `Option<ClickTarget>` (was `Option<String>`). URL match → `Url`; file match → `File { path, line }`.
- `find_file_path_at_column` → returns `Option<(String, Option<u32>)>` (path WITHOUT the `:line[:col]` suffix, plus the parsed line). Extend it to also match **relative** path tokens with a tight heuristic (low false-positive): a token bounded by `is_path_boundary` that **contains a `/`** OR **looks like `name.ext`** (has a `.` with a 1–8 char alnum extension). The line is the first numeric `:N` after the path.
- Caller (`input.rs:handle_pointer_button`): on `Url(u)` → `horizon_core::open_url(&u)`; on `File { path, line }` → resolve relative against the panel cwd (`panel.launch_cwd`), then call a new `crate::file_tree_widget::open_path_in_vscode(&resolved, line)`. Capture `panel.launch_cwd.clone()` BEFORE the `panel.terminal()` borrow in the let-chain to avoid a borrow conflict.
- New `pub(crate) fn open_path_in_vscode(path: &Path, line: Option<u32>)` in `file_tree_widget.rs`: spawns `code` with `--goto <path>:<line>` when `line` is Some, else `code <path>`; fire-and-forget, `tracing::warn!` on spawn failure (no path/content logged). Reuse the detached-spawn style of `try_launch_editor` (null stdio).

**Files:**
- Modify: `crates/horizon-core/src/terminal/support.rs` (detection + tests)
- Modify: `crates/horizon-core/src/terminal/content.rs` (`clickable_at_point` return type) + re-export `ClickTarget`
- Modify: `crates/horizon-ui/src/terminal_widget/input.rs` (caller dispatch; hover path stays `is_some()`)
- Modify: `crates/horizon-ui/src/file_tree_widget.rs` (`open_path_in_vscode`)

- [ ] **Step 1: Failing tests (horizon-core, pure detection).** In support.rs tests, add cases for `find_file_path_at_column` new contract `(String, Option<u32>)`:
  - absolute, no line: `/a/b.rs` → `("/a/b.rs", None)`
  - absolute with line: `/a/b.rs:12` → `("/a/b.rs", Some(12))`
  - absolute with line:col: `/a/b.rs:12:5` → `("/a/b.rs", Some(12))`
  - relative with slash + line: `crates/foo/bar.rs:7` (clicked inside it) → `("crates/foo/bar.rs", Some(7))`
  - bare `name.ext` with line: `main.rs:3` → `("main.rs", Some(3))`
  - a plain word with no slash and no extension (e.g. `hello`) → `None`
  - URL still detected by `find_url_at_column` (unchanged).
  Run `cargo test -p horizon-core` for these names; confirm FAIL.

- [ ] **Step 2: Implement detection.** Update `find_file_path_at_column` to the new return type, add relative-token matching with the tight heuristic, and parse the first `:N` line from the stripped suffix. Keep `strip_line_col_suffix_chars` (or adapt it to also yield the first numeric suffix as the line). Add `ClickTarget` enum (export from the terminal module the same way `open_url` is exported). Update `clickable_at_point` to build `ClickTarget`. Run the tests; confirm PASS.

- [ ] **Step 3: Wire the caller + opener.** Add `open_path_in_vscode` to file_tree_widget.rs. Update `handle_pointer_button` to match on `ClickTarget` and the hover block (`input.rs:165-171`) stays as `clickable_at_point(...).is_some()`. Resolve relative paths via `panel.launch_cwd` (clone before the terminal borrow).

- [ ] **Step 4: Gates + commit.** Run all gates (fmt, strict clippy, `cargo test --workspace`). Commit:
  ```bash
  git add crates/horizon-core/src/terminal/support.rs crates/horizon-core/src/terminal/content.rs \
          crates/horizon-ui/src/terminal_widget/input.rs crates/horizon-ui/src/file_tree_widget.rs
  git commit -m "feat(terminal): Ctrl+click a file path (incl. relative) to open in VS Code at line"
  ```

**Acceptance:** Ctrl/Cmd+click on a relative path claude prints (e.g. `crates/x/y.rs:12`) opens it in VS Code at line 12, resolved against the panel cwd, even while claude runs in alt-screen+mouse-mode; absolute paths likewise open in code; URLs still open in the browser; a plain word does not show the pointing hand. **Risk:** relative-path heuristic may have occasional false positives (a slashless word with a dotted token) — accepted; the pointing-hand only appears with Ctrl/Cmd held and the click is otherwise harmless if the resolved path doesn't exist.

## Risks
1. **Focus interaction (Task 1):** the unconditional Escape read happens inside the explorer panel render; confirm it doesn't fire when the panel isn't visible (it can't — `show_search_panel` only runs when `search.active`). Verify the terminal doesn't also consume the same Escape (its focus-lock filter has `escape: false`, so Escape is a normal event — fine).
2. **Double vs single click (Task 2):** on a double-click egui reports `clicked()` on the first press frame, so a quick reveal-then-open can occur. Acceptable (reveals then opens). Documented.
3. **`if let` chains:** `let ... && let Some(a) = ...` requires the edition's let-chains; the repo is edition 2024 (`Cargo.toml`) which supports them — already used elsewhere (`mod.rs:141-142`). If clippy/compile complains, fall back to nested `if let`.
4. **`TermMode` flag names:** confirm `MOUSE_REPORT_CLICK` / `APP_CURSOR` exist in alacritty_terminal 0.26 (used in `input/mod.rs` tests already) — if a name differs, use any non-alt flag for the test.

## Self-review
- Spec coverage: Task 1 ↔ bug 3 (close), Task 2 ↔ bug 4 (open in VS Code), Task 3 ↔ bug 2 (alt-screen scrollbar). All three PRD ACs mapped.
- No placeholders: every step has concrete code/commands.
- Type consistency: `SearchUiAction::Open(PathBuf)` defined in Task 2 Step 3, used in Steps 5–6; `result_row_action`/`search_close_requested`/`terminal_scrollbar_visible` signatures consistent across their tasks.
