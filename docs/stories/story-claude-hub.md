# Story: Claude Hub — profiles, usage bar, workspace forms, terminal link

**Status:** Ready for dev
**Epic:** Claude Hub (branch `feat/claude-hub`)
**Created:** 2026-11-12
**BMAD:** template (BMAD not installed; this file follows the repo story format)
**Spec:** `docs/prompts/2026-11-12-claude-hub.md` (all decisions closed; S1–S10 are binding assumptions)

## Story

As a Horizon user with four Claude accounts, I want (A) each account to be a
profile that opens a session already running `claude` with the right
`CLAUDE_CONFIG_DIR`, (C) per-account 5h/7d usage chips in the top toolbar,
(B) to create/edit workspaces from forms instead of YAML, and (D) to let
terminal sessions talk to each other through a local link, so I stop juggling
aliases, tabs and `/usage` by hand.

## Context (verified 2026-11-12 against the tree)

- `PresetConfig` (no `env`): `crates/horizon-core/src/config.rs:86-100`; `to_panel_options` :105; `resolved_presets` :678; `ShortcutsConfig` :367/396/435/462; `FeaturesConfig` :493; `WorkspaceConfig` :508; `TerminalConfig` :525.
- Env reaches the PTY via `TerminalSpawnOptions.env`; `agent_env` (`panel/spawn.rs:911`) only injects `HORIZON=1`.
- Resume/detect pinned to `~/.claude`: `runtime_state/claude_live_sessions.rs:17,75`; `agent_detect.rs:33` (`classify_agent_from_environ`).
- Shortcuts: `shortcuts.rs:254,284` + `config.rs` (`toggle_scroll_pan` is the template); settings rows `app/settings/shortcuts.rs:124`.
- Settings tabs `app/settings/mod.rs` (`SettingsTab` General/Shortcuts/Presets/Yaml; `editing_config`).
- Toolbar: `app/root_chrome.rs` (`ToolbarItem` :49, `ToolbarAction` :22, `RootToolbarLayout` :66), `app/sidebar/toolbar.rs`.
- Panel creation: `app/actions/panels.rs:111` `add_panel_to_workspace`; panel context menu `app/panels.rs:716`; sidebar context menus `app/sidebar/menu.rs`.
- Board: `panel_mut` `board.rs:355`, `panel_id_by_local_id` :379, `remove_workspace` `board/workspaces.rs:189`; CLI parse `horizon-ui/src/main.rs:325`.
- No HTTP crate, no IPC in the repo. Large files that must be split when touched: `config.rs` (1112), `panel.rs` (1241), `panel/spawn.rs` (1178), `app/sidebar.rs` (1011), `app/settings/speech.rs` (1058, do not touch).

## Acceptance Criteria

### A — Profiles and launcher
1. `claude_profiles: [{name, config_dir}]` in config; absent/empty falls back to auto-discovery of `~/.claude` and `~/.claude-*` containing `.credentials.json` (order: `~/.claude` first, then alphabetical; name derived: `.claude`→`claude`, `.claude-<x>`→`<x>`). Names unique case-insensitive and non-empty.
2. `PresetConfig.env` (generic, `BTreeMap`, omitted from YAML when empty) flows through `PanelOptions` to the PTY, merged with `agent_env` (preset env wins except `HORIZON`), stored on `Panel` and persisted so restart and session restore keep it.
3. `resolved_presets` adds `Claude · <profile>` per profile (alias = profile name, kind `claude`, `CLAUDE_CONFIG_DIR` = expanded dir, args inherited from the user's `Claude Code` preset); shown in preset picker and command palette; never written to YAML as presets.
4. `--resume`/`--session-id` decisions and live-session lookup use the panel's config dir; `agent_detect` classifies by `CLAUDE_CONFIG_DIR` against configured profiles.
5. Shortcut `claude_launcher` (default `Ctrl+Shift+L`, editable) opens a small picker (name + 5h/7d % when known), keys `1`–`9`, Enter/click confirm, Esc closes; opens a Claude session in the active workspace via `add_panel_to_workspace`.
6. Settings tab **Claude**: list profiles (name, config_dir with dir picker, credential state ok/expired/missing, never the token), add/remove/reorder, "Detect accounts".
7. With the seeded config, `claude2` opens a panel whose child has `CLAUDE_CONFIG_DIR=~/.claude-conta2` in `/proc/<pid>/environ`.

### C — Usage bar
1. `horizon-core` module reads `accessToken`/`expiresAt` from `<dir>/.credentials.json` read-only; if valid calls `GET https://api.anthropic.com/api/oauth/usage` (`Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`, 10 s timeout); extracts `five_hour`/`seven_day` `utilization`+`resets_at` (fallback `limits[]` by `group`); reads `oauthAccount.displayName/emailAddress` from `<dir>/.claude.json`.
2. Never refreshes OAuth, never writes credential files, never logs/shows/caches tokens. Expired = `expiresAt` ≤ now+60 s or empty token; 401/403 → `Expired`; network/timeout → `Stale` keeping last value; `null` window → "—".
3. One app-owned poller (std thread + mpsc): every 5 min + manual refresh; cache `~/.horizon/claude-usage-cache.json` (name, %, resets_at, fetched_at, state only), loaded at boot.
4. New `ToolbarItem` with one chip per profile (`5h NN%`, `7d NN%`, severity colors ≥75% warn, ≥90% critical or API `severity`), dimmed "expired" chip, tooltip (displayName/e-mail, used+remaining %, local reset time, data age, today's aggregate tokens via `usage_stats`, "open `<profile>` in a terminal to renew login"); left click opens a session of that profile; right click "Refresh now"; narrow widths compact chips (name + 7d) and overflow into the existing overflow menu.
5. `features.claude_usage_bar` (default `true`) toggle in General.
6. Credential file mtimes unchanged after 2 poll cycles; cache has no `accessToken`/`refreshToken`; no continuous repaint attributable to chips at idle.

### B — Workspaces without YAML
1. Settings tab **Workspaces** (form over `editing_config.workspaces`): add/rename/remove (confirm)/reorder templates; name, cwd (dir picker), color; terminals per template (add by preset incl. Claude profiles, name, read-only kind, optional cwd, remove, reorder); new terminals get automatic grid positions (logic in `horizon-core`); "Open now" instantiates the template in the current session; saves through the existing `editing_config`→`to_yaml`→`atomic_write`→`apply_runtime_config` flow; coexists with the YAML tab.
2. Sidebar workspace context menu: **Rename**, **New terminal ▸** (presets incl. profiles), **Save as template** (name, cwd, color, terminals with kind/name/cwd/position/size/preset env; replaces the linked template or creates one; never deletes other templates), **Delete** (confirm; `remove_workspace`).
3. A template with 1 shell + 1 `Claude · claude3` is created without touching the YAML tab, YAML is valid and hot-reloads.

### D — Terminal link (Unix only; no-op on Windows)
1. Server in `horizon-core`: socket `$XDG_RUNTIME_DIR/horizon/<pid>.sock` (fallback `~/.horizon/run/<pid>.sock`), dir 0700, socket 0600, removed on shutdown; one JSON line per request with a version field; max 64 KiB; requests forwarded to the UI thread and answered after frame processing (Board never touched off-thread).
2. Every panel gets `HORIZON_PANEL_ID` (local_id), `HORIZON_PANEL_NAME`, `HORIZON_SOCKET`; preset env cannot override them.
3. `horizon link list | send <target> <text> [--no-enter] | read <target> [--lines N]` (default 50, max 500, plain text without escapes); target resolution: exact id, exact name, case-insensitive name; ambiguity → error listing candidates; missing `HORIZON_SOCKET` → clear error; non-zero exit on any error. `send` only writes to the target PTY (bracketed paste when the target enabled it).
4. Skill `horizon-link` in the plugin dir injected via `--plugin-dir` (`HorizonHome::claude_plugin_dir()`).
5. Panel context menu: **Send to panel…**, **Copy panel address**; receiving panel shows a short-lived titlebar badge.

### Cross-cutting
- All AGENTS.md validation commands green on the final branch; UI changes have native screenshots; `~/.horizon/config.yaml` synced (backup kept); temporary smoke plan deleted after validation.

## Tasks

See `docs/superpowers/plans/2026-11-12-claude-hub.md` (17 tasks, order A → C → B → D → delivery). Checklist is tracked there.

- [ ] Part A — Tasks 1–6
- [ ] Part C — Tasks 7–9
- [ ] Part B — Tasks 10–12
- [ ] Part D — Tasks 13–16
- [ ] Delivery — Task 17

## Out of scope
OAuth refresh/login renewal, per-account token counts, MCP server, cross-machine sync, drag-position editor inside Settings, upstream changes, release/tag.

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes

### File List
