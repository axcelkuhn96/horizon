# claude-hub: spec visual da interface

Alvo: egui/eframe nativo. Paleta atual = "Ember Forge" (charcoal quente + âmbar), não Catppuccin: usar SEMPRE as funções de `theme.rs` (`theme::FG()` etc.), nunca `Color32` literal. Dark e light funcionam pelas mesmas funções. Fontes: `FontId::proportional` (texto), `FontId::monospace` (números/atalhos). Escala única: 8.5 micro, 10.5 meta, 11.5 corpo-chip, 12 corpo, 13 título-linha, 18 título-tela. Espaçamento em múltiplos de 2 (4/8/12/16). Raios: chip/pílula 10, botão/linha 8, card 10 (`section_card`), modal 16 (`window_corner_radius`), badge 4, swatch 6.

## 0. Primitivos compartilhados (um módulo `app/hub_ui.rs`, todos o usam)

| Primitivo | Spec |
|---|---|
| `profile_accent(name, idx_taken)` | cor estável do perfil (tabela de tokens abaixo); índice = FNV-1a(name) % 6; colisão entre perfis visíveis → próximo livre; resultado cacheado em `hub_state` por (lista de nomes), recalculado só quando a lista muda |
| `mini_bar(painter, rect, frac, fill)` | trilha `usage_track()` raio 2, preenchimento `fill` raio 2, largura mínima 2 px se frac>0; sem animação (dados mudam a cada 5 min) |
| `severity_color(pct)` | <75 → `bar_normal()`; 75..<90 → `PALETTE_YELLOW`; ≥90 → `PALETTE_RED`. Se a API traz `severity`, ela vence |
| `pill(painter, rect, text, color)` | altura 18, raio 9, fill `alpha(color, 36)`, stroke 1 `alpha(color,150)`, texto 10.5 `color`; usado em ok/expired/missing/danger |
| `swatch(ui, color, selected)` | 22x22, raio 6, fill cor; selected = anel 2 px `FG()` por fora (offset 2) + check pintado (2 segmentos, 1.6 px, `BG()`); hover = anel 1 px `FG_SOFT()`; focus = anel 2 px `ACCENT()` |
| `key_badge(painter, rect, n, active)` | 20x20, raio 6; inativo: fill `PANEL_BG_ALT()`, texto monospace 11 `FG_DIM()`; ativo: fill accent do perfil, texto `BG()` |
| `kbd_hint(ui, "Enter", "open")` | cápsula 16 alto, raio 4, stroke `BORDER_SUBTLE()`, monospace 10 `FG_SOFT()` + rótulo 10.5 `FG_DIM()` |
| Botão primário | fill `ACCENT()`, texto `BG()` 12 strong, altura 30, raio 8, hover `blend(ACCENT(),FG(),0.12)`, pressed `blend(ACCENT(),BG(),0.18)`, disabled `alpha(ACCENT(),70)` + texto `alpha(BG(),160)` |
| Botão secundário | fill `PANEL_BG_ALT()`, stroke `BORDER_SUBTLE()`, texto `FG()` 12, altura 28, raio 8; hover fill/stroke do tema (já estilizados) |
| Botão ícone (reordenar/remover/browse) | 26x26, raio 8, sem fill em repouso; hover fill `PANEL_BG_ALT()`; remover hover texto `BTN_CLOSE()`; ícones pintados como segmentos (seta ▲▼ = triângulo 8x5, X = 2 linhas 9x9, 1.5 px) para não depender de glifos da fonte |
| Foco de teclado | todo controle focável: `rect_stroke` 2 px `ACCENT()` StrokeKind::Outside |
| Scrim de modal | retângulo da viewport `scrim()`, `Order::Foreground`; modal em `Order::Tooltip`; clique fora = fecha; Esc = fecha |
| Frame de modal | fill `PANEL_BG()`, stroke 1 `BORDER_STRONG()`, raio 16, sombra de `popup_shadow` (já no tema), margin 0 e seções com padding próprio de 16 |

Tokens novos para `theme.rs` (única adição; tudo mais reusa):

| Token | Dark | Light | Uso |
|---|---|---|---|
| `PROFILE_ACCENTS[6]` | âmbar=`ACCENT()`, teal=`PALETTE_CYAN()`, rosa (214,145,196), sálvia=`PALETTE_GREEN()`, azul-pervinca (122,162,222), violeta (178,150,226) | âmbar=`ACCENT()`, teal, rosa (150,64,132), sálvia, azul (52,98,176), violeta (106,76,170) | acento de perfil; os 4 primeiros vêm de funções existentes, os 3 novos são literais na paleta |
| `usage_track()` | `alpha(BORDER_STRONG,110)` | `alpha(BORDER_STRONG,140)` | trilha das barras |
| `bar_normal()` | `blend(BORDER_STRONG,FG_SOFT,0.55)` | idem | barra < 75% (neutra de propósito: cor só quando importa) |
| `scrim()` | preto alpha 120 | preto alpha 60 | fundo de modais |

## 1. Chips de uso na toolbar

Posição: `ToolbarItem::UsageChips`, entre a busca e os botões de ação, mesma faixa de 46 px; chip centralizado verticalmente, altura 28 (fps meter = 24, botões = 30). Gap entre chips 6.

Chip cheio (largura calculada, máx ~330). **Pedido do usuário:** o chip mostra só o nick da conta = parte local do e-mail (antes do `@`), proportional 11.5 `FG()`, máx 14 chars + `…`; sem e-mail, cai para o nome do perfil. Depois do 7d vem o tempo até o reset semanal (` · 4d`, proportional 10.5 `FG_DIM()`): `Nd` (≥1 dia, arredonda p/ baixo), `Nh` (<1 dia), `Nm` (<1 h); oculto se o reset é desconhecido/passado ou se o chip está expirado/sem login. O texto é cacheado com o layout e renovado no máximo uma vez por minuto (um `request_repaint_after` até a virada do minuto, só com contador visível). Nome do perfil, e-mail e reset exato ficam no tooltip:
```
 ┌──────────────────────────────────────────────────────────┐
 │ ● gutosjc  5h ▰▰▰▰▱▱▱▱ 42%  7d ▰▰▱▱▱▱▱▱ 18% · 4d │   28h
 └──────────────────────────────────────────────────────────┘
  8 6 dot  nick(max 104, ellipsis)  8  grupo=label10+4+bar28x4+4+pct26  8 gap  grupo  contador  8
```
- Fill `alpha(blend(PANEL_BG_ALT(), accent, 0.10), 232)`, stroke 1 `alpha(blend(BORDER_SUBTLE(), accent, 0.36), 220)` (mesma receita do fps meter), raio 10.
- Dot: círculo r=3, `accent` (acento do perfil). Nome: proportional 11.5 `FG()`. Rótulos `5h`/`7d`: proportional 8.5 `FG_DIM()` (mesmo tamanho do "fps"). Percentual: monospace 10.5, cor = `severity_color` quando ≥75, senão `FG_SOFT()`; largura fixa 26 (direita alinhada) para o chip não "dançar" entre 9% e 100%.
- Barras 28x4, centralizadas; frac = pct/100 clamp.
- Compacto (nick + 7d, sem contador): dot, nick (máx 8 chars + `…`), barra 7d 20x4, pct. Largura ~150. Ativa para todos os chips ao mesmo tempo (nunca mistura cheio+compacto).
- Prioridade de espaço: (1) busca encolhe até o mínimo atual; (2) todos compactam; (3) chips que não cabem vão para o menu de overflow existente como linhas `● nome  5h 42%  7d 18%` (mesmo layout da linha do launcher, sem badge); o `OverflowMenu` aparece se houver itens de ação OU chips excedentes.
- Estados:
  - hover: fill `blend(PANEL_BG_ALT(), accent, 0.20)`, stroke `blend(BORDER_STRONG, accent, 0.45)`, cursor pointer; pressed: fill `blend(PANEL_BG_ALT(), accent, 0.30)`.
  - focus (Tab): anel de foco padrão.
  - stale (dado OK mas `fetched_at` > 15 min): dot vira anel (stroke 1.2, sem fill), percentuais `FG_DIM()`.
  - expired/missing: conteúdo todo a alpha 0.55; barras mostram o último valor conhecido a alpha 0.4; o grupo 7d é substituído por pílula `expired` (`PALETTE_YELLOW`) ou `no login` (`FG_DIM`); compacto: só a pílula.
  - sem dado ainda (boot sem cache, perfil nunca buscado): barras vazias, pct `—`; o `—` vale só para perfil nunca buscado. Janela que a conta não tem (perfil já buscado, sem percentual) fica oculta, sem grupo nem espaço.
  - erro de rede com dado antigo: tratado como stale; tooltip diz o erro (1 linha, `PALETTE_RED`).
- Interação: clique esq = abre sessão do perfil (mesmo caminho do launcher); clique dir = menu de contexto com um único item `Refresh now` (durante fetch: item desabilitado `Refreshing…`).
- Tooltip (`on_hover_ui_at_pointer`, largura fixa 280, margin 12, frame de popup do tema):
```
 ● conta2                         [ok]      linha 1: dot + nome 13 strong; pílula de estado à direita
 Fulano Silva · fulano@exemplo.com          11 FG_DIM (displayName · e-mail; omite se desconhecido)
 ─────────────────────────────────────      1 px BORDER_SUBTLE, margin vertical 8
 5h  ▰▰▰▰▱▱▱▱ 42% used · 58% left           barra 120x5 + texto 11 (usado FG, restante FG_DIM)
     resets 14:30 (in 1h 12m)               10.5 FG_DIM, hora local HH:MM; "in …" relativo
 7d  ▰▰▱▱▱▱▱▱ 18% used · 82% left
     resets Fri 09:00 (in 3d 4h)
 ─────────────────────────────────────
 Tokens today   1.24M                       11 FG_SOFT / monospace 11 FG
 Updated 3 min ago                          10.5 FG_DIM
 Open conta2 in a terminal to renew login   só se expired/missing; 10.5 PALETTE_YELLOW
 Click: open session · Right-click: refresh   10 FG_DIM, rodapé
```
- Cache (nada disso por frame): `UsageChipsCache { version, width_bucket, ppp, theme, chips: Vec<ChipLayout{ galley_name, galley_pct5, galley_pct7, width_full, width_compact, accent }> }`. Reconstruir só quando muda `usage_version` (contador incrementado quando chega resultado do poller), a lista de perfis, a largura disponível em passos de 8 px, `pixels_per_point` ou o tema. Strings do tooltip (reset local, tokens, e-mail) são formatadas na chegada do dado e guardadas; só "age"/"in 1h 12m" são recalculados quando o tooltip está aberto. Sem `request_repaint` nos chips: o poller acorda a UI uma vez por resultado.

## 2. Launcher Claude (Ctrl+Shift+L)

Modal central, largura 480 (ou viewport-48), topo a 18% da altura da viewport, scrim ligado. Frame de modal.
```
┌──────────────────────────────────────────────────────┐
│  Open Claude session                      workspace: │  header 44h, padding 16: título 13 strong; à direita
│                                           api-core   │  nome do workspace alvo 10.5 FG_DIM
├──────────────────────────────────────────────────────┤  1 px BORDER_SUBTLE
│ ▌[1] ● conta1   fulano@exemplo.com   5h ▰▰▱▱ 42%     │  linha 52h, margin lateral 8
│               (selecionada)          7d ▰▱▱▱ 18%     │
│  [2] ● conta2   outra@exemplo.com    5h ▰▰▰▰ 91%     │
│               [expired]              7d ▰▰▰▱ 77%     │
│  [3] ● pessoal  —                    5h —   7d —     │  perfil nunca buscado
├──────────────────────────────────────────────────────┤
│ [↑↓] move  [Enter] open  [1–9] pick  [Esc] close     │  footer 36h, kbd_hint
└──────────────────────────────────────────────────────┘
```
- Linha: raio 8, padding horizontal 12. Janela que a conta não tem (perfil já buscado) não é desenhada; `5h —` aparece só em perfil nunca buscado. Esquerda: `key_badge` (20x20; só 1–9, a partir do 10º perfil o espaço fica vazio), gap 10, dot r=4 acento, gap 8, coluna de texto: nome 13 `FG()` strong (linha 1), e-mail 11 `FG_DIM()` (linha 2; `—` se desconhecido; ellipsis). Direita (largura 132): duas linhas empilhadas `5h` / `7d` com barra 56x4 + pct monospace 10.5 (mesma cor/regra de severidade). Estado `expired`/`missing`: pílula na linha 2 do texto, barras a alpha 0.4.
- Selecionada: fill `blend(PANEL_BG_ALT(), accent, 0.22)`, stroke 1 `alpha(accent,150)`, barra esquerda 3x28 raio 2 em `accent`, badge ativo. Hover (mouse): fill `blend(PANEL_BG_ALT(), accent, 0.12)` e move a seleção junto (seleção única, sem estados concorrentes). Pressed: fill 0.30. Mouse sai da lista: seleção permanece.
- Teclado: ↑/↓ circulam (wrap), Home/End, 1–9 abrem direto (sem Enter), Enter abre selecionado, Esc fecha. Foco inicial na 1ª linha (ou no último perfil usado, memorizado em `hub_state`).
- Vazio (0 perfis): ícone (círculo r=14 stroke `BORDER_STRONG()` com ponto central), título 13 "No Claude profiles", texto 11 `FG_DIM` "Add one in Settings → Claude or run Detect accounts.", botão secundário `Open Settings → Claude` (foco inicial; Enter ativa). Altura total 220.
- Perfil `missing`: linha ainda selecionável (abre sessão para o usuário logar); pílula `no login`.
- Custo: lista construída do snapshot em cache (`ChipLayout` partilhado); sem repaint contínuo; abrir/fechar sem animação (corte seco, é uma ferramenta de teclado).

## 3. Settings → Claude

Aba nova (mesma barra de abas de `settings/mod.rs`, rótulo `Claude`). Conteúdo em `section_card` por bloco. Título de seção 13 strong `FG_SOFT`, ajuda 11 `FG_DIM` (`dim_label`).
```
 Claude profiles                                    [Detect accounts] [+ Add profile]
 dim: "Each profile is a CLAUDE_CONFIG_DIR. Credentials are never shown or modified."
┌──────────────────────────────────────────────────────────────────────────────┐
│ ● [ conta2        ]  [ /home/me/.claude-conta2          ] [Browse]  (ok)  ▲▼ ✕ │  card 56h
└──────────────────────────────────────────────────────────────────────────────┘
│ ● [ pessoal       ]  [ ~/.claude                        ] [Browse]  (expired) ▲▼ ✕│
```
- Um card por perfil (fill `PANEL_BG_ALT()` blend 0.5 com `PANEL_BG()`, stroke `BORDER_SUBTLE()`, raio 10, padding 12x10), faixa esquerda 3 px na cor de acento do perfil. Colunas: dot (acento) | nome (TextEdit 140, 12) | config_dir (TextEdit monospace 11 flex) | `Browse` (secundário, abre `dir_picker`) | pílula de credencial | ▲ ▼ (desabilitados no 1º/último) | ✕.
- Pílula: `ok` `PALETTE_GREEN`, `expired` `PALETTE_YELLOW`, `missing` `BTN_CLOSE`/`PALETTE_RED`; hover mostra tooltip 10.5 ("Token expired at 12:40 — open this profile in a terminal to renew"); nunca exibe token. Estado vem do snapshot do poller (não reler arquivo por frame).
- ✕: remove só da lista editada (nada no disco até Save), sem confirmação; linha some com ajuste instantâneo.
- Validação inline: linha de erro sob o card, 11 `PALETTE_RED`, com stroke do TextEdit inválido `PALETTE_RED`. Casos: nome vazio, nome duplicado, config_dir inexistente (aviso `PALETTE_YELLOW`, não bloqueia), dir sem `.credentials.json` (pílula `missing`). Erros bloqueiam o Save (botão Save global desabilitado com tooltip).
- Detect accounts: botão secundário; ao rodar, texto vira `Detecting…` (disabled) e depois resultado em 11 `FG_DIM` abaixo: "Found 2 new profiles" / "No new accounts found". Perfis novos entram ao fim com destaque de fill `alpha(ACCENT(),28)` até a próxima interação.
- Vazio: card único centralizado, dim 12 "No profiles configured. Detect accounts finds ~/.claude and ~/.claude-*." + botões Detect (primário) e Add.
- Abaixo: card "Usage bar" com checkbox `Show usage chips in toolbar` só se o toggle ficar aqui; o toggle oficial permanece na aba General (spec A/C5).

## 4. Settings → Workspaces

Duas colunas dentro da área da aba: esquerda 240 fixa, direita flex; divisor 1 px `BORDER_SUBTLE`; gap 16.
```
┌ Templates ─────────┬ Editor ───────────────────────────────────────────────┐
│ ■ api-core       ▲▼│ Name  [ api-core                         ]            │
│ ■ frontend         │ Cwd   [ ~/code/api                  ] [Browse]        │
│ ■ infra            │ Color ■ ■ ■ ■ ■ ■ ■ ■                                 │
│                    │ ─ Terminals (3) ──────────────── [+ Add terminal ▾]    │
│ [+ New template]   │ ▌⌨ Shell        shell            ▲ ▼ ✕                │
│                    │ ▌● Claude·conta2 claude  cwd:—   ▲ ▼ ✕                │
│                    │ ───────────────────────────────────────────────────── │
│                    │              [Delete template]       [ Open now ]      │
└────────────────────┴───────────────────────────────────────────────────────┘
```
- Lista esquerda: linha 36h, raio 8, swatch 10x10 raio 3 + nome 12 (ellipsis) + ▲▼ só no hover/seleção (16x16). Selecionada: fill `blend(PANEL_BG_ALT(), template_color, 0.22)` + barra esquerda 3 px na cor; hover 0.10. Rodapé: `+ New template` secundário largura total. Vazio: dim 11 "No templates yet. Create one or use Save as template from a workspace."
- Editor: labels 11 `FG_DIM` em coluna de 52, campos TextEdit 12 altura 28. Cores: 8 `swatch`es em fila (as cores de workspace já usadas na sidebar; se não houver paleta, `PROFILE_ACCENTS` + `PALETTE_RED` + `PALETTE_YELLOW`); o formato salvo segue o parser de cor existente em `horizon-core` (não inventar formato).
- Terminais: linhas 36h (fill `PANEL_BG_ALT()`/0.5, raio 8, barra esquerda 3 px na cor do kind: claude = acento do perfil, shell = `FG_DIM`, outros = `ACCENT`). Ícone de kind pintado (16x16): shell = chevron `>` + underscore; claude = dot grande r=5 no acento; outro agente = losango; ssh = duas barras. Nome (TextEdit inline, 12), kind em texto 10.5 `FG_DIM` somente leitura, cwd opcional (TextEdit monospace 11, placeholder "inherit"), ▲▼✕ (botões ícone).
- `+ Add terminal ▾`: botão secundário abre popup 260 largura: seção "Presets" (linhas 30h: ícone kind + nome), separador, cabeçalho "CLAUDE PROFILES" 9.5 `FG_DIM` letter-case caps, linhas `● Claude · conta2` com dot de acento. Hover linha: fill `blend(PANEL_BG_ALT(), ACCENT(), 0.18)`. Teclado: ↑↓ Enter Esc.
- `Open now`: primário, altura 32, largura 120, à direita; desabilitado se zero terminais (tooltip). Apertado → feedback inline 11 `PALETTE_GREEN` "Opened in current session" por 3 s (sem repaint contínuo: `request_repaint_after(3s)`).
- Remover template: botão `Delete template` (texto `BTN_CLOSE()`, stroke `alpha(BTN_CLOSE,120)`, fundo transparente) abre modal de confirmação (seção 5) com texto "Delete template "X"? Running panels are not affected."
- Alternar para a aba YAML preserva edições (estado do editor fica em `editing_config`; seleção do template em `hub_state`).

## 5. Menu de contexto do workspace na sidebar

Menu egui padrão (popup do tema), largura mín. 220, itens 28h, padding 8x6, texto 12 `FG()`, atalho/seta à direita 10.5 `FG_DIM`.
```
┌──────────────────────────┐
│ Rename                   │
│ New terminal           ▸ │
│ Save as template         │
│ ─────────────────────────│
│ Delete…                  │  texto BTN_CLOSE()
└──────────────────────────┘
        ┌──────────────────────────┐   submenu, largura 240
        │ PRESETS                  │   cabeçalho 9.5 caps FG_DIM, altura 22
        │ ⌨ Shell                  │
        │ ◆ Codex                  │
        │ ─────────────────────────│
        │ CLAUDE PROFILES          │
        │ ● Claude · conta1        │   dot r=4 no acento do perfil
        │ ● Claude · conta2        │
        └──────────────────────────┘
```
- Hover do item: fill `blend(PANEL_BG_ALT(), ACCENT(), 0.22)`; Delete hover: fill `alpha(BTN_CLOSE(), 40)`. Disabled: texto `alpha(FG_DIM(),160)`. Submenu abre em hover com 150 ms (padrão egui `menu_button`).
- Rename: reusa o edit inline de título já existente na sidebar (TextEdit no lugar do nome, Enter confirma, Esc cancela, foco automático, seleciona tudo).
- Save as template: ação imediata; feedback é toast pequeno do app se existir, senão linha na sidebar `Saved template "X"` 11 `PALETTE_GREEN` por 3 s. Se já existir template vinculado: texto do item vira `Update template "X"`.
- Delete: modal de confirmação (frame de modal, largura 380, scrim). Título 14 strong; corpo 12 `FG_SOFT` ("Delete workspace "api-core" and its N terminals? Running processes will be closed."); botões à direita: `Cancel` (secundário, foco inicial) e `Delete` (fill `BTN_CLOSE()`, texto `BG()` strong, hover `blend(BTN_CLOSE(),FG(),0.12)`). Enter = botão focado, Esc = cancelar. N e nome calculados ao abrir o modal, não por frame.

## 6. "Send to panel…" e badge de mensagem recebida

Item no menu de contexto do painel: `Send to panel…` e `Copy panel address` (separados dos itens existentes por separador; `Send to panel…` desabilitado se não há outro painel). `Copy panel address` copia `nome` (ou id se nome ambíguo) e mostra toast/linha "Copied" 1.5 s.

Diálogo (modal 520, frame de modal, scrim):
```
┌─────────────────────────────────────────────────────────┐
│ Send to panel                                           │  header 44h, título 13 strong
│ from  Claude · conta2                                   │  10.5 FG_DIM
├─────────────────────────────────────────────────────────┤
│ [ filter…                                  ]            │  TextEdit 28h, foco inicial
│ ▌⌨ api-shell      api-core       shell                  │  linha 40h
│  ● claude-main    api-core       claude · conta1  agent │
│  ⌨ logs           infra          shell                  │  lista altura máx 200, scroll
├─────────────────────────────────────────────────────────┤
│ Message                                                 │
│ ┌─────────────────────────────────────────────────────┐ │  multiline 96h, monospace 11.5,
│ │ <seleção atual prefilled>                           │ │  prefilled; vazio com placeholder
│ └─────────────────────────────────────────────────────┘ │  "Type a message…"
│ [Esc] close                [Send without Enter] [ Send ]│  footer
└─────────────────────────────────────────────────────────┘
```
- Linha de alvo: ícone de kind (mesmos do §4) | nome 12 `FG()` | workspace 11 `FG_DIM` (coluna 120) | kind 10.5 `FG_DIM` + pílula `agent` (`ACCENT`) se for agente. Selecionada: fill `blend(PANEL_BG_ALT(), ACCENT(), 0.22)` + barra 3 px; hover 0.12. O painel de origem não aparece. Filtro casa nome/workspace/kind (subsequência, sem distinção de caixa), ↑↓ movem, Tab alterna lista ↔ mensagem.
- Send: primário (altura 30); `Send without Enter`: secundário. Ctrl+Enter = Send, Ctrl+Shift+Enter = sem Enter. Ambos desabilitados sem alvo ou mensagem vazia (tooltip dizendo o que falta). Após enviar: fecha; sem erro de entrega → nenhum toast; erro (painel fechado) → linha 11 `PALETTE_RED` acima do footer e o diálogo permanece.
- Lista de alvos = snapshot montado ao abrir (não por frame).

Badge "message received" (titlebar do painel receptor):
- Reusa a geometria de `paint_ssh_status_badge` (altura 18, raio 4, texto proportional 10): à esquerda do medidor de histórico/close, texto `from <nome>` (nome truncado em 14 chars com …), cor `ACCENT()`; fill `alpha(blend(TITLEBAR_BG(), ACCENT(), 0.18), 200)`, stroke 1 `alpha(ACCENT(), 140)`; um dot r=2.5 `ACCENT()` antes do texto. Se o badge de ssh/exit ocupa o lugar, empilha à esquerda dele (gap 6).
- Linha do tempo (`received_at: Instant` no painel): 0–2.3 s opacidade 1; 2.3–3.0 s fade linear até 0; depois não pinta nem agenda repaint. Ao receber: `request_repaint_after(2.3 s)`; dentro da janela de fade `request_repaint()` por frame (≤ 0.7 s, ~42 frames); depois nada. Nova mensagem reinicia o relógio. O alpha é aplicado via `theme::alpha`/`multiply_opacity` nas três primitivas (fill, stroke, texto).

## Inventário de cache (resumo)

| Item | Chave de invalidação |
|---|---|
| layout/galleys dos chips + larguras full/compact | `usage_version`, lista de perfis, largura em passos de 8, ppp, tema |
| acentos de perfil | lista de nomes dos perfis |
| strings do tooltip (reset local, tokens, e-mail) | `usage_version` |
| linhas do launcher | `usage_version`, lista de perfis (compartilha `ChipLayout`) |
| estado de credencial na aba Claude | `usage_version` |
| lista de alvos do Send | ao abrir o diálogo |
| contagem/nome na confirmação de Delete | ao abrir o modal |

## 7. Git e worktrees na sidebar

Cada workspace com `cwd` dentro de um repo git ganha, logo abaixo da linha de cabeçalho (32 px), uma **linha git** compacta de 20 px e, expansível, uma sub-lista **worktrees**. Workspace sem repo: nenhuma linha (zero altura). Dados vêm de `RepoOverview` (Task 18); a UI só lê strings/galleys prontos.

```
 ▾ api-core                          ← cabeçalho existente
   ⎇ feat/claude-hub  ↑2 ↓1  ● 5     ← linha git 20h, indent 28
   ▸ worktrees 3                     ← toggle 20h (só se há >1 worktree)
 ▾ api-core                          ← expandido:
   ⎇ feat/claude-hub  ↑2 ↓1  ● 5
   ▾ worktrees 3
       main            ../api-core        ●   ← linha 22h, indent 40
     ▌ feat/claude-hub ../api-core-hub        ← barra 3 px ACCENT = atual
       fix/login       ../api-core-fix  🔒
```
- **Linha git:** glifo de branch pintado (círculo r=2 + 2 segmentos, 1.2 px, `FG_DIM()`) | nome 10.5 `FG_SOFT()` (máx ~22 chars + `…`, meio-truncado) | `↑n` `PALETTE_GREEN()` e `↓n` `PALETTE_YELLOW()` monospace 10, omitidos se 0 ou sem upstream | `● n` dirty: dot r=3 `PALETTE_YELLOW()` + n monospace 10 `FG_SOFT()`; limpo = nada (sem dot verde: cor só quando importa). Setas pintadas como triângulos 5x7, não glifos.
- **Estados:** detached → nome `HEAD @ <7 hex>` em itálico `FG_DIM()`; sem upstream → sem setas; repo sem commits → `(no commits)` `FG_DIM()`; erro de leitura → `git?` `FG_DIM()` com tooltip do erro; aguardando 1º dado → linha oculta (sem placeholder, sem pulo > 20 px). Painel recolhido na sidebar: só o dot dirty no cabeçalho.
- **Linha de worktree:** branch 10.5 `FG()` (detached: `HEAD @ abc1234` `FG_DIM`), caminho curto 10 monospace `FG_DIM()` relativo ao pai do repo principal (`../api-core-fix`; principal = `.`), dot dirty r=3 `PALETTE_YELLOW`, cadeado pintado se `locked`, `prunable` = texto a `alpha 0.55` + pílula `prunable` `FG_DIM`. **current** (cwd do workspace dentro dessa worktree): barra 3 px `ACCENT()` + fill `blend(PANEL_BG_ALT(), ACCENT(), 0.14)`. Hover: fill `PANEL_BG_ALT()`, cursor pointer; foco: anel padrão. Principal listada primeiro, depois ordem alfabética por branch.
- **Interação:** clique em linha de worktree = abre terminal shell com `cwd` = worktree, dentro do workspace (foco no novo painel). Clique no toggle expande/recolhe (estado por workspace em `HubState`, não persistido). Clique na linha git: sem ação. Menu de contexto da linha de worktree (`context_menu_above_sidebar`, itens 12, altura 26): **Open terminal here**; **Open Claude here ▸** (submenu largura 240, perfis `Claude · <perfil>` com dot de `profile_accent`, mesmo cabeçalho do §5); **New workspace from worktree** (cria workspace nomeado pelo branch com cwd da worktree); separador; **Copy path** (caminho absoluto; linha "Copied" 1.5 s). Worktree `prunable` desabilita as 3 primeiras ações.
- **Tooltip** (linha git ou de worktree, `on_hover_ui_at_pointer`, largura 300, margin 12): branch 13 strong + pílula `current`/`main`/`locked`; caminho absoluto monospace 10.5 `FG_DIM` (quebra no meio); `upstream origin/main · ↑2 ahead · ↓1 behind`; `5 changed files`; `Updated 12 s ago` 10.5 `FG_DIM`; rodapé 10 `FG_DIM`: `Click: open terminal · Right-click: more`.
- **Cache (nada por frame):** `git2` roda só no `OverviewPoller` (thread própria, 15 s + `refresh_now`). Ao chegar `RepoOverview`, a UI constrói uma vez `GitLineCache { overview_version, branch_label, badges, worktree_rows: Vec<WorktreeRow{label, short_path, tooltip}> }` por workspace e só reconstrói quando `overview_version` muda ou ppp/tema muda. Sem `request_repaint` periódico: o poller acorda a UI uma vez por resultado diferente do anterior (resultados idênticos são descartados). `refresh_now` ao abrir o menu da worktree e ao focar a janela.

---

# Parte F — status, sessões, worktrees, Auto, link 2, prompts, prefs

Convenções: mesmas do §0 (primitivos de `hub_ui`), cores só por funções de `theme.rs` (`FG()`, `FG_SOFT()`, `FG_DIM()`, `ACCENT()`, `PALETTE_GREEN/YELLOW/RED()`, `PANEL_BG()`, `PANEL_BG_ALT()`, `TITLEBAR_BG()`, `blend`, `alpha`). Nada anima continuamente; repaint só em transição de estado ou fade com `request_repaint_after`. Modais do hub: sem `ComboBox` (seletores inline).

## 8. Status do agente (dot) e toasts

Quatro estados, distinguíveis por **forma** além de cor (daltonismo):

| Estado | Forma (raio titlebar 4.5 / sidebar 3.5) | Cor | Tooltip |
|---|---|---|---|
| working | anel 1.5 px com ponto central r/3 | `ACCENT()` | `Working…` |
| waiting | ponto cheio + halo r+2 (stroke 1, `alpha 0.45`) | `PALETTE_YELLOW()` | `Waiting for you` |
| done | ponto cheio com tick branco 1.2 px | `PALETTE_GREEN()` (esmaecido `alpha 0.55` após o painel ser focado = visto) | `Done — ready for next prompt` |
| error | ponto cheio com × 1.2 px | `PALETTE_RED()` | `Stopped with an error` |

Unknown/sem agente: **nada** é pintado (zero largura).

- **Titlebar:** o dot fica à esquerda do botão "✳ Claude ▾" (T21) e do medidor de histórico; gap 6; hit-rect 16x16 só para tooltip (sem clique). Se o badge de ssh/exit/mensagem ocupa o lugar, o dot vem primeiro (mais à esquerda).
- **Sidebar:** o dot substitui o ícone de kind **não** — vai logo antes do texto secundário à direita (coluna fixa 12 px, alinhada em todas as linhas do workspace), para a linha não "pular" quando o estado muda. Workspace recolhido: o dot de maior prioridade entre seus painéis (waiting > error > working > done) ao lado da contagem `(4)`.
- **Toast** (canto inferior direito, 12 px das bordas, largura 320, `Order::Tooltip`, máx. 3 empilhados, gap 6, TTL 6 s, hover pausa o relógio; fill `PANEL_BG_ALT()`, stroke 1 `alpha(FG_DIM(), 90)`, raio 8, barra esquerda 3 px na cor do tipo): linha 1 título 12 strong `FG()` (`conta2 · api-core needs you` / `conta2 · api-core finished`), linha 2 10.5 `FG_DIM()` (primeira linha do `Notification`/"Ready for next prompt"); clique no toast foca o painel e fecha; `✕` 14 px aparece no hover; Esc não fecha (não rouba foco). Aparecem só se o painel não está focado/visível. Entrada também cria `AttentionItem` (aparece no painel de atenção existente) e some quando o painel volta a `working`.
- **Estados:** vários toasts do mesmo painel/estado são deduplicados; tipos `Info` (accent), `Warn` (`PALETTE_YELLOW`), `Error` (`PALETTE_RED`).

## 10. Sessões recentes do Claude

Modal 560 (frame de modal, scrim), aberto por: item **Recent Claude sessions…** do menu do workspace, `Claude: Recent sessions…` na palette e **Recent sessions…** no launcher/popup do T21.

```
┌───────────────────────────────────────────────────────────┐
│ Recent Claude sessions            api-core · 14 sessions  │ header 44h
├───────────────────────────────────────────────────────────┤
│ [ filter…                                    ]            │ 28h, foco inicial
│▌Fix login redirect on expired tokens                      │ linha 44h
│  ● conta2   2 h ago   ⎇ feat/login   1.2 MB               │
│ Refactor billing client                                   │
│  ● conta1   yesterday                                     │
│ Investigate flaky e2e                          ? unknown  │
│  — account unknown   Nov 8                                │
├───────────────────────────────────────────────────────────┤
│ [Esc] close   ↑↓ select   Enter resume    [Resume here][Resume]│
└───────────────────────────────────────────────────────────┘
```
- **Agregação por worktree:** a lista junta o repo principal + todas as worktrees do repo (nomeadas pelo `RepoOverview`). Cada linha ganha um **chip de branch** (⎇ pintado + nome 10 monospace `FG_SOFT`, máx. 18 chars meio-truncado, fill `alpha(FG_DIM(), 40)`, raio 4) quando a sessão não é do repo principal; o do principal não mostra chip. Worktrees de agente (`.claude/worktrees/agent-*`) ficam num **grupo recolhido** `▸ Agent worktrees (7)` no fim da lista (cabeçalho 26h, `FG_DIM`), expandível no clique; dentro, linhas normais com chip. Mostra as **30 mais recentes** no total; rodapé da lista `Showing 30 of 112 · Show all` (link-texto `ACCENT`, 11) expande tudo (scroll preguiçoso). Resume abre o painel com `cwd` = a worktree da sessão (tooltip da linha: caminho absoluto e `Resumes in <worktree>`).
- **Linha (44h, 2 linhas):** título 12 `FG()` (summary ou primeiro prompt, controle removido, truncado com `hub_ui::elided`); linha 2 10.5 `FG_DIM()`: dot de `profile_accent` + nome da conta, data relativa (`just now`, `5 min ago`, `2 h ago`, `yesterday`, `Nov 8`), branch/cwd relativo se o projeto é um worktree, tamanho. Conta desconhecida: pílula `account unknown` `FG_DIM` e dot hueco. Selecionada: fill `blend(PANEL_BG_ALT(), ACCENT(), 0.22)` + barra 3 px; hover 0.12.
- **Lista:** altura máx. 320 com scroll, mais recentes primeiro, teto 200; scroll preguiçoso (linhas fora da tela não calculam galley); strings/galleys cacheados por `scan_version`. Carregando: 3 linhas esqueleto `FG_DIM` alpha 0.15 (sem animação); vazio: `No Claude sessions for this project yet` centralizado `FG_DIM`; erro de leitura: linha `PALETTE_RED` 11 sem detalhe de conteúdo.
- **Ações:** **Resume** (primário 30h) abre **novo painel** com a conta da sessão; **Resume here** (secundário) escreve o comando no shell ativo; Enter = Resume, Shift+Enter = Resume here; duplo clique = Resume.
- **Conta desconhecida:** o botão vira o passo inline "Which account?": faixa de chips (um por perfil, 28h, dot + nome, `1–9` atalho) no lugar do footer; escolha grava o índice e continua o resume. Sem ComboBox.

## 11. Worktrees: criar com Claude e limpeza

**Entradas:** menu do workspace e da linha git (§5/§7): **New worktree + Claude…**; menu da linha de worktree: **Remove worktree…** (desabilitado se `safe()` falso, com tooltip do bloqueio); menu da seção worktrees (cabeçalho do toggle): **Clean up worktrees…**.

**Diálogo New (modal 480):**
```
┌──────────────────────────────────────────────────┐
│ New worktree + Claude              api-core      │
├──────────────────────────────────────────────────┤
│ Branch  [ feat/new-thing                  ]      │ TextEdit 28h, foco inicial
│  existing: fix/login · chore/deps · release/2.1   │ chips clicáveis (máx. 6, filtro)
│ From    [ HEAD ▪ main ]  (segmented 2 opções)    │ só se o branch não existe
│ Path    ../api-core-feat-new-thing   (editável)   │ 10.5 monospace FG_DIM + lápis
│ Account [Auto][conta1][conta2]                    │ chips inline; Auto = T26
│ [x] also open as a workspace                      │
│ <erro de validação / branch já em uso>           │ 11 PALETTE_RED
│ [Esc]                       [Create + open Claude]│
└──────────────────────────────────────────────────┘
```
- Validação inline enquanto digita (nome inválido, branch já checked out em `<path>`, destino existente); botão primário desabilitado com tooltip do motivo. Criar roda em worker: botão vira "Creating…" (spinner estático de 3 pontos, sem repaint contínuo) e o diálogo trava Esc só até terminar. Sucesso → fecha, abre o painel Claude no worktree e toast `Info`; erro → permanece com a linha vermelha.

**Diálogo Cleanup (modal 560):**
```
┌───────────────────────────────────────────────────────────┐
│ Clean up worktrees                         api-core       │
│ Only clean, unlocked worktrees can be removed.            │ 10.5 FG_DIM
├───────────────────────────────────────────────────────────┤
│ [x] ../api-core-fix-login   fix/login        [merged]     │ linha 40h
│ [x] ../api-core-old         (dir missing)     [prunable]  │
│ [ ] ../api-core-wip         feat/wip     ● dirty  blocked │ desabilitada, alpha 0.55
│ [ ] ../api-core-pin         chore/x      🔒 locked blocked │
│ [ ] ../api-core-hub         feat/hub     in use  blocked  │
├───────────────────────────────────────────────────────────┤
│ [x] also delete merged branches                           │
│ [Esc]                        [ Remove 2 worktrees ]       │ botão destrutivo PALETTE_RED
└───────────────────────────────────────────────────────────┘
```
- Pílulas de motivo (`pill`): `merged` `PALETTE_GREEN`, `prunable` `FG_DIM`, `orphan` `PALETTE_YELLOW`; bloqueios: `dirty` (dot `PALETTE_YELLOW`), `locked` (cadeado pintado), `in use` (workspace/painel aberto), `unreachable commits`, `main` nunca aparece. Linhas bloqueadas: checkbox desabilitado, tooltip com o motivo exato; **nada bloqueado pode ser marcado**. Marcados por padrão: só `safe()`. Caminho absoluto completo no tooltip da linha. Botão mostra a contagem e fica desabilitado com 0 marcados. Depois de remover: resumo `Removed 2, kept 1 (became dirty)` (a reclassificação no momento da remoção pode manter itens) como linha final + toast; refresh imediato do poller.

## 12. Conta Auto e alerta de uso

- **Linha Auto** (topo do launcher, do popup T21 e do chip de conta do §11): mesma geometria das linhas de perfil (§2), ícone ✳ pintado em `ACCENT()` com pequeno raio de "estrela" no canto, nome `Auto`, e texto secundário cacheado `→ conta2 · 66% left` (`FG_DIM`, 10.5; `PALETTE_YELLOW` se o escolhido tem < 20% sobrando). Atalho `0` (perfis seguem `1–9`). Sem perfil utilizável: linha desabilitada, `alpha 0.55`, secundário `No account available`, tooltip lista por perfil o motivo (`expired`, `no login`). O destino é reavaliado **no clique**; a linha só informa.
- **Toast de alerta** (§8, tipo Warn/Error): título `conta1 · 80% of weekly limit` (80: barra `PALETTE_YELLOW`; 90: barra e texto do %% `PALETTE_RED`), linha 2 `Resets in 2d 4h · switch to Auto?` com link-texto `Open launcher` (abre o launcher; não troca nada sozinho). Uma vez por cruzamento; sem som. TTL 8 s.

## 13. Link nível 2

- **Send to all Claudes:** no diálogo Send (§6) a primeira linha da lista é `All Claudes in <workspace> (3)` com ícone de grupo (3 pontos pintados) e pílula `broadcast` `FG_DIM`; ao selecioná-la o campo de alvos mostra a lista dos nomes (10.5 `FG_DIM`, truncada com `+N`). Footer: `Send to 3` no lugar de `Send`. Com 0 alvos a linha é omitida. Resultado: toast `Sent to 3 · skipped 1 (not an agent)`.
- **Ask:** o `horizon link ask` não tem UI própria; no painel **solicitante** aparece um badge `asking <alvo>…` (geometria do badge §6, cor `PALETTE_YELLOW`, sem animação) até a resposta/erro; no **alvo** o badge `from <nome>` do §6 ganha prefixo `ask`. Reply recebido: o badge do solicitante vira `reply from <alvo>` por 2.3 s + fade.
- **Linha visual transitória** (camada de canvas, abaixo da titlebar dos painéis, acima do fundo): curva de Bézier suave (controle puxado 40% na horizontal) entre as bordas mais próximas dos dois painéis (clipada na borda, não no centro), stroke 1.5 px `alpha(ACCENT(), 170)`, ponta de seta 6 px no destino; `reply` em `PALETTE_GREEN`. Tempo: 0–2.3 s opaca, 2.3–3.0 s fade; ask pendente permanece estática (tracejada 6/4) até resolver, depois some em 0.7 s. Máximo 8 linhas simultâneas (excedentes não pintam); entre workspaces diferentes não há linha. Tooltip: nenhum (não interativa).

## 14. Prompts salvos

- **Palette:** entradas `Prompt: <nome>` (ícone de aspas pintado; escopo no hint à direita: `workspace` `ACCENT` ou `global` `FG_DIM`); workspace primeiro. Enter insere **sem** Enter no terminal focado; Ctrl+Enter insere e envia. Sem terminal focado: toast `Warn` "Focus a terminal first".
- **Popup do T21:** abaixo da lista de perfis, separador e linha `Insert prompt ▸` (mesma altura 32); abre uma lista no mesmo popup (passo 3, mesmas regras de teclado: ↑↓ Enter Esc, `1–9`), cada linha com nome 12 `FG()` e primeira linha do texto 10.5 `FG_DIM` (truncada); rodapé `Enter insert · Ctrl+Enter insert & send`. Oculta se não há prompts.
- **Settings:** seção **Saved prompts** no fim de Settings → Claude (globais) e **Prompts** no editor de cada workspace em Settings → Workspaces. Layout por linha (cartão 8 px de padding, raio 6, fill `PANEL_BG_ALT()`): `TextEdit` do nome (28h, 160 de largura) + botão `✕` (icon_button, tooltip "Delete prompt") na mesma linha; abaixo `TextEdit` multilinha monospace 11.5, 3 linhas (cresce até 8) para o texto; contador `n / 8192` 10 `FG_DIM` à direita; erro inline 11 `PALETTE_RED` (`Name already used`, `Too long`). Botão `+ Add prompt` (secundário) ao fim; reordenar não é necessário (ordem de criação). Sem ComboBox, sem modal.

## 15. Minimap: auto-hide e persistência

- **Auto-hide:** como a sidebar (`overlays.sidebar_auto_hide`): com a opção ligada o minimap fica oculto (zero pixels, sem repaint) e **revela** quando o ponteiro entra na zona do minimap expandida em 16 px (canto onde ele fica); entra com fade de 120 ms (alpha 0→1) e sai 600 ms depois que o ponteiro deixa a zona (fade 160 ms). Arrastar/clicar dentro mantém aberto. O atalho `toggle_minimap` continua o interruptor mestre: minimap oculto manualmente não revela por hover. Indicador de revelação: linha fina 2 px `alpha(ACCENT(), 90)` de 48 px no canto (hit-zone visível) só quando oculto por auto-hide, para o usuário saber onde apontar; some quando o minimap está aberto.
- **Settings → General:** checkbox `Minimap auto-hide` ao lado de `Sidebar auto-hide`, mesma linha de ajuda 10.5 `FG_DIM` ("Show the minimap only when the pointer is near it").
- **Persistência:** minimap/HUD ligados ou desligados pelo atalho ou pelo menu das viewports destacadas voltam como estavam ao reabrir; sem UI própria.

## 16. Rename de painel (referência)

Sem visual novo: duplo clique na titlebar já abre o editor inline (§existente); na sidebar o duplo clique numa linha de painel abre o mesmo editor inline sobre a linha (altura 26, `TextEdit` com a mesma fonte do rótulo, Enter confirma, Esc cancela, vazio/só espaços descarta). O nome custom aparece em sidebar, badge do §6, diálogo Send e `horizon link list`.

## 17. Marcador de conta na titlebar (anel de cota restante)

Em painéis Claude com perfil e em shells rodando claude com `CLAUDE_CONFIG_DIR` conhecido, a titlebar mostra um **badge de conta** (altura 18, largura = anel + gap 5 + nome elidido, máx. 120), posicionado entre o dot de status (§8) e o botão "✳ Claude ▾" / medidor de histórico; o título do painel cede espaço via `panel_title_content_rect`. Sem perfil/sem agente: nada pintado (zero largura).

```
 ◔ conta2        ← anel de 14 px + nome 10.5
```
- **Disco + anel:** disco central r=3.5 na cor do perfil (`hub_ui::accent_color(slot)`, o mesmo do chip, launcher e sidebar). Em volta, anel de raio 7.5: trilho stroke 1.8 `alpha(FG_DIM(), 70)` (círculo completo) e **arco de cota restante** stroke 2.4, cap redondo, começando às 12h e varrendo no sentido horário `restante/100 × 360°` (mín. fatia visível de 2% se > 0). Arco por polilinha (`ring_points`, ≤ 48 segmentos pela escala), nunca por galley.
- **Cor do arco:** severidade pelo usado (`severity_color`, mesma escala dos chips do §1): ok `PALETTE_GREEN()`, atenção (≥ 80% usado) `PALETTE_YELLOW()`, crítico (≥ 90%) `PALETTE_RED()`. Expirado/sem login/sem dado: arco ausente, só o trilho tracejado (6/4), disco oco (stroke 1.2 na cor do perfil, sem fill) e nome a `alpha 0.55`; o texto `expired` não é pintado (o tooltip explica). Acessibilidade: o comprimento do arco carrega o dado além da cor, e o badge tem `widget_info` com `"conta2, 34% of weekly quota remaining"`.
- **Nome:** 10.5 proportional `FG_SOFT()`, elidido com `hub_ui::elided` na largura restante; perfil default exibe `default`.
- **Hover/clique:** hover = fill `alpha(PANEL_BG_ALT(), 200)` raio 4 e cursor pointer; clique abre o popup do T21 no passo de contas (trocar de conta = nova sessão; não altera a atual). Foco de teclado: `paint_focus_ring`.
- **Tooltip** (`on_hover_ui_at_pointer`, largura 280, margin 12): nome do perfil 13 strong + pílula `active`; e-mail 11 `FG_DIM`; `7d: 66% used · 34% left` com `mini_bar` 4 px na cor de severidade; `5h: 12% used`; `Resets Mon 14:00 (in 2d 4h)` 10.5 `FG_DIM`; credencial inválida → linha `Not logged in — run claude /login with CLAUDE_CONFIG_DIR=…` `PALETTE_YELLOW` 10.5.
- **Cache/perf:** layout (galley do nome, geometria do anel, strings do tooltip) por painel, chave `(perfil, usage_version, ppp)`; sem formatação por frame; repaint só com snapshot novo. Resolução panel→perfil usa env/detector já guardados no painel (sem `/proc` por frame).
