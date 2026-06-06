# Story: Surface child process exit on non-SSH terminal panels ("ghost panel" fix)

**Status:** Approved (Ready for Dev)
**Epic:** v1 UX polish (feat/v1-navigation)
**Created:** 2026-06-06

## Story

As a Horizon user, when the child process of a Shell/Command/agent panel dies
(e.g. Ctrl+C kills the whole process group), I want the panel to clearly show
that the process exited and offer a one-keystroke restart, so I never type
into a dead pty without feedback.

**Real incident:** on 2026-06-06 11:38 a Ctrl+C killed claude+zsh+script of a
panel; Horizon kept rendering the last frame and silently forwarded keystrokes
to the dead pty.

## Context (verified 2026-06-06)

The core already detects death — only SSH panels consume it:

- `Event::Exit`/`Event::ChildExit` → `child_exited`/`child_exit_status`:
  `crates/horizon-core/src/terminal/events.rs:120-126`; getters
  `terminal/content.rs:170-179`.
- `Panel::process_output` consumes it only for `kind == Ssh`
  (`panel.rs:294-300` → `SshConnectionStatus::Disconnected`).
- Restart infra exists and works for all terminal-backed kinds:
  `Panel::restart()` (`panel.rs:452-527`), `Board::restart_panel`
  (`board.rs:237`), consumed by `apply_panel_transitions`
  (`horizon-ui/src/app/lifecycle.rs:261-269`).
- SSH reconnect UX to mirror: `SSH_RECONNECT_SHORTCUT = Ctrl/Cmd+Shift+R`
  (`terminal_widget/input.rs:18-19`), gate + request predicate
  (`input.rs:574-584`, `:656-673`, tests `:912+`), `reconnect_requested`
  plumbing (`terminal_widget/mod.rs:59,177`; `app/panels.rs:120,148,239-263,505-538`),
  titlebar badge (`app/panel_chrome.rs:486-531`, call site `:208-215`),
  sidebar button (`sidebar.rs:694-712`).

## Acceptance Criteria

1. A Shell panel whose child exits with code 3 shows a red `Exited (3)` badge
   in the titlebar within one frame of `process_output`.
2. A child killed by a signal shows `Exited (signal N)` (Unix); no status →
   `Exited`.
3. A non-modal footer banner "Process exited — Ctrl+Shift+R to restart" is
   visible over the dead panel body; it disappears after restart.
4. Ctrl+Shift+R on a dead non-SSH terminal panel restarts it: live shell,
   badge/banner gone, keyboard flows again.
5. Sidebar shows a working "Restart" button for Shell/Command panels.
6. Keyboard input is NOT forwarded to a dead panel's pty; mouse selection,
   scrollback and copy still work; the restart shortcut still works.
7. SSH flow unchanged — existing SSH tests pass without assert changes.
8. Editor/GitChanges/Usage panels unaffected.
9. `cargo test --workspace` green (301 baseline + new), `cargo clippy` clean,
   Windows compilation preserved (cfg-gate `ExitStatusExt`).

## Tasks

- [x] Task 1 (core): expose generic exited state on Panel (`process_exited()` /
      exit-status accessor), consumed from `child_exited`/`child_exit_status`
      for all terminal-backed non-SSH kinds. TDD: label formatting
      (code/signal/none) + predicate tests.
- [ ] Task 2 (UI, domínio: frontend-rust-egui — N/A web rulebook): titlebar
      badge `Exited (…)` mirroring `paint_ssh_status_badge` (PALETTE_RED).
- [ ] Task 3 (UI): footer banner "Process exited — Ctrl+Shift+R to restart"
      over dead panel body, theme-coherent, non-modal.
- [ ] Task 4 (input): generalize the reconnect-request predicate + keyboard
      gate to dead non-SSH terminal panels (TDD following `input.rs:912+`
      test patterns). Mouse/scrollback/copy untouched.
- [ ] Task 5 (sidebar): extend Restart button condition to Shell/Command.
- [ ] Task 6 (delivery): full gates → release build → install
      `~/.local/bin/horizon` → conventional commit → push branch → merge main
      → push main.

## Dev Agent Record

### File List
- `crates/horizon-core/src/panel.rs` — added `process_exit_label` free fn, `Panel::process_exited()`, `Panel::process_exit_label()`, and `mod process_exit_tests`
- `crates/horizon-core/src/terminal/lifecycle.rs` — gated `write_input` to no-op when `child_exited` is set (via `should_drop_input`)
- `crates/horizon-core/src/terminal.rs` — extracted `should_drop_input` predicate + tests covering the dead-pty `write_input` gate (drop on exit/empty, forward when alive)

### Notes
- Do NOT touch replay/transcript flow or spawn logic.
- Prefer new Panel methods over changing existing signatures.
- Restart is always an explicit user action (shortcut or button) — no
  auto-respawn.
