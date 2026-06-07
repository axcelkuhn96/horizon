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
- [ ] Task 2 (core): expansion state for the changed-file filter tree in
      `file_tree.rs` — default collapsed at all depths, expand/collapse
      mutations following the `TreeAction` pattern, reset when
      `show_only_changes` is re-enabled. TDD: default-collapsed, expand,
      collapse, reset-on-retoggle, compaction regression.
- [ ] Task 3 (UI, domínio: frontend-rust-egui — N/A web rulebook): filter view
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
- Do not touch sidebar, panels, terminal_widget.
- No public horizon-core contract changes unless strictly needed.
- No new dependencies.
- Tests live in the same file under `#[cfg(test)]` (repo convention).
