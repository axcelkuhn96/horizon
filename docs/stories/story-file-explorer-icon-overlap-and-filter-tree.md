# Story: File Explorer — fix icon/text overlap and make git filter a collapsible green tree

**Status:** Draft (awaiting implementation approval)
**Epic:** v1 UX polish
**Branch:** fix/file-explorer-icons-filter-tree
**Created:** 2026-06-07
**Origin:** /piloto → refinador → executor-bmad-superpowers (regression report with screenshots after commit 3796526)

## Story

As a Horizon user browsing the Files panel, I want file/folder icons to never
paint over the row's filename, and — when I enable the uncommitted-files git
filter — I want a clean tree of **collapsed green folders** that I expand
manually to drill into the changed files, so the panel is readable and I
control how much detail I see.

**Reported regression (2026-06-07, screenshots):** after commit `3796526`
("group uncommitted-files filter into folders and truncate long names") the
icon glyph is drawn on top of the middle of the name (e.g. `SKILL.📄d`,
`custom📄ze.toml`) in BOTH views, and the filter view comes fully expanded
with every file listed.

## Context (verified 2026-06-07 via Explore)

- Rendering: `crates/horizon-ui/src/file_tree_widget.rs` (~405 lines)
  - `render_row()` :308-378 — normal-tree row: row rect allocated
    (`ROW_HEIGHT 22.0`), left-to-right scope: indent (`BASE_INDENT 8.0` +
    `depth * INDENT_PER_DEPTH 14.0`) → caret 7pt + 4px (dirs) / 11px space
    (files) → **icon via `ui.label()` 13pt** → 6px gap → `set_max_width(row
    width - LETTER_RESERVE 26.0 | PLAIN_RESERVE 10.0)` → truncated label.
  - `render_changed_row()` :414-470 — filter row, same icon/label approach,
    **no carets, always expanded**.
  - `paint_status_letter()` :474-481 — status letter painted absolutely at
    `row_rect.max.x - 12.0` (outside the layout) — this part is fine.
  - Icon overlap hypothesis (to confirm before fixing): the Nerd Font icon
    glyph's advance width in egui doesn't match its painted extent, so the
    following label starts under the glyph. Robust fix: reserve a FIXED width
    for the icon (manual rect / `allocate_exact_size`) instead of relying on
    glyph advance.
- Tree state/build: `crates/horizon-core/src/file_tree.rs` (~480 lines)
  - `FileTreeState { root, roots, loaded, git_status, code_missing,
    show_only_changes }`
  - `ChangedTreeNode { name, abs_path, is_dir, status, children }` built by
    `changed_file_tree()` :132-174, VSCode-style single-child compaction
    (`a/b/c` one row). No expansion state today.
  - Existing pattern: render collects `TreeAction` (Expand/Collapse/Open)
    immutably, applied after render with mutable borrow — REUSE for filter.
- Folder status propagation: does not exist anywhere today (folders always
  `theme::ACCENT()`); green folder treatment applies ONLY to the filter view.
- Colors: use the theme's existing untracked/added green (same one used by
  status letters) — no new hardcoded colors.
- Existing tests to keep green: ~11 in `file_tree.rs` (scan, gitignore,
  `changed_file_tree_*` grouping/compaction) + 5 in `file_tree_widget.rs`
  (icons, status letters).
- Delivery facts: binary installed at `~/.local/bin/horizon`; release build
  ~2m30s; push ONLY to remote `fork` (origin is third-party upstream); rustfmt
  is a hard gate in this repo; 6 pre-existing clippy f32 warnings in
  `sidebar/auto_hide.rs` are NOT ours.

## Acceptance Criteria

1. No icon glyph overlaps any filename text in the normal tree NOR in the
   filter view (evidence: real-app screenshot + geometry unit test where
   extractable). Row layout: indent → caret (dirs) → icon → gap → truncated
   name with ellipsis → status letter right-aligned.
2. With the git filter ON, every folder (all depths, root included) renders
   COLLAPSED by default.
3. Filter folders render icon AND name in the theme's untracked/added green;
   they have an expand/collapse caret matching the normal tree's.
4. Files inside keep their individual status letter and color (U/M/D…).
5. VSCode-style single-child compaction is preserved (existing
   `changed_file_tree_*` tests stay green).
6. Expansion state is remembered while the filter stays ON; toggling the
   filter OFF and ON again resets everything to collapsed.
7. Normal tree (filter OFF) behavior unchanged except the icon fix.
8. No folder count badges; no status propagation in the normal tree.
9. `cargo test --workspace` green, `cargo clippy` no new warnings,
   `cargo fmt --all -- --check` clean.
10. New release binary installed at `~/.local/bin/horizon` and validated by
    actually running the app with the filter on (autonomous screenshot
    evidence; if capture is impossible, report it and ask for manual
    validation — do not claim validated without evidence).

## Tasks

- [x] Task 1 (UI, domínio: frontend-rust-egui — N/A web rulebook): diagnose
      the icon/text overlap with systematic debugging (reproduce the geometry,
      confirm root cause — do NOT guess), then fix both `render_row` and
      `render_changed_row` by reserving fixed icon width. Extract a testable
      geometry helper if needed; TDD where testable without GPU.
- [x] Task 2 (core): expansion state for the changed-file filter tree in
      `file_tree.rs` — default collapsed at all depths, expand/collapse
      mutations following the `TreeAction` pattern, reset when
      `show_only_changes` is re-enabled. TDD: default-collapsed, expand,
      collapse, reset-on-retoggle, compaction regression.
- [x] Task 3 (UI, domínio: frontend-rust-egui — N/A web rulebook): filter view
      renders as a collapsible tree — carets like the normal tree, folders
      icon+name in theme green, children only when expanded, files keep
      status letters. Collect actions immutably, apply after render.
- [ ] Task 4 (delivery): full gates (test/clippy/fmt) → release build →
      install `~/.local/bin/horizon` → open Horizon on a repo with
      uncommitted changes → verify AC 1-4, 6 visually with screenshots →
      conventional commit(s) → push to `fork`.

## Dev Agent Record

### File List
- `crates/horizon-ui/src/file_tree_widget.rs` (Task 1: ICON_COL_WIDTH constant,
  `paint_icon_column` helper, used in `render_row` and `render_changed_row`)
- `crates/horizon-core/src/file_tree.rs` (Task 2: `FileTreeState.changed_expanded`
  `HashSet<PathBuf>` + `set_show_only_changes` / `is_changed_expanded` /
  `expand_changed` / `collapse_changed`; collapsed-by-default filter-tree
  expansion state, cleared whenever the filter toggle changes value)
- `crates/horizon-ui/src/file_tree_widget.rs` (Task 3: `TreeAction::ExpandChanged`
  / `CollapseChanged` variants; `changed_dir_visuals` caret/folder-glyph helper;
  collapsible green filter tree — `render_changes_only` / `render_changed_nodes` /
  `render_changed_row` take expansion state, render dirs collapsed-by-default with
  a normal-tree caret and green icon+name, recurse only when expanded, dirs toggle
  on single click and files open on double click; `show()` wired via
  `set_show_only_changes` + `state.changed_expanded` + Expand/CollapseChanged match
  arms; `paint_icon_column` uses `Sense::empty()` so the icon strip never steals
  the row hover tint — Task 1 QA polish)

### Notes
- **Task 1 diagnosis (root cause, verified):** the Symbols Nerd Font IS
  correctly registered — `crates/horizon-ui/src/app/mod.rs:502-518` inserts
  `symbols-nerd-font` (`assets/fonts/SymbolsNerdFont-Regular.ttf`) at index 3
  of both proportional and monospace fallback stacks, ahead of egui's bundled
  defaults (unit tests at mod.rs:625-649 assert this). So the overlap is the
  glyph-metrics case: the PUA glyph's painted extent is wider than its font
  advance, so `ui.label(icon)` allocates too little width and the filename
  label starts under the glyph. Fix: icons are now painted centered inside a
  fixed-width 18px column (`paint_icon_column`, clipped to the column rect),
  removing any dependence on glyph advance metrics. Applied in both
  `render_row` (normal tree) and `render_changed_row` (filter view).
- Task 1 gates: `cargo test -p horizon-ui` 315 passed / 0 failed;
  `cargo clippy -p horizon-ui --all-targets` clean;
  `cargo fmt --all -- --check` clean.
- **Task 2 (core expansion state, TDD):** added `changed_expanded:
  HashSet<PathBuf>` to `FileTreeState` (Clone/Debug compatible) keyed by
  `ChangedTreeNode.abs_path` (works with compacted nodes). Empty = fully
  collapsed (default). `set_show_only_changes(on)` clears the set only when the
  value actually changes (re-asserting the same value every frame is a no-op,
  per AC 6); `is_changed_expanded` / `expand_changed` / `collapse_changed`
  drive per-dir state. Only additive public API. RED evidence: `cargo test -p
  horizon-core file_tree` gave E0599 "no method named is_changed_expanded /
  expand_changed / collapse_changed / set_show_only_changes". GREEN: 14
  file_tree tests pass (3 new + 11 pre-existing grouping/compaction
  regressions, AC 5), full `cargo test -p horizon-core` 330 passed / 0 failed.
- Task 2 gates: `cargo clippy -p horizon-core --all-targets` clean;
  `cargo fmt --all -- --check` clean.
- **Task 3 (UI collapsible green filter tree, TDD):** RED — `cargo test -p
  horizon-ui changed_dir_visuals` gave E0425 "cannot find function
  `changed_dir_visuals` in this scope" (x2). GREEN — new test passes; the filter
  view now renders folders COLLAPSED by default at every depth with a normal-tree
  caret (`\u{25b6}`/`\u{25bc}`) and a green folder icon+name (`theme::PALETTE_GREEN`,
  the U/A status green), children render only when expanded (single click toggles
  via Expand/CollapseChanged actions applied after render), files unchanged
  (status letter/color, double-click opens). `show()` now goes through
  `set_show_only_changes` (AC 6 reset guard) instead of the direct field write.
  Normal tree (`render_nodes`/`render_row`) untouched; green confined to the
  filter view.
- Task 3 gates: `cargo test -p horizon-ui -p horizon-core` 316 (horizon-ui, +1
  new) / 330 (horizon-core) passed, 0 failed; `cargo clippy --workspace
  --all-targets` clean (no warnings); `cargo fmt --all -- --check` clean.
- Do not touch sidebar, panels, terminal_widget.
- No public horizon-core contract changes unless strictly needed.
- No new dependencies.
- Tests live in the same file under `#[cfg(test)]` (repo convention).
