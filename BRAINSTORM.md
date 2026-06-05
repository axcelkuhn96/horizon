# Horizon Fork — Brainstorm (input para /piloto)

**Projeto:** fork local de `peters/horizon` (MIT, Rust workspace: `horizon-core`, `horizon-cursor`, `horizon-ui`).
**Premissa:** o Horizon é o **canvas/host de terminais**. A orquestração de IA é feita **dentro do Claude Code** — o Horizon NÃO gerencia agentes. Logo, as melhorias são puramente **UX de navegação + conveniências de terminal/canvas**.
**Abordagem:** A — fork + patch + build local (sem PR). Manter o patch rebaseável sobre o upstream v0.2.6.

---

## ✅ v1 — escopo trancado (este patch)

### 1. Foco direcional entre painéis por SETAS + modificador
- Foca o painel à esquerda/direita/cima/baixo do painel atual (espacial, combina com o canvas).
- Binding **configurável** via seção `shortcuts` do `config.yaml` (ex.: `focus_panel_left/right/up/down`), default a definir (testar conflito com readline/terminal — Alt vs Ctrl+Alt).
- Módulos: `horizon-core/src/shortcuts.rs`, `horizon-ui/src/input/keyboard.rs`, `horizon-ui/src/app/shortcuts.rs`.

### 2. Pan no touchpad — configurável
- **Default:** segurar um modificador (ex.: Alt, interceptado pelo app ANTES do terminal) + 2 dedos = move o canvas em qualquer lugar, inclusive sobre terminais. Sem o modificador, 2 dedos continua rolando o scrollback do terminal.
- **Toggle no `config.yaml`** (ex.: `input.scroll_pans_over_panels: true`) para o modo "2 dedos sempre move o canvas".
- Módulos: `horizon-ui/src/input/mouse.rs`, `horizon-ui/src/terminal_widget/input.rs`, `horizon-core/src/config.rs`.

### 3. Auto-fit ao focar painel
- Ao focar um painel (via setas), ele centraliza/encaixa sozinho no viewport — navegação sem mouse fica completa.
- Decisão aberta: animado vs instantâneo + toggle on/off.
- Módulos: `horizon-ui/src/app/panels.rs` + lógica de viewport/zoom.

---

## ✔️ Já feito fora do fork (config manual, sem compilar)
Presets/workspaces por projeto criados direto no `~/.horizon/config.yaml`: um workspace por repositório git em `~/`
(expia, CobraAI, Vitanza, Sigamina, MizuConecta-Camunda, MizuConecta-Laravel, Camunda-Express), cada um com shell no `cwd`.
**Opcional para o fork:** uma feature que **auto-descobre** repos git em `~/` e gera os workspaces sozinha (hoje é manual).

---

## ❌ Fora de escopo (feito no Claude Code)
Broadcast para agentes · attention feed · badges de estado de agente · hooks de "agente terminou" · resume de agente.

---

## 💡 Backlog / ideias futuras (não-agente)
- UX/canvas: solo/esconder os outros painéis · sensibilidade de pan/zoom · lembrar zoom/posição por workspace
- Terminal QoL: scrollback configurável · busca melhor · copiar URL/arquivo
- Não-UI: controle CLI (abrir workspace / mandar comando pra painel via script) · painel de dados não-terminal (tail de log) · export de telemetria de tokens

---

## ⚠️ Pontos a resolver no /piloto
- Modificador default das setas sem conflito com terminal/readline.
- Auto-fit: animado vs instantâneo + toggle.
- Sintaxe da nova seção `input` no `config.yaml` (pan).
- Patch rebaseável sobre upstream v0.2.6.
