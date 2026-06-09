# Changelog

Fork pessoal do [peters/horizon](https://github.com/peters/horizon) com melhorias de navegação e UX.
Patch aditivo sobre o upstream **v0.2.6**.

## [0.3.1] — 2026-06-08

### Sessões
- **Auto-resume de `claude`/`claude2` digitado dentro de um painel Shell**: se você rodou `claude` (conta 1) ou `claude2` (conta 2) num terminal Shell e fechou o app, ao reabrir o painel **religa na mesma conversa** via `<binário> --resume <session_id>`. O id da sessão é capturado de forma exata por painel: primeiro do `--resume <id>` no argv do processo claude vivo; senão (sessão nova, sem id no argv) resolve pela sessão `.jsonl` mais recente daquele diretório, com **reivindicação distinta por painel** — dois shells no mesmo diretório nunca mais retomam a mesma sessão. A conta (claude vs claude2) vem do `CLAUDE_CONFIG_DIR` do processo; o `session_id` é validado (uuid-like) antes de qualquer injeção.
- Abordagens descartadas com evidência (não confiáveis): ler o fd aberto do processo (`claude` não mantém o `.jsonl` aberto) e varrer o scrollback pela linha de resume (só impressa na saída → obsoleta enquanto o claude está vivo).

## [0.3.0] — 2026-06-08

### File Explorer
- **Arquivos/pastas ignorados aparecem esmaecidos** (em vez de sumir): entradas no `.gitignore` (ex.: `temp/`, `tmp/`) agora são listadas em cinza, estilo VSCode, respeitando `.gitignore` aninhado e dos diretórios-pai. `.git`/`node_modules`/`target` seguem sempre ocultos.
- **Busca de conteúdo (`Ctrl+Shift+F`)**: com o painel Files focado, busca texto **dentro** dos arquivos do projeto. Resultados agrupados por arquivo com a linha que casou, clicáveis (revela o arquivo na árvore). Roda em **thread de background** (não trava a UI), com debounce, e pula binários/`node_modules`/`target`. O atalho é **contextual**: focado num terminal, `Ctrl+Shift+F` segue sendo a busca do terminal.
- **Arrastar e `Ctrl+V` pra copiar arquivos pra dentro de uma pasta**: soltar arquivos do gerenciador do SO sobre uma pasta (ou colar com `Ctrl+V`) **copia** (não move) pra dentro dela — pasta sob o cursor no drag, pasta selecionada no `Ctrl+V`; cai na raiz se não houver alvo. Resolução de colisão de nome sem sobrescrever (`a.txt` → `a (2).txt`). Highlight da pasta-alvo no hover.
- **Status do git atualiza sozinho**: as cores de "changed" (verde/`M`/`U`…) agora se reatualizam automaticamente (poll throttled de ~1,5s enquanto o painel está visível + na hora que ele reganha foco), sem depender de refresh manual. Antes só atualizavam em `git add`/`commit`.

### Sessões
- **Retomada de sessão de agente ao reabrir o app**: se um painel estava rodando um agente (ex.: `claude`) e o app fechou, ao reabrir o painel **religa na mesma sessão** via `--resume <session_id>` (recuperando o id da sessão pelo histórico do agente), em vez de começar do zero. Só acontece pra painéis que de fato rodaram (têm output), pra não retomar uma sessão antiga por engano. *(implementado na branch `feat/restore-agent-session`.)*

## [feat/v1-navigation] — 2026-06-06

### Navegação
- **Foco direcional entre painéis** com `Ctrl+Shift+Setas` (←↑↓→) — foca o terminal vizinho no workspace **sem alterar o zoom** (só troca o foco; o auto-fit inicial foi removido).
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

### File Explorer (painel "Files")
- **Árvore de arquivos estilo VSCode** do projeto do workspace: ícones por tipo de arquivo (Symbols Nerd Font), pastas com expand/collapse lazy, respeita `.gitignore` e pula `.git`/`node_modules`/`target`.
- **Decorações de git ao vivo** (mesmo esquema do VS Code): untracked/added verde (`U`/`A`), modificado amarelo (`M`), deletado vermelho (`D`).
- **Duplo clique abre o arquivo no VS Code** (`code <path>`); aviso não-fatal no rodapé se o `code` não estiver no PATH.
- **Filtro "só não-commitados"** (funil no cabeçalho): mostra apenas os arquivos pendentes, **agrupados por pasta** (cadeias de pasta única compactadas tipo VSCode).
- **Nomes longos truncam com `…`** sem atropelar a letra de status; scroll do mouse funciona e a árvore fica contida no painel.
- Preset "Files" (alias `fx`, kind `file_explorer`) disponível no palette/config.

### Terminais / painéis
- **Detecção de processo morto (ghost panel)**: badge vermelho "Exited" na titlebar, banner não-modal no rodapé, teclado bloqueado no painel morto e botão **Restart** na sidebar pra shells/commands.
- **Collapse por terminal**: caret (▼/▶) na titlebar colapsa o terminal pra só a titlebar (corpo escondido, painel encolhe) e restaura a altura ao expandir. O processo/PTY continua vivo. Persiste entre sessões.
- **Menu de contexto (botão direito)** no terminal: **Copy**, **Paste** e **Paste Image**.
- **Paste de imagem**: `Ctrl+V` (ou menu) com uma imagem no clipboard salva um PNG temporário e cola o **caminho do arquivo** no terminal (útil pra CLIs como o Claude Code).

### Configuração
- Nova seção `input` no `config.yaml` (`scroll_pans_over_panels`, `pan_modifier`). O campo `auto_fit_on_focus` foi removido junto com o auto-fit (configs antigas com a chave seguem carregando).
- Novos campos `overlays.sidebar_auto_hide` e `terminals[].collapsed` / `workspaces[].sidebar_collapsed`.
- Migração de config **v8 → v9 → v10**, backward-compatible (configs antigas carregam com defaults via serde).

### Polish (UI/UX)
- Feedback de hover e melhor contraste nos carets de collapse e no handle da faixa de auto-hide.

### Qualidade
- Todas as mudanças com TDD, `cargo fmt`, `clippy` (blocking + strict, sem `allow`), `check-maintainability` e `cargo test --workspace` verdes.
