# PRD: Busca do file-explorer (fechar + abrir no VS Code) + barra de scroll em alt-screen

Status: discovery (gerado por `/investigar`, sem código editado). Próximo passo: `/executar`.
Data: 2026-06-09. Branch alvo: `main` (sem worktree). Push: só `fork`. Subagentes: Sonnet.

## Contexto

Três defeitos de UX reportados na v0.3.1, mais uma frente de release. Esta PRD cobre os
dois bugs da **busca de conteúdo do file-explorer** (foco principal) e registra a decisão
sobre a **barra de scroll dentro do claude** (alt-screen). O Windows da release é tratado
fora desta PRD (re-disparo do workflow após os fixes — ver "Fora de escopo").

### Bug 3 — A busca não fecha
- Abre com `Ctrl+Shift+F` (painel Files focado) → `open_explorer_search()` → `search.active = true`
  (`crates/horizon-core/src/file_tree.rs:566`).
- Único caminho de fechar: `Escape` **gated em `response.has_focus()`** do TextEdit
  (`crates/horizon-ui/src/file_search_widget.rs:252`).
- O terminal rouba o foco egui de volta via `is_active_panel && !other_widget_has_focus`
  (`crates/horizon-ui/src/terminal_widget/mod.rs:112-116`); o TextEdit perde foco e o Escape
  nunca chega ao handler. **Não há botão de fechar (X).** → busca fica presa aberta.

### Bug 4 — Clicar no resultado não abre no VS Code
- `SearchUiAction` só tem `Close` e `Reveal`. Header e match-row emitem `Reveal`
  (`file_search_widget.rs:395,437`) → `reveal_in_tree` (só expande pastas, não abre arquivo).
- O caminho que funciona na árvore normal é `TreeAction::Open` → `open_in_vscode`
  (`file_tree_widget.rs:288,110-130`), que **não** é acionado pela busca.

### Bug 2 (decisão registrada) — Barra de scroll some dentro do claude
- `render_scrollbar` recebe `history_size()` como limite (`terminal_widget/mod.rs:144`),
  que é **sempre 0 em alt-screen** (o grid alternativo do alacritty nasce com `max_scroll_limit=0`),
  então a barra é desenhada como uma pílula cheia inútil.
- **Decisão:** é tecnicamente **impossível** mostrar uma barra fiel à conversa do claude — ele
  gerencia o scroll internamente e não expõe altura/offset por nenhuma sequência de escape (nem
  o alacritty 0.26 expõe viewport de alt-screen). A rolagem em si já funciona (wheel encaminhado
  via mouse-mode em `input/mouse.rs:73-79`). Portanto: **esconder a barra quando em `ALT_SCREEN`**,
  em vez de exibir uma pílula que engana.

## Abordagem escolhida

**Busca (bugs 3+4):** adicionar fechamento robusto e ação de abrir.
- Fechar não pode depender do foco do TextEdit: (a) botão **X** sempre visível no cabeçalho da
  busca **e** (b) `Escape` tratado de forma incondicional enquanto `search.active` (no nível do app
  ou do widget, antes do terminal consumir o foco).
- Clique no resultado: adicionar variante `SearchUiAction::Open(PathBuf)`, emitida no clique de
  header/match-row, tratada no caller (`file_tree_widget.rs:198-204`) chamando `open_in_vscode`,
  espelhando `TreeAction::Open`. (Definir: clique simples vs. duplo — manter consistente com a árvore,
  que usa duplo-clique pra abrir; clique simples pode seguir revelando na árvore.)

**Scrollbar (bug 2):** detectar `TermMode::ALT_SCREEN` no ponto de render
(`terminal_widget/mod.rs:143-174`) e **não desenhar** a scrollbar quando ativo (a barra do terminal
normal segue igual). *Tradeoff aceito: abrimos mão de "mostrar a conversa" (impossível) em troca de
uma UI que não engana; revisitar só se o alacritty expuser viewport de alt-screen.*

## Escopo

- **Inclui:**
  - Botão X de fechar na barra de busca + Escape incondicional com `search.active`.
  - `SearchUiAction::Open(PathBuf)` + wiring pra `open_in_vscode` no clique do resultado.
  - Esconder a scrollbar do terminal quando `ALT_SCREEN` ativo.
  - Testes (TDD): predicado de fechar não-dependente de foco; mapeamento clique→Open; gate de
    render da scrollbar por modo.
- **Não inclui:**
  - Qualquer tentativa de "ler" o scroll interno do claude (inviável).
  - Mudar o encaminhamento de wheel/mouse-mode (a rolagem já funciona).
  - Re-disparo da release / build Windows (frente separada).

## Critérios de aceite

- [ ] Com a busca aberta, clicar no **X** fecha; e `Esc` fecha **mesmo que o TextEdit tenha perdido o foco**.
- [ ] Abrir busca → procurar → fechar → a árvore normal volta a renderar e a tecla volta ao terminal.
- [ ] Clicar (conforme convenção definida) num resultado abre o arquivo no VS Code (`code <path>`);
      aviso não-fatal no rodapé se `code` não estiver no PATH (mesma UX do duplo-clique da árvore).
- [ ] Dentro do claude (alt-screen) **não** aparece a pílula de scrollbar do terminal; num shell normal
      com scrollback ela continua aparecendo e funcional.
- [ ] `cargo fmt --all -- --check`, clippy blocking + strict (`-D clippy::unwrap_used -D clippy::expect_used`),
      `check-version-sync.sh` e `cargo test --workspace` todos verdes.

## Riscos / questões abertas

- Mexer no foco egui da busca pode interagir com o heurístico de foco do terminal
  (`terminal_widget/mod.rs:112-116`) — testar que o terminal não rouba o foco enquanto a busca está ativa.
- Definir clique simples vs. duplo no resultado (recomendado: duplo-clique abre, pra alinhar com a árvore;
  clique simples revela).
- **Windows/compile:** confirmar que o código novo Linux-only (`/proc` em `agent_detect.rs`, com stub
  `#[cfg(not(target_os="linux"))]`) compila no Windows — nunca testado lá. Validar com `cargo check`
  cross ou no próprio run da release.

## Fora de escopo (frente de release — tratar no deploy)

- A v0.3.1 saiu só com binário Linux porque `Validate Release` falhou no version-sync (tag antiga) e os
  artifact builds Windows/macOS foram `skipped`; re-runs ficaram `cancelled`. version-sync e tag já corrigidos.
- **Decisão do usuário:** disparar a release só **depois** de corrigir os bugs. Como os fixes geram commits
  novos, o ideal é publicar uma **v0.3.2** (a v0.3.1 fica só-Linux); decidir tag/versão no momento do deploy.
  Comando: `gh workflow run release.yml -R axcelkuhn96/horizon -f tag_name=v0.3.2`.

## Sugestão de próximo passo

`/executar docs/2026-06-09-file-search-ux-and-alt-screen-scrollbar.md` (subagentes em Sonnet, TDD,
branch `main`, sem push). Depois dos fixes verdes: bump de versão + tag v0.3.2 + `gh workflow run release.yml`.
