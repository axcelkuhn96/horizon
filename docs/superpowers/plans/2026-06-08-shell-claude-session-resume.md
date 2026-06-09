# Plan: Resume claude/claude2 running inside a shell panel on app restart

Branch: `main` (no worktree). No push. Subagents: Sonnet. BMAD personas injected (no `_bmad/` in repo).

## Story
As a user who runs `claude` (account 1) or `claude2` (account 2 — alias `CLAUDE_CONFIG_DIR=$HOME/.claude-conta2 claude`) by typing it inside a plain `kind: Shell` (zsh) terminal panel, when I close Horizon and reopen it, the shell should automatically re-run `<binary> --resume <session_id>` (auto-executed) so the same Claude conversation resumes — but ONLY for shells that were actually running claude/claude2, and resuming from the correct account's session store.

## Verified facts (from Explore + environment probe)
- `claude2` is a zsh alias `CLAUDE_CONFIG_DIR=$HOME/.claude-conta2 claude` (same binary, 2nd account). Sessions: claude → `~/.claude/projects/`, claude2 → `~/.claude-conta2/projects/`.
- Process tree under a shell PTY: `child_pid` (wrapper `/usr/bin/script`) → zsh → claude (node). Both accounts run the SAME binary; only `CLAUDE_CONFIG_DIR` in the process environ differs.
- `panel.terminal_title` (`panel.rs:159`, updated in `process_output` ~l.282-295) holds the live OSC title ("✳ Claude Code") but is NOT persisted in `PanelState`.
- `Panel::write_input`/`Terminal::write_input` (`panel.rs:389`, `terminal/lifecycle.rs:92`) → `Msg::Input` injects bytes into the PTY. Reuse.
- `current_cwd_for_pid` / `shell_pid_behind_wrapper` (`terminal/support.rs:93-129`) reads `/proc/<pid>/cwd` skipping the `script` wrapper — reference for walking the proc tree.
- `PanelState.cwd` already reflects the shell's live cwd at save (`should_track_live_cwd`, `panel.rs:622`).
- `AgentSessionCatalog` (`runtime_state/agent_sessions.rs:26`) reads `~/.claude/projects` only today; `recent_for(kind,cwd)`.
- Restore: `Board::from_runtime_state_with_transcripts` (`board.rs:143`) and `poll_startup_bootstrap` (`session.rs:256-273`, board built + catalog loaded) — injection point.
- agents.rs: `CLAUDE.resume_mode = ExactFlag{flag:"--resume"}`.

## Tasks

### Task 1 — Agent detection helper (horizon-core, Linux) [pure-TDD core]
- New module (e.g. `crates/horizon-core/src/agent_detect.rs`, or extend `terminal/support.rs`). Public type `RunningAgent { binary: String, config_dir: PathBuf }` (Clone, Debug, PartialEq, serde Serialize/Deserialize).
- PURE, UNIT-TESTED helper `classify_agent_from_environ(environ: &[u8], home: &Path) -> RunningAgent`:
  - Parse the NUL-separated environ bytes; find `CLAUDE_CONFIG_DIR=<value>`.
  - If value ends with `.claude-conta2` (or equals `<home>/.claude-conta2`) → `RunningAgent{ binary:"claude2", config_dir: <that dir> }`.
  - Else (absent or other) → `RunningAgent{ binary:"claude", config_dir: <home>/.claude }`.
  - Tests: environ with conta2 → claude2+conta2 dir; environ without CLAUDE_CONFIG_DIR → claude+~/.claude; environ with some other CLAUDE_CONFIG_DIR → claude (default account, since binary name is `claude` unless conta2); multiple env vars; malformed/empty → default claude.
- THIN, non-tested Linux fn `detect_running_agent(child_pid: i32) -> Option<RunningAgent>`:
  - Walk `/proc/<pid>/task/<pid>/children` recursively from `child_pid` to find a process whose `/proc/<pid>/comm` or `/proc/<pid>/cmdline` indicates claude (cmdline contains `.local/bin/claude`, or comm is `claude`/`node` running the claude entrypoint). Reference `shell_pid_behind_wrapper`.
  - Read `/proc/<claude_pid>/environ`, pass to `classify_agent_from_environ`. Return None if no claude process found / unreadable.
  - `#[cfg(not(target_os="linux"))]` stub returns None. No unwrap/expect; all IO errors → None.
- AC: classify tests pass; no panics; strict clippy clean.

### Task 2 — Persist `running_agent` on PanelState [core]
- `crates/horizon-core/src/runtime_state.rs`: add `pub running_agent: Option<RunningAgent>` to `PanelState` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (retro-compat: old runtime.yaml → None).
- In `RuntimeState::from_board` (panel closure ~l.218-248): for `kind == PanelKind::Shell` ONLY, if `panel.terminal_title` indicates Claude (case-insensitive contains "claude code" or "claude"), call `detect_running_agent(panel.child_pid())` (expose child_pid accessor if needed) and set `running_agent`. Everything else → None. Do NOT log the environ.
- Update any exhaustive `PanelState { .. }` literals (search `board/tests`, etc.) to include the new field.
- AC: field serializes only when Some; from_board sets it only for Shell running claude; unit/build green; retro-compat (deserialize old yaml → None).

### Task 3 — Session lookup by cwd in an arbitrary config dir [core, TDD]
- `crates/horizon-core/src/runtime_state/agent_sessions.rs`: generalize so the projects enumeration can target `<config_dir>/projects` (not hardcoded `~/.claude/projects`). Add a function `most_recent_session_for(config_dir: &Path, cwd: &str) -> Option<String>` (returns session_id) that scans `<config_dir>/projects/**/*.jsonl`, reads the `"cwd"` field, filters by normalized cwd equality, returns the most-recent (by mtime / updated_at) session_id. Reuse existing parsing; don't duplicate.
- Keep existing `AgentSessionCatalog` behavior intact (default ~/.claude) — add the parameterized path, don't break callers.
- Tests: temp `<dir>/projects/<enc>/<uuid>.jsonl` with a `cwd` field; assert most-recent session returned for that cwd; None when cwd has no session; newest wins among two.
- AC: tests pass; strict clippy clean.

### Task 4 — Restore injection [horizon-ui, TDD on the pure command builder]
- `crates/horizon-ui/src/app/session.rs` `poll_startup_bootstrap` (after the board is built, ~l.265-273): iterate the restored `runtime_state` panel states; for each `kind == Shell && running_agent == Some(a)` with `cwd == Some(c)`:
  - `let Some(session_id) = most_recent_session_for(&a.config_dir, &c) else { continue };`
  - Build the command with a PURE helper `resume_command(binary: &str, session_id: &str) -> Option<String>`: validate session_id is uuid-like (`[0-9a-fA-F-]{32,}`); return `format!("{binary} --resume {session_id}")` or None if invalid (security: no command injection). UNIT-TEST it: valid → "claude --resume <id>"/"claude2 --resume <id>"; invalid/empty id → None.
  - Find the live panel by `local_id` and call `panel.write_input(format!("{cmd}\n").as_bytes())` (auto-execute). Do this once per restored shell (guard so it doesn't re-fire on a second bootstrap poll — e.g. a `injected` set or a flag).
- Only inject for shells that passed detection; idle/other shells untouched.
- AC: pure `resume_command` tests pass; manual validation works (claude + claude2 in shells → close → reopen → resume in the right account); Story 6 / normal restore / scrollback / cwd-tracking / shutdown unaffected; both clippy gates clean; build release ok.

## Risks
1. Finding the claude pid under the `script`→zsh→node tree may be fragile (comm is often `node`, not `claude`). Mitigation: match cmdline containing the claude entrypoint; fallback to title-only → default "claude".
2. claude2 detection depends on `CLAUDE_CONFIG_DIR` being in the claude process environ (it is, since the alias sets it). If absent → treated as claude (account 1). Acceptable.
3. Injection timing race vs zsh rc init (accepted v1).
4. Double-bootstrap poll could inject twice — guard with an injected-once flag.
5. Two shells same cwd both resume the same most-recent session (accepted v1).
6. Security: validate session_id before injecting; never inject arbitrary text; don't log environ.
7. Non-Linux: detection returns None → no injection (no panic).
