# Changelog

Fork pessoal do [peters/horizon](https://github.com/peters/horizon) com melhorias de navegação e UX.
Patch aditivo sobre o upstream **v0.2.6**.

## [feat/v1-navigation] — 2026-06-06

### Navegação
- **Foco direcional entre painéis** com `Ctrl+Shift+Setas` (←↑↓→) — foca o terminal vizinho no workspace, com **auto-fit** do viewport no painel focado.
- **Foco de digitação na navegação**: ao trocar de terminal com as setas, o terminal recém-focado recebe o foco de teclado — dá pra digitar imediatamente, sem clicar.
- Atalhos de foco configuráveis na seção `shortcuts` do `config.yaml` (`focus_panel_left/right/up/down`).

### Pan / canvas (touchpad)
- **Pan configurável sobre os painéis**: `input.scroll_pans_over_panels` (dois dedos movem o canvas mesmo sobre terminais) e `input.pan_modifier`.
- **`Ctrl+Shift+P`** — atalho que alterna o modo "dois dedos movem o canvas" em runtime.
- **Segurar Tab + dois dedos** = move o canvas em 2D (com deferral: o Tab não vaza pro terminal enquanto panando; toque simples de Tab segue normal).
- **Segurar Espaço + scroll** também pana o canvas sobre os painéis.
- Latch de modificadores pra contornar limitação do X11 (Alt+scroll não entrega o modificador).

### Sidebar
- **Auto-hide** da barra lateral de workspaces (`overlays.sidebar_auto_hide`): recolhe pra uma faixa fina na borda e reaparece ao passar o mouse. A área liberada **vira canvas usável** (não fica espaço morto).
- **Collapse de grupos de workspace**: caret (▼/▶) no cabeçalho dobra/expande a lista de terminais daquele workspace na sidebar. Persiste entre sessões.
- Menus de contexto da sidebar agora renderizam **por cima** da barra (z-order corrigido).

### Terminais / painéis
- **Collapse por terminal**: caret (▼/▶) na titlebar colapsa o terminal pra só a titlebar (corpo escondido, painel encolhe) e restaura a altura ao expandir. O processo/PTY continua vivo. Persiste entre sessões.
- **Menu de contexto (botão direito)** no terminal: **Copy**, **Paste** e **Paste Image**.
- **Paste de imagem**: `Ctrl+V` (ou menu) com uma imagem no clipboard salva um PNG temporário e cola o **caminho do arquivo** no terminal (útil pra CLIs como o Claude Code).

### Configuração
- Nova seção `input` no `config.yaml` (`scroll_pans_over_panels`, `pan_modifier`, `auto_fit_on_focus`).
- Novos campos `overlays.sidebar_auto_hide` e `terminals[].collapsed` / `workspaces[].sidebar_collapsed`.
- Migração de config **v8 → v9 → v10**, backward-compatible (configs antigas carregam com defaults via serde).

### Polish (UI/UX)
- Feedback de hover e melhor contraste nos carets de collapse e no handle da faixa de auto-hide.

### Qualidade
- Todas as mudanças com TDD, `cargo fmt`, `clippy` (blocking + strict, sem `allow`), `check-maintainability` e `cargo test --workspace` verdes.
