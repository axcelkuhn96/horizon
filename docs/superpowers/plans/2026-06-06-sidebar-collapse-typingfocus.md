# Plano executável — Horizon: foco-de-digitação + auto-hide sidebar + collapse por terminal

Branch: `feat/v1-navigation` (continuação; sem worktree). Patch aditivo, rebaseável sobre v0.2.6.

## Regras globais (TODAS as tasks)
- `#![forbid(unsafe_code)]`; sem `.unwrap()`/`.expect()` em runtime (clippy strict). Erros tipados.
- State/layout-math em horizon-core; UI só renderiza/coleta ação. Defaults centralizados. `#[cfg(test)]` no fim.
- PERF: sem `request_repaint` contínuo/animação para polish; sem trabalho por-frame incondicional em todos os painéis; gated por interação. Auto-hide usa `request_repaint_after` pontual.
- Guardrail 1000 linhas: extrair submódulo se necessário (sidebar.rs já tem ~834 linhas).
- A cada task: `. "$HOME/.cargo/env"`, `cargo fmt --all`, `cargo test`, clippy blocking+strict verdes nos arquivos tocados. TDD: teste falhando primeiro onde houver lógica.

---

## Story 1 — Foco de digitação na navegação por setas (FIX)
**Arquivos:** `crates/horizon-ui/src/app/actions/command_palette.rs` (focus_panel_in_direction ~149-169); investigar `terminal_widget/input.rs` + `app/mod.rs` (terminal_keyboard_events) pra achar como o CLIQUE dá foco ao terminal.
**Critérios:**
- Ao focar via Ctrl+Shift+Seta, o terminal alvo recebe o foco de teclado (mesmo mecanismo do clique — provável `ctx.memory_mut().request_focus(id)` com Id estável por painel, OU o que o app usa pra rotear `terminal_keyboard_events` ao painel focado). Digitar funciona sem clicar.
- Não muda o atalho; não altera roteamento de input em outros caminhos; auto-fit existente preservado.
**Testes:** onde houver seam puro (ex.: função que resolve o Id de foco do painel), testar; senão validar no smoke ao vivo (digitar após navegar).
**Risco:** o mecanismo de foco do terminal pode não ser um egui focus id simples — se for roteado por `board.focused`, o fix pode ser trivial ou já parcial; o implementer deve DESCOBRIR e reportar NEEDS_CONTEXT se ambíguo.

## Story 2 — Config + migração v10 (FUNDAÇÃO p/ F2 e F3)
**Arquivos:** `crates/horizon-core/src/config.rs` (OverlaysConfig ~461, TerminalConfig ~504), `config_migration.rs` (CURRENT_CONFIG_VERSION 9→10).
**Critérios:**
- `OverlaysConfig` ganha `sidebar_auto_hide: bool` (default false) com `#[serde(default)]` + helper default centralizado.
- `TerminalConfig` ganha `collapsed: bool` (default false) com `#[serde(default)]`.
- `CURRENT_CONFIG_VERSION` 9→10; `migrate_v9_to_v10` aditivo (no-op body justificado por serde defaults) + arm no loop.
**Testes:** config v9 sem os campos carrega e migra p/ v10 com defaults; round-trip serde; migração idempotente.
**Risco:** quebrar deserialização — mitigado por `#[serde(default)]`.

## Story 3 — Collapse por terminal
**Depende de:** Story 2 (TerminalConfig.collapsed).
**Arquivos:** `crates/horizon-core/src/panel.rs` (Panel ~151), `crates/horizon-ui/src/app/panels.rs` (PanelFrame, show_panel_body_contents), `crates/horizon-ui/src/app/panel_chrome.rs` (paint_panel_chrome, controles), runtime_state.rs/session_store (persistência TerminalConfig↔estado).
**Critérios:**
- `Panel.collapsed: bool` (default false) + altura expandida guardada (`expanded_height: Option<f32>` ou equivalente) pra restaurar.
- Caret ▼/▶ na titlebar (ao lado do close), hit-test + paint espelhando o botão close; clique alterna `collapsed`.
- Colapsado: renderiza só a titlebar (corpo escondido), altura desenhada ~PANEL_TITLEBAR_HEIGHT; terminal/PTY VIVO (não matar). Reexpandir restaura a altura anterior.
- Estado persiste via TerminalConfig.collapsed (volta colapsado após relaunch).
**Testes (core/puros):** toggle collapsed; cálculo de altura colapsada e restauração da expandida; round-trip de persistência collapsed.
**Risco:** layout/colisão no canvas ao encolher — guardar e restaurar altura; não regredir resize/auto-fit.

## Story 4 — Auto-hide da sidebar
**Depende de:** Story 2 (overlays.sidebar_auto_hide).
**Arquivos:** `crates/horizon-ui/src/app/sidebar.rs` (render_sidebar, effective_sidebar_width, paint_sidebar_frame), `crates/horizon-ui/src/app/mod.rs` (estado runtime + hover timestamp).
**Critérios:**
- `sidebar_auto_hide=true`: sidebar recolhe pra faixa fina na borda esquerda; reaparece (largura cheia) ao passar o mouse na faixa/borda; recolhe ~1.5s após o mouse sair. `=false`: comportamento atual.
- Lógica de revelar/esconder como **fn pura** (hover, last_hover_time, delay) → testável. Repaint via `request_repaint_after` (sem loop contínuo).
**Testes:** fn pura de visibilidade (hover→revela; saiu há <delay→fica; saiu há >delay→esconde).
**Risco:** perf (repaint) — usar repaint pontual; smoke pra confirmar sem jitter.

## Story 5 — Validação final + sync config + push
- Sincronizar `~/.horizon/config.yaml` (campos novos/defaults). Plano de smoke temporário em `docs/testing/` (baseline, F1 digitar-após-navegar, F2 auto-hide, F3 collapse+persistência, migração) → EXECUTAR com screenshot ao vivo → apagar.
- Suite pré-push completa (fmt, check-maintainability, test -D warnings, clippy blocking+strict).
- **Commit por story + PUSH pro fork do usuário** (verificar remote `origin`; se apontar pro upstream peters/horizon, NÃO push — configurar/pedir o remote do fork do usuário).

## Riscos numerados
1. Mecanismo de foco do terminal (F1) pode não ser egui-focus simples → implementer descobre, escala se ambíguo.
2. Quebra de config na migração → serde defaults + teste v9→v10.
3. Collapse mexe no layout do canvas → guardar/restaurar altura.
4. Perf do auto-hide → repaint pontual, smoke.
5. Push: remote pode ser o upstream (peters/horizon) e não o fork do usuário → confirmar antes de push.
