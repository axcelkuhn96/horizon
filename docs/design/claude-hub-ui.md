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

Chip cheio (largura calculada, máx ~330). **Pedido do usuário:** depois do nome vem o identificador da conta = parte local do e-mail (antes do `@`), proportional 10.5 `FG_DIM()`, separado por ` · `, máx 10 chars + `…`; omitido se o e-mail é desconhecido. O mesmo identificador aparece no compacto (máx 5 chars) e nas linhas de overflow:
```
 ┌────────────────────────────────────────────────────┐
 │ ● conta2 · gutosjc  5h ▰▰▰▰▱▱▱▱ 42%  7d ▰▰▱▱▱▱▱▱ 18% │   28h
 └────────────────────────────────────────────────────┘
  8 6 dot  name(max 84, ellipsis)  8  grupo=label10+4+bar28x4+4+pct26  8 gap  grupo  8
```
- Fill `alpha(blend(PANEL_BG_ALT(), accent, 0.10), 232)`, stroke 1 `alpha(blend(BORDER_SUBTLE(), accent, 0.36), 220)` (mesma receita do fps meter), raio 10.
- Dot: círculo r=3, `accent` (acento do perfil). Nome: proportional 11.5 `FG()`. Rótulos `5h`/`7d`: proportional 8.5 `FG_DIM()` (mesmo tamanho do "fps"). Percentual: monospace 10.5, cor = `severity_color` quando ≥75, senão `FG_SOFT()`; largura fixa 26 (direita alinhada) para o chip não "dançar" entre 9% e 100%.
- Barras 28x4, centralizadas; frac = pct/100 clamp.
- Compacto (nome + identificador curto + 7d): dot, nome (máx 64), ` · ` + identificador (máx 5 chars + `…`, `FG_DIM`), barra 7d 20x4, pct. Largura ~185. Ativa para todos os chips ao mesmo tempo (nunca mistura cheio+compacto).
- Prioridade de espaço: (1) busca encolhe até o mínimo atual; (2) todos compactam; (3) chips que não cabem vão para o menu de overflow existente como linhas `● nome  5h 42%  7d 18%` (mesmo layout da linha do launcher, sem badge); o `OverflowMenu` aparece se houver itens de ação OU chips excedentes.
- Estados:
  - hover: fill `blend(PANEL_BG_ALT(), accent, 0.20)`, stroke `blend(BORDER_STRONG, accent, 0.45)`, cursor pointer; pressed: fill `blend(PANEL_BG_ALT(), accent, 0.30)`.
  - focus (Tab): anel de foco padrão.
  - stale (dado OK mas `fetched_at` > 15 min): dot vira anel (stroke 1.2, sem fill), percentuais `FG_DIM()`.
  - expired/missing: conteúdo todo a alpha 0.55; barras mostram o último valor conhecido a alpha 0.4; o grupo 7d é substituído por pílula `expired` (`PALETTE_YELLOW`) ou `no login` (`FG_DIM`); compacto: só a pílula.
  - sem dado ainda (boot sem cache): barras vazias, pct `—`.
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
│  [3] ● pessoal  —                    5h —   7d —     │
├──────────────────────────────────────────────────────┤
│ [↑↓] move  [Enter] open  [1–9] pick  [Esc] close     │  footer 36h, kbd_hint
└──────────────────────────────────────────────────────┘
```
- Linha: raio 8, padding horizontal 12. Esquerda: `key_badge` (20x20; só 1–9, a partir do 10º perfil o espaço fica vazio), gap 10, dot r=4 acento, gap 8, coluna de texto: nome 13 `FG()` strong (linha 1), e-mail 11 `FG_DIM()` (linha 2; `—` se desconhecido; ellipsis). Direita (largura 132): duas linhas empilhadas `5h` / `7d` com barra 56x4 + pct monospace 10.5 (mesma cor/regra de severidade). Estado `expired`/`missing`: pílula na linha 2 do texto, barras a alpha 0.4.
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
