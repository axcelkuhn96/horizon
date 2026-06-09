# Plan: Resume FRESH claude/claude2 shell sessions (argv + newest-jsonl distinct-cycle)

Branch: `main` (no worktree). No push. Subagents: Sonnet. BMAD personas injected.
Follow-up: the prior fix (a73448c..5f5c0f7) only resumes sessions that already had `--resume <id>` in argv;
a FRESH `claude` (typed bare, conversed, closed) was never resumed. This adds fresh-session capture.

## Findings that shape this (verified live — do NOT re-litigate)
- Start-time↔jsonl correlation is UNRELIABLE: the jsonl's first record timestamp is the first USER message
  (minutes after process start, variable), and concurrent fresh claudes have overlapping windows. NOT used.
- A live claude does NOT keep its `.jsonl` open (open-write-close); the jsonl has no pid/ppid field. So there is
  NO exact external process→session mapping for a fresh session. Only fuzzy (mtime recency) is available.
- For THIS user, `~/.claude-conta2/projects` is a symlink to `~/.claude/projects` (same inode) — both accounts
  share the session store. Using `<config_dir>/projects` is still correct generally; for this user the two
  account dirs coincide via symlink (sessions pooled). No special-casing needed.

## Approach
1. argv (already implemented): if the live claude was started with `--resume <id>`, that exact id is captured.
2. NEW — fresh fallback: for Shell+claude panels whose `running_agent` was detected but `session_id` is still None,
   resolve via the NEWEST `.jsonl` (by mtime) in `<config_dir>/projects/<enc-cwd>/` matching the panel cwd,
   with DISTINCT CLAIMING across panels in the same save pass (two panels never get the same id). Single claude in
   a cwd → its actively-written jsonl is the newest → correct. N claudes same cwd → N distinct newest sessions.
3. Persist the resolved id on `running_agent.session_id`; restore injection (already exact-or-nothing) then works.

## Tasks

### Task 1 — Distinct newest-session resolver [horizon-core, TDD]
- In `crates/horizon-core/src/runtime_state/agent_sessions.rs`, add a pure-ish resolver. Reuse the existing jsonl
  parsing helpers (`collect_claude_project_files`, `load_claude_project_session_summary`, `normalize_cwd`) — do NOT
  duplicate. Signature something like:
  `most_recent_unclaimed_session_for(config_dir: &Path, cwd: &str, claimed: &HashSet<String>) -> Option<String>`
  returns the newest-by-mtime session_id for that cwd whose id is NOT in `claimed`. (This is the distinct-cycle
  primitive; the caller threads a growing `claimed` set across panels.)
- Tests (TDD): temp `<dir>/projects/<enc>/<uuid>.jsonl` with cwd field + controlled mtimes (use the same approach the
  prior tests used; if filetime dev-dep is needed re-add it):
  (a) single session, not claimed → returns it;
  (b) two sessions same cwd, none claimed → returns the newest;
  (c) newest already in `claimed` → returns the SECOND newest;
  (d) all claimed → None;
  (e) cwd with no sessions → None;
  (f) missing projects dir → None (no panic).
- AC: tests pass; existing AgentSessionCatalog/recent_for untouched; strict clippy clean.

### Task 2 — Wire fresh resolution into from_board (distinct across panels) [horizon-core]
- In `crates/horizon-core/src/runtime_state.rs` `RuntimeState::from_board`: after all PanelStates are built (or in a
  post-pass), iterate Shell panels that have `running_agent = Some(a)` with `a.session_id == None` and `cwd = Some(c)`.
  Thread a `claimed: HashSet<String>` seeded with all session_ids ALREADY set from argv (so a fresh panel can't claim
  an id another panel is resuming via argv). For each such panel in a stable order, call
  `most_recent_unclaimed_session_for(&a.config_dir, &c, &claimed)`; if Some(id), set `running_agent.session_id = Some(id)`
  and insert id into `claimed`.
- Tighten/relax nothing else. Do NOT touch story-6 binding. Never log session contents.
- AC: a single fresh Shell+claude panel gets the newest cwd session; two fresh panels same cwd get DISTINCT ids;
  argv-resolved panels keep their id and block fresh panels from claiming it; build + both clippy gates clean.
- NOTE: from_board builds PanelState in a per-panel closure today. You may need a second pass over the collected
  Vec<PanelState> (mutating session_id). Keep it readable; if the structure makes a clean post-pass hard, report.

### Task 3 — Re-enable the bootstrap gate for fresh (session_id filled before gate) + verify injection [horizon-ui]
- The gate `HorizonApp::runtime_state_needs_session_bootstrap` requires Shell+running_agent+session_id.is_some().
  Since Task 2 fills session_id at SAVE time (persisted), restored PanelStates already carry it → the gate fires
  correctly. CONFIRM this ordering: session_id is resolved in from_board at SAVE (written to runtime.yaml), so on
  RESTORE the PanelState already has it. (If instead we needed to resolve at restore time, adjust — but SAVE-time is
  correct since the live claude/jsonl exist at save, not at restore.)
- No code change expected here beyond confirming/adjusting; add an integration-flavored test if a seam exists.
- AC: end-to-end — fresh claude at save → session_id persisted → restore injects `<binary> --resume <id>`.

## Risks
1. Two FRESH claudes in the exact same cwd may resume each other's session (distinct but possibly swapped). Accepted —
   both are the user's own sessions in that repo; never duplicated, never wrong account/binary.
2. A fresh claude that wrote NOTHING (no jsonl yet) → nothing to resume → skipped (correct).
3. Resolving by newest mtime could pick a recently-touched OLD session if the fresh one wrote nothing; mitigated by (2).
4. Account binary still from environ; session store shared via symlink for this user (harmless).
5. Save-time resolution requires the claude process + jsonl to exist at save (they do — app closing while claude runs).
