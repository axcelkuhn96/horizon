# Plano executável — Horizon v1 Navigation (BRAINSTORM.md)

Branch: `feat/v1-navigation` (local, sem PR upstream). Patch aditivo, rebaseável sobre v0.2.6.

## Regras globais (valem para TODAS as tasks)
- `#![forbid(unsafe_code)]`; sem `.unwrap()`/`.expect()` em runtime (clippy strict bloqueia). Use `Result`/`Option` + `thiserror`.
- Defaults centralizados (`Default`/consts/helpers). Naming idiomático. `#[cfg(test)]` no fim do arquivo.
- State/layout-math → `horizon-core`. UI só renderiza/coleta ação.
- PERF: nada de `request_repaint`/animação para polish; nada de trabalho por-frame incondicional em todos os painéis; checagens gated por interação.
- A cada task: `cargo fmt --all`, `cargo test --workspace`, clippy blocking + strict verdes nos arquivos tocados.
- TDD: teste falhando primeiro quando houver lógica testável.

---

## Story 1 — Config: seção `input:` + migração v8→v9 (FUNDAÇÃO)
**Arquivos:** `crates/horizon-core/src/config.rs`, `crates/horizon-core/src/config_migration.rs`
**Critérios de aceite:**
- Novo struct `InputConfig` com `#[serde(default)]` e campos:
  - `scroll_pans_over_panels: bool` (default `false`)
  - `pan_modifier: String` (default `"Alt"`)
  - `auto_fit_on_focus: bool` (default `true`)
  - defaults via helpers/`Default` impl (não literais espalhados).
- `Config` ganha campo `pub input: InputConfig` com `#[serde(default)]`.
- `CURRENT_CONFIG_VERSION` 8→9; `migrate_v8_to_v9` aditivo (defaults cobrem ausência) + case no loop de `migrate_if_needed`.
- Helper para parsear `pan_modifier` → `ShortcutModifiers` (reusar parser de `shortcuts.rs`); valor inválido = erro tipado (sem panic), com fallback documentado para o default.
**Testes:** config v8 sem seção `input:` carrega e migra para v9 com defaults; `pan_modifier` inválido retorna erro tipado/usa default; round-trip serde mantém valores.
**Risco:** quebrar deserialização de configs existentes → mitigado por `#[serde(default)]` em tudo.

## Story 2 — Shortcuts `focus_panel_*` + UI de settings (FUNDAÇÃO)
**Arquivos:** `crates/horizon-core/src/shortcuts.rs`, `crates/horizon-core/src/config.rs` (ShortcutsConfig), `crates/horizon-ui/src/app/settings/shortcuts.rs`
**Critérios de aceite:**
- `AppShortcuts` ganha `focus_panel_left/right/up/down: ShortcutBinding`; defaults `Ctrl+Shift+ArrowLeft/Right/Up/Down`.
- `ShortcutsConfig` ganha os 4 campos `String` + defaults (`"Ctrl+Shift+Left"` etc.); `resolve()` parseia e inclui no array de `validate_distinct_shortcuts`.
- UI settings: `EditableShortcut` +4 variantes, `ALL` com contagem atualizada, `label()` ("Focus Panel Left"…), `value_mut()`.
**Testes:** defaults parseiam; bindings conflitantes entre `focus_panel_*` e existentes são rejeitados; round-trip config→AppShortcuts.
**Risco:** overlap com shortcut existente que use setas → verificar no `resolve()`; nenhum binding atual usa setas.

## Story 3 — Foco direcional (core geometry) + dispatch + auto-fit
**Depende de:** Story 1 (auto_fit_on_focus), Story 2 (bindings).
**Arquivos:** `crates/horizon-core/src/board.rs` (ou novo `board/navigation.rs`), `crates/horizon-ui/src/app/view.rs`, dispatch no loop de update da UI (`crates/horizon-ui/src/app/`).
**Critérios de aceite:**
- `horizon-core`: `Board::panel_in_direction(from: PanelId, dir: Direction) -> Option<PanelId>` (enum `Direction { Left, Right, Up, Down }`). Considera só painéis do workspace ativo; candidato no semiplano da direção; menor distância no eixo da direção, desempate por menor desvio ortogonal. Sem candidato → `None`.
- UI: ao detectar `shortcut_pressed(focus_panel_*)`, computa vizinho, faz `board.focus(id)`, e se `input.auto_fit_on_focus` → encaixa o painel no viewport (instantâneo).
- `view.rs`: `fit_panel_in_rect(panel_id, canvas_rect)` análogo a `fit_workspace_in_rect`, reusando `panel_focus_frame`/`fit_zoom_for_frame`/`aligned_pan_offset`/`CanvasViewState`. Sem animação (`pan_target = None`, snap).
- Sem painel focado ou sem candidato → no-op silencioso.
**Testes (core, puros):** sem painel focado→None; grid 2x2 (cada direção acha o vizinho certo); sem candidato na borda→None; empate resolvido por desvio ortogonal; ignora painéis de outro workspace.
**Risco:** auto-fit disparar em foco-por-clique → restringir dispatch ao caminho das setas.

## Story 4 — scroll→pan configurável (Feature 2)
**Depende de:** Story 1 (`scroll_pans_over_panels`, `pan_modifier`).
**Arquivos:** `crates/horizon-ui/src/terminal_widget/input.rs`, `crates/horizon-ui/src/input/mouse.rs` (se necessário), caminho de pan do canvas em `crates/horizon-ui/src/app/` (mod.rs/canvas.rs).
**Critérios de aceite:**
- Em `handle_terminal_pointer_input`, ANTES de `wheel_action()` consumir: se (`pan_modifier` segurado) OU (`input.scroll_pans_over_panels`), converte o delta de scroll em pan do `CanvasViewState` (reusa caminho de pan existente) e NÃO repassa ao terminal.
- Sem modificador e toggle off: comportamento upstream preservado (2 dedos = scrollback).
- Checagem barata, gated por interação real (sem custo por-frame em painéis inativos).
**Testes:** `wheel_action`/helper de decisão — modifier→pan; toggle on→pan; nenhum→scrollback. Teste de unidade na função de decisão (extrair lógica pura se preciso).
**Risco:** regressão no hot path de input → manter a checagem mínima; medir com smoke (pan/zoom/scroll existentes intactos).

## Story 5 — Validação final + smoke + sync de config
- Sincronizar `~/.horizon/config.yaml` do usuário com os novos campos/defaults (seção `input:` + `focus_panel_*`).
- Plano de smoke-test temporário em `docs/testing/` (baseline, 3 features, migração v8→v9, regressão visual de pan/zoom). Executar com screenshot ao vivo (`target/debug/horizon`). Apagar o plano temporário após validação.
- Rodar suite pré-push completa (fmt, check-maintainability, test -D warnings, clippy blocking+strict).

## Riscos numerados
1. Quebra de config existente na migração → `#[serde(default)]` + teste de v8→v9.
2. Default `Ctrl+Shift+Setas` colidir com terminal (seleção por teclado) → app intercepta antes; configurável.
3. Hot path de input (Story 4) → checagem mínima + smoke de regressão.
4. Auto-fit em foco não-setas → dispatch restrito.
5. Maintainability guardrail (arquivos >1000 linhas) ao adicionar em board.rs/config.rs → extrair submódulo se necessário.
