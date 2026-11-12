# Claude Hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Claude profiles with ready-running sessions (A), per-account usage chips in the toolbar (C), workspace forms without YAML (B), and a local terminal-to-terminal link (D) in the Horizon fork.

**Architecture:** Domain logic (profiles, env merge, usage parsing/poller/cache, template layout, link protocol/server) lives in `horizon-core`; `horizon-ui` only renders and collects actions. Usage polling and the link server are std threads + mpsc owned by the app; the Board is only touched on the UI thread. New config fields are optional with `serde(default)` (S9, no migration).

**Tech Stack:** Rust 2024, eframe/egui 0.33, alacritty_terminal, serde/serde_yaml/serde_json, new dep `ureq` 3.5.2 (blocking HTTP, rustls).

**Spec:** `docs/stories/story-claude-hub.md` (acceptance criteria) and `docs/prompts/2026-11-12-claude-hub.md` (refined spec, decisions closed — do not reopen).

## Global Constraints

Repo `/home/axcel/horizon-fork`, branch `feat/claude-hub` (already checked out). One commit per task, imperative scoped message (`feat(core): …`), ending with the attribution line the harness provides. No push. No worktree for sequential execution; parallel groups use one worktree per track as defined in `## Parallel Tracks`.

### Rulebooks CBS (ler ANTES de agir; primeiro caminho existente vence; ausente → seguir sem ele e anotar `rulebook ausente: <caminho>` no report)
- Persona dev (todo implementer): `~/.claude/skills/executor-bmad-superpowers/referencia/persona-dev.md`
- Transversal (implementer e reviewers): `~/.claude/cbs-overrides/personas/ponytail.md` → `~/.claude/personas/ponytail.md`
- Domínio (conforme a marcação da task): `~/.claude/cbs-overrides/personas/dominios/<dominio>.md` → `~/.claude/personas/dominios/<dominio>.md` (`frontend | integracao`; Rust não tem rulebook — omitir)
- Lentes de review (só reviewers): `~/.claude/cbs-overrides/personas/review/<lente>.md` → `~/.claude/personas/review/<lente>.md`
  - `seguranca`: task reviewer de toda task marcada `review: seguranca`; tabela em `seguranca-referencia.md`, por caminho
  - `cobertura-de-testes`: reviewer final da branch

### Resumo vinculante (≤10 linhas)
1. Menor superfície: reusar > stdlib > nativo > dependência já instalada > 1 linha inline > build mínimo. Nunca cortar validação, erro, segurança, a11y ou teste.
2. Ordem de leitura: persona dev → ponytail → área (frontend | integracao). Regra de área vence o corte ponytail.
3. TDD: `regra-de-negocio` = RED→GREEN com evidência no report; `operacional`/`ui` = validação E2E/screenshot nativo aceita, saída no report.
4. UI aqui é egui nativo, não web: ler o rulebook frontend só pelos princípios de UX/a11y; não invocar skills web/browser; validação visual = screenshot nativo da janela do Horizon pelo PID (`DISPLAY=:1`, XWayland).
5. Input externo validado; segredo nunca logado/exibido/gravado; `node ~/.claude/scripts/gate-seguranca.js` antes de DONE em task `review: seguranca` (exit 1 = não está DONE; "gate parcial" = anotar no report).
6. Branch `feat/claude-hub`; um commit por task; worktree só nos grupos paralelos de `## Parallel Tracks` (merge de volta em `feat/claude-hub`).
7. Subagent não dispara subagents; devolve só status, commits, 1 linha de teste, preocupações; detalhe no report file. Não narrar: só tool calls até o fim.

### Validação AGENTS.md (cada task deixa verde antes do commit)
```bash
cargo fmt --all -- --check
./scripts/check-maintainability.sh
RUSTFLAGS="-D warnings" cargo test --workspace
cargo clippy --all-targets --features speech,trace-profiling -- -D warnings
cargo clippy --workspace --lib --bins --examples --features speech -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
```
(`--features speech` test run and pedantic: Task 17.) Tests: unit tests in `#[cfg(test)]` at file end or `module/tests/`; integration under `crates/*/tests/`. Fix clippy warnings in touched files. Split files at ~600 lines when touched (`config.rs`, `panel.rs`, `panel/spawn.rs`, `app/sidebar.rs`): new code goes into new modules, not into those files. `#![forbid(unsafe_code)]`; no `unwrap`/`expect` in runtime code; thiserror error enums; `tracing` for logs. Windows/macOS must still compile (`cfg(unix)` gating for link).

## Review Focus

1. Credentials file missing, empty, malformed JSON, or a dangling symlink → state `Missing`, no panic, no log of contents (Task 7 tests).
2. Profile `config_dir` with `~`, trailing slash, nonexistent dir, or names differing only by case → rejected/normalized, never a crash (Tasks 1, 6).
3. Existing `config.yaml` without any new field loads unchanged and round-trips without new noise keys (`env`, `claude_profiles` omitted when empty) (Tasks 1, 2).
4. `link send` with newlines, ESC sequences, text > 64 KiB, or a target panel that just closed → typed error / sanitized, never a hang or a write to the wrong PTY (Tasks 13, 14).
5. Zero profiles / one profile / narrow window with 4 chips → bar hidden or compacted into overflow, no layout overflow (Task 9).

---

## Parallel Tracks

Execution may run independent tasks concurrently. Each parallel track runs in its own git worktree (branch `feat/claude-hub-<track>` cut from the current tip of `feat/claude-hub`, own `CARGO_TARGET_DIR` per worktree to avoid build locks) and is merged back into `feat/claude-hub` (`git merge --no-ff`) at the end of its wave; the next wave is cut only after all merges of the previous wave are green (`cargo test --workspace`). Sequential execution of tasks in numeric order remains valid; Task 0 must still run first.

### Shared files and single owners

These files are edited by several tasks as written. In parallel mode each has exactly one owner; every other task must NOT edit it (edits below that the task bodies list for these files are reassigned as stated).

| Shared file | Owner | Rule for the others |
|---|---|---|
| `crates/horizon-core/src/lib.rs` | Task 0 | Task 0 adds `pub mod claude_usage;` and `pub mod link;`; tasks 1, 7, 10, 13 do not touch `lib.rs` and expose their API by path (`horizon_core::claude_usage::…`, `horizon_core::link::…`, `horizon_core::config::…`); ignore "Modify `lib.rs`" in those tasks |
| `crates/horizon-core/src/config.rs` | Wave-1 track A (T1→T2→T3), then T5 (wave 2), then T9 (wave 3) — never two at once | Task 0 adds the `mod claude_profiles; mod workspace_template;` lines; Task 10 does not edit `config.rs` |
| `crates/horizon-core/src/panel.rs`, `panel/spawn.rs`, `runtime_state.rs` | Track A (T2, T4); later T14 (`spawn.rs` only, wave 2) | no other task |
| `crates/horizon-ui/src/app/mod.rs` | Task 0 (declares `mod claude_launcher; mod usage_chips; mod link;` and the field `pending_delete_workspace: Option<WorkspaceId>`), T5 (field `claude_launcher`, wave 2), T9 (field `claude_usage`, wave 3), T16 (fields `link_server`, `link_badges`, `send_to_panel`, wave 4) | one per wave; T12 uses the Task 0 field and does not edit `app/mod.rs` |
| `app/lifecycle.rs` | T9 (wave 3), then T16 (wave 4) | never concurrent |
| `app/settings/mod.rs` | Task 0 (adds `SettingsTab::{Claude, Workspaces}`, `ALL: [Self; 6]`, labels, dispatch to `claude::show` / `workspaces::show`, and the editor fields `claude_tab`, `workspaces_tab`) | T6 and T11 only fill their own `settings/claude.rs` / `settings/workspaces.rs`; ignore "Modify `settings/mod.rs`" in those tasks |
| `command_registry.rs`, `app/actions/command_palette.rs`, `app/actions/pickers.rs`, `shortcuts.rs`, `app/shortcuts.rs`, `app/shortcut_inventory.rs`, `app/settings/shortcuts.rs` | T5 only | no other task |
| `app/actions.rs` (mod declarations) | Task 0 (declares `mod open_template; mod workspace_templates;`) | T11 creates `actions/open_template.rs`, T12 creates `actions/workspace_templates.rs` |
| `app/root_chrome.rs`, `app/sidebar/toolbar.rs`, `app/settings/general.rs` | T9 only | no other task |
| `app/sidebar.rs` | T12 only | no other task |
| `crates/horizon-ui/src/main.rs`, `plugin_install.rs` | T15 only | no other task |
| `Cargo.toml` / `Cargo.lock` | T8 only | no other task |
| `app/panels.rs`, `app/panel_chrome.rs` | T16 only | no other task |

### Dependency graph

```
T0 ─┬─ T1 ─ T2 ─ T3 ─ T4            (track A, sequential: config/panel/runtime_state)
    ├─ T7 ─ T8                       (track B: claude_usage + Cargo.toml)
    └─ T13                           (track C: link pure core)

T3 ─ T5 (launcher)          T1 ─ T6 (settings Claude)
T2 ─ T10 (template core)    T2+T13 ─ T14 (socket server)    T13 ─ T15 (CLI + skill)

T5+T8 ─ T9 (chips)    T3+T10 ─ T11 (settings Workspaces)    T3+T10 ─ T12 (sidebar menu)
T14 ─ T16 (drain + panel menu + badge)           all ─ T17
```

### Waves

| Wave | Runs in parallel (one worktree each) | Why they do not collide |
|---|---|---|
| 0 | **Task 0** (wiring), on `feat/claude-hub` directly | single task |
| 1 | **A:** T1→T2→T3→T4 (sequential inside the track) ‖ **B:** T7→T8 ‖ **C:** T13 | A owns config/panel/runtime_state/agent_detect; B owns `claude_usage/` + `Cargo.toml`; C owns `link/` (pure files) |
| 2 | **T5** ‖ **T6** ‖ **T10** ‖ **T14** ‖ **T15** | T5: shortcuts/config.rs/palette/pickers/app/mod.rs; T6: `settings/claude.rs`; T10: `config/workspace_template.rs`; T14: `link/server.rs`, `panel/spawn.rs`, `board/link.rs`, `terminal.rs`; T15: `main.rs`, `link_cli.rs`, `plugin_install.rs` — disjoint |
| 3 | **T9** ‖ **T11** ‖ **T12** | T9: config.rs (features), general.rs, root_chrome, toolbar, app/mod.rs, lifecycle; T11: `settings/workspaces.rs`, `actions/open_template.rs`; T12: `sidebar.rs`, `sidebar/workspace_menu.rs`, `actions/workspace_templates.rs` — disjoint |
| 4 | **T16** | needs app/mod.rs + lifecycle.rs after T9 merged |
| 5 | **T17** | needs everything; single task |

### Rules for parallel execution
- Task 0 and the post-merge `cargo test --workspace` after each wave are mandatory gates; a failing merge is fixed on `feat/claude-hub` by a `build-fixer` before the next wave is cut.
- UI visual tasks in the same wave (T5, T6, T9, T11, T12) each launch their own `target/debug/horizon` from their own worktree with an isolated `HOME`/`--config` and a different PID; screenshot by that exact PID only. Evidence directories are per task, so they do not collide.
- If a task discovers it needs a file owned by another task of the same wave, it stops and reports `NEEDS_CONTEXT` instead of editing it.
- A track that finishes early waits for the wave merge; tasks never rebase onto in-flight sibling branches.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/horizon-core/src/config/claude_profiles.rs` (new) | `ClaudeProfile`, validation, discovery, `~` expansion, derived-preset builder |
| `crates/horizon-core/src/config/workspace_template.rs` (new) | grid layout for new terminals, panel→template conversion |
| `crates/horizon-core/src/panel/env.rs` (new) | spawn env merge |
| `crates/horizon-core/src/claude_usage/{mod,model,credentials,api,cache,poller}.rs` (new) | usage domain |
| `crates/horizon-core/src/link/{mod,protocol,resolve,text,server}.rs` (new, `cfg(unix)` server) | link domain |
| `crates/horizon-ui/src/app/claude_launcher.rs` (new) | launcher picker overlay |
| `crates/horizon-ui/src/app/settings/{claude,workspaces}.rs` (new) | settings tabs |
| `crates/horizon-ui/src/app/usage_chips.rs` (new) | toolbar chips paint + layout metrics |
| `crates/horizon-ui/src/app/link.rs`, `link_cli.rs` (new) | UI-thread drain; CLI client |
| `plugins`: `HorizonHome::claude_plugin_dir()` content | skill `horizon-link/SKILL.md` |

---

### Task 0: Wiring de módulos e pontos compartilhados (domínio: ; tipo: operacional)

**Files:**
- Modify: `crates/horizon-core/src/lib.rs`, `crates/horizon-core/src/config.rs` (mod lines only), `crates/horizon-ui/src/app/mod.rs`, `crates/horizon-ui/src/app/actions.rs`, `crates/horizon-ui/src/app/settings/mod.rs`
- Create (empty or stub): `crates/horizon-core/src/claude_usage/mod.rs`, `link/mod.rs`, `config/claude_profiles.rs`, `config/workspace_template.rs`, `crates/horizon-ui/src/app/{claude_launcher,usage_chips,link}.rs`, `app/settings/{claude,workspaces}.rs`, `app/actions/{open_template,workspace_templates}.rs`

**Interfaces:**
- Produces: module declarations only (see "Shared files and single owners"); `SettingsTab::{Claude, Workspaces}` (labels "Claude", "Workspaces", `ALL: [Self; 6]`) dispatching to `claude::show(ui: &mut egui::Ui, editor: &mut SettingsEditor)` and `workspaces::show(ui: &mut egui::Ui, editor: &mut SettingsEditor)` (stubs rendering "Coming soon" text, signatures fixed so owners never edit `settings/mod.rs`); editor fields `claude_tab: claude::ClaudeTabState` and `workspaces_tab: workspaces::WorkspacesTabState` (both `#[derive(Default)]`); `HorizonApp.pending_delete_workspace: Option<WorkspaceId>`.
- Dead-code warnings from not-yet-used stub items: use `#[expect(dead_code, reason = "wired in Task N")]` on each (the owning task deletes the attribute); this is the only allowed suppression.

- [ ] **Step 1:** Add the declarations and stubs above; no behavior change.
- [ ] **Step 2:** Run Global validation — Expected: all green, identical test count to baseline (`cargo test --workspace` before/after).
- [ ] **Step 3:** Commit `chore: wire claude-hub module skeleton`.

### Task 1: Perfis Claude no config (domínio: ; tipo: regra-de-negocio)

**Files:**
- Fill (file created empty by Task 0): `crates/horizon-core/src/config/claude_profiles.rs`
- Modify: `crates/horizon-core/src/config.rs` (re-export from the `claude_profiles` module declared by Task 0, field `claude_profiles` on `Config`, validation call); `lib.rs` is Task 0's — expose the API by path `horizon_core::config::…`
- Test: inline `#[cfg(test)]` in `claude_profiles.rs`

**Interfaces:**
- Produces:
  - `pub struct ClaudeProfile { pub name: String, pub config_dir: String }` (`Debug, Clone, PartialEq, Eq, Serialize, Deserialize`)
  - `Config.claude_profiles: Vec<ClaudeProfile>` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`)
  - `pub fn expand_config_dir(raw: &str, home: &Path) -> PathBuf` (`~` and `~/x`)
  - `pub fn discover_claude_profiles(home: &Path) -> Vec<ClaudeProfile>` (`~/.claude` first, rest alphabetical; only dirs containing `.credentials.json`; `.claude-<x>`→`<x>`, `.claude`→`claude`; `config_dir` returned as `~/.claude…` strings)
  - `pub fn validate_claude_profiles(profiles: &[ClaudeProfile]) -> Result<(), <existing config error type>>` (non-empty names, case-insensitive unique, non-empty dir)
  - `impl Config { pub fn effective_claude_profiles(&self) -> Vec<ClaudeProfile> }` (configured list, else discovery with `HOME`)

- [ ] **Step 1: Write failing tests** with `tempfile` homes: `discover_orders_claude_first_then_alpha` (dirs `.claude`, `.claude-conta3`, `.claude-conta2`, `.claude-nocreds` without credentials → names `claude, conta2, conta3`), `discover_derives_names`, `validate_rejects_case_insensitive_duplicates`, `validate_rejects_empty_name`, `expand_config_dir_handles_tilde`, `config_without_profiles_round_trips_without_key` (YAML of default config has no `claude_profiles`), `config_with_profiles_parses`.
- [ ] **Step 2:** Run `cargo test -p horizon-core claude_profiles` — Expected: FAIL (unresolved items).
- [ ] **Step 3:** Implement the interfaces above in the new module; wire `Config` (validation runs wherever shortcuts are validated at load). Keep `config.rs` growth to the minimum lines.
- [ ] **Step 4:** Run `cargo test -p horizon-core` — Expected: PASS.
- [ ] **Step 5:** Run Global validation block; commit `feat(core): add claude profiles config and discovery`.

### Task 2: `env` genérico em preset → painel → PTY → estado persistido (domínio: ; tipo: regra-de-negocio)

**Files:**
- Create: `crates/horizon-core/src/panel/env.rs`
- Modify: `config.rs` (`PresetConfig.env`, `to_panel_options`, `TerminalConfig.env` for templates — same attrs), `panel.rs` (`PanelOptions.env`, `Panel.env` field + accessor, restart :544 uses it), `panel/spawn.rs` (use merge at :360 and the restart path; move `agent_env` into `env.rs` if touching it), `runtime_state.rs`/panel state struct (`PanelState.env`), session restore path
- Test: `panel/env.rs` tests; a runtime-state round-trip test next to existing ones

**Interfaces:**
- Produces:
  - `PresetConfig.env`, `TerminalConfig.env`, `PanelOptions.env`, `Panel.env`, `PanelState.env`: `BTreeMap<String, String>` with `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]` where serialized
  - `pub(crate) fn merge_spawn_env(kind: PanelKind, preset_env: &BTreeMap<String, String>, injected: &HashMap<String, String>) -> HashMap<String, String>`: start from `agent_env(kind)`; apply `preset_env` except key `HORIZON`; then apply `injected` last (Task 14 uses it for `HORIZON_PANEL_*`; preset cannot override those)
- Consumes: Task 1 not required.

- [ ] **Step 1: Write failing tests:** `merge_keeps_horizon_one_for_agents`, `merge_preset_overrides_other_keys`, `merge_preset_cannot_override_horizon`, `merge_injected_wins_over_preset`, `preset_without_env_serializes_without_key`, `panel_state_env_round_trips_yaml`, `old_runtime_yaml_without_env_loads`.
- [ ] **Step 2:** `cargo test -p horizon-core env` — Expected: FAIL.
- [ ] **Step 3:** Implement; propagate `env` through `PanelOptions` → `spawn_terminal` → `TerminalSpawnOptions.env`; persist in `PanelState` and restore it; `Panel::restart` reuses `Panel.env`. Move new logic out of `panel.rs`/`spawn.rs` into `env.rs`.
- [ ] **Step 4:** `cargo test -p horizon-core` — Expected: PASS (existing tests untouched).
- [ ] **Step 5:** Global validation; commit `feat(core): carry preset env to PTY, restart and restore`.

### Task 3: Presets derivados por perfil (domínio: ; tipo: regra-de-negocio)

**Files:**
- Modify: `crates/horizon-core/src/config/claude_profiles.rs`, `config.rs` (`resolved_presets` :678)
- Test: inline in `claude_profiles.rs` / existing config tests

**Interfaces:**
- Consumes: `ClaudeProfile`, `Config::effective_claude_profiles`, `expand_config_dir` (T1); `PresetConfig.env` (T2)
- Produces:
  - `pub const CLAUDE_BASE_PRESET_NAME: &str = "Claude Code"`
  - `pub fn claude_profile_preset(base: &PresetConfig, profile: &ClaudeProfile, home: &Path) -> PresetConfig` → `name "Claude · <profile>"`, `alias Some(profile.name)`, `kind PanelKind::Claude`, `command/args/resume` copied from `base`, `env {"CLAUDE_CONFIG_DIR": expanded}`
  - `resolved_presets` appends one derived preset per effective profile (skip if a user preset already has that name); derived presets are not part of `self.presets`

- [ ] **Step 1: Failing tests:** `derived_preset_inherits_base_args_and_sets_config_dir`, `resolved_presets_lists_profile_presets_after_base`, `derived_presets_not_serialized_to_yaml`, `user_preset_with_same_name_wins`.
- [ ] **Step 2:** `cargo test -p horizon-core resolved_presets` — Expected: FAIL.
- [ ] **Step 3:** Implement as specified; base = user's preset named `Claude Code` (case-insensitive), fallback to first `kind: claude` preset, fallback to defaults.
- [ ] **Step 4:** `cargo test -p horizon-core` — PASS.
- [ ] **Step 5:** Global validation; commit `feat(core): derive Claude presets from profiles`.

### Task 4: Resume e detecção cientes do config dir (domínio: ; tipo: regra-de-negocio)

**Files:**
- Modify: `runtime_state/claude_live_sessions.rs`, `runtime_state.rs:267-340,497` (callers), `agent_detect.rs`, callers of `classify_agent_from_environ`
- Test: inline tests with temp dirs / synthetic environ

**Interfaces:**
- Consumes: `ClaudeProfile`, `expand_config_dir` (T1); `Panel.env` (T2) to read the panel's `CLAUDE_CONFIG_DIR`
- Produces:
  - `pub fn live_claude_session_ids(config_dir: &Path) -> HashSet<String>`
  - `pub fn claude_session_transcript_exists(config_dir: &Path, session_id: &str) -> bool`
  - `pub fn classify_agent_from_environ(environ: &[u8], home: &Path, profiles: &[ClaudeProfile]) -> RunningAgent` (matches `CLAUDE_CONFIG_DIR` against expanded profile dirs; unknown dir → still `claude` binary with that `config_dir`; absent var → `~/.claude`)
  - default config dir helper `default_claude_config_dir(home) -> PathBuf` (`~/.claude`)

- [ ] **Step 1: Failing tests:** `live_sessions_read_from_given_config_dir` (two temp dirs with different `sessions/*.json`), `transcript_exists_checks_given_dir`, `classify_four_profiles_by_config_dir_env`, `classify_without_env_is_default_claude`, `resume_decision_uses_panel_config_dir` (session id present only under a non-default dir → `--resume`, absent → `--session-id`).
- [ ] **Step 2:** `cargo test -p horizon-core claude_live agent_detect runtime_state` — FAIL.
- [ ] **Step 3:** Change signatures; thread the panel's config dir (from `Panel.env["CLAUDE_CONFIG_DIR"]`, expanded, else default) from the resume/bootstrap call sites; pass effective profiles to detection.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Global validation; commit `feat(core): make Claude resume and detection config-dir aware`.

### Task 5: Atalho `claude_launcher` + picker (domínio: frontend; tipo: ui)

**Files:**
- Fill (stub from Task 0): `crates/horizon-ui/src/app/claude_launcher.rs`
- Modify: `horizon-core/src/shortcuts.rs` (:254,284 `claude_launcher: ShortcutBinding`, default Ctrl+Shift+L), `config.rs` (ShortcutsConfig :367/396/435/462 + collision test), `horizon-ui/src/app/settings/shortcuts.rs`, `app/shortcut_inventory.rs`, `app/shortcuts.rs`, `app/mod.rs` (state `claude_launcher: Option<ClaudeLauncherState>`), `command_registry.rs` + `app/actions/command_palette.rs` (entry "Claude launcher"), `app/actions/pickers.rs`
- Test: core shortcut tests mirroring `toggle_scroll_pan` ones; pure-logic tests in `claude_launcher.rs`

**Interfaces:**
- Consumes: `Config::effective_claude_profiles`, derived presets (T3), `add_panel_to_workspace(workspace_id, preset, None)` (`app/actions/panels.rs:111`)
- Produces:
  - `ShortcutsConfig.claude_launcher: String`, `AppShortcuts.claude_launcher: ShortcutBinding`
  - `pub(super) struct ClaudeLauncherState { selected: usize }` and pure fns `launcher_key_to_index(key: egui::Key, len: usize) -> Option<usize>`, `move_selection(selected: usize, delta: i32, len: usize) -> usize`
  - `HorizonApp::open_claude_launcher(&mut self)` / `close_claude_launcher` / `launch_claude_profile(&mut self, profile_name: &str)` (resolves the derived preset by alias and calls `add_panel_to_workspace` on the active workspace; Task 9 reuses `launch_claude_profile`)
  - Usage % column reads `self.claude_usage` snapshot when present (added in Task 9; until then render name only — keep a `Option<&UsageSnapshot>` seam via a method `fn profile_usage_label(&self, name:&str)->Option<String>` returning `None`)

- [ ] **Step 1: Failing tests:** shortcut default parses to `Ctrl+Shift+L`; colliding binding rejected with `claude_launcher` in message; `launcher_key_to_index(Key::Num2, 4) == Some(1)`, `(Key::Num5, 4) == None`; `move_selection` wraps.
- [ ] **Step 2:** `cargo test --workspace claude_launcher` — FAIL.
- [ ] **Step 3:** Implement shortcut plumbing, picker overlay (centered modal, rows `N  name`, Esc closes, Enter/click launches, keys 1–9), palette entry. Repaint only on input while open.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual: `cargo build`, launch `target/debug/horizon` with `DISPLAY=:1` and a temp `--config` containing 2 profiles; screenshot the Horizon window by PID (golden path + empty-profiles state). Save to `.superpowers/sdd/2026-11-12-claude-hub/task-5-visual/`.
- [ ] **Step 6:** Global validation; commit `feat(ui): add Claude launcher shortcut and picker`.

### Task 6: Aba Settings "Claude" (domínio: frontend; tipo: ui)

**Files:**
- Fill (stub from Task 0): `crates/horizon-ui/src/app/settings/claude.rs`
- No edit to `app/settings/mod.rs` (Task 0 owns tab registration and dispatches to `claude::show(ui, editor)`; this task implements `show` and `ClaudeTabState` in `settings/claude.rs`)
- Test: pure-logic tests in `claude.rs`

**Interfaces:**
- Consumes: `ClaudeProfile`, `validate_claude_profiles`, `discover_claude_profiles`, `expand_config_dir` (T1); `dir_picker` (modal API as used by `settings/presets.rs` or `app/actions/pickers.rs`)
- Produces:
  - `pub fn credential_state(config_dir: &Path, now_ms: i64) -> CredentialState` where `CredentialState { Ok, Expired, Missing }` — reads only `expiresAt` and non-empty `accessToken` presence via a minimal serde struct; the token string is dropped immediately and never returned (Task 7 will provide `claude_usage::credentials`; to avoid a dependency inversion this task owns a tiny local reader and Task 7 may re-use it — if Task 7 lands a shared reader, replace in Task 7)
  - `fn move_profile(profiles: &mut Vec<ClaudeProfile>, from: usize, to: usize)`, `fn add_profile`, `fn remove_profile`, `fn merge_detected(existing: &mut Vec<ClaudeProfile>, detected: Vec<ClaudeProfile>)` (adds only unseen names/dirs)

- [ ] **Step 1: Failing tests:** `credential_state_ok_expired_missing` (temp files; expired = `expiresAt` ≤ now+60 000), `credential_state_never_exposes_token` (return type has no string), `merge_detected_skips_known`, `move_profile_reorders`.
- [ ] **Step 2:** `cargo test -p horizon-ui settings::claude` — FAIL.
- [ ] **Step 3:** Implement tab: table of profiles (name text edit, config_dir text edit + "Browse" using the dir picker, state badge ok/expired/missing, up/down/remove buttons), "Add profile", "Detect accounts"; edits go to `editing_config.claude_profiles`; save via the existing flow; show validation error from `validate_claude_profiles` inline. Cache `CredentialState` per profile (refresh on tab open / "Detect"), not per frame.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual: screenshot of tab with an ok, an expired and a missing profile (temp config + temp dirs). Evidence in `task-6-visual/`.
- [ ] **Step 6:** Global validation; commit `feat(ui): add Claude profiles settings tab`.

### Task 7: Domínio de uso — modelo, credenciais, parser (domínio: integracao; tipo: regra-de-negocio; review: seguranca)

**Files:**
- Fill (mod.rs stub from Task 0; others new): `crates/horizon-core/src/claude_usage/{mod.rs,model.rs,credentials.rs}`
- No edit to `lib.rs` (Task 0 declares `pub mod claude_usage;`)
- Test: inline tests with fixture JSON strings

**Interfaces:**
- Produces:
  - `pub enum UsageState { Ok, Expired, Missing, Stale }` (serde, lowercase)
  - `pub struct UsageWindow { pub percent: Option<f32>, pub resets_at: Option<String> }`
  - `pub struct ProfileUsage { pub name: String, pub display_name: Option<String>, pub email: Option<String>, pub five_hour: UsageWindow, pub seven_day: UsageWindow, pub severity: Option<String>, pub state: UsageState, pub fetched_at_ms: Option<i64> }` (no token fields)
  - `pub struct AccessToken(String)` with `Debug` printing `AccessToken(<redacted>)`, no `Display`, no `Serialize`; method `pub(crate) fn expose(&self) -> &str`
  - `pub fn read_credentials(config_dir: &Path) -> Result<Credentials, UsageError>` where `Credentials { token: AccessToken, expires_at_ms: i64 }`; file opened read-only; errors: `Missing`, `Malformed`
  - `pub fn token_expired(expires_at_ms: i64, now_ms: i64) -> bool` (`expires_at_ms <= now_ms + 60_000`)
  - `pub fn read_account(config_dir: &Path) -> (Option<String>, Option<String>)` (`oauthAccount.displayName/emailAddress` from `.claude.json`)
  - `pub fn parse_usage_response(body: &str) -> Result<(UsageWindow, UsageWindow, Option<String>), UsageError>` (`five_hour`/`seven_day` `.utilization`/`.resets_at`; `null` → empty window; fallback to `limits[]` entries by `group`; severity from the limit)
  - `pub fn remaining_percent(used: f32) -> u32` (`100 - round(used)`, min 0), `pub fn used_percent_label(w:&UsageWindow)->String` (`"37"` or `"—"`), `pub fn parse_rfc3339_unix(s:&str)->Option<i64>` (hand-written, no new dep)
  - `UsageError` thiserror enum: `Missing, Malformed, Http(u16), Network, Timeout`

- [ ] **Step 1: Failing tests:** `expired_when_within_60s`, `read_credentials_missing_file`, `read_credentials_malformed_json`, `read_credentials_empty_token_is_expired_state_input`, `access_token_debug_is_redacted`, `parse_five_hour_seven_day`, `parse_null_windows_are_dashes`, `parse_falls_back_to_limits_by_group`, `remaining_clamps_at_zero`, `read_account_missing_file_is_none`, `rfc3339_parses_offset_and_z`, `read_credentials_does_not_modify_mtime` (set file mtime, read, assert unchanged).
- [ ] **Step 2:** `cargo test -p horizon-core claude_usage` — FAIL.
- [ ] **Step 3:** Implement with `serde_json` only; open files with `File::open` (read-only); never `tracing` token or file contents.
- [ ] **Step 4:** `cargo test -p horizon-core claude_usage` — PASS.
- [ ] **Step 5:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(core): add Claude usage domain model and credential reader`.

### Task 8: Cliente HTTP, cache e poller (domínio: integracao; tipo: regra-de-negocio; review: seguranca)

**Files:**
- Create: `crates/horizon-core/src/claude_usage/{api.rs,cache.rs,poller.rs}`
- Modify: root `Cargo.toml` (`ureq = { version = "3.5.2", default-features = false, features = ["rustls"] }` under `[workspace.dependencies]`; justification in commit: blocking HTTP client needed for one GET, rustls avoids OpenSSL, no tokio runtime added, latest stable 3.5.2 checked on crates.io 2026-11-12; verify the feature set builds with `cargo tree -p ureq`), `crates/horizon-core/Cargo.toml` (`ureq.workspace = true`), `claude_usage/mod.rs`
- Test: inline; HTTP tested against a local `std::net::TcpListener` stub through an injectable base URL

**Interfaces:**
- Consumes: everything from Task 7; `HorizonHome` for the cache path (`~/.horizon/claude-usage-cache.json`)
- Produces:
  - `pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage"`
  - `pub fn fetch_usage(base_url: &str, token: &AccessToken) -> Result<String, UsageError>` (GET, headers `Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`, 10 s timeout; 401/403 → `Http(code)`; transport → `Network`/`Timeout`); refuses any host other than the configured base
  - `pub fn refresh_profile(profile: &ClaudeProfile, home: &Path, base_url: &str, now_ms: i64, previous: Option<&ProfileUsage>) -> ProfileUsage` (Missing/Expired when no valid token without HTTP; 401/403 → `Expired` keeping previous values; network error → `Stale` keeping previous values)
  - `pub fn load_cache(path: &Path) -> Vec<ProfileUsage>` / `pub fn save_cache(path: &Path, usage: &[ProfileUsage]) -> io::Result<()>` (atomic write, mode 0600 on unix)
  - `pub struct UsagePoller` with `pub fn spawn(profiles: Vec<ClaudeProfile>, home: PathBuf, cache_path: PathBuf, interval: Duration) -> Self`, `pub fn refresh_now(&self)`, `pub fn set_profiles(&self, Vec<ClaudeProfile>)`, `pub fn try_recv(&self) -> Option<Vec<ProfileUsage>>`, `Drop` stops the thread; command channel + `recv_timeout(interval)` loop, no busy wait

- [ ] **Step 1: Failing tests:** `fetch_sends_required_headers` (stub captures request), `fetch_401_maps_to_http_error`, `refresh_expired_token_skips_http` (stub counts 0 requests), `refresh_network_error_keeps_previous_as_stale`, `refresh_401_marks_expired_keeps_values`, `cache_round_trip_has_no_token_strings` (serialize, assert no `accessToken`/`refreshToken`/token value substring), `poller_refresh_now_delivers_result`, `poller_drop_stops_thread`, `credential_files_mtime_unchanged_after_two_refreshes`.
- [ ] **Step 2:** `cargo test -p horizon-core claude_usage` — FAIL.
- [ ] **Step 3:** Implement. Production caller uses `USAGE_URL`; tests pass the stub URL. Do not follow redirects to other hosts (`max_redirects(0)`).
- [ ] **Step 4:** `cargo test --workspace` — PASS; `cargo tree -p horizon-core | rg ureq` shows the dep.
- [ ] **Step 5:** E2E (operacional, report only): run `cargo run -p horizon-core --example usage_probe`-style throwaway under the scratchpad (do not commit) calling `refresh_profile` for `~/.claude*` and compare with a manual `curl` of the endpoint with the valid account; record the two numbers (±1 p.p.). Never print the token.
- [ ] **Step 6:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(core): add Claude usage poller, ureq client and cache`.

### Task 9: Chips de uso no toolbar (domínio: frontend; tipo: ui)

**Files:**
- Fill (stub from Task 0): `crates/horizon-ui/src/app/usage_chips.rs`
- Modify: `config.rs` or a new `config/features.rs` (`FeaturesConfig.claude_usage_bar: bool`, default `true`), `app/settings/general.rs` (toggle), `app/mod.rs` (state: `claude_usage: ClaudeUsageState { poller, snapshot: Vec<ProfileUsage>, loaded_at }`; start poller at boot from effective profiles + load cache; rebuild on config apply), `app/root_chrome.rs` (`ToolbarItem::UsageChips`, width estimation, compact mode, overflow), `app/sidebar/toolbar.rs` (paint), `app/lifecycle.rs` (drain `try_recv` each frame — cheap; repaint only when a new snapshot arrives)
- Test: pure fns in `usage_chips.rs`; layout tests in `root_chrome.rs` style

**Interfaces:**
- Consumes: `ProfileUsage`, `UsagePoller` (T8), `remaining_percent`, `parse_rfc3339_unix` (T7), `HorizonApp::launch_claude_profile` (T5), `usage_stats` for today's tokens (cached, refreshed with the snapshot, not per frame)
- Produces:
  - `pub(super) fn severity_for(percent: Option<f32>, api: Option<&str>) -> Severity` (`Normal | Warn(≥75) | Critical(≥90 or api "critical")`)
  - `pub(super) fn chip_label(u: &ProfileUsage, compact: bool) -> String` (`"claude2  5h 37%  7d 12%"`; compact `"claude2 7d 12%"`; expired adds `expired`)
  - `pub(super) fn chips_width(count: usize, compact: bool) -> f32`
  - `ToolbarItem::UsageChips` participates in `RootToolbarLayout`: shown only when `features.claude_usage_bar` and ≥1 profile; compact before dropping; excess chips fall to the overflow menu
  - Interactions: left click → `launch_claude_profile(name)`; right click context menu "Refresh now" → `poller.refresh_now()`; tooltip text built lazily on hover only

- [ ] **Step 1: Failing tests:** `severity_thresholds_75_90`, `severity_api_critical_overrides`, `chip_label_dash_for_null_window`, `chip_label_expired_marker`, `layout_hides_chips_when_disabled_or_no_profiles`, `layout_compacts_then_overflows_when_narrow`, `features_default_has_usage_bar_true_and_old_yaml_loads`.
- [ ] **Step 2:** `cargo test --workspace usage_chips root_chrome features` — FAIL.
- [ ] **Step 3:** Implement. Redraw surface: only new poll result, hover, click. No `request_repaint` loop; cache galleys/labels keyed by snapshot version.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual + perf: screenshots (4 chips with one expired using a seeded cache file; narrow window; bar disabled); idle trace per AGENTS.md "Performance Profiling" (`HORIZON_TRACE_SPANS=1`) before/after, workload recorded in the commit message. Evidence in `task-9-visual/`.
- [ ] **Step 6:** Global validation; commit `feat(ui): show Claude usage chips in the toolbar`.

### Task 10: Layout e conversão de templates em `horizon-core` (domínio: ; tipo: regra-de-negocio)

**Files:**
- Fill (stub from Task 0): `crates/horizon-core/src/config/workspace_template.rs`
- No edit to `config.rs` or `lib.rs` (Task 0 declares the `workspace_template` module; API exposed as `horizon_core::config::workspace_template::…`)
- Test: inline

**Interfaces:**
- Consumes: `TerminalConfig.env` (T2), `PresetConfig`
- Produces:
  - `pub fn grid_position(index: usize, panel_size: [f32; 2], columns: usize) -> [f32; 2]` (simple grid with fixed gap constant)
  - `pub fn terminal_from_preset(preset: &PresetConfig, name: Option<String>, index: usize) -> TerminalConfig` (kind/command/args/resume/ssh/env from preset; automatic position/size)
  - `pub fn template_from_workspace(name: &str, color: Option<String>, cwd: Option<String>, panels: &[PanelSnapshotForTemplate]) -> WorkspaceConfig` with `PanelSnapshotForTemplate { name, kind, command, args, cwd, position: [f32;2], size: [f32;2], env, resume, ssh_connection }` (workspace-relative positions)
  - `pub fn upsert_template(workspaces: &mut Vec<WorkspaceConfig>, template: WorkspaceConfig, linked_name: Option<&str>)` (replace entry with `linked_name` match case-insensitively, else push; never removes others)
  - `pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize)`

- [ ] **Step 1: Failing tests:** `grid_positions_fill_rows_left_to_right`, `terminal_from_claude_profile_preset_keeps_env`, `template_from_workspace_keeps_panels_and_positions`, `upsert_replaces_linked_and_keeps_others`, `upsert_appends_when_no_link`, `template_yaml_round_trip_valid`.
- [ ] **Step 2:** `cargo test -p horizon-core workspace_template` — FAIL.
- [ ] **Step 3:** Implement; reuse existing default panel size constants.
- [ ] **Step 4:** `cargo test -p horizon-core` — PASS.
- [ ] **Step 5:** Global validation; commit `feat(core): add workspace template layout and upsert helpers`.

### Task 11: Aba Settings "Workspaces" (domínio: frontend; tipo: ui)

**Files:**
- Fill (stub from Task 0): `crates/horizon-ui/src/app/settings/workspaces.rs`
- No edit to `app/settings/mod.rs` (Task 0 registers the tab and dispatches to `workspaces::show(ui, editor)`); "Open now" lives in `app/actions/open_template.rs` (declared by Task 0), not `sidebar.rs`
- Test: pure logic tests

**Interfaces:**
- Consumes: T10 helpers, `Config::resolved_presets()` (incl. profile presets), `dir_picker`, existing save flow (`save_settings`)
- Produces:
  - tab state `WorkspacesTabState { selected: Option<usize>, confirm_remove: Option<usize>, preset_pick: usize }`
  - `HorizonApp::open_template_now(&mut self, ctx:&Context, template:&WorkspaceConfig)` → creates a workspace and its panels in the live board (reuse board creation from session bootstrap, `Board::create_workspace_at` + `create_panel_with_options`)
  - tab edits mutate `editing_config.workspaces` only; switching to the YAML tab re-serializes from `editing_config` (and back parses YAML), preserving edits

- [ ] **Step 1: Failing tests:** `add_template_appends_default_named`, `remove_requires_confirmation_state`, `adding_terminal_by_preset_uses_terminal_from_preset`, `tab_switch_preserves_edits_through_yaml` (editing_config → yaml → parse equal), `rename_rejects_empty_name`.
- [ ] **Step 2:** `cargo test -p horizon-ui settings::workspaces` — FAIL.
- [ ] **Step 3:** Implement tab; removing a template never closes live workspaces; kind shown read-only.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual: create template with 1 shell + `Claude · claude3` via the UI, "Open now", verify YAML tab shows valid YAML and workspace materialized. Evidence in `task-11-visual/`.
- [ ] **Step 6:** Global validation; commit `feat(ui): add workspaces settings tab with Open now`.

### Task 12: Menu de contexto do workspace na sidebar (domínio: frontend; tipo: ui)

**Files:**
- Create: `crates/horizon-ui/src/app/sidebar/workspace_menu.rs`
- Modify: `app/sidebar.rs` (call site only, ~:500-543; extract rather than grow), `app/sidebar/menu.rs` (reuse `context_menu_above_sidebar`), `app/actions/` (new `workspace_templates.rs` for save-as-template + delete), config write via `atomic_write` (`app/util.rs:211`) + `apply_runtime_config`
- Test: pure logic tests in `workspace_menu.rs` / actions

**Interfaces:**
- Consumes: T10 `template_from_workspace`, `upsert_template`; presets (T3); `Board::remove_workspace` (`board/workspaces.rs:189`); `add_panel_to_workspace`
- Produces:
  - menu entries **Rename**, **New terminal ▸** (all resolved presets), **Save as template**, **Delete** (confirmation modal state `pending_delete_workspace: Option<WorkspaceId>`)
  - `HorizonApp::save_workspace_as_template(&mut self, id: WorkspaceId)` (loads current `config.yaml`, `upsert_template`, `to_yaml`, `atomic_write`, `apply_runtime_config`)
  - `fn confirm_label(workspace_name:&str, live_panels: usize) -> String`

- [ ] **Step 1: Failing tests:** `save_as_template_replaces_same_name_keeps_others` (temp config path), `save_as_template_creates_when_absent`, `confirm_label_mentions_panel_count`.
- [ ] **Step 2:** `cargo test -p horizon-ui workspace_menu workspace_templates` — FAIL.
- [ ] **Step 3:** Implement menu + actions; if `sidebar.rs` is touched beyond the call site, move that section into the new module.
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual: right-click menu screenshot, rename in place, Save as template then confirm in Settings → Workspaces, Delete confirmation. Evidence in `task-12-visual/`.
- [ ] **Step 6:** Global validation; commit `feat(ui): workspace context menu rename, new terminal, save as template, delete`.

### Task 13: Protocolo, resolução e texto puro do link (domínio: integracao; tipo: regra-de-negocio; review: seguranca)

**Files:**
- Fill (mod.rs stub from Task 0; others new): `crates/horizon-core/src/link/{mod.rs,protocol.rs,resolve.rs,text.rs}`
- No edit to `lib.rs` (Task 0 declares `pub mod link;`)
- Test: inline

**Interfaces:**
- Produces:
  - `pub const LINK_PROTOCOL_VERSION: u32 = 1`, `pub const MAX_REQUEST_BYTES: usize = 64 * 1024`, `pub const MAX_READ_LINES: usize = 500`, `pub const DEFAULT_READ_LINES: usize = 50`
  - `pub enum LinkRequest { List, Send { target: String, text: String, enter: bool }, Read { target: String, lines: usize } }` and `pub struct RequestEnvelope { pub v: u32, #[serde(flatten)] pub request: LinkRequest }` (serde, tag `op`)
  - `pub enum LinkResponse { Ok(LinkOk), Err { code: LinkErrorCode, message: String } }`, `LinkOk::{Panels(Vec<PanelInfo>), Sent, Text(String)}`, `PanelInfo { id, name, workspace, kind, is_agent }`
  - `pub enum LinkErrorCode { BadRequest, VersionMismatch, TooLarge, NotFound, Ambiguous, Closed }`
  - `pub fn parse_request_line(line: &[u8]) -> Result<LinkRequest, LinkError>` (size, UTF-8, version check; `lines` clamped to 1..=500)
  - `pub fn resolve_target<'a>(panels: &'a [PanelInfo], target: &str) -> Result<&'a PanelInfo, LinkError>` (exact id → exact name → case-insensitive name; ambiguity lists candidates in message)
  - `pub fn strip_escapes(text: &str) -> String` (CSI/OSC/ESC sequences removed), `pub fn sanitize_send_text(text: &str) -> String` (removes ESC and other C0 controls except `\n`/`\t`)
  - `LinkError` thiserror enum mapping to `LinkErrorCode`

- [ ] **Step 1: Failing tests:** `parse_rejects_over_64k`, `parse_rejects_wrong_version`, `parse_clamps_lines`, `resolve_exact_id_beats_name`, `resolve_case_insensitive_fallback`, `resolve_ambiguous_lists_candidates`, `resolve_not_found`, `strip_escapes_removes_csi_osc`, `sanitize_send_text_drops_esc_keeps_newline`, `response_json_round_trip`.
- [ ] **Step 2:** `cargo test -p horizon-core link` — FAIL.
- [ ] **Step 3:** Implement pure functions (no IO).
- [ ] **Step 4:** `cargo test -p horizon-core link` — PASS.
- [ ] **Step 5:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(core): add link protocol, target resolution and text sanitizing`.

### Task 14: Servidor do socket, env do painel e execução de requisições (domínio: integracao; tipo: regra-de-negocio; review: seguranca)

**Files:**
- Create: `crates/horizon-core/src/link/server.rs` (`#[cfg(unix)]`; non-unix stub with same API returning `Err(LinkError::Unsupported)`)
- Modify: `link/mod.rs`, `panel/env.rs` caller in `panel/spawn.rs` (inject `HORIZON_PANEL_ID`, `HORIZON_PANEL_NAME`, `HORIZON_SOCKET` as the `injected` map of `merge_spawn_env`), `board.rs` (new method below, in a new `board/link.rs` file), `terminal.rs` (read text of last N lines; bracketed-paste state accessor if missing)
- Test: unit + integration `crates/horizon-core/tests/link_socket.rs`

**Interfaces:**
- Consumes: protocol (T13), `merge_spawn_env` (T2), `Board::panel_mut`/`panel_id_by_local_id` (`board.rs:355,379`), `Panel::write_input` (`panel.rs:403`)
- Produces:
  - `pub fn socket_path(pid: u32, xdg_runtime_dir: Option<&Path>, home: &Path) -> PathBuf` (`<xdg>/horizon/<pid>.sock` else `<home>/.horizon/run/<pid>.sock`)
  - `pub struct LinkServer` with `pub fn start(path: PathBuf) -> Result<Self, LinkError>` (dir 0700, socket 0600, replaces a stale socket whose pid is dead), `pub fn path(&self) -> &Path`, `pub fn try_recv(&self) -> Option<PendingRequest>`, `Drop` removes the socket and joins threads; per-connection thread reads one line ≤ 64 KiB with a read timeout, forwards `PendingRequest { request: LinkRequest, reply: mpsc::Sender<LinkResponse> }` and writes the response line
  - `impl Board { pub fn handle_link_request(&mut self, request: LinkRequest) -> LinkResponse }` (list/send/read; send = `sanitize_send_text`, bracketed paste wrapper when the target terminal has mode enabled, append `\r` unless `enter == false`; read = visible+scrollback last N lines via `strip_escapes`; closed/missing → typed error)
  - Panel env always includes the three vars; `HORIZON_PANEL_NAME` = title at spawn

- [ ] **Step 1: Failing tests:** `socket_path_prefers_xdg`, `socket_path_falls_back_to_home`, `server_sets_modes_0700_0600`, `server_removes_socket_on_drop`, `server_rejects_oversized_line`, `board_send_writes_text_and_cr_to_target`, `board_send_no_enter_omits_cr`, `board_send_uses_bracketed_paste_when_enabled`, `board_read_returns_plain_last_lines`, `board_closed_panel_returns_closed_error`, `preset_env_cannot_override_horizon_panel_vars`, integration `link_socket_roundtrip_list` (client connects to temp socket, UI side loop answers via `handle_link_request`).
- [ ] **Step 2:** `cargo test -p horizon-core link` — FAIL.
- [ ] **Step 3:** Implement; no Board access outside `handle_link_request` (called by the UI thread only).
- [ ] **Step 4:** `cargo test --workspace` — PASS; check `cargo check --target x86_64-pc-windows-gnu -p horizon-core` if the target is installed, else note unverified in the report.
- [ ] **Step 5:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(core): add link socket server and board request handling`.

### Task 15: Subcomando `horizon link` e skill `horizon-link` (domínio: integracao; tipo: operacional; review: seguranca)

**Files:**
- Create: `crates/horizon-ui/src/link_cli.rs`; skill file in the plugin source that `HorizonHome::claude_plugin_dir()` is populated from (find the installer that writes the plugin, e.g. `plugin_install.rs`, and add `skills/horizon-link/SKILL.md` there)
- Modify: `crates/horizon-ui/src/main.rs` (before `parse_cli_args`/eframe: if `argv[1] == "link"` run `link_cli::run(args) -> ExitCode`), `plugin_install.rs`
- Test: unit tests for arg parsing; E2E against a real instance

**Interfaces:**
- Consumes: protocol types (T13), `HORIZON_SOCKET` env
- Produces:
  - `pub fn parse_link_args(args: &[String]) -> Result<LinkCommand, String>` where `LinkCommand::{List, Send{target, text, enter}, Read{target, lines}}` (`--no-enter`, `--lines N`)
  - `pub fn run(args: &[String]) -> ExitCode` (prints `list` as an aligned table, `read` as raw text, errors to stderr, exit 1; missing `HORIZON_SOCKET` → "HORIZON_SOCKET is not set; run inside a Horizon terminal")
  - Skill content: when/how to use `horizon link list/send/read`, message arrives as user input in the other session, identify yourself with `$HORIZON_PANEL_NAME`, never send secrets, prefer short messages

- [ ] **Step 1: Failing tests:** `parse_send_with_no_enter`, `parse_read_default_and_explicit_lines`, `parse_rejects_unknown_subcommand`, `missing_socket_env_is_clear_error`, `plugin_install_writes_horizon_link_skill`.
- [ ] **Step 2:** `cargo test -p horizon-ui link_cli plugin_install` — FAIL.
- [ ] **Step 3:** Implement client (std `UnixStream`, `cfg(unix)`; non-unix prints "link is not supported on this platform", exit 1) and the skill file (frontmatter `name: horizon-link`, `description`).
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** E2E after Task 16 is merged is covered in Task 17; here only parse/IO tests.
- [ ] **Step 6:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(ui): add horizon link CLI and horizon-link skill`.

### Task 16: Drenagem na UI, menu do painel e badge (domínio: frontend, integracao; tipo: ui; review: seguranca)

**Files:**
- Fill (stub from Task 0): `crates/horizon-ui/src/app/link.rs`
- Modify: `app/mod.rs` (state `link_server: Option<LinkServer>`, `link_badges: HashMap<PanelId, Instant>`, `send_to_panel: Option<SendToPanelState>`), `app/lifecycle.rs` (start server at boot, set `HORIZON_SOCKET` in the spawn context before panels spawn, drain `try_recv` each frame with a per-frame cap, answer replies; request repaint only when a request was handled), `app/panels.rs:716` `show_panel_context_menu` (two entries; extract new menu code to `app/link.rs`), `app/panel_chrome.rs` (badge, mirror `paint_ssh_status_badge`)
- Test: pure logic tests

**Interfaces:**
- Consumes: `LinkServer`, `Board::handle_link_request` (T14); `PanelInfo`
- Produces:
  - `fn badge_visible(received_at: Instant, now: Instant) -> bool` (3 s) and `fn badge_remaining(...) -> Duration` used to schedule exactly one `request_repaint_after`
  - `fn panel_address(info: &PanelInfo) -> String` (name, or id when name ambiguous)
  - **Send to panel…** (target picker + text field prefilled with current selection; submit builds `LinkRequest::Send` and calls `handle_link_request` directly), **Copy panel address**
  - Badge "message received" on the target titlebar for 3 s

- [ ] **Step 1: Failing tests:** `badge_visible_for_3_seconds`, `panel_address_uses_id_when_name_duplicated`, `drain_respects_per_frame_cap`.
- [ ] **Step 2:** `cargo test -p horizon-ui app::link` — FAIL.
- [ ] **Step 3:** Implement; shutdown drops `LinkServer` (socket removed).
- [ ] **Step 4:** `cargo test --workspace` — PASS.
- [ ] **Step 5:** Visual + perf: screenshot of context menu and badge (2 panels); idle trace shows no continuous repaint from the server/badge. Evidence in `task-16-visual/`.
- [ ] **Step 6:** `node ~/.claude/scripts/gate-seguranca.js`; Global validation; commit `feat(ui): drain link requests, panel send menu and receive badge`.

### Task 17: Sync do config do usuário, plano de smoke e smoke com screenshot nativo (domínio: ; tipo: operacional)

**Files:**
- Modify (outside repo): `~/.horizon/config.yaml` (backup first to `~/.horizon/config.yaml.bak-2026-11-12-claude-hub`)
- Create then delete: `docs/testing/2026-11-12-claude-hub-smoke.md`
- Modify: `docs/architecture/maintainability.md` (note new module boundaries: `claude_usage/`, `link/`, `config/*` splits)

**Interfaces:** Consumes everything; produces report `.superpowers/sdd/2026-11-12-claude-hub/task-17-report.md` with evidence.

- [ ] **Step 1:** Backup `~/.horizon/config.yaml`; add `claude_profiles` (`claude`→`~/.claude`, `claude2`→`~/.claude-conta2`, `claude3`→`~/.claude-conta3`, `claude-sigamina`→`~/.claude-sigamina`) and `claude_launcher: Ctrl+Shift+L` only if non-default is needed; preserve everything else; `diff` the backup (expected: only added keys); load it with the new binary (`target/debug/horizon --config ~/.horizon/config.yaml` boots without error).
- [ ] **Step 2:** Write `docs/testing/2026-11-12-claude-hub-smoke.md` (self-contained for another agent): baseline, flows A/B/C/D, edges (expired token, narrow window, zero profiles, ambiguous link target, closed target, 64 KiB payload), persistence/restore (env kept across restart and relaunch), visual regression checklist, perf idle check.
- [ ] **Step 3:** Execute it: `cargo build`; launch `target/debug/horizon` with `DISPLAY=:1`, note the PID; screenshot only that PID's window (`xwininfo -root -tree` to find the window id for the PID via `xdotool search --pid`, then `import -window <id>` or equivalent); no Playwright/browser. Verify: `/proc/<child>/environ` of a `claude2` panel contains `CLAUDE_CONFIG_DIR=…/.claude-conta2`; socket mode 0600 (`stat -c %a`) and removed after close; `horizon link list/send/read/--no-enter/ambiguous/missing` behaviors; two Claude sessions exchanging a message through the skill (manual, evidence in report); credential mtimes unchanged (`stat -c %Y`) after 2 poll cycles (use manual refresh); `rg -c 'accessToken|refreshToken' ~/.horizon/claude-usage-cache.json` → 0.
- [ ] **Step 4:** Run full validation including `RUSTFLAGS="-D warnings" cargo test --workspace --features speech` and the pedantic tier (advisory); expected all blocking tiers green.
- [ ] **Step 5:** Delete the smoke plan file; update `docs/architecture/maintainability.md`; commit `docs: sync maintainability notes for claude hub` (smoke plan not committed).

---

## Risks

1. **Large files** — `config.rs` (1112), `panel.rs` (1241), `panel/spawn.rs` (1178), `app/sidebar.rs` (1011) exceed the ~600-line guideline: every task touching them must put new logic in new modules (`config/claude_profiles.rs`, `config/workspace_template.rs`, `panel/env.rs`, `app/sidebar/workspace_menu.rs`) and keep edits to wiring only; `check-maintainability.sh` must stay green.
2. **Credentials** — only `File::open` read-only; no `OpenOptions` write, no refresh, no logging of contents; `AccessToken` is redacted and not `Serialize`; the cache serializer type has no token field (compile-time guarantee); redirects disabled so the bearer cannot leave `api.anthropic.com`.
3. **Repaint/perf of chips** — no per-frame string building or galley layout; labels cached per snapshot version; repaint only on new snapshot/hover/click; idle trace before/after is mandatory in Task 9.
4. **Link injection** — `send` writes text to a PTY, i.e. effectively executes input in the target session; mitigations: same-user socket (0700/0600), 64 KiB cap, ESC/C0 sanitizing, bracketed paste when supported, no shell exec by the server; the skill tells agents to identify themselves. Residual: any same-user process can drive panels (accepted, documented).
5. **PTY bracketed paste** — alacritty mode bit must be read on the UI thread at send time; if the mode accessor is missing the task adds one in `terminal.rs`; Enter is sent after the paste end marker as a separate write.
6. **Windows/macOS compile** — link server gated by `cfg(unix)` with a non-unix stub; `/proc` detection paths already gated; `ureq`/rustls builds on all three; verify with `cargo check` for a Windows target when available.
7. **New dependency `ureq` 3.5.2** (crates.io latest stable checked 2026-11-12, MSRV 1.85 ≤ project 1.88): chosen over `reqwest` (pulls tokio full runtime/hyper) and over shelling out to `curl` (would put the token on a command line); `default-features = false` + `rustls` only; justification repeated in the Task 8 commit.
8. **Config compatibility (S9)** — every new field is `serde(default)` and skipped when empty; no migration; a test per field asserts old YAML loads and round-trips without noise.
9. **Resume signature changes (Task 4)** — callers of `live_claude_session_ids`/`classify_agent_from_environ` span core and UI; `cargo test --workspace` after the change is the safety net, and the panel's config dir defaults to `~/.claude` so non-profile panels behave as before.
10. **Scope** — large (18 tasks). If the session runs out, stop after a part boundary (A, C, B, D are independently shippable in that order); `/mapear` was the alternative.
