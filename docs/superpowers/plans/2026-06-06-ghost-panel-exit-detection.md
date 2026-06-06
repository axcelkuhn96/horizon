# Ghost Panel Exit Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface child-process death on non-SSH terminal panels (badge + banner + input gate + one-keystroke restart) so users never type into a dead pty without feedback.

**Architecture:** The core already tracks `child_exited`/`child_exit_status` (alacritty `Event::ChildExit` → `terminal/events.rs:120-126`); only SSH panels consume it. We mirror the existing SSH-disconnected pattern: a generic `Panel::process_exited()` predicate feeds a titlebar badge, a footer banner, a keyboard gate, and the existing `reconnect_requested → Board::restart_panel` plumbing — reusing `SSH_RECONNECT_SHORTCUT` (Ctrl/Cmd+Shift+R). SSH behavior is untouched.

**Tech Stack:** Rust workspace (crates `horizon-core`, `horizon-ui`), egui, alacritty_terminal. Tests co-located in `mod tests`. Branch: `feat/v1-navigation` (NO worktree).

**Story:** `docs/stories/story-ghost-panel-exit-detection.md`

---

## Reference — verified call chain (2026-06-06)

- Detection: `crates/horizon-core/src/terminal/events.rs:120-126`; getters `terminal/content.rs:170-179` (`child_exited() -> bool`, `child_exit_status() -> Option<std::process::ExitStatus>`).
- `Panel`: struct `panel.rs:153-187`; `ssh_status()` accessor `:208`; `process_output` SSH consumption `:294-300`; `child_exited()` `:346-348`; `write_input` `:371-375`; `restart()` `:452-527` (already respawns Shell/Command/agents; resets `ssh_status`).
- Input: `crates/horizon-ui/src/terminal_widget/input.rs` — `SSH_RECONNECT_SHORTCUT` `:18-19`; `handle_terminal_keyboard_input` `:574+` (SSH gate `:582-584`); `disconnected_ssh_reconnect_requested` `:656-673`; tests `:909+` (helper `key_event` near `:1169`).
- Plumbing: `terminal_widget/mod.rs:55-63` (`TerminalKeyboardContext`), `:176-185` (reconnect bit); `app/panels.rs:239-265` and `:505-541` (two call sites pushing to `panels_to_restart`); `app/lifecycle.rs:261-269` (`apply_panel_transitions` → `board.restart_panel`).
- Chrome: snapshot struct `app/panels.rs:~25-44` (field `ssh_status: Option<SshConnectionStatus>` at `:42`, built at `:369` via `panel.ssh_status()`, consumed at `:480`); `PanelChrome` struct `app/panel_chrome.rs:9-28`; badge call site `:204-216`; `paint_ssh_status_badge` `:486-531`.
- Sidebar: restart/reconnect button `app/sidebar.rs:693-712`.
- `Terminal::write_input`: `crates/horizon-core/src/terminal/lifecycle.rs:92-101`.

**Hard constraints:** SSH flow unchanged (existing tests must pass without assert edits). No auto-respawn. No persistence of exited state. Don't touch replay/transcript/spawn flow. Windows must keep compiling (`ExitStatusExt` is Unix-only — cfg-gate). UI text in English. Conventional commits.

---

### Task 1: Core — generic exited state + label on Panel

**Files:**
- Modify: `crates/horizon-core/src/panel.rs` (new methods near `child_exited()` at `:346`; tests in existing `mod tests` at the bottom of the file — if the file has none, add `#[cfg(test)] mod tests` at the end)
- Modify: `crates/horizon-core/src/terminal/lifecycle.rs:92-101` (gate `write_input`)

- [ ] **Step 1: Write the failing tests** (in `panel.rs` tests module; these are pure-function tests, no PTY spawn)

```rust
#[cfg(test)]
mod process_exit_tests {
    use super::process_exit_label;

    #[test]
    #[cfg(unix)]
    fn exit_code_is_formatted_into_label() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(3 << 8);
        assert_eq!(process_exit_label(Some(status)), "Exited (3)");
    }

    #[test]
    #[cfg(unix)]
    fn signal_death_is_formatted_into_label() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(2);
        assert_eq!(process_exit_label(Some(status)), "Exited (signal 2)");
    }

    #[test]
    fn missing_status_falls_back_to_plain_label() {
        assert_eq!(process_exit_label(None), "Exited");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p horizon-core process_exit -- --nocapture`
Expected: compile error "cannot find function `process_exit_label`" (that's the failing state for a missing unit).

- [ ] **Step 3: Implement `process_exit_label` + Panel methods**

Add a free function in `panel.rs` (near the bottom, before tests):

```rust
/// Human-readable label for a dead child process, e.g. `Exited (3)`,
/// `Exited (signal 2)` (Unix), or `Exited` when no status is available.
#[must_use]
pub fn process_exit_label(status: Option<std::process::ExitStatus>) -> String {
    let Some(status) = status else {
        return "Exited".to_string();
    };
    if let Some(code) = status.code() {
        return format!("Exited ({code})");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("Exited (signal {signal})");
        }
    }
    "Exited".to_string()
}
```

Add methods on `impl Panel`, right after `child_exited()` (`panel.rs:346-348`):

```rust
    /// `true` when this panel's child process died and the panel is NOT an SSH
    /// panel (SSH has its own `SshConnectionStatus::Disconnected` flow).
    /// Editor/GitChanges/Usage panels have no terminal, so this is `false`.
    #[must_use]
    pub fn process_exited(&self) -> bool {
        self.kind != PanelKind::Ssh && self.child_exited()
    }

    /// UI label for the dead child process (badge text). `None` while alive
    /// or for SSH panels.
    #[must_use]
    pub fn process_exit_label(&self) -> Option<String> {
        self.process_exited().then(|| {
            process_exit_label(self.content.terminal().and_then(Terminal::child_exit_status))
        })
    }
```

NOTE: check `Terminal::child_exit_status` signature in `terminal/content.rs:178` — it takes `&self` and returns `Option<ExitStatus>` (Copy), so `.and_then(Terminal::child_exit_status)` works on `Option<&Terminal>` via `.and_then(|t| t.child_exit_status())`. Use the closure form if the method-path form doesn't satisfy the borrow checker. Export `process_exit_label` from the crate if `horizon-ui` needs it (check `lib.rs` re-exports — `Panel`, `PanelKind` etc. are re-exported there; add `process_exit_label` only if a UI task imports it; current plan keeps label-building inside `Panel`, so no re-export needed).

- [ ] **Step 4: Gate `Terminal::write_input` on dead child** (single choke point: keyboard, paste, context menu, IME all funnel here)

In `crates/horizon-core/src/terminal/lifecycle.rs:92-101`, change:

```rust
    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
```

to:

```rust
    pub fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() || self.child_exited {
            // Writing to a dead pty is a silent no-op that misleads the user —
            // drop the bytes; restart spawns a fresh Terminal with a live pty.
            return;
        }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p horizon-core process_exit -- --nocapture`
Expected: `test result: ok. 3 passed` (2 on non-Unix).
Also run: `cargo test -p horizon-core` — full crate green (no SSH/terminal regressions; the `write_input` gate must not break `shutdown_with_timeout_waits_for_pty_exit` and friends).

- [ ] **Step 6: Commit**

```bash
git add crates/horizon-core/src/panel.rs crates/horizon-core/src/terminal/lifecycle.rs
git commit -m "feat(core): expose process_exited state on panels and gate dead-pty writes"
```

---

### Task 2: UI — "Exited" titlebar badge

**Files:**
- Modify: `crates/horizon-ui/src/app/panels.rs` (snapshot struct field `:42` area, builder `:369` area, chrome construction `:480` area)
- Modify: `crates/horizon-ui/src/app/panel_chrome.rs` (`PanelChrome` struct `:9-28`, badge call site `:204-216`, new paint fn after `paint_ssh_status_badge` `:531`)

- [ ] **Step 1: Add the label to the snapshot + chrome structs**

In `app/panels.rs`, in the snapshot struct (the one holding `ssh_status: Option<SshConnectionStatus>` at `:42`), add below it:

```rust
    process_exit_label: Option<String>,
```

Where the snapshot is built (`:369`, `ssh_status: panel.ssh_status(),`), add below it:

```rust
                process_exit_label: panel.process_exit_label(),
```

Where `PanelChrome` is constructed (`:480`, `ssh_status: snapshot.ssh_status,`), add below it:

```rust
                        process_exit_label: snapshot.process_exit_label.as_deref(),
```

In `app/panel_chrome.rs`, `PanelChrome` struct (`:9-28`), add after `ssh_status`:

```rust
    pub process_exit_label: Option<&'a str>,
```

(`Option<&str>` is `Copy`, so the existing `#[derive(Clone, Copy)]` stays valid.)

IMPORTANT: `PanelChrome` may be constructed in more than one place (fullscreen path) — run `cargo build -p horizon-ui` and fix every missing-field error by passing `process_exit_label: None` ONLY where no `Panel` is available; where a `panel` is in scope, pass the real `panel.process_exit_label()` (own it in a local before building the chrome if lifetimes demand).

- [ ] **Step 2: Paint the badge**

In `app/panel_chrome.rs`, at the badge call site (inside `if !chrome.collapsed`, after the `ssh_status` block at `:208-216`), add:

```rust
        if let Some(label) = chrome.process_exit_label {
            paint_process_exited_badge(
                &painter,
                chrome.titlebar_rect,
                chrome.close_rect,
                chrome.scrollback_limit > 0,
                label,
            );
        }
```

Add the paint fn right after `paint_ssh_status_badge` (`:531`), mirroring its geometry exactly:

```rust
#[profiling::function]
fn paint_process_exited_badge(
    painter: &egui::Painter,
    titlebar_rect: Rect,
    close_rect: Rect,
    has_history_meter: bool,
    label: &str,
) {
    let color = theme::PALETTE_RED();
    let font = egui::FontId::proportional(10.0);
    let badge_right = if has_history_meter {
        panel_history_badge_rect(titlebar_rect, close_rect).min.x - 6.0
    } else {
        close_rect.min.x - 8.0
    };
    let text_width = painter
        .layout_no_wrap(label.to_string(), font.clone(), color)
        .size()
        .x;
    let badge_width = text_width + 16.0;
    let badge_height = 18.0;
    let badge_left = (badge_right - badge_width).max(titlebar_rect.min.x + 60.0);
    let badge_rect = Rect::from_min_size(
        Pos2::new(badge_left, titlebar_rect.center().y - badge_height * 0.5),
        Vec2::new(badge_right - badge_left, badge_height),
    );

    painter.rect_filled(
        badge_rect,
        CornerRadius::same(4),
        Color32::from_rgba_unmultiplied(color.r() / 6, color.g() / 6, color.b() / 6, 72),
    );
    painter.rect_stroke(
        badge_rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme::alpha(color, 140)),
        StrokeKind::Inside,
    );
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        color,
    );
}
```

- [ ] **Step 3: Build to verify**

Run: `cargo build -p horizon-ui`
Expected: green (fix any missed `PanelChrome` construction sites per Step 1 note).

Run: `cargo test -p horizon-ui`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/horizon-ui/src/app/panels.rs crates/horizon-ui/src/app/panel_chrome.rs
git commit -m "feat(ui): red 'Exited' titlebar badge for dead non-SSH terminal panels"
```

---

### Task 3: UI — footer banner over the dead panel body

**Files:**
- Modify: `crates/horizon-ui/src/terminal_widget/mod.rs` (in `TerminalView::show`, after the grid render block ending `:174`; new helper fn at file scope)

- [ ] **Step 1: Render the banner when the process is dead**

In `TerminalView::show`, right after the grid-render `if` block (the one ending with `self.grid_cache = grid_cache;` at `:173-174`) and BEFORE the keyboard-input block at `:176`, add:

```rust
        if self.panel.process_exited() && ui.is_rect_visible(interaction.layout.body) {
            render_process_exited_banner(ui, interaction.layout.body);
        }
```

Add the helper at file scope (near the other free fns in `terminal_widget/mod.rs`):

```rust
/// Non-modal footer strip painted over a dead panel's body so the exited
/// state is obvious at a glance (the titlebar badge alone is easy to miss).
fn render_process_exited_banner(ui: &egui::Ui, body: egui::Rect) {
    use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke};

    const BANNER_HEIGHT: f32 = 24.0;
    let banner = Rect::from_min_max(
        Pos2::new(body.min.x, (body.max.y - BANNER_HEIGHT).max(body.min.y)),
        body.max,
    );
    let painter = ui.painter().with_clip_rect(body);
    let red = crate::theme::PALETTE_RED();
    painter.rect_filled(
        banner,
        CornerRadius::ZERO,
        Color32::from_rgba_unmultiplied(red.r() / 5, red.g() / 5, red.b() / 5, 230),
    );
    painter.line_segment(
        [banner.left_top(), banner.right_top()],
        Stroke::new(1.0, crate::theme::alpha(red, 160)),
    );
    painter.text(
        banner.center(),
        Align2::CENTER_CENTER,
        "Process exited — Ctrl+Shift+R to restart",
        FontId::proportional(11.0),
        crate::theme::alpha(crate::theme::FG(), 230),
    );
}
```

NOTE: check how `theme` is imported in this file (`crate::theme` vs a `use` — grep `theme::` in `terminal_widget/mod.rs` and follow the existing style). On macOS the shortcut renders as Cmd — if the codebase has a helper that formats shortcut labels per-platform (grep `paste_shortcut_label` / `copy_selection_shortcut_label` in `app/shortcut_inventory.rs`), reuse it; otherwise keep the literal "Ctrl+Shift+R" (Linux-first fork) and note it in the story's Dev Agent Record.

- [ ] **Step 2: Build + test**

Run: `cargo build -p horizon-ui && cargo test -p horizon-ui`
Expected: green.

- [ ] **Step 3: Commit**

```bash
git add crates/horizon-ui/src/terminal_widget/mod.rs
git commit -m "feat(ui): footer banner with restart hint on dead terminal panels"
```

---

### Task 4: Input — keyboard gate + Ctrl+Shift+R restart for dead panels

**Files:**
- Modify: `crates/horizon-ui/src/terminal_widget/input.rs` (handler `:574-588`, predicates `:656-673`, tests in `mod tests` `:909+`)

- [ ] **Step 1: Write the failing tests** (in `mod tests`, alongside the SSH ones at `:1106-1167`; reuse the existing `key_event` helper)

```rust
    #[test]
    fn dead_shell_panels_request_restart_from_local_shortcut() {
        assert!(exited_panel_restart_requested(
            true,
            &[key_event(
                Key::R,
                Some(Key::R),
                None,
                true,
                false,
                Modifiers::COMMAND | Modifiers::SHIFT,
            )],
        ));
    }

    #[test]
    fn live_panels_ignore_local_restart_shortcut() {
        assert!(!exited_panel_restart_requested(
            false,
            &[key_event(
                Key::R,
                Some(Key::R),
                None,
                true,
                false,
                Modifiers::COMMAND | Modifiers::SHIFT,
            )],
        ));
    }

    #[test]
    fn repeated_restart_shortcut_does_not_queue_another_restart() {
        assert!(!exited_panel_restart_requested(
            true,
            &[key_event(
                Key::R,
                Some(Key::R),
                None,
                true,
                true,
                Modifiers::COMMAND | Modifiers::SHIFT,
            )],
        ));
    }

    #[test]
    fn plain_typing_on_dead_panel_does_not_request_restart() {
        assert!(!exited_panel_restart_requested(
            true,
            &[key_event(Key::A, Some(Key::A), Some("a"), true, false, Modifiers::NONE)],
        ));
    }
```

Add `exited_panel_restart_requested` to the `use super::{...}` list at the top of `mod tests` (`:911-917`).
NOTE: confirm the exact `key_event` helper signature near `:1169+` before writing — match the argument order used by the existing SSH tests.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p horizon-ui exited_panel -- --nocapture`
Expected: compile error "cannot find function `exited_panel_restart_requested`".

- [ ] **Step 3: Implement — extract shared shortcut check + new predicate + handler gate**

In `input.rs`, refactor `disconnected_ssh_reconnect_requested` (`:656-673`) to share the key-matching body:

```rust
fn restart_shortcut_pressed(events: &[TerminalInputEvent]) -> bool {
    events.iter().any(|input_event| {
        matches!(
            &input_event.event,
            egui::Event::Key {
                pressed: true,
                repeat: false,
                ..
            }
        ) && shortcut_event_matches(&input_event.event, SSH_RECONNECT_SHORTCUT)
    })
}

fn disconnected_ssh_reconnect_requested(
    kind: PanelKind,
    ssh_status: Option<SshConnectionStatus>,
    events: &[TerminalInputEvent],
) -> bool {
    kind == PanelKind::Ssh
        && matches!(ssh_status, Some(SshConnectionStatus::Disconnected))
        && restart_shortcut_pressed(events)
}

/// Dead non-SSH terminal panel + local Ctrl/Cmd+Shift+R → restart request.
fn exited_panel_restart_requested(process_exited: bool, events: &[TerminalInputEvent]) -> bool {
    process_exited && restart_shortcut_pressed(events)
}
```

In `handle_terminal_keyboard_input` (`:574+`), after the SSH gate (`:582-584`), add:

```rust
    if panel.process_exited() {
        // The pty is dead: swallow all keyboard input instead of forwarding it
        // into the void. Only the restart shortcut acts (when not shadowed by
        // a conflicting global shortcut).
        return local_ssh_reconnect_enabled && exited_panel_restart_requested(true, events);
    }
```

(The `Terminal::write_input` gate from Task 1 is defense-in-depth for paste/context-menu paths; this early return gives the shortcut its restart semantics and prevents IME/clipboard side effects.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p horizon-ui -- input`
Expected: new tests pass AND the four existing SSH reconnect tests (`disconnected_ssh_panels_request_reconnect_from_local_shortcut`, `connected_ssh_panels_ignore_local_reconnect_shortcut`, `non_ssh_panels_ignore_local_reconnect_shortcut`, `repeated_reconnect_shortcut_does_not_queue_another_restart`) pass UNCHANGED.

- [ ] **Step 5: Commit**

```bash
git add crates/horizon-ui/src/terminal_widget/input.rs
git commit -m "feat(input): gate keyboard on dead panels and wire Ctrl+Shift+R restart"
```

---

### Task 5: Sidebar — Restart button for Shell/Command

**Files:**
- Modify: `crates/horizon-ui/src/app/sidebar.rs:693-712`

- [ ] **Step 1: Extend the button condition**

Change the condition at `:694` from:

```rust
            if (kind.is_agent() || kind == horizon_core::PanelKind::Ssh)
```

to:

```rust
            if (kind.is_agent()
                || matches!(
                    kind,
                    horizon_core::PanelKind::Ssh | horizon_core::PanelKind::Shell | horizon_core::PanelKind::Command
                ))
```

The label logic stays as-is (`Ssh` → "Reconnect", everything else → "Restart").

- [ ] **Step 2: Build + test**

Run: `cargo build -p horizon-ui && cargo test -p horizon-ui`
Expected: green. (Watch the file's 1000-line guard if CI re-added it — keep the diff minimal.)

- [ ] **Step 3: Commit**

```bash
git add crates/horizon-ui/src/app/sidebar.rs
git commit -m "feat(sidebar): offer Restart for shell/command panels"
```

---

### Task 6: Full gates, story update, delivery

**Files:**
- Modify: `docs/stories/story-ghost-panel-exit-detection.md` (check task boxes, File List, Dev Agent Record)

- [ ] **Step 1: Full quality gates**

```bash
cargo fmt --all -- --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: all green; test count ≥ 301 baseline + ~7 new; zero new clippy warnings. If `cargo fmt --check` fails, run `cargo fmt --all` and amend the relevant commit.

- [ ] **Step 2: Update the story** (mark tasks done, fill File List + Dev Agent Record, status → Ready for Review) and commit:

```bash
git add docs/stories/story-ghost-panel-exit-detection.md docs/superpowers/plans/2026-06-06-ghost-panel-exit-detection.md
git commit -m "docs(story): ghost-panel exit detection ready for review"
```

- [ ] **Step 3: Release build + install**

```bash
cargo build --release
install -m 755 target/release/horizon ~/.local/bin/horizon
ls -la ~/.local/bin/horizon   # confirm fresh mtime
```
Expected: build green; binary mtime = now. NOTE: the currently running Horizon instance keeps the old binary until relaunched — tell the user.

- [ ] **Step 4: Push branch, merge to main, push main** (ONLY with Steps 1-3 green)

```bash
git push origin feat/v1-navigation
git checkout main
git pull origin main --ff-only
git merge feat/v1-navigation --no-edit
git push origin main
git checkout feat/v1-navigation
```
Expected: both pushes accepted. If a GitHub ruleset rejects the main push, STOP and report — do not force.

---

## Risks

1. `PanelChrome` extra construction sites (fullscreen path) — compiler will catch; fix each with the real label, not `None`, when a panel is available.
2. `Terminal::write_input` gate could in theory drop a final burst of input racing with exit detection — acceptable: the child is already dead, bytes had nowhere to go.
3. Banner overlaps the last terminal line — intentional (it's a dead panel), clipped to the body rect.
4. Shortcut label says "Ctrl+Shift+R" on macOS where it's Cmd — Linux-first fork; note in story if no platform helper exists.
5. Merge to main carries the whole feat/v1-navigation history — explicitly requested by the user.
