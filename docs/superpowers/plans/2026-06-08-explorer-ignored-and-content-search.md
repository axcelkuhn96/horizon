# Plan: File Explorer — show gitignored entries (dimmed) + contextual content search

Branch: `feat/explorer-ignored-and-search` (off `main`; push to remote `fork`).
Source prompt: refined via /piloto on 2026-06-08. No `_bmad/` in this repo → BMAD personas injected into subagents.

## Background / root causes (verified in code)

- `crates/horizon-core/src/file_tree.rs:33` `scan_dir()` builds `ignore::WalkBuilder` with
  `.hidden(false)` (dotfiles already shown — NOT the cause) and `.git_ignore(true)`/`.git_global(true)`/`.git_exclude(true)`.
  → gitignored dirs like `temp`/`tmp` are filtered out by the walk. This is the only cause of the bug.
- `HARD_SKIP = [".git","node_modules","target"]` (`file_tree.rs:23`) is enforced via `.filter_entry`.
- `crates/horizon-ui/src/file_tree_widget.rs` is 886 lines (cap 1000) → search-results UI MUST live in a new module.
- `crates/horizon-core/src/file_tree.rs` is 623 lines → adding a field is fine; the search engine goes in a NEW core module.
- Focus is available: `app/panels.rs:141` calls `FileExplorerView::new(panel).show(ui, is_focused)`; `is_focused` derives from `board.focused == panel id` (`app/mod.rs:421` shows the `board.focused` concept).
- ⚠️ `Ctrl+Shift+F` is ALREADY the `search` shortcut bound to `CommandId::ToggleSearch` (terminal search): `config.rs:361/390`, dispatched in `app/actions/command_palette.rs:133/211`. (Note: `shortcut_inventory.rs:108` shows a `Ctrl+Shift+G` default for `search` — reconcile during impl; config string is authoritative at runtime.)
  → The contextual gate is the design: same key, dispatched by focused panel kind.
- Theme dim token exists: `theme::fg_dim()` (`theme.rs:233`) → use for dimmed ignored rows. Do NOT hardcode hex.

## Conventions (AGENTS.md)

- Split files approaching ~600 lines; CI FAILS >1000 lines in horizon-core/src & horizon-ui/src (non-test). No `#[allow(clippy::too_many_lines)]`.
- `#[cfg(test)]` at end of file. Commits imperative + scoped (`feat(explorer):`, `fix(explorer):`).
- Gates: `cargo clippy --all-targets --all-features -- -D warnings` (blocking); strict adds `-D clippy::unwrap_used -D clippy::expect_used`. `cargo test --workspace`.

---

## Story 1 — Bug: gitignored entries visible but dimmed

**As a** user browsing a project (e.g. mizuconecta) **I want** gitignored folders like `temp`/`tmp` to appear in the explorer (visually dimmed) **so that** I can navigate them, matching VSCode.

### Task 1.1 — Failing test: prove gitignored entries are hidden today
- File: `crates/horizon-core/src/file_tree.rs` (tests at end).
- Create a temp dir (`tempfile` if already a dev-dep; else `std::env::temp_dir()` + unique subdir cleaned up) with: a tracked file `keep.txt`, a `.gitignore` containing `tmp/` and `temp/`, and dirs `tmp/`, `temp/`, plus `node_modules/`.
- Assert (RED): current `scan_dir` does NOT yield `tmp`/`temp`. This documents the bug. (If `scan_dir` already changed, adapt: assert the NEW contract instead.)
- AC: test exists and characterizes the behavior.

### Task 1.2 — Add `ignored` flag to `FileNode`; classify during scan
- File: `crates/horizon-core/src/file_tree.rs`.
- Add `pub ignored: bool` to `FileNode` (`file_tree.rs:15`). Update all constructions (search the crate + horizon-ui for `FileNode {` literals).
- Change scan strategy: set `.git_ignore(false)` (and `.git_global(false)`/`.git_exclude(false)` as needed) so the walk yields gitignored entries, BUT build a gitignore matcher (`ignore::gitignore::GitignoreBuilder` rooted at the scan dir, honoring parents/global as the prior behavior did) and set `ignored = matcher matches path`. Keep `HARD_SKIP` via `.filter_entry` so `.git`/`node_modules`/`target` stay hidden regardless of ignore status.
- Decision (explicit): dot-entries other than `.git` ARE shown (already were, `.hidden(false)`), and inherit `ignored` from the matcher. `.git` stays hidden via HARD_SKIP.
- Make Task 1.1's test GREEN: `tmp`/`temp` now appear with `ignored == true`; `node_modules` absent; `keep.txt` present with `ignored == false`.
- Watch line count; if `file_tree.rs` crosses ~600 lines meaningfully, extract the ignore-classification helper into a sibling module (e.g. `file_tree/scan.rs` or `file_tree_ignore.rs`) — keep public API stable.
- AC: new unit tests cover: gitignored dir → ignored=true; normal file → ignored=false; HARD_SKIP hidden; dir without `.gitignore` → all ignored=false.

### Task 1.3 — Render ignored entries dimmed in the UI
- File: `crates/horizon-ui/src/file_tree_widget.rs` `paint_tree_row()` (~l.403).
- When `node.ignored`, paint the row label (and icon) using `theme::fg_dim()` instead of the normal fg. Must remain visually distinct from the green "changed" color and from normal entries. Do not alter chevron/selection geometry.
- Verify the "changed files" green tree and git status letters still render correctly (ignored + changed can coexist — changed/green wins for the status letter; name may stay dimmed — pick the rule and note it).
- AC: live screenshot shows `temp`/`tmp` dimmed; `.git`/`node_modules`/`target` absent; changed tree unaffected.

---

## Story 2 — Feature: contextual content search (Ctrl+Shift+F)

**As a** user with the explorer focused **I want** to search file contents under the explorer root and jump to matches **so that** I get VSCode-style "search in files".

### Task 2.1 — Search engine in a new core module (TDD)
- New file: `crates/horizon-core/src/file_search.rs` (declare `mod file_search;` in `lib.rs`).
- Types: `pub struct FileSearchOptions { pub case_sensitive: bool, pub regex: bool, pub max_results: usize, pub max_file_bytes: u64 }` with sane defaults (case-insensitive, substring, 1000, e.g. 2 MiB); `pub struct SearchMatch { pub line_number: usize, pub line_text: String, pub span: (usize, usize) }`; `pub struct FileSearchResult { pub path: PathBuf, pub matches: Vec<SearchMatch> }`.
- `pub fn search_files(root: &Path, query: &str, opts: &FileSearchOptions) -> SearchOutcome` where `SearchOutcome { results: Vec<FileSearchResult>, truncated: bool }`.
- Use `ignore::WalkBuilder` with the SAME HARD_SKIP filter; mirror Story 1 visibility (gitignored light files searchable; never descend node_modules/target/.git). Skip binary files (NUL byte in first ~1KB) and files larger than `max_file_bytes`. Stop and set `truncated=true` once `max_results` matches reached.
- Regex: compile with `regex` crate; invalid regex returns an error variant (NOT a panic). Empty query → empty results (no work).
- TDD: tests for substring hit (path+line+span), case-insensitive default, case-sensitive opt, regex hit, invalid regex → error, binary skipped, truncation cap, query empty.
- AC: all tests green; no `unwrap`/`expect` (strict clippy).

### Task 2.2 — Background search runner (no UI freeze)
- File: state lives in horizon-core (e.g. `file_search.rs`) or a small `file_search_runner.rs`; UI holds the handle.
- `std::thread::spawn` on search start; results delivered via `std::sync::mpsc`. A generation/epoch counter discards results from superseded queries. State: `Idle | Searching { generation } | Done { outcome }`.
- UI side polls the receiver each frame and calls `ctx.request_repaint()` while `Searching`.
- Inspect existing async patterns in the app first and mirror them; do not add a new async runtime.
- TDD where possible (engine is sync-testable; runner: test the generation/dedup logic with a fake).
- AC: searching a large tree keeps UI responsive (live check); stale query results are dropped.

### Task 2.3 — New command + contextual dispatch gate
- File: `crates/horizon-ui/src/command_registry.rs` — add `CommandId::SearchFileContents`.
- Dispatch: in `app/actions/command_palette.rs` (the shortcut-binding loop ~l.200-225), when `Ctrl+Shift+F` (`self.shortcuts.search`) is pressed, branch on the focused panel kind: if `board.focused` panel is `PanelKind::FileExplorer` → emit `SearchFileContents` (open/focus explorer content search); else → existing `ToggleSearch` (terminal search). Keep the terminal path byte-for-byte unchanged when explorer not focused.
- Add a focus-gate unit test: with explorer focused the predicate yields `SearchFileContents`; with a terminal focused it yields `ToggleSearch`. Extract a small pure predicate fn to make it testable (mirror existing `shortcut_pressed`-style helpers).
- AC: tests prove the gate; no regression to terminal search.

### Task 2.4 — Search panel UI (new widget module)
- New file: `crates/horizon-ui/src/file_search_widget.rs` (keep `file_tree_widget.rs` under 1000 lines).
- Search input (focused on open via the command), results list grouped by file with line snippets; truncation banner when `truncated`; "Searching…" state. Clicking a result reveals/selects the file in the tree (reuse existing selection in `FileTreeState`); opening in an editor only if a mechanism already exists — otherwise leave as a Next Step (do not invent).
- `render_header()` (`file_tree_widget.rs:232`) wires the open/focus affordance.
- Use theme tokens; manual painting consistent with `paint_tree_row` style. `#[cfg(test)]` at end.
- AC: live screenshot of the panel with results; click reveals file in tree.

---

## Story 3 — Delivery (ONLY after Stories 1 & 2 tested & approved live)

### Task 3.1 — README + CHANGELOG
- `README.md`: document dimmed gitignored entries + contextual `Ctrl+Shift+F` content search (note it's scoped to the focused explorer).
- `CHANGELOG.md`: new entry following existing style.

### Task 3.2 — Version bump + release
- Bump `0.2.8 → 0.3.0` (minor; new feature) in root `Cargo.toml:6` and workspace dep versions (lines 22-23+), mirroring how commit `df94f50` did the 0.2.8 bump (verify the exact set of files it touched).
- Commit, tag, push to remote **fork** (never `origin`). Only after live approval.

---

## Story 4 — Bug: git "changed" green does not auto-refresh (added 2026-06-08 mid-execution)

**Reported:** the tree itself refreshes when files are created (new files appear), but the git "changed" green coloring (`git_status` in `FileTreeState`, surfaced via `dir_contains_changes()`/`status_for_path()`) stays stale — it does not update when the working tree's git state changes. User must currently trigger a manual refresh.

### Task 4.1 — Investigate when/how `git_status` is loaded & refreshed
- Map: where `FileTreeState.git_status` is populated (`file_tree.rs` + the UI load path in `file_tree_widget.rs`/`app/lifecycle.rs`), whether there's a manual refresh in `render_header()`, and why it doesn't auto-update. Confirm the tree-structure refresh path (which DOES work) vs the git-status path (which doesn't).

### Task 4.2 — Auto-refresh git status (design TBD — see open question)
- Likely: recompute git status on a throttled cadence while the explorer is visible/focused, and/or when the panel regains focus, reusing the existing git-status computation. Avoid recomputing every frame (git2 status is not free). Decide trigger after 4.1.
- AC: after creating/modifying/staging a file, the green/changed coloring updates within a short, bounded delay without a manual refresh; no per-frame git calls; no UI stutter.

> OPEN QUESTION (user): preferred refresh trigger — (a) throttled timer poll (e.g. every ~1.5s while visible), (b) on explorer regaining focus, (c) filesystem watch via `notify` crate (heavier, new dep — discouraged per no-new-deps), or (d) combination of a+b. Pending answer.

## Story 5 — Feature: drop external files into an explorer folder to COPY them (added 2026-06-08 mid-execution)

**Reported:** user can't drag files into a folder in the explorer. Want: drop OS files onto the explorer → COPY them into the target folder (decided: copy, non-destructive). Drop target = folder under the cursor (folder row → into it; file row → its parent dir; empty area → explorer root), with the target folder highlighted on hover.

**Foundation exists:** `crates/horizon-ui/src/app/file_drop.rs` already handles `dropped_files`/`hovered_files`, partitions editor vs non-editor drops, has `select_terminal_drop_target`, `native_file_drop_local_pos`, paste-into-terminal. Extend this for the explorer.

### Task 5.1 — Hit-test the explorer drop target
- Map screen drop position → the file-tree row under it → resolve the destination directory (folder row → that dir; file row → its parent; none/empty → explorer root). Needs row rects from `file_tree_widget.rs` painting (it paints rows manually; capture row rect + path during paint into a per-frame map the drop handler can query). Extract a PURE helper `fn drop_target_dir(hit: Option<(&Path, bool /*is_dir*/)>, root: &Path) -> PathBuf` and unit-test it.

### Task 5.2 — Copy the dropped files into the target dir
- Core helper in horizon-core (e.g. `fn copy_into_dir(srcs: &[PathBuf], dest_dir: &Path) -> Vec<Result<PathBuf, CopyError>>`): copy each file into dest; recurse for directories; handle name collisions (e.g. append " copy"/" (2)" — pick a rule, test it); never overwrite silently. No panics, strict-clippy clean. Unit-test the name-collision/resolve logic with a pure helper `fn unique_dest_name(dest_dir, file_name) -> PathBuf` or similar.
- After copy, refresh the affected folder's children in the tree so the new files appear (reuse the tree refresh path).

### Task 5.3 — Wire into file_drop.rs + hover highlight
- When non-editor OS files are dropped over a FileExplorer panel, route to 5.1+5.2 instead of (or in addition to) the terminal paste path. While files are HOVERED over the explorer, paint a highlight on the target folder row (use theme token).
- AC: dropping files from the OS file manager onto a folder row copies them in; original untouched; tree shows them; wrong-panel/terminal drop behavior unchanged; live screenshot.

### Task 5.4 — Ctrl+V to paste files from the system clipboard into a folder
- When the explorer is focused, Ctrl+V should COPY files referenced by the system clipboard into a target folder (same copy/collision logic as 5.2).
- TARGET (no selection model exists today): introduce a minimal "selected entry" on the explorer (clicking a row marks it selected, painted highlighted). Ctrl+V pastes into the selected folder (or the selected file's parent), falling back to the explorer root if nothing selected. This selection also lets "reveal-in-tree" (Task F) highlight, and the drop highlight, share one concept. Keep it small; pure helper `fn paste_target_dir(selected: Option<(&Path,bool)>, root:&Path)->PathBuf` unit-tested.
- ✅ SPIKE RESULT (2026-06-08): FEASIBLE, NO NEW DEP. `arboard 3.6.1` (already in the build) exposes a public cross-platform getter `arboard::Clipboard::new()?.get().file_list() -> Result<Vec<PathBuf>>` (lib.rs:205), backed by `text/uri-list` on both Wayland (`WlDataControl::get_file_list`) and X11 (`x11.get_file_list`). Env is Wayland (`wl-paste` + `xclip` present as backup). So Ctrl+V reads copied file paths directly via arboard — implement it.
- AC: with files copied in the OS file manager, Ctrl+V in the focused explorer copies them into the selected folder (if the platform spike succeeds); original untouched; tree refreshes. If the spike fails, report the limitation honestly and gate this task.

> Note: validate live; do NOT restart the user's running horizon instance (build + swap binary only).
> Story 5 contains a real platform risk (clipboard file-list on Linux). Drag-and-drop (5.1-5.3) is the reliable core; Ctrl+V (5.4) is best-effort pending the spike.

## Story 6 — Feature: restore running terminal session (e.g. `claude`) across app restart (added 2026-06-08)

**Reported:** if the app is closed while a terminal panel has a process running (e.g. `claude`), reopening the app does NOT restore/resume that session — the running program is gone.

**Status:** NOT YET SCOPED. Meatier than Stories 1-5; needs investigation + a scoping decision with the user before planning. Open questions:
- Does the fork already persist session state? There's `.horizon/sessions/<id>/runtime.yaml` and terminal `replay_bytes`/scrollback replay (per memory). Map what survives today (scrollback only? cwd? command?) vs what's lost (the live child process).
- Restoring a *running* foreign process across a real app exit is generally impossible unless the process survives the parent (detached) — true session persistence usually needs a tmux/abduco/dtach-style layer or a persistent backend the PTY reattaches to. Decide: (a) reattach to a still-alive backend (requires running shells under a persistence layer / not killing them on close), (b) re-run the last command on restore (re-launches `claude` fresh, losing in-memory state), or (c) just restore scrollback + cwd (what may already partially exist). Each is a very different scope.
- Clarify with user what "restore the claude session" must mean: the live conversation/process resumed, or just the panel/command/cwd re-created.

> Tackle AFTER Stories 1-5 land. Likely its own branch.

## Risks
1. Replicating WalkBuilder's exact gitignore semantics with a standalone matcher (nested `.gitignore`, parents, global) — mismatch could mis-tag entries. Mitigation: TDD against a temp tree with nested ignores.
2. Line-count cap (1000) on `file_tree_widget.rs` (already 886). Mitigation: search UI in a NEW module; extract from file_tree.rs if it grows.
3. Background thread cancellation/races. Mitigation: generation counter + drop receiver; test the dedup logic.
4. Shortcut conflict with terminal `ToggleSearch`. Mitigation: contextual dispatch by focused panel kind; regression test both branches.
5. `shortcut_inventory.rs` (Ctrl+Shift+G) vs `config.rs` (Ctrl+Shift+F) discrepancy for `search`. Mitigation: confirm runtime binding before wiring; keep both consistent.
6. Working on `main` — create feature branch first.
