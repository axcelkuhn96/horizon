# Story: File Explorer Panel (VSCode-style file tree)

**Status:** Draft (awaiting implementation approval)
**Branch:** feat/v1-navigation
**Origin:** /piloto → refinador → executor-bmad-superpowers (2026-06-06)

## Story

As a Horizon user, I want a **File Explorer panel** that shows my workspace's
project files as a VSCode-style tree with per-filetype icons and live git status
decorations, so I can visually browse the project and see what changed without
leaving Horizon — and open any file in VS Code with a double-click.

## Acceptance Criteria

1. A `FileExplorer` panel can be created from the UI (command palette preset /
   `ALL_KINDS` dropdown) and renders inside a workspace.
2. The tree lists files/folders from the panel/workspace `cwd`, each with a
   per-filetype icon drawn from a real **icon font** (not emoji), visually close
   to VSCode.
3. Folders expand/collapse; directory contents are read **lazily** (only when a
   folder is expanded). `.git`, `node_modules`, `target` and `.gitignore`d
   entries do not pollute the tree.
4. Each file shows git status in the VSCode scheme: untracked (green, `U`),
   added (green, `A`), modified (yellow, `M`), deleted (red, `D`), clean
   (neutral). Decorations update live via the existing `GitWatcher`.
5. Double-clicking a file opens it in external VS Code (`code <abs_path>`). If
   `code` is not on `PATH`, a non-fatal footer warning shows and the app does
   not crash.
6. A refresh button re-scans the currently expanded tree.
7. `cargo build`, `cargo test`, `cargo clippy --all-targets` (no new warnings),
   `cargo fmt --check`, and `./scripts/check-maintainability.sh` all pass.
8. Live screenshot attached proving icons + git decorations + double-click.

## Out of scope

Editing/creating/renaming/deleting files, drag-and-drop, search/filter,
multi-root, automatic filesystem watching (manual refresh only in v1).

## Plan

See `docs/superpowers/plans/2026-06-06-file-explorer-panel.md`.
