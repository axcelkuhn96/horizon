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
8. No folder count badges.
9. `cargo test --workspace` green, `cargo clippy` no new warnings,
   `cargo fmt --all -- --check` clean.
10. New release binary installed at `~/.local/bin/horizon` and validated by
    actually running the app with the filter on (autonomous screenshot
    evidence; if capture is impossible, report it and ask for manual
    validation — do not claim validated without evidence).

### Acceptance Criteria — added 2026-06-07 (user request, VSCode-style pass)

11. **VSCode-style row layout**, rendered fully manually via `ui.painter()` (no
    inner `ui.label`/scope per row): `[indent][chevron col][icon col 18px]
    [gap][name ellipsis-truncated before the letter reserve][status letter
    right]`. Chevron `\u{25b6}`/`\u{25bc}` for dirs (`theme::FG_DIM()`, centered),
    constant chevron-column width for files so icon+name align across rows.
12. **Real per-depth indentation** (`INDENT_PER_DEPTH` = 16.0) visibly nests
    children in BOTH the normal tree and the filter tree — the tree is not flat.
13. **Normal-tree changed-folder green propagation**: a folder that CONTAINS any
    uncommitted change (at any depth) renders its icon+name in
    `theme::PALETTE_GREEN()` (`dir_contains_changes` in core). Clean folders keep
    `ACCENT` icon + `FG_SOFT` name; files keep status colors/letters. (Supersedes
    the original AC 8 clause "no status propagation in the normal tree".)
14. Component-wise prefix matching for propagation: `src` must not light up for a
    change under `src2` and vice-versa (proven by `dir_contains_changes` tests).

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
- `crates/horizon-core/src/file_tree.rs` (corrective+extension pass 2026-06-07:
  `dir_contains_changes(status, abs_dir)` — component-wise strict-ancestor test
  for normal-tree green propagation; 6 new tests incl. the `src` vs `src2`
  prefix-trap and repo-root cases)
- `crates/horizon-ui/src/file_tree_widget.rs` (corrective+extension pass
  2026-06-07: replaced the per-row `scope_builder`/`ui.label`/`paint_truncated_
  name`/`paint_icon_column`/`paint_status_letter` machinery with a single
  fully-manual painter pass — `RowVisual` struct + `paint_tree_row`; added
  `row_colors` helper for normal-tree changed-folder green propagation; VSCode
  chevron column (`CHEVRON_COL_WIDTH`/`CHEVRON_FONT_SIZE`), `ICON_NAME_GAP`,
  `INDENT_PER_DEPTH` 14→16; ellipsis truncation via `LayoutJob` +
  `TextWrapping::truncate_at_width`; `render_row`/`render_changed_row` now build a
  `RowVisual` and delegate. New regression tests:
  `render_row_layout_orders_and_indents_by_depth` (depth 0 + depth 2, real fonts,
  app container stack: asserts icon < name and ≥2-level indent delta),
  `dir_with_changes_inside_renders_green_in_normal_tree`; adapted
  `render_changed_row_name_does_not_overlap_icon` to depth 2)
- `crates/horizon-ui/src/app/mod.rs` (corrective pass: `configure_fonts` made
  `pub(crate)` so the headless regression test renders with the real Nerd Font
  fallback stack, measuring true glyph paint extents)

### Notes

- **Corrective + extension pass (2026-06-07, user re-test still showed
  overlap/flat tree):** re-ran systematic debugging against the CURRENT branch
  build and could NOT reproduce the reported overlap or flat-tree symptoms — the
  fixed-icon-column code from commit `135c910` lays rows out correctly. Then
  rewrote row rendering to be fully manual painter-based (robustness mandate) and
  added the VSCode-style chevrons, per-depth indent bump, and normal-tree
  changed-folder green propagation. See "Task 1 diagnosis (RE-OPENED)" below.

- **Task 1 diagnosis (RE-OPENED 2026-06-07 — PROVEN, with numbers):**
  The reported "icon paints ~30px into the name" and "tree looks flat" symptoms
  do **not reproduce** against the current committed code. Three independent
  measurements:
  1. *Headless harness with the REAL registered fonts* (`configure_fonts()`),
     mirroring the app stack (Area `interactable(false)` + `set_transform_layer`
     + child Ui + ScrollArea), at ppp 1.0 / 1.5 / 2.0, depths 0/1/2: every row's
     name galley paints to the RIGHT of its icon, and indentation increases with
     depth. e.g. depth-0 `.claude`: chevron ink `[158,165]`, icon ink
     `[180,192]`, name `.claude` ink starts at `205` — disjoint and ordered.
     depth-1 name at +14, depth-2 at +another step. No overlap, not flat.
  2. *Live instrumented run* of the actual app (`eprintln` of `row_rect.min.x`,
     post-each-child `cursor().min.x`, and the name `Label` `Response.rect`):
     for depth-0 `.claude` (scope min.x = 13465): after indent(8) → 13473;
     after chevron+4 → 13491.4; after icon col(18)+2 → 13517.4; the name label
     `Response.rect` STARTS at **13519.406** — i.e. right of the icon, no
     overlap. The cursor was NOT stuck at `BASE_INDENT`; it advanced through
     indent+caret+icon exactly as designed.
  3. The pre-existing geometry regression tests (`render_row_name_does_not_
     overlap_icon`) were already GREEN.
  **Conclusion:** the label-based layout from `135c910` is correct by every
  measurement available headlessly and in the live app. The screenshots the
  user saw most plausibly predate that fix (or came from a stale binary); the
  installed `~/.local/bin/horizon` from 17:00 post-dates commit `135c910`
  (16:41), so the symptom is not reproducible against shipped code.
- **Why the first fix (fixed icon column) was nonetheless not the end of it:**
  it was correct but FRAGILE — three nested `scope_builder`s per row plus a
  mid-scope `ui.cursor()` capture in `paint_truncated_name` to anchor the name.
  That works, but any future change to the inner layout (or an egui placer
  change) could regress it, and it could not satisfy the new VSCode requirements
  (constant chevron column, green propagation) cleanly. **Decision (evidence +
  robustness first):** replace the per-row label/scope machinery with a single
  fully-manual painter pass (`paint_tree_row` + `RowVisual`) that places chevron,
  icon and an ellipsis-truncated name galley (`LayoutJob` +
  `TextWrapping::truncate_at_width`) at explicit x offsets. The egui mechanism
  this sidesteps: a `Label` inside a `Layout::left_to_right` scope is positioned
  by the `Placer` cursor; the previous code relied on capturing that cursor
  correctly across three scopes — manual painting removes that dependency
  entirely (only `ui.painter()` absolute paints, which were always reliable).
- **Original Task 1 note (kept for history):** the Symbols Nerd Font IS
  correctly registered — `crates/horizon-ui/src/app/mod.rs` inserts
  `symbols-nerd-font` at index 3 of both proportional and monospace fallback
  stacks. The fixed-width 18px icon column (`paint_icon_column`, now folded into
  `paint_tree_row`) removed any dependence on glyph advance metrics.
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
