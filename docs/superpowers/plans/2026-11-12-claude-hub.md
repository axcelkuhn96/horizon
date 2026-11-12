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
| `crates/horizon-core/src/lib.rs` | Task 0 (wave 0), then T18 (wave 2, adds only `pub mod git_overview;`) | Task 0 adds `pub mod claude_usage;` and `pub mod link;`; tasks 1, 7, 10, 13 do not touch `lib.rs` and expose their API by path (`horizon_core::claude_usage::…`, `horizon_core::link::…`, `horizon_core::config::…`); ignore "Modify `lib.rs`" in those tasks |
| `crates/horizon-core/src/config.rs` | Wave-1 track A (T1→T2→T3), then T5 (wave 2), then T9 (wave 3) — never two at once | Task 0 adds the `mod claude_profiles; mod workspace_template;` lines; Task 10 does not edit `config.rs` |
| `crates/horizon-core/src/panel.rs`, `panel/spawn.rs`, `runtime_state.rs` | Track A (T2, T4); later T14 (`spawn.rs` only, wave 2) | no other task |
| `crates/horizon-ui/src/app/mod.rs` | Task 0 (declares `mod claude_launcher; mod usage_chips; mod link;` and the field `pending_delete_workspace: Option<WorkspaceId>`), T5 (field `claude_launcher`, wave 2), T9 (field `claude_usage`, wave 3), T16 (fields `link_server`, `link_badges`, `send_to_panel`, wave 4) | one per wave; T12 uses the Task 0 field and does not edit `app/mod.rs` |
| `app/lifecycle.rs` | T9 (wave 3), then T16 (wave 4), then T19 (wave 5, git overview poller drain only) | never concurrent |
| `app/hub_state.rs` | Task 0 creates; each UI task adds its own fields in its wave (T5 w2, T9/T12 w3, T16 w4, T19 w5) | one editor per wave; wave 3 T9 and T12 touch disjoint fields but must not both edit the struct literal in the same commit window — T12 owns `pending_delete_workspace`, T9 the usage fields; merge conflicts resolved by the merger |
| `app/settings/mod.rs` | Task 0 (adds `SettingsTab::{Claude, Workspaces}`, `ALL: [Self; 6]`, labels, dispatch to `claude::show` / `workspaces::show`, and the editor fields `claude_tab`, `workspaces_tab`) | T6 and T11 only fill their own `settings/claude.rs` / `settings/workspaces.rs`; ignore "Modify `settings/mod.rs`" in those tasks |
| `command_registry.rs`, `app/actions/command_palette.rs`, `app/actions/pickers.rs`, `shortcuts.rs`, `app/shortcuts.rs`, `app/shortcut_inventory.rs`, `app/settings/shortcuts.rs` | T5 only | no other task |
| `app/actions.rs` (mod declarations) | Task 0 (declares `mod open_template; mod workspace_templates;`) | T11 creates `actions/open_template.rs`, T12 creates `actions/workspace_templates.rs` |
| `app/root_chrome.rs`, `app/sidebar/toolbar.rs`, `app/settings/general.rs` | T9 only | no other task |
| `app/sidebar.rs` | T12 (wave 3), then T19 (wave 5, call sites only) | no other task; never concurrent |
| `crates/horizon-ui/src/main.rs`, `plugin_install.rs` | T15 only | no other task |
| `Cargo.toml` / `Cargo.lock` | T8 only | no other task |
| `app/panels.rs`, `app/panel_chrome.rs` | T16 only | no other task |

### Dependency graph

```
T0 ─┬─ T1 ─ T2 ─ T3 ─ T4            (track A, sequential: config/panel/runtime_state)
    ├─ T18                           (git overview core, new files only)
    ├─ T7 ─ T8                       (track B: claude_usage + Cargo.toml)
    └─ T13                           (track C: link pure core)

T3 ─ T5 (launcher)          T1 ─ T6 (settings Claude)
T2 ─ T10 (template core)    T2+T13 ─ T14 (socket server)    T13 ─ T15 (CLI + skill)

T5+T8 ─ T9 (chips)    T3+T10 ─ T11 (settings Workspaces)    T3+T10 ─ T12 (sidebar menu)
T14 ─ T16 (drain + panel menu + badge)
T3+T18 ─ T19 (sidebar git line + worktrees; after T12 and T16 in file order)      all ─ T17
```

### Waves

| Wave | Runs in parallel (one worktree each) | Why they do not collide |
|---|---|---|
| 0 | **Task 0** (wiring), on `feat/claude-hub` directly | single task |
| 1 | **A:** T1→T2→T3→T4 (sequential inside the track) ‖ **B:** T7→T8 ‖ **C:** T13 | A owns config/panel/runtime_state/agent_detect; B owns `claude_usage/` + `Cargo.toml`; C owns `link/` (pure files) |
| 2 | **T5** ‖ **T6** ‖ **T10** ‖ **T14** ‖ **T15** ‖ **T18** | T5: shortcuts/config.rs/palette/pickers/app/mod.rs; T6: `settings/claude.rs`; T10: `config/workspace_template.rs`; T14: `link/server.rs`, `panel/spawn.rs`, `board/link.rs`, `terminal.rs`; T15: `main.rs`, `link_cli.rs`, `plugin_install.rs`; T18: `git_overview/` (new) + the single `pub mod git_overview;` line in `lib.rs` — disjoint |
| 3 | **T9** ‖ **T11** ‖ **T12** | T9: config.rs (features), general.rs, root_chrome, toolbar, app/mod.rs, lifecycle; T11: `settings/workspaces.rs`, `actions/open_template.rs`; T12: `sidebar.rs`, `sidebar/workspace_menu.rs`, `actions/workspace_templates.rs` — disjoint |
| 4 | **T16** | needs app/mod.rs + lifecycle.rs after T9 merged |
| 5 | **T19** | touches `sidebar.rs` after T12, `hub_state.rs` and `lifecycle.rs` after T16; consumes T18 and T3 presets |
| 6 | **T17** | needs everything (also smokes Part E); single task |

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
| `crates/horizon-core/src/git_overview/{mod,model,compute,poller}.rs` (new) | repo + worktree overview domain (git2) |
| `crates/horizon-ui/src/app/sidebar/git_overview.rs` (new) | sidebar git line, worktrees list, menu, cache |

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
  - `pub(super) fn chip_label(u: &ProfileUsage, compact: bool) -> String` (`"claude2 · gutosjc  5h 37%  7d 12%"` — account identifier = local part of `ProfileUsage.email` before `@`, max 10 chars + `…`, omitted when email unknown; compact `"claude2 · guto… 7d 12%"` (identifier max 5 chars); expired adds `expired`) — USER REQUEST: the user cannot tell accounts apart by profile name alone
  - `pub(super) fn chips_width(count: usize, compact: bool) -> f32`
  - `ToolbarItem::UsageChips` participates in `RootToolbarLayout`: shown only when `features.claude_usage_bar` and ≥1 profile; compact before dropping; excess chips fall to the overflow menu
  - Interactions: left click → `launch_claude_profile(name)`; right click context menu "Refresh now" → `poller.refresh_now()`; tooltip text built lazily on hover only

- [ ] **Step 1: Failing tests:** `severity_thresholds_75_90`, `severity_api_critical_overrides`, `chip_label_dash_for_null_window`, `chip_label_expired_marker`, `chip_label_shows_email_local_part`, `chip_label_truncates_identifier`, `chip_label_without_email_omits_identifier`, `layout_hides_chips_when_disabled_or_no_profiles`, `layout_compacts_then_overflows_when_narrow`, `features_default_has_usage_bar_true_and_old_yaml_loads`.
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
- [ ] **Step 4b:** Part E smoke (T18/T19): sidebar shows the git line for a repo workspace (branch, ↑↓, dirty count) and none for a non-repo cwd; a linked worktree appears in the list with the `current` marker when applicable; click opens a shell in the worktree cwd (`pwd`); "Open Claude here" panel has the profile's `CLAUDE_CONFIG_DIR` and the worktree cwd; "New workspace from worktree" and "Copy path" work; `git commit` in the repo updates the line within ~15 s; idle trace shows no continuous repaint from the overview poller. Add the flow to the smoke plan and evidence to the report.
- [ ] **Step 5:** Delete the smoke plan file; update `docs/architecture/maintainability.md`; commit `docs: sync maintainability notes for claude hub` (smoke plan not committed).

### Task 18: Git overview core (domínio: ; tipo: regra-de-negocio)

**Files:**
- Create: `crates/horizon-core/src/git_overview/{mod.rs,model.rs,compute.rs,poller.rs}`
- Modify: `crates/horizon-core/src/lib.rs` (single line `pub mod git_overview;` plus re-exports are NOT added; API used by path `horizon_core::git_overview::…`)
- Test: inline `#[cfg(test)]` in `compute.rs` and `poller.rs`; temp repos built with `git2` (no shell-out, no `git` binary)

**Interfaces:**
- Consumes: `git2` (already a dependency, no default features), `thiserror`, `tracing`
- Produces:
  - `pub struct WorktreeInfo { pub path: PathBuf, pub branch: Option<String>, pub is_main: bool, pub is_current: bool, pub locked: bool, pub prunable: bool, pub dirty_count: usize }` (`branch: None` = detached; `Clone, Debug, PartialEq, Eq`)
  - `pub struct RepoOverview { pub repo_root: PathBuf, pub branch: Option<String>, pub detached: bool, pub dirty_count: usize, pub upstream: Option<String>, pub ahead: usize, pub behind: usize, pub worktrees: Vec<WorktreeInfo> }` (`Clone, Debug, PartialEq, Eq`; `repo_root` = main working tree; worktrees ordered main first, then by branch name)
  - `#[derive(Debug, thiserror::Error)] pub enum GitOverviewError { Git(#[from] git2::Error), Io(#[from] std::io::Error) }`
  - `pub fn compute_repo_overview(cwd: &Path) -> Result<Option<RepoOverview>, GitOverviewError>`: `Repository::discover`; not a repo → `Ok(None)`; bare repo → `Ok(None)`; unborn HEAD → `branch` from `HEAD` symbolic target, no upstream; ahead/behind via `graph_ahead_behind(local_oid, upstream_oid)` only when the branch has a configured upstream that resolves; `dirty_count` from `statuses` (include untracked, exclude ignored); worktrees from `Repository::worktrees()` + `find_worktree` (`is_locked`, `validate` → `prunable`) plus the main tree; each worktree's dirty count and branch opened with `Repository::open_from_worktree` (failure → that entry gets `dirty_count: 0`, never fails the whole call); `is_current` = canonicalized `cwd` starts with the canonicalized worktree path (longest match wins when worktrees nest)
  - `pub struct OverviewPoller` with `pub fn start(cwds: Vec<(K, PathBuf)>, interval: Duration) -> Self` generic over `K: Clone + Eq + Hash + Send + 'static` (the UI passes `WorkspaceId`), `pub fn set_cwds(&self, cwds: Vec<(K, PathBuf)>)`, `pub fn refresh_now(&self)`, `pub fn try_recv(&self) -> Option<(K, Result<Option<RepoOverview>, GitOverviewError>)>`, `Drop` signals stop and joins; single `std::thread`; default interval constant `pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15)`; wakes via `mpsc::Receiver::recv_timeout` on a command channel; skips sending a result equal to the previous one for that key; a panic-free loop (errors are sent as results, never logged per tick above `debug`)

- [ ] **Step 1: Failing tests:** `non_repo_returns_none`, `plain_repo_reports_branch_and_clean`, `dirty_count_counts_modified_and_untracked_not_ignored`, `detached_head_sets_detached_and_no_branch`, `unborn_head_reports_branch_without_error`, `upstream_ahead_behind_counts` (two commits ahead, one behind a local `refs/remotes/origin/main` fixture with `branch.main.remote/merge` config), `no_upstream_gives_zero_and_none`, `linked_worktree_listed_with_branch_and_dirty`, `cwd_inside_linked_worktree_marks_it_current` (and the main one not current), `locked_worktree_flagged`, `missing_worktree_dir_is_prunable_and_does_not_fail`, `poller_delivers_result_and_dedups_identical`, `poller_refresh_now_wakes_before_interval`, `poller_drop_stops_thread` (join returns within 2 s).
- [ ] **Step 2:** `CARGO_BUILD_JOBS=3 cargo test -p horizon-core git_overview` — FAIL (RED).
- [ ] **Step 3:** Implement model, compute, poller; no `unwrap`/`expect` in runtime code; files < 600 lines.
- [ ] **Step 4:** Same filtered test run — PASS (GREEN). `cargo fmt --all -- --check`. Full validation deferred per compile-budget ruling.
- [ ] **Step 5:** Commit `feat(core): add git overview with worktrees, ahead/behind and poller`.

### Task 19: Sidebar git line + worktrees list (domínio: frontend; tipo: ui)

**Files:**
- Create: `crates/horizon-ui/src/app/sidebar/git_overview.rs` (render, cache, menu, pure helpers)
- Modify: `app/sidebar.rs` (call sites only: one call after the header row in `render_sidebar_workspace`, `SidebarActions` fields for the new actions, handling in `apply_sidebar_actions`), `app/sidebar/` module declaration, `app/hub_state.rs` (fields `git_overviews: HashMap<WorkspaceId, GitLineCache>`, `git_poller: Option<OverviewPoller<WorkspaceId>>`, `git_expanded: HashSet<WorkspaceId>`, `git_version: u64`), `app/lifecycle.rs` (start/refresh poller from workspace cwds; drain `try_recv` in the existing per-frame drain point with a cap of 16 results; request repaint only when a result changed the cache), `app/actions/` only if a helper is needed to create the terminal/workspace
- Test: pure logic tests inside `git_overview.rs`

**Interfaces:**
- Consumes: T18 `RepoOverview`, `WorktreeInfo`, `OverviewPoller`; T3 derived presets `Claude · <profile>` and `profile_accent`; `HorizonApp::create_panel_with_options(PanelOptions, WorkspaceId)` (`app/actions/panels.rs:25`) with `PanelOptions.cwd = Some(worktree.path)`; `Workspace.cwd` (`horizon-core/src/workspace.rs:26`); `menu::context_menu_above_sidebar`; spec `docs/design/claude-hub-ui.md` §7 and §0 primitives
- Produces:
  - `pub(super) struct GitLineCache { overview_version: u64, branch_label: String, ahead: usize, behind: usize, dirty_count: usize, worktree_rows: Vec<WorktreeRow> }` and `struct WorktreeRow { path: PathBuf, label: String, short_path: String, tooltip: String, is_current: bool, dirty: bool, locked: bool, prunable: bool }`
  - `fn build_cache(overview: &RepoOverview, version: u64) -> GitLineCache`
  - `fn branch_label(branch: Option<&str>, detached: bool, head_short: Option<&str>) -> String` (`HEAD @ abc1234` when detached; middle-ellipsis above 22 chars)
  - `fn short_worktree_path(main_root: &Path, wt: &Path) -> String` (relative to the main repo parent: `.` for main, `../name` for siblings, absolute display with `~` for paths outside)
  - `fn sync_poller_cwds(workspaces: &[(WorkspaceId, PathBuf)], poller: &OverviewPoller<WorkspaceId>)` called only when the workspace set or a cwd changed
  - Actions: click row → shell terminal in worktree; menu **Open terminal here**, **Open Claude here ▸** (derived presets with cwd override; `PanelOptions.cwd` wins over the workspace cwd), **New workspace from worktree** (name = branch or dir name, cwd = worktree path), **Copy path**; prunable disables the first three; `refresh_now` on menu open and window focus
  - No git call on the UI thread and none per frame; strings only built in `build_cache`

- [ ] **Step 1: Failing tests:** `branch_label_detached_shows_short_sha`, `branch_label_middle_ellipsis_over_22`, `short_path_main_is_dot`, `short_path_sibling_is_relative_to_repo_parent`, `short_path_outside_uses_tilde_or_absolute`, `build_cache_orders_main_first_and_marks_current`, `build_cache_hides_ahead_behind_when_zero`, `no_cache_entry_for_non_repo_workspace` (`Ok(None)` removes the entry), `identical_overview_does_not_bump_version`.
- [ ] **Step 2:** `CARGO_BUILD_JOBS=3 cargo test -p horizon-ui sidebar::git_overview` — FAIL (RED).
- [ ] **Step 3:** Implement helpers, cache and drain; then paint per spec §7 (git line, toggle, rows, current bar, tooltip, menu) using `theme.rs` constants by name; keep `sidebar.rs` edits to call sites (sidebar.rs is above the size guideline: new code stays in the new module).
- [ ] **Step 4:** Same filtered test run — PASS (GREEN). `cargo fmt --all -- --check`. Record "visual validation deferred to final smoke" in the report (Task 17 Step 4b covers it).
- [ ] **Step 5:** Commit `feat(ui): sidebar git line and worktrees list with open-here actions`.

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
10. **Scope** — large (20 tasks). If the session runs out, stop after a part boundary (A, C, B, D, E are independently shippable in that order; E needs A's T3 for "Open Claude here"); `/mapear` was the alternative.

---

## Part F — Status, sessões, worktrees, Auto, link nível 2, prompts, prefs, rename

Extensão pedida pelo usuário após o smoke. Mesmas regras de `## Global Constraints` e dos controller rulings vigentes: build liberado (UM processo cargo por worktree, `CARGO_BUILD_JOBS=4`, `free -g` ≥ 4 GB antes); por task: teste focado RED→GREEN, `cargo fmt --all -- --check`, e ao final da task `RUSTFLAGS="-D warnings" cargo test --workspace` + os dois tiers de clippy bloqueantes (`cargo clippy --all-targets --features trace-profiling -- -D warnings` e `cargo clippy --workspace --lib --bins --examples -- -D warnings -D clippy::unwrap_used -D clippy::expect_used`; sem `--features speech` local, anotar no report). Estado novo da UI vive em `HubState` (nunca campos soltos em `app/mod.rs`); sem `ComboBox` em modais do hub (seletores inline); nunca commitar `.superpowers/` nem `Cargo.lock`; nenhum `unwrap`/`expect` em runtime; segredos/prompt do usuário nunca logados. Visual: `docs/design/claude-hub-ui.md` §8–§14, constantes por nome de `theme.rs` (se faltar constante, derivar com `theme::blend`/`theme::alpha` de uma existente; não editar `theme.rs`). Commits terminam com `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

**Pré-condição da Parte F:** T21 (titlebar do painel: `panel_chrome.rs`, `panels.rs`, `claude_quick_start.rs`) e T22 (sidebar: `sidebar.rs`, `sidebar/*`) mergeados em `feat/claude-hub`. Só T27 e T29 (wave F1) não dependem deles e podem ser cortados enquanto T21/T22 ainda estão em worktree.

**F8 (painéis novos nunca se sobrepõem):** tratado pelo smoke T17 em curso; esta parte só referencia. Os painéis criados por T24/T25 (resume, Claude no worktree) usam o mesmo caminho de criação de painel já validado pelo T17, sem posicionamento próprio.

### Mapa F-item → task

| Item | Task | Domínio |
|---|---|---|
| F1 status por painel | T23 | OSC `HORIZON_STATUS`, hooks do plugin, dot, toast |
| F2 sessões recentes | T24 | scanner JSONL + índice de contas + lista/resume |
| F3 worktree + Claude, limpeza | T25 | git2 ops + diálogos |
| F4 conta Auto + alertas de uso | T26 | seleção pura + rastreador de cruzamento |
| F5 link nível 2 | T27 | broadcast, ask/reply, linha visual |
| F6 prompts salvos | T28 | config + inserção + Settings |
| F7 prefs de UI + minimap auto-hide | T29 | runtime_state + overlay |
| F9 rename do painel | T30 | auditoria + nome custom em sidebar/link |

---

### Task 23: Status do agente por painel — hooks, OSC, dot e notificação (domínio: frontend, integracao; tipo: regra-de-negocio (núcleo) + ui; review: seguranca)

**Contexto verificado:** `terminal/events.rs` já interpreta `Event::Title` com prefixo `HORIZON_NOTIFY:`/`HORIZON_TITLE:` (`Terminal::parse_horizon_title` → `HorizonOscTitle::{Notification,SetTitle,ClearTitle,Ignore}`) e `attention.rs` já tem `AttentionItem`/`is_agent_ready_for_input`. O plugin Claude (`HorizonHome::claude_plugin_dir()`, embutido em `plugin_install.rs`) hoje só tem skills; o plugin pode trazer `hooks/hooks.json`. Sem polling: o estado é empurrado pelo hook.

**Files:**
- Create: `crates/horizon-core/src/agent_status.rs` (`AgentStatus`, `StatusEvent`, parse, regras de transição, política de atenção) — lib.rs: **uma linha** `pub mod agent_status;` (T23 é o dono de `lib.rs` na wave F2)
- Create: `crates/horizon-ui/src/hook_cli.rs` (subcomando `horizon hook <evento>`), `assets/plugins/claude-code/hooks/hooks.json.tmpl` (modelo; o instalador renderiza com o caminho absoluto do executável)
- Create: `crates/horizon-ui/src/app/agent_status_ui.rs` (paint do dot, helpers puros)
- Modify: `terminal/events.rs` + `terminal.rs` (novo `HorizonOscTitle::Status`, campo `pending_status` drenável), `panel.rs` + novo `panel/agent_status.rs` (campo `agent_status: AgentStatusState`, `apply_status_event`, `clear_on_exit`; panel.rs só ganha o campo e uma chamada — a lógica vai ao módulo novo), `main.rs` (despacho `argv[1]=="hook"` antes do eframe, junto do `link`), `plugin_install.rs` (renderizar/sincronizar `hooks/hooks.json`), `app/panel_chrome.rs` e `app/panels.rs` (somente call site do dot na titlebar, à esquerda do medidor de histórico/botão Claude), `app/sidebar/rows.rs` (dot na linha do painel; **não** tocar `sidebar.rs`), `app/hub_state.rs` (um campo `status_ui: StatusUi`), `app/lifecycle.rs` só se a drenagem de toasts precisar de um ponto por frame (preferir o ponto existente de drenagem de atenção)
- Test: inline nos módulos novos; `crates/horizon-core/tests/agent_status.rs` para transições

**Interfaces:**
- Produces (core): `pub enum AgentStatus { Unknown, Working, Waiting, Done, Error }`; `pub struct StatusEvent { pub state: AgentStatus, pub session_id: Option<String> }`; `pub fn parse_status_osc(body: &str) -> Option<StatusEvent>` para `HORIZON_STATUS:<working|waiting|done|error|idle|end>[:<session_id>]` (estado desconhecido/`session_id` fora de `[A-Za-z0-9-]{8,64}` → `None`, ignorado; nunca panic); `pub fn attention_policy(prev, next, panel_focused) -> StatusAttention` (`None | Notify{severity}`: `Waiting` → `High`, `Done` após `Working` → `Medium`, `Error` → `High`; sem atenção se o painel está focado/visível ou se `prev == next`); `Panel::agent_status() -> AgentStatus`, `Panel::take_status_event()`; ao sair do processo/`agent_detect` perder o agente → `Unknown` (sem timer); `end` → `Unknown`; restore de sessão → `Unknown` (status não é persistido)
- Produces (ui): `horizon hook <working|waiting|done|error|session-start|session-end>`: lê stdin (≤ 64 KiB, JSON, só extrai `session_id`, ignora o resto, nada logado), exige `HORIZON_PANEL_ID` no env (senão sai 0 em silêncio — protege Claude fora do Horizon), abre `/dev/tty` e escreve `ESC ] 2 ; HORIZON_STATUS:<estado>[:<session_id>] BEL`; **sempre exit 0** e termina em < 100 ms; sem `/dev/tty` → exit 0 sem efeito (cfg(unix); no-op nas outras plataformas); `pub fn render_hooks_json(exe: &Path) -> String` (caminho com aspas simples seguras, JSON válido; eventos `UserPromptSubmit`/`PreToolUse`→working, `Notification`→waiting, `Stop`→done, `StopFailure`→error, `SessionStart`/`SessionEnd`; `timeout: 5`); `agent_status_ui::paint_status_dot(painter, center, radius, status, seen)` + `status_label(status) -> &'static str` (tooltip/a11y)
- Consumes: `HubToasts` (criado pelo T27 na F1), `AttentionItem` (fonte `"claude-status"`, resolve no próximo `Working`), `PanelChrome` (T21), rows (T22), `claude_plugin_dir()`/`sync_file_if_changed` (T15)

- [ ] **Step 1: Failing tests (core):** `parse_status_accepts_known_states`, `parse_status_rejects_unknown_state_and_bad_session_id`, `parse_status_oversized_body_is_ignored`, `working_then_done_notifies_medium_when_unfocused`, `waiting_notifies_high`, `focused_panel_does_not_notify`, `same_state_twice_is_deduped`, `process_exit_resets_to_unknown`, `done_marks_unseen_until_panel_focused`, `status_osc_does_not_break_horizon_notify_title_parsing` (regressão dos testes existentes de `parse_horizon_title`), `terminal_event_status_populates_pending_status`.
- [ ] **Step 2: Failing tests (ui):** `render_hooks_json_is_valid_json_and_quotes_exe_with_spaces_and_quotes`, `render_hooks_json_maps_events`, `plugin_install_writes_rendered_hooks_json`, `plugin_install_hooks_json_updates_when_exe_path_changes`, `hook_cli_parses_session_id_from_stdin_json`, `hook_cli_ignores_malformed_stdin`, `hook_cli_noop_without_panel_id_env`, `hook_osc_bytes_match_protocol`, `status_dot_shape_per_state_is_distinct` (geometria pura: working=anel, waiting=ponto+halo, done=ponto+tick, error=ponto+x).
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core agent_status` e `-p horizon-ui hook_cli agent_status_ui hub_toast plugin_install` — FAIL (RED).
- [ ] **Step 4:** Implementar núcleo → CLI/hooks → dot/toast. Eventos de status entram pelo mesmo ponto de drenagem das notificações (`Notification`→atenção): nenhuma thread, nenhum timer. Toast e `AttentionItem` só quando `attention_policy` manda (não spammar: `PreToolUse` repete `Working` → dedupe). Dot: titlebar 4.5 px de raio, sidebar 3.5 px, conforme §8.
- [ ] **Step 5:** mesmos filtros — PASS (GREEN); fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; os dois clippy; `node ~/.claude/scripts/gate-seguranca.js` (stdin externo, escrita em `/dev/tty`, JSON do hook: tamanho, tipo, sem eco do conteúdo).
- [ ] **Step 6:** Verificação real (Task 17 ampliado): iniciar Claude num painel Horizon com o plugin, enviar prompt, ver working→done; disparar permissão, ver waiting + toast + atenção; sair do Claude, ver dot sumir. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-23-visual/`. Claude iniciado manualmente **sem** `--plugin-dir` não reporta status (limitação documentada no report e no skill).
- [ ] **Step 7:** Commit `feat(ui): per-panel agent status via Claude hooks with dot and toast`.

---

### Task 24: Sessões recentes do Claude por projeto/workspace e resume em 1 clique (domínio: frontend, integracao; tipo: regra-de-negocio (núcleo) + ui; review: seguranca)

**Contexto verificado:** transcripts ficam em `~/.claude/projects/<cwd codificado>/*.jsonl`, compartilhados entre todas as contas por symlink (`usage_stats/claude.rs` já varre esse diretório); logo o caminho **não** diz qual conta rodou a sessão — a atribuição vem de um índice próprio do Horizon. `runtime_state/claude_live_sessions.rs` e `agent_sessions.rs` já ligam painel↔session id; T23 passa a entregar o `session_id` via `HORIZON_STATUS`.

**Files:**
- Create: `crates/horizon-core/src/claude_sessions/{mod,scan,index,cache}.rs`; `crates/horizon-ui/src/app/claude_sessions.rs` (modal + helpers) e `app/claude_sessions/rows.rs` se > ~500 linhas
- Modify: `lib.rs` (uma linha `pub mod claude_sessions;`; T24 é o dono de `lib.rs` na wave F4), `app/hub_state.rs` (campo `sessions_ui: SessionsUi`), `app/lifecycle.rs` (registrar `session_id` do `StatusEvent` no índice — chamada, não lógica), `app/claude_quick_start.rs` (passo "Resume…" ganha "Recent sessions…"; novo modo `ResumeId(String)` em `quick_start_command` + teste), `app/claude_launcher.rs` (entrada "Recent sessions…"), `app/sidebar/workspace_menu/*` (item **Recent Claude sessions…** — dono único na wave F4), `app/actions/command_palette.rs` (entrada `Claude: Recent sessions…`; dono único na F4)
- Test: inline + `crates/horizon-core/tests/claude_sessions.rs` (projeto temporário com JSONL sintéticos)

**Interfaces:**
- Produces (core): `pub fn encode_project_dir(cwd: &Path) -> String` (todo caractere fora de `[A-Za-z0-9]` vira `-`; **validar o algoritmo contra os diretórios reais de `~/.claude/projects` no Step 1** e fixar a amostra no teste); `pub struct SessionSummary { id: String, title: String, first_prompt: String, updated: SystemTime, size: u64, cwd: PathBuf, account: SessionAccount }`; `pub enum SessionAccount { Profile(String), Unknown }`; `pub struct SessionScanner` (thread única + `mpsc`, como `OverviewPoller`): `request(project_dirs: Vec<PathBuf>)`, `try_recv() -> Option<ScanResult>`; leitura **lazy**: lista por `read_dir` + mtime, e só lê o **início** (≤ 64 KiB) de cada JSONL novo/alterado para título (`summary` do Claude se existir) e primeiro prompt do usuário (ignora linhas `isMeta`, `<command-…>`, `<local-command-…>`, tool results), cache por `(path, mtime, len)`, teto de 200 sessões por projeto (mais recentes) e 1 000 por varredura; linhas inválidas/arquivo truncado/UUID inválido → pula, nunca panic; nada de conteúdo em log
  - `pub struct AccountIndex` em `~/.horizon/claude-session-accounts.json` (`{"version":1,"sessions":{"<uuid>":{"profile":"conta2","config_dir":"/home/u/.claude-conta2","ts":1731400000}}}`): `load(path)`, `record(session_id, profile, config_dir, ts)`, `lookup(id) -> Option<&Entry>`, `save_atomic()` (tmp+rename, modo 0600, teto 5 000 entradas descartando as mais antigas), tolera arquivo ausente/corrompido (começa vazio, renomeia o corrompido para `.bad`), gravação fora da thread de UI (writer em thread própria com canal)
  - `pub fn resume_command_args(session_id: &str) -> Result<Vec<String>, SessionError>` (`["--resume", id]`, id deve ser UUID — rejeita qualquer outra coisa antes de montar comando)
- Produces (ui): `SessionsUi` (estado do modal; lista cacheada por `scan_version`), `fn open_sessions(workspace)`; modal §10; "Resume" cria **novo painel** com preset derivado do perfil + `--resume <id>` e `cwd` do projeto (mesmo caminho de "Open Claude here" do T19/T25; F8 garante posicionamento), ou "Resume here" que escreve o comando no shell ativo via `quick_start_command(..., ResumeId)` (T21); conta `Unknown` → passo inline "Which account?" (chips de perfis, sem ComboBox) antes do resume, e a escolha grava no índice (`record`)
- Consumes: `StatusEvent.session_id` (T23), `ClaudeProfile`/presets derivados (T1/T3), `git_overview` (worktrees do repo também entram na lista: dirs codificados de cada worktree), `HubToasts` (T27)

- [ ] **Step 1: Failing tests (core):** `encode_project_dir_matches_real_claude_dirs` (amostras reais `/home/axcel/horizon-fork` → `-home-axcel-horizon-fork`, `/a/b.c/d_e`), `scan_lists_sessions_newest_first`, `scan_title_prefers_summary_then_first_user_prompt`, `scan_skips_meta_and_command_lines`, `scan_reads_only_head_bytes`, `scan_cache_hit_when_mtime_and_len_unchanged`, `scan_cache_invalidates_on_mtime_change`, `scan_tolerates_truncated_and_binary_lines`, `scan_caps_sessions_per_project`, `scan_ignores_non_uuid_filenames`, `index_roundtrip_and_mode_0600`, `index_corrupt_file_starts_empty_and_renames_bad`, `index_caps_entries_dropping_oldest`, `index_atomic_save_leaves_no_partial_file`, `resume_args_reject_non_uuid_and_injection`, `account_attribution_uses_index_else_unknown`.
- [ ] **Step 2: Failing tests (ui):** `quick_start_command_resume_id_quotes_dir_and_validates_uuid`, `session_row_label_sanitizes_control_chars_and_truncates`, `relative_date_label_buckets` (`just now`, `5 min`, `yesterday`, data), `sessions_list_cache_rebuilds_only_on_scan_version`, `unknown_account_requires_account_pick_before_resume`.
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core claude_sessions` / `-p horizon-ui claude_sessions claude_quick_start` — RED.
- [ ] **Step 4:** Implementar núcleo → índice → UI. A varredura só dispara ao abrir o modal (e em focus do modal ao mudar de workspace); resultado volta por canal; a UI nunca lê disco por frame.
- [ ] **Step 5:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2; `node ~/.claude/scripts/gate-seguranca.js` (JSONL do usuário é input não confiável: limites de leitura, sanitização do prompt exibido, UUID estrito no comando, índice 0600).
- [ ] **Step 6:** Verificação real: criar 2 sessões em contas diferentes pelo Horizon, abrir o modal, ver conta atribuída; sessão antiga sem índice → pede conta; resume abre painel com `--resume`. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-24-visual/`.
- [ ] **Step 7:** Commit `feat(ui): recent Claude sessions per workspace with one-click resume`.

---

### Task 25: Worktree + Claude em 1 clique e limpeza segura (domínio: frontend; tipo: regra-de-negocio (núcleo) + ui; review: seguranca)

**Contexto verificado:** `git_overview` (T18) já lista worktrees com `dirty_count`, `locked`, `prunable`, `is_main`, `is_current` via git2 no poller; T19/T22 deram a linha git/menus na sidebar. Aqui entram **operações** (criar/remover) — sempre em thread de trabalho, nunca na UI, nunca shell-out.

**Files:**
- Create: `crates/horizon-core/src/git_overview/worktree_ops.rs` (+ `worktree_ops/tests.rs` se > ~500 linhas), `crates/horizon-ui/src/app/worktree_dialog.rs` (modais New/Cleanup)
- Modify: `git_overview/mod.rs` (declara o módulo; **sem** `lib.rs`), `app/hub_state.rs` (campo `worktree_ui: WorktreeUi`), `app/sidebar.rs` (**dono único na F2**: `SidebarActions` + `apply_sidebar_actions` para as novas ações), `app/sidebar/git_overview/*` (itens de menu na linha git e na linha de worktree; `paint.rs` só se o menu exigir), `app/sidebar/workspace_menu/*` (itens **New worktree + Claude…** e, se T27 já mergeou, **Send to all Claudes…** abrindo `SendToPanelState::open_for_workspace`), `app/lifecycle.rs` (drenar resultados do worker de ops; chamada apenas)
- Test: inline; `crates/horizon-core/tests/worktree_ops.rs` com repos temporários git2

**Interfaces:**
- Produces (core): `pub fn plan_worktree_path(main_root: &Path, branch: &str, exists: impl Fn(&Path) -> bool) -> PathBuf` (irmão do repo: `<repo>-<slug(branch)>`, `/`→`-`, sufixo `-2`,`-3` se existir); `pub fn validate_branch_name(name: &str) -> Result<(), WorktreeError>` (`git2::Branch::name_is_valid`, rejeita vazio, prefixo `-`, `..`, controle, > 200); `pub fn create_worktree(repo_root: &Path, req: &CreateRequest) -> Result<Created, WorktreeError>` (`CreateRequest { branch, base: Option<String>, path: PathBuf }`; branch existente não ocupada → checkout dela; existente já ocupada em outro worktree → `BranchCheckedOut{path}`; inexistente → cria de `base` (padrão HEAD) e faz o worktree; destino existente → erro; falha após criar o branch → remove o branch criado por nós, sem deixar lixo); `pub fn classify_cleanup(repo_root: &Path, protected_paths: &[PathBuf]) -> Result<Vec<CleanupCandidate>, WorktreeError>`; `pub struct CleanupCandidate { path, branch, reason: CleanupReason, blockers: Vec<Blocker> }` com `CleanupReason::{Merged, Prunable, Orphan}` (Merged = ponta do branch é ancestral do branch padrão — `origin/HEAD` → `main`/`master` — ou do upstream; Prunable = git2 `is_prunable`; Orphan = registrado mas branch deletado e árvore limpa) e `Blocker::{Dirty, Locked, Main, ProtectedByWorkspace, UnreachableCommits, DefaultBranchUnknown}`; **`safe() == blockers.is_empty()`**; `pub fn remove_worktrees(repo_root: &Path, items: &[PathBuf], delete_merged_branches: bool) -> Vec<RemoveOutcome>` — **reclassifica cada item imediatamente antes de remover** (TOCTOU: dirty/locked entre o diálogo e o clique), recusa caminho que não esteja em `repo.worktrees()` (canonicalizado) ou que seja o principal, usa `WorktreePruneOptions` sem `locked(true)`/`valid(true)` forçados, apaga branch só se `Merged` e a flag está ligada, uma falha não aborta os demais; **nunca** apaga worktree dirty/locked mesmo se o chamador pedir
- Produces (ui): `WorktreeUi` (estado dos modais, worker via thread + `mpsc`, resultados com cap por frame); `fn open_new_worktree(workspace, branch_hint)`; ação "criar + abrir Claude": após `Created`, abre painel Claude com preset derivado (conta escolhida inline; "Auto" entra com T26) com `cwd` = worktree, opcional "also create workspace" (padrão ligado quando o worktree está fora do cwd do workspace); `HubToasts` para sucesso/erro
- Consumes: `RepoOverview`/`WorktreeInfo` (T18), `OverviewPoller::refresh_now` (T19) após cada operação, ações "Open Claude here" (T19), `HubToasts` (T27), `SendToPanelState::open_for_workspace` (T27, opcional)

- [ ] **Step 1: Failing tests (core):** `plan_path_is_sibling_with_slugged_branch`, `plan_path_appends_suffix_when_taken`, `validate_branch_rejects_bad_names`, `create_checks_out_existing_branch`, `create_new_branch_from_base`, `create_existing_branch_checked_out_elsewhere_errors`, `create_failure_rolls_back_created_branch`, `classify_marks_merged_branch`, `classify_unmerged_branch_not_candidate`, `classify_blocks_dirty_including_untracked`, `classify_blocks_locked`, `classify_never_lists_main_worktree`, `classify_blocks_protected_by_open_workspace`, `classify_blocks_unreachable_detached_commits`, `classify_prunable_when_dir_missing`, `classify_default_branch_unknown_blocks_merged_reason`, `remove_rechecks_dirty_before_removing`, `remove_refuses_unregistered_path_and_traversal` (`../`, symlink para fora), `remove_keeps_unmerged_branch`, `remove_deletes_merged_branch_only_when_flag`, `remove_partial_failure_continues_others`.
- [ ] **Step 2: Failing tests (ui):** `new_worktree_form_validation_messages`, `cleanup_selection_excludes_blocked_rows`, `cleanup_confirm_label_counts_selected`, `worktree_result_drain_respects_cap`.
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core worktree_ops` / `-p horizon-ui worktree_dialog` — RED.
- [ ] **Step 4:** Implementar núcleo (somente git2) → worker → modais §11 e itens de menu. Cleanup mostra caminhos absolutos exatos e motivo; linhas bloqueadas ficam desabilitadas com o motivo; remover exige o botão destrutivo (sem checkbox pré-marcado para nada que não seja `safe()`; `safe()` marcados por padrão).
- [ ] **Step 5:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2; `node ~/.claude/scripts/gate-seguranca.js` (remoção de diretório: lista fechada de caminhos vindos do `git2`, canonicalização, TOCTOU, nenhum shell).
- [ ] **Step 6:** Verificação real em repo temporário: criar worktree+Claude, tornar um dirty/outro locked/outro merged, abrir limpeza e confirmar que só o merged limpo é removido. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-25-visual/`.
- [ ] **Step 7:** Commit `feat(ui): one-click worktree with Claude and safe worktree cleanup`.

---

### Task 26: Conta "Auto" e alertas de uso semanal (domínio: frontend, integracao; tipo: regra-de-negocio (núcleo) + ui)

**Files:**
- Create: `crates/horizon-core/src/claude_usage/auto_pick.rs`, `crates/horizon-core/src/claude_usage/alerts.rs` (declarados em `claude_usage/mod.rs`, **não** `lib.rs`)
- Modify: `app/claude_launcher.rs` (linha **Auto** no topo), `app/claude_quick_start.rs` (linha **Auto** no 1º passo; dono único na F3), `app/worktree_dialog.rs` (chip Auto no seletor de conta, T25 já mergeado), `app/hub_state.rs` (campo `alerts: UsageAlertState`), `app/lifecycle.rs` (no ponto onde snapshots de uso já são drenados: `tracker.observe` e `toasts.push`; dono único na F3)
- Test: inline + `crates/horizon-core/tests/claude_usage_auto.rs`

**Interfaces:**
- Produces: `pub struct ProfileUsageView { pub name: String, pub state: UsageState, pub weekly_pct: Option<f32>, pub five_hour_pct: Option<f32>, pub resets_at: Option<SystemTime>, pub order: usize }` (projeção do snapshot do T7/T8, sem token); `pub fn pick_auto(views: &[ProfileUsageView]) -> Option<AutoPick>` com `AutoPick { name, remaining_pct: f32, reason: AutoReason }` — maior `100 - weekly_pct`; ignora `Expired`/`Missing`/`NoLogin`/erro; empate → menor `five_hour_pct`, depois ordem do config; sem nenhum dado de uso mas com credencial válida → primeiro perfil logado (`reason: NoUsageData`); nenhum utilizável → `None` (UI desabilita Auto e explica); `pub enum AlertLevel { Warn80, Crit90 }`, `pub struct AlertTracker`, `observe(&mut self, name: &str, weekly_pct: Option<f32>, window_resets_at: Option<SystemTime>) -> Option<AlertLevel>`: dispara **uma vez por cruzamento** (subir de <80 para ≥80 → `Warn80`; de <90 para ≥90 → `Crit90`; 80→90 dispara `Crit90` mesmo se `Warn80` já foi emitido); rearma ao cair abaixo de 75 (histerese) ou quando `window_resets_at` muda (nova janela); primeira observação de um perfil só define a linha de base (sem toast ao abrir o app já acima de 80; os chips já mostram a cor); `None`/estado não-ok não dispara nem rearma; estado puramente em memória
- UI: Auto resolve **no clique** (reavalia `pick_auto` com o snapshot vigente, nunca um valor velho da linha); a linha mostra `Auto → conta2 · 66% left` (string cacheada por `usage_version`); toast §12 via `HubToasts` (T27) com `conta1 · 80% of weekly limit · resets in 2d 4h` (amarelo 80, vermelho 90); sem som, sem modal
- Consumes: snapshot/poller (T8/T9), launcher (T5), quick-start (T21/T28), `HubToasts` (T27)

- [ ] **Step 1: Failing tests (core):** `auto_picks_most_remaining_weekly`, `auto_skips_expired_missing_and_no_login`, `auto_tie_breaks_on_five_hour_then_config_order`, `auto_without_usage_data_picks_first_logged_in`, `auto_none_when_nothing_usable`, `auto_ignores_profile_with_weekly_100`, `alert_fires_once_at_80`, `alert_fires_crit_when_jumping_to_90`, `alert_80_then_90_fires_both_once`, `alert_does_not_refire_while_above`, `alert_rearms_below_75`, `alert_rearms_on_new_window`, `alert_first_observation_is_baseline_only`, `alert_ignores_unknown_pct`, `alert_state_is_per_profile`.
- [ ] **Step 2: Failing tests (ui):** `auto_row_label_formats_remaining_and_reason`, `auto_row_disabled_when_none_usable`, `alert_toast_text_formats_level_and_reset`.
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core claude_usage_auto auto_pick alerts` / `-p horizon-ui claude_launcher claude_quick_start` — RED.
- [ ] **Step 4:** Implementar núcleo puro → integrar launcher/quick-start/worktree dialog → alertas no ponto existente de drenagem de snapshots (sem novo poller, sem timer).
- [ ] **Step 5:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2.
- [ ] **Step 6:** Verificação real com cache de uso forjado (isolated `HOME`): Auto escolhe a conta certa; forçar 82%→91% e ver dois toasts; re-render não repete. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-26-visual/`.
- [ ] **Step 7:** Commit `feat(ui): Auto Claude account and weekly usage alerts`.

---

### Task 27: Link nível 2 — broadcast, ask/reply e linha visual (domínio: integracao, frontend; tipo: regra-de-negocio; review: seguranca)

**Contexto verificado:** o link atual (T13–T16) é uma linha de requisição → uma linha de resposta por conexão (`link/protocol.rs`, `link/server.rs`, `board/link.rs::handle_link_request`, `link_cli.rs`, `app/link.rs`, `app/link/send_dialog.rs`). `ask` precisa que o servidor **segure a conexão do solicitante** até chegar o `reply`.

**Files:**
- Modify: `crates/horizon-core/src/link/protocol.rs` (novos requests/respostas), `link/server.rs` (limite de conexões e espera com timeout), `link/resolve.rs` (alvo `AllInWorkspace`), `board/link.rs` (broadcast + cabeçalho de ask; `Board::agent_panels_in_workspace(ws)`), `crates/horizon-ui/src/link_cli.rs` (`ask`, `reply`, `send --all-in-workspace`), `app/link.rs` (tabela de asks pendentes + drenagem), `app/link/send_dialog.rs` (linha "All Claudes in <workspace>"), `app/hub_state.rs` (campos `link_asks: AskTable`, `link_visuals: Vec<LinkVisual>`, `toasts: HubToasts`), `app/workspace.rs` (call site da linha visual na passada de canvas, depois dos painéis; **se só for alcançável por `panels.rs`, devolver NEEDS_CONTEXT e deixar a linha para depois do T21**), `assets/plugins/claude-code/skills/horizon-link/SKILL.md`
- Create: `crates/horizon-ui/src/app/link/visual.rs` (geometria pura + paint); `crates/horizon-ui/src/app/hub_toast.rs` (fila de toasts do hub, reaproveitada por T23/T24/T25/T26/T28; render em `Order::Tooltip` canto inferior direito, §8)
- Test: unit + `crates/horizon-core/tests/link_socket.rs` (estende o existente com ask/reply/broadcast)

**Interfaces:**
- Produces (protocolo, uma linha JSON por mensagem, mesmos limites de 64 KiB e sanitização do T13): `LinkRequest::Send { target: SendTarget, text, enter }` com `SendTarget::{Panel(String), AllInWorkspace}` (o workspace é o do remetente, derivado de `HORIZON_PANEL_ID`; remetente excluído; só painéis de agente — Claude/Codex detectados —; teto de 16 alvos; resposta `Sent { delivered: Vec<String>, skipped: Vec<String> }`); `LinkRequest::Ask { from, target, text, timeout_secs }` (`timeout_secs` padrão 120, **máx. 600**, mín. 5) → resposta final `LinkResponse::Reply { from, text }` ou erro `AskTimeout`, `AskTargetClosed`, `AskCancelled`; `LinkRequest::Reply { from, ask_id, text }` → `Ok` ou `UnknownAsk`/`NotAddressee`/`AskExpired`; mensagem entregue ao alvo: `[ask <id> from <nome>] <pergunta>\n(reply with: horizon link reply <id> "<answer>")` — id = 6 hex estáveis por pedido; só o painel alvo pode responder (checa `from == target`); auto-ask recusado; no máximo 8 asks pendentes por solicitante e 32 no total; conexões simultâneas do servidor limitadas (16) para não esgotar threads; thread de conexão do ask espera `recv_timeout(timeout+2 s)` e escreve erro de timeout; ao fechar o painel alvo ou o solicitante, a UI resolve as pendências (`AskTargetClosed`/`AskCancelled`); varredura de expirados por frame com cap
- CLI: `horizon link ask <target> "pergunta" [--timeout 120]` (imprime a resposta em stdout, exit 0; timeout/erro em stderr, exit 1/2 distintos), `horizon link reply <ask-id> "resposta"`, `horizon link send --all-in-workspace "texto" [--no-enter]`; `parse_link_args` estendido
- Skill: quando usar `send` (fire-and-forget) vs `ask` (bloqueia o turno até 120 s — usar para perguntas curtas e objetivas), responder um ask recebido com `horizon link reply` (ao ver `[ask …]` no input), identificar-se com `$HORIZON_PANEL_NAME`, nunca enviar segredos, broadcast só com aviso curto e raro
- Produces (toast): `HubToasts::push(Toast{ kind: Info|Warn|Error, text, panel: Option<PanelId> })` / `drain_visible(now)` (máx. 3 visíveis, TTL 6 s, dedupe por `(kind, text)`), `paint_toasts` chamado uma vez por frame de `app/root_chrome`-equivalente que já pinta overlays do hub (call site único; dono T27); T27 usa o toast para o resultado do broadcast (`Sent to 3, skipped 1`).
- UI: `LinkVisual { from: PanelId, to: PanelId, started: Instant, kind: Send|Ask|Reply }`; linha transitória §13 com fade (reaproveita a janela 2.3 s+0.7 s do badge), ask pendente = linha estática até resolver (sem animação contínua; repaint só nas transições); `SendToPanelState::open_for_workspace(ws)` seleciona o alvo "All Claudes"
- Consumes: protocolo/servidor (T13/T14), diálogo (T16), `Board::handle_link_request`, detecção de agente (`agent_detect`)

- [ ] **Step 1: Failing tests (core):** `parse_ask_clamps_timeout_range`, `ask_message_header_contains_id_and_reply_hint`, `ask_text_is_sanitized_like_send`, `broadcast_excludes_sender_and_non_agent_panels`, `broadcast_caps_at_16_targets`, `broadcast_reports_delivered_and_skipped`, `reply_requires_addressee`, `reply_unknown_or_expired_ask_errors`, `ask_to_self_rejected`, `ask_table_caps_per_asker_and_total`, `closing_target_resolves_pending_ask_with_error`, `expired_asks_are_swept`, `server_limits_concurrent_connections`, integração `link_socket_ask_reply_roundtrip` (cliente A pergunta, UI-loop entrega, cliente B responde, A recebe), `link_socket_ask_times_out_cleanly`.
- [ ] **Step 2: Failing tests (ui):** `parse_ask_with_timeout_flag`, `parse_reply`, `parse_send_all_in_workspace`, `parse_ask_rejects_timeout_over_600`, `ask_exit_codes_distinguish_timeout`, `plugin_install_horizon_link_skill_mentions_ask_and_reply`, `link_visual_endpoints_clip_to_panel_edges`, `link_visual_alpha_timeline`, `toasts_dedupe_and_cap_three`, `toast_expires_after_ttl`, `send_dialog_all_claudes_row_count`.
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core link` / `-p horizon-ui link` — RED.
- [ ] **Step 4:** Implementar protocolo/Board → servidor → CLI/skill → UI. A thread de conexão **só** espera num canal; nenhum acesso ao Board fora da UI.
- [ ] **Step 5:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2; `node ~/.claude/scripts/gate-seguranca.js` (injeção por ask/broadcast, esgotamento de threads/memória, spoof de `from`, vazamento de pendências).
- [ ] **Step 6:** Verificação real: dois Claudes num workspace; A faz `ask`, B responde, A segue; broadcast para 3; timeout de ask. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-27-visual/`.
- [ ] **Step 7:** Commit `feat(core): link ask/reply and workspace broadcast with visual link line`.

---

### Task 28: Prompts salvos — config, inserção e Settings (domínio: frontend; tipo: regra-de-negocio (núcleo) + ui)

**Files:**
- Create: `crates/horizon-core/src/config/prompts.rs` (`SavedPrompt`, validação, merge), `crates/horizon-ui/src/app/prompts.rs` (inserção, estado, helpers puros), `crates/horizon-ui/src/app/settings/prompts_section.rs` (editor reutilizável)
- Modify: `config.rs` (apenas `mod prompts;`, campo `prompts: Vec<SavedPrompt>` no `Config` e no config de workspace, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`; **dono único na F2, depois de T29**), `board/link.rs` (extrair `pub fn deliver_text(&mut self, panel: PanelId, text: &str, enter: bool) -> Result<(), LinkError>` do caminho de `send`, sem mudar comportamento; os testes do T14 seguem verdes), `app/settings/claude.rs` (seção **Saved prompts** global ao final), `app/settings/workspaces.rs` (seção **Prompts** no editor de cada workspace), `app/actions/command_palette.rs` (entradas dinâmicas **Prompt: <nome>**; dono único na F2), `app/claude_quick_start.rs` (linha/passo **Insert prompt ▸** no popup; dono único na F2), `app/hub_state.rs` (campo `prompts_ui: PromptsUi`)
- Test: inline + `crates/horizon-core/tests/config_prompts.rs`

**Interfaces:**
- Produces (core): `pub struct SavedPrompt { pub name: String, pub text: String }`; `pub fn validate_prompts(list: &[SavedPrompt]) -> Vec<PromptIssue>` (nome 1–40 chars sem controle, único sem distinção de caixa por lista, texto 1–8 192 bytes, sem NUL/ESC — newlines permitidos); `pub fn effective_prompts(global: &[SavedPrompt], workspace: &[SavedPrompt]) -> Vec<EffectivePrompt>` (workspace primeiro; mesmo nome do workspace sobrescreve o global; `EffectivePrompt { name, text, scope: Global|Workspace }`); YAML antigo sem `prompts` carrega e re-serializa sem chave nova (S9)
- Produces (ui): `fn insert_prompt(app, panel, prompt, enter: bool)` → `Board::deliver_text` (mesmo caminho de bracketed paste/sanitização do link; **padrão = sem Enter** para o usuário revisar; Ctrl+Enter / botão "Insert & send" envia); terminal sem bracketed paste e texto multilinha → recusa com toast "This terminal can't paste multi-line text" (não achata linhas em silêncio); alvo = painel focado se for terminal; senão toast "Focus a terminal first"; lista efetiva cacheada por versão do config (sem alocação por frame); editor Settings §14 (sem ComboBox, validação inline, salvar pelo fluxo normal da aba)
- Consumes: link send path (T14), `HubToasts` (T27), config/Settings (T6/T11)

- [ ] **Step 1: Failing tests (core):** `prompts_default_empty_and_omitted_from_yaml`, `old_yaml_round_trips_without_prompts_key`, `validate_rejects_empty_name_and_duplicate_case_insensitive`, `validate_rejects_oversize_text_and_control_chars`, `effective_workspace_overrides_global_by_name`, `effective_orders_workspace_first`, `deliver_text_matches_link_send_behaviour` (regressão: bracketed paste e `\r`), `deliver_text_closed_panel_errors`.
- [ ] **Step 2: Failing tests (ui):** `palette_entries_built_once_per_config_version`, `multiline_without_bracketed_paste_is_refused`, `insert_defaults_to_no_enter`, `settings_prompt_editor_add_remove_and_issue_markers`, `target_panel_prefers_focused_terminal`.
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core prompts link` / `-p horizon-ui prompts claude_quick_start` — RED.
- [ ] **Step 4:** Implementar núcleo → `deliver_text` → UI/Settings/palette/quick-start.
- [ ] **Step 5:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2.
- [ ] **Step 6:** Verificação real: criar prompt global e de workspace, inserir via palette e via popup num Claude; editar nas Settings. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-28-visual/`.
- [ ] **Step 7:** Commit `feat(ui): saved prompts with palette, quick-start and settings editor`.

---

### Task 29: Persistir toggles de UI e auto-hide do minimap (domínio: frontend; tipo: ui — helpers puros TDD)

**Contexto verificado:** `minimap_visible` e `hud_visible` são campos de `HorizonApp` (`app/mod.rs`, inicializados em `true`/`false`) alternados por atalho (`toggle_minimap`, `toggle_hud`) e pelo menu das viewports destacadas (`detached_viewports.rs`); nada é persistido. A sidebar já tem auto-hide (`overlays.sidebar_auto_hide`, `sidebar.rs`, `SidebarAutoHide { revealed, last_hover }`).

**Files:**
- Modify: `crates/horizon-core/src/runtime_state.rs` (bloco `ui: UiPrefs { minimap_visible: Option<bool>, hud_visible: Option<bool> }`, `serde(default, skip_serializing_if)`; **dono único**), `config.rs` (`overlays.minimap_auto_hide: bool`, default `false`, ao lado de `sidebar_auto_hide`; **dono único na F1**), `app/mod.rs` (inicialização de `minimap_visible`/`hud_visible` a partir do runtime state — edição mínima das linhas de init; **dono único na F1**), `app/detached_viewports.rs` e o handler dos atalhos (todos passam por um único `fn set_minimap_visible(&mut self, v)` / `set_hud_visible`, que marca o runtime state sujo pelo mecanismo de salvamento existente), `app/minimap.rs` (consulta `auto_hide::minimap_alpha_target`), `app/settings/general.rs` (checkbox **Minimap auto-hide**, ao lado do da sidebar; dono único na F1)
- Create: `crates/horizon-ui/src/app/minimap/auto_hide.rs` (estado + decisão pura)
- Test: inline; `crates/horizon-core/tests/runtime_state_ui_prefs.rs`

**Interfaces:**
- Produces: `UiPrefs` (ausente no arquivo → defaults atuais: minimap visível, HUD oculto); `pub(super) struct MinimapAutoHide { revealed: bool, last_hover: Option<Instant> }` e `pub(super) fn should_reveal(enabled: bool, state: &MinimapAutoHide, pointer_in_zone: bool, now: Instant) -> bool` (zona = retângulo do minimap expandido 16 px; some 600 ms depois que o ponteiro sai; sempre visível se desligado ou se está arrastando/interagindo; atalho `toggle_minimap` continua sendo o interruptor mestre — oculto manualmente não revela por hover); repaint só nas transições (`request_repaint_after` até o fim do atraso), nenhum repaint contínuo enquanto oculto
- Consumes: padrão do `SidebarAutoHide` (copiar a forma, não o código)

- [ ] **Step 1: Failing tests:** `ui_prefs_default_when_missing_in_old_state_file`, `ui_prefs_round_trip`, `ui_prefs_omitted_from_yaml_when_default`, `toggle_minimap_marks_state_dirty` (puro, sobre o setter), `should_reveal_always_true_when_disabled`, `should_reveal_on_hover_zone`, `should_hide_after_600ms_outside_zone`, `manual_hidden_overrides_hover`, `minimap_auto_hide_config_defaults_false_and_old_yaml_loads`.
- [ ] **Step 2:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core runtime_state_ui_prefs config` / `-p horizon-ui minimap` — RED.
- [ ] **Step 3:** Implementar; manter `app/mod.rs` com edição mínima (T29 é o dono nesta wave).
- [ ] **Step 4:** GREEN; fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2.
- [ ] **Step 5:** Verificação real: alternar minimap/HUD, fechar e reabrir o Horizon (isolated `HOME`) e confirmar o estado; ativar auto-hide, ver revelar/ocultar; trace idle sem repaint contínuo. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-29-visual/`.
- [ ] **Step 6:** Commit `feat(ui): persist minimap/HUD visibility and minimap auto-hide`.

---

### Task 30: Rename de painel — auditoria e nome custom em sidebar/link (domínio: frontend, integracao; tipo: ui + regra-de-negocio pequena)

**Estado verificado:** o rename **já existe**: duplo clique na titlebar (`app/panels.rs` ~714, `show_inline_rename_editor` em `panel_chrome.rs`, buffer `panel_rename_buffer`) → `Board::rename_panel` → `Panel::rename` que marca `has_custom_name`; `display_title()` já distingue nome custom. O que falta é conferir que o nome custom é a fonte nos outros lugares e dar um ponto de entrada na sidebar.

**Files:**
- Modify: `crates/horizon-core/src/panel.rs` (apenas um accessor `pub fn custom_name(&self) -> Option<&str>` se ainda não houver; edição mínima — o arquivo está acima de 600 linhas), `crates/horizon-core/src/board/link.rs` e `link/resolve.rs` (nome exibido/ordenação por nome atual, não o título de spawn), `crates/horizon-core/src/runtime_state.rs` (**só se** a auditoria mostrar que `has_custom_name`/nome custom não persiste entre launches), `app/sidebar/labels.rs` (T22: confirmar prioridade nome custom > resto; ajuste se necessário), `app/sidebar/rows.rs` + `app/sidebar.rs` (duplo clique numa linha de painel da sidebar inicia o mesmo rename inline — **somente se** duplo clique estiver livre nessa linha; senão item **Rename** no menu de contexto da linha; dono único na F3), `assets/plugins/claude-code/skills/horizon-link/SKILL.md` (nota: o nome pode mudar; `HORIZON_PANEL_ID` é estável; use `horizon link list`)
- Test: inline + estende `crates/horizon-core/tests/link_socket.rs`

**Interfaces:**
- Produces: `link list` mostra o nome atual (custom se houver) e resolve `target` por esse nome; nome ambíguo continua erro tipado listando ids; `HORIZON_PANEL_NAME` documentado como "nome no spawn" (env não muda depois); rename vazio/só espaços/> 80 chars/controle → rejeitado sem mudar o nome (testar o caminho existente); rename na sidebar reutiliza `panel_rename_buffer` e `Board::rename_panel` (nenhuma lógica nova de rename)
- Consumes: T21/T22 (já mergeados), link (T13/T14/T27)

- [ ] **Step 1: Auditoria (registrar no report, sem código):** (a) rename persiste ao reabrir? (b) sidebar usa nome custom? (c) `link list/send` usa nome custom? (d) `Copy panel address` usa nome custom? Cada "não" vira teste abaixo.
- [ ] **Step 2: Failing tests (conforme a auditoria):** `rename_trims_and_rejects_blank_and_overlong`, `custom_name_survives_runtime_state_round_trip`, `link_list_reports_current_custom_name`, `link_resolves_target_by_custom_name`, `link_ambiguous_custom_name_is_typed_error`, `sidebar_label_prefers_custom_name`, `sidebar_double_click_starts_rename_state` (lógica pura do estado).
- [ ] **Step 3:** `CARGO_BUILD_JOBS=4 cargo test -p horizon-core rename link` / `-p horizon-ui sidebar::labels` — RED, implementar, GREEN.
- [ ] **Step 4:** fmt; `RUSTFLAGS="-D warnings" cargo test --workspace`; clippy ×2; verificação real: renomear por duplo clique na titlebar e na sidebar, ver sidebar/`horizon link list`/badge com o nome novo; reabrir. Screenshot por PID em `.superpowers/sdd/2026-11-12-claude-hub/task-30-visual/`.
- [ ] **Step 5:** Commit `fix(ui): panel custom name is the source for sidebar, link and rename entry points`.

---

### Part F parallel waves

**Dono único por arquivo compartilhado (Parte F):**

| Arquivo | Dono por wave |
|---|---|
| `horizon-core/src/lib.rs` | T23 (F2, `pub mod agent_status;`), T24 (F4, `pub mod claude_sessions;`) |
| `config.rs` | T29 (F1) → T28 (F2) |
| `runtime_state.rs` | T29 (F1), T30 (F3 só se a auditoria exigir) |
| `panel.rs`, `terminal.rs`, `terminal/events.rs` | T23 (F2) → T30 (F3, accessor) |
| `board/link.rs`, `link/*`, `link_cli.rs`, `app/link.rs`, `app/link/send_dialog.rs`, skill horizon-link | T27 (F1) → T28 (F2, só `deliver_text` em `board/link.rs`) → T30 (F3) |
| `main.rs`, `plugin_install.rs` | T23 (F2) |
| `app/panel_chrome.rs`, `app/panels.rs` | T23 (F2) (T21 já mergeado) |
| `app/sidebar.rs`, `app/sidebar/git_overview/*`, `app/sidebar/workspace_menu/*` | T25 (F2) → T24 (F4, menu item) |
| `app/sidebar/rows.rs`, `labels.rs` | T23 (F2, só dot em `rows.rs`) → T30 (F3) |
| `app/claude_quick_start.rs`, `app/actions/command_palette.rs` | T28 (F2) → T26 (F3, quick-start) → T24 (F4) |
| `app/claude_launcher.rs`, `app/worktree_dialog.rs` | T26 (F3; `worktree_dialog.rs` nasce no T25/F2), T24 (F4 launcher) |
| `app/lifecycle.rs` | T23 (F2, só se necessário) → T26 (F3) → T24 (F4) |
| `app/mod.rs`, `app/settings/general.rs`, `app/minimap*`, `app/detached_viewports.rs` | T29 (F1) |
| `app/settings/claude.rs`, `settings/workspaces.rs` | T28 (F2) |
| `app/workspace.rs` | T27 (F1) |
| `app/hub_toast.rs` | T27 (F1) |
| `app/hub_state.rs` | uma linha por task (campo de tipo próprio definido em módulo próprio + init); conflitos triviais resolvidos pelo merger |

**Grafo de dependências:**
```
(T21, T22 mergeados) ─┬─ T23 ──┬─ T26 (Auto)
                      │        └─ T24 (session_id do status)
                      ├─ T25 ──── T26 (Auto no worktree dialog), T24 (menu do workspace)
                      └─ T28 ──── T26 (quick-start), T24 (palette/quick-start)
T27 (independente; cria HubToasts → T23, T24, T25, T26, T28) ── T25 (item "Send to all Claudes", opcional) , T30
T29 (independente) ── T28 (config.rs)
T25, T23 ── T30 (sidebar rows/menus)
```

**Waves (≤ 4 tasks, uma worktree por task, um cargo por worktree, `CARGO_BUILD_JOBS=4`; cada wave só é cortada depois do merge verde da anterior: `RUSTFLAGS="-D warnings" cargo test --workspace`):**

| Wave | Tasks | Por que não colidem |
|---|---|---|
| **F1** (pode iniciar enquanto T21/T22 ainda estão em voo) | **T27** ‖ **T29** | T27: `hub_toast.rs`, `link/*`, `board/link.rs`, `link_cli.rs`, `app/link*`, `app/workspace.rs`, skill; T29: `runtime_state.rs`, `config.rs`, `app/mod.rs`, `minimap*`, `settings/general.rs` — nenhum toca titlebar nem sidebar |
| **F2** (após merge de T21 + T22 + F1) | **T23** ‖ **T25** ‖ **T28** | T23: `lib.rs`, `agent_status*`, terminal/panel, `main.rs`, `plugin_install.rs`, `panel_chrome.rs`/`panels.rs`, `sidebar/rows.rs`, `hub_toast.rs`; T25: `git_overview/worktree_ops*`, `sidebar.rs`, `sidebar/git_overview/*`, `sidebar/workspace_menu/*`, `worktree_dialog.rs`; T28: `config.rs`, `config/prompts.rs`, `settings/{claude,workspaces}.rs`, palette, `claude_quick_start.rs`, `board/link.rs` (`deliver_text`) |
| **F3** (após merge de F2) | **T26** ‖ **T30** | T26: `claude_usage/{auto_pick,alerts}.rs`, launcher, quick-start, `worktree_dialog.rs`, `lifecycle.rs`; T30: `panel.rs` accessor, `link/resolve.rs`, `sidebar/{rows,labels}.rs`, `sidebar.rs` |
| **F4** (após merge de F3) | **T24** | `claude_sessions/*`, `lib.rs`, quick-start/launcher/palette/workspace menu/lifecycle (todos livres nesta wave) |
| **F5** | Smoke ampliado (extensão da Task 17: status, sessões, worktrees, Auto/alertas, ask/reply, prompts, prefs, rename; F8 já coberto) | needs everything |

**Regras:** tasks de uma wave que precisarem de arquivo de outro dono da mesma wave devolvem `NEEDS_CONTEXT`; T27 (F1) define `HubToasts` e o `paint_toasts`; T23/T24/T25/T26/T28 só consomem. Ordem de merge dentro da F2: T23 → T25 → T28.
