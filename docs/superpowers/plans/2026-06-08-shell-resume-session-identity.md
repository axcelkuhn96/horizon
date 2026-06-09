# Plan: Correct per-panel session identity for shell claude/claude2 resume

Branch: `main` (no worktree). No push. Subagents: Sonnet. BMAD personas injected (no `_bmad/` in repo).
Follow-up to the base feature (commits 96feb1c..5a4d5a6). Fixes the materialized Risk #5: two shells in the
same cwd/account resumed the SAME session because resolution was `most_recent_session_for(config_dir, cwd)`.

## Decision (confirmed with user)
Capture the EXACT session_id per panel from signals Horizon can actually read; if none, DON'T inject.
- `/proc/<pid>/fd` does NOT work — claude does not hold the session `.jsonl` open (probed live: open-write-close).
- Signal 1 (primary): the live claude process argv. `claude --resume <uuid>` carries the id (covers every
  session Horizon itself auto-resumed → self-perpetuating in steady state).
- Signal 2 (secondary): the panel terminal scrollback — claude prints `Resume this session with:\nclaude --resume <uuid>`
  on graceful exit, and a user-typed `claude --resume <uuid>` is echoed there too.
- Fallback: a fresh `claude` still running at save (no id anywhere) → DO NOT inject (never resume the wrong session).
  This supersedes `most_recent_session_for` for the shell path (which caused the bug).
- A SIGHUP/SIGTERM-killed claude (Horizon closing while claude runs) does NOT print the resume line — so kill-time
  capture is intentionally NOT relied upon.

## Verified facts (live probe + base feature)
- `detect_running_agent(child_pid)` (`agent_detect.rs`) already walks /proc to the claude pid and reads environ for
  the account. Adding an argv read of `/proc/<claude_pid>/cmdline` there is cheap; the pid is already in hand.
- argv proof: PID `claude --resume f054f32b-…` → id in cmdline. Fresh `claude` (no --resume) → id NOT in argv/environ/fd.
- `RunningAgent { binary: String, config_dir: PathBuf }` (Clone/Debug/PartialEq/Eq/serde). Persisted on `PanelState.running_agent`.
- Injection: `crates/horizon-ui/src/app/session.rs` `poll_startup_bootstrap` Ok(bootstrap) arm (~l.300-346), gated also by
  the extended `HorizonApp::runtime_state_needs_session_bootstrap` (fires for Shell+running_agent). `resume_command(binary, id)`
  validates uuid-like (hex+hyphen, 32..=128) + binary allowlist (claude/claude2).
- `most_recent_session_for` (`runtime_state/agent_sessions.rs`) + its re-exports (`runtime_state.rs`, `lib.rs`) + `filetime`
  dev-dep were added for the cwd-only resolution being replaced. The shell injection is its ONLY caller.
- Do NOT touch Story-6 native-agent binding (`session_binding`, `bootstrap_missing_agent_bindings`, `needs_session_bootstrap`).

## Tasks

### Task 1 — Capture session_id from claude argv at detection time [horizon-core, TDD]
- `crates/horizon-core/src/agent_detect.rs`: add `pub session_id: Option<String>` to `RunningAgent`
  (keep derives; serde `#[serde(default, skip_serializing_if = "Option::is_none")]` so old runtime.yaml → None).
- PURE helper `session_id_from_cmdline(cmdline: &[u8]) -> Option<String>` (NUL-separated argv bytes):
  find a `--resume` token immediately followed by a uuid-like token; OR a `--resume=<uuid>` form. Validate the
  candidate is uuid-like (`[0-9a-fA-F-]`, len 32..=128) — else None. Return the uuid.
  - Tests (TDD first): `["claude","--resume","f054...uuid"]` → Some(uuid); `["claude"]` → None;
    `--resume` as last token (no value) → None; `--resume <non-uuid e.g. ./path>` → None; `--resume=<uuid>` → Some;
    multiple args around it → still found; over-long/garbage value → None.
- In the Linux `detect_running_agent`, after finding the claude pid: read `/proc/<claude_pid>/cmdline`, pass to
  `session_id_from_cmdline`, set `RunningAgent.session_id` (None on any IO error / not found). Non-linux stub: session_id None.
  Never log cmdline/environ contents.
- AC: cmdline tests pass; RunningAgent gains the field with retro-compat serde; strict clippy clean; no panics.

### Task 2 — Capture session_id from panel scrollback as fallback [horizon-core, TDD on the pure scanner]
- PURE helper `session_id_from_resume_line(text: &str) -> Option<String>` in `agent_detect.rs`: scan the text for
  occurrences of `claude --resume <uuid>` (also matches `claude2 --resume <uuid>`; the leading binary is incidental —
  match on `--resume` followed by a uuid-like token). Return the LAST (most recent) uuid found, validated uuid-like; None if absent.
  - Tests (TDD): a blob containing `Resume this session with:\nclaude --resume 6c81741c-...uuid` → Some(uuid);
    two resume lines → returns the LAST; no resume line → None; a line with `--resume` but garbage value → None;
    ANSI/color escape codes interleaved around the uuid → still extracts (decide: strip non-[0-9a-fA-F-] around token).
- Wire it in `RuntimeState::from_board` (`runtime_state.rs`) for the Shell+claude path: when `running_agent` was detected
  but its `session_id` is still None (argv gave nothing), obtain the panel terminal's scrollback/visible text and run
  `session_id_from_resume_line`; if Some, set it on the running_agent before persisting.
  - INVESTIGATE FIRST: how to read the terminal scrollback/screen text from a live `Panel`/`Terminal` at save time
    (e.g. an existing accessor on Terminal for scrollback lines or the alacritty/vte grid). If there is a clean
    accessor, use it. If reading scrollback text is NOT cleanly available or would require invasive new plumbing,
    STOP and report — ship Task 1 (argv) alone is acceptable; do not fabricate a fragile scrollback reader.
- AC: scanner tests pass; from_board sets session_id from scrollback only when argv didn't; no regression to cwd/running_agent
  detection; clippy clean. If scrollback access is infeasible, the task is reported as deferred with the reason (argv still ships).

### Task 3 — Restore injects EXACT id only; remove most_recent fallback [horizon-ui]
- `crates/horizon-ui/src/app/session.rs` injection loop: for each restored Shell panel with `running_agent = Some(a)`:
  - if `a.session_id` is `Some(id)` → `resume_command(&a.binary, &id)` and inject `"{cmd}\n"` (existing validation/allowlist).
  - if `a.session_id` is `None` → DO NOT inject (drop the `most_recent_session_for` call entirely for shells).
  - keep the inject-once guarantee and the live-panel lookup by local_id.
- Remove the now-dead `most_recent_session_for`: delete the function, its tests, the re-exports in `runtime_state.rs` and
  `lib.rs`, and the `filetime` dev-dependency IF nothing else uses them (grep to confirm the shell injection was the only
  caller). If something else uses it, leave it and just stop calling it for shells (report which caller).
- The bootstrap gate (`runtime_state_needs_session_bootstrap` extended for Shell+running_agent) stays — but now a Shell with
  running_agent whose session_id is None will spawn bootstrap and then inject nothing; that's fine (cheap, no-op). Optionally
  tighten the gate to `running_agent.session_id.is_some()` so we don't spin bootstrap for nothing — implementer's judgment,
  note the choice.
- AC: with a captured id → injects `<binary> --resume <id>`; without id → injects nothing (no duplicate/wrong resume);
  two shells same cwd/account each resume THEIR own id when both ids were captured; existing resume_command tests still pass;
  both clippy gates clean; release build ok.

## Risks
1. Scrollback text may not be cleanly readable at save time → Task 2 deferred; argv-only still fixes the steady-state
   (auto-resumed) case and removes the collision (no-inject when unknown).
2. A genuinely fresh claude (typed bare, never exited, running at save) won't auto-resume — accepted by user ("Não injetar").
3. argv `--resume` parsing must reject non-uuid values (e.g. a path) — covered by uuid validation + tests.
4. Security: never log cmdline/environ/scrollback; validate id before injecting (existing resume_command guards).
5. Removing most_recent_session_for must not break other callers — grep before deleting.
6. Non-Linux: argv read returns None (no /proc) → no injection (no panic).
