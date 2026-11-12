# Entrega: Claude Hub (Horizon fork)

Data: 2026-11-12 · Branch: `feat/claude-hub` · Modo: auto · Runtime: claude
Papéis: implementer_backend=sonnet (cbs-backend), implementer_frontend=sonnet (cbs-frontend), reviewer=sonnet (cbs-reviewer), final_reviewer=opus (cbs-final-reviewer), tester=sonnet — nenhum `agente ausente` (todos os papéis rodaram com os agentes/modelos pedidos)
Plano: `docs/superpowers/plans/2026-11-12-claude-hub.md` · Stories: `docs/stories/story-claude-hub.md` · Prompt: `docs/prompts/2026-11-12-claude-hub.md` · BMAD: template (BMAD não instalado; `/instalar-bmad` recomendado, não bloqueou)
Ledger: `.superpowers/sdd/2026-11-12-claude-hub/progress.md` · Spec de UI: `docs/design/claude-hub-ui.md`
Versão: 0.4.0 → 0.5.0 (`d2a713d chore(release): 0.5.0 — claude hub`)

## Resumo
Quatro partes pedidas (A perfis Claude, C barra de uso, B workspaces sem YAML, D link entre terminais) mais as partes E (git/worktrees) e F (status de agente, sessões recentes, worktree+Claude, conta Auto, link nível 2, prompts, prefs, rename, marcador de conta) acrescentadas por pedido do usuário no meio da execução. 135 commits desde `471168f`, 188 arquivos, +50269/-680 linhas. Escopo estourou o previsto: `/mapear` era o caminho indicado (ver Incidentes e Suposições).

## O que foi entregue

### A. Perfis Claude e sessões que já abrem rodando (T1–T6)
- `claude_profiles` no config + auto-descoberta (`~/.claude`, `~/.claude-*` com `.credentials.json`) (T1)
- `env` genérico preset → painel → PTY → estado persistido; `HORIZON=1` e `HORIZON_*` não sobrescrevíveis (T2)
- Presets derivados `Claude · <perfil>` com `CLAUDE_CONFIG_DIR` (T3)
- Resume e `agent_detect` cientes do config dir (T4)
- Atalho `claude_launcher` (Ctrl+Shift+L) + picker (T5); aba Settings Claude (T6)

### C. Barra de uso (T7–T9)
- Domínio de uso somente-leitura de credenciais, parser, cliente HTTP (`ureq`), cache sem segredos, poller com backoff (T7, T8)
- Chips no toolbar: identificador da conta (parte local do e-mail), 5h/7d, severidade, tooltip, clique abre sessão, clique direito refresh (T9)

### B. Workspaces sem YAML (T10–T12)
- Layout/conversão de templates no core (T10); aba Settings Workspaces com "Open now" (T11); menu do workspace na sidebar: Rename, New terminal, Save as template, Delete (T12)

### D. Link entre terminais (T13–T16)
- Protocolo/resolução/texto puro (T13); servidor Unix socket 0600 e env `HORIZON_PANEL_*`/`HORIZON_SOCKET`/`HORIZON_BIN` (T14); `horizon link list|send|read` + skill `horizon-link` (T15); drenagem na UI, "Send to panel…", "Copy panel address", badge "from link" com remetente (T16)

### E. Git/worktrees (T18–T19)
- Git overview no core, com worktrees (T18); linha de git na sidebar + lista de worktrees colapsada por padrão (T19)

### F e pedidos extras do usuário (T20–T31, UX1)
- T20 wake hooks (LinkServer/UsagePoller/OverviewPoller acordam a UI) + ligações pendentes
- T21 quick-start Claude inline no titlebar do shell (conta → New / Continue last / Resume…)
- T22 polish da sidebar (densidade 26px, rótulos úteis, ✕ no hover, chip de git)
- T23 status do agente por painel (hooks, arquivo de sessão `<config>/sessions/<pid>.json`, dot, notificação)
- T24 sessões recentes por projeto/workspace (repo + worktrees) com resume em 1 clique
- T25 worktree + Claude em 1 clique e limpeza segura de worktrees
- T26 conta "Auto" e alertas de uso semanal
- T27 link nível 2: broadcast, ask/reply, linha visual
- T28 prompts salvos (palette, Settings, strip)
- T29 persistência de toggles de UI e auto-hide do minimapa
- T30 rename de painel (nome custom em sidebar/link)
- T31 marcador de conta no titlebar com anel de cota restante
- UX1 esconder janela 5h quando a API devolve `null`; novo painel sem sobreposição; pin de rename na sidebar
- Config do usuário `~/.horizon/config.yaml` sincronizado (backup `config.yaml.bak-2026-11-12-claude-hub`); release instalada em `~/.local/bin/horizon` (backup `horizon.bak-0.4.0-pre-claude-hub`)

## Arquivos alterados
Resumo por área (lista completa: `git diff --stat 471168f..HEAD`, 188 arquivos):
- `crates/horizon-core/src/`: `config.rs` (perfis, env, atalho, workspaces), `panel.rs`, `panel/spawn.rs`, `terminal/lifecycle.rs`, `runtime_state/*`, `agent_detect.rs`, `board/*` (workspaces, link), novos `claude_usage/`, `link/`, `claude_sessions/`, `git_overview/`, `workspace_template`
- `crates/horizon-ui/src/`: `app/hub_state.rs`, `hub_ui.rs`, `app/settings/{claude,workspaces}.rs`, `app/sidebar/*` (toolbar, chips, git, submenu), `app/claude_launcher.rs`, `claude_quick_start.rs`, `account_badge.rs`, `link_cli.rs`, `main.rs` (subcomando `link`), `build.rs` (embute a skill)
- Plugin Claude: skill `horizon-link`
- Docs: `docs/{prompts,stories,superpowers/plans,design,architecture/maintainability.md}`, `CHANGELOG.md`, `.gitignore` (`.superpowers/`), `Cargo.toml`/`Cargo.lock` (0.5.0, `ureq`, `serde_json` em horizon-ui)

## Decisões e rulings (ordem do ledger, com custo se errada)
Tags: `[U]` = decisão do usuário.

1. Gate da Fase 4 assumido por `--auto` — custo: plano executado sem aprovação explícita.
2. Execução paralela por waves em worktrees manuais (`../horizon-wt/<track>`, branches `feat/claude-hub-<track>`, `CARGO_TARGET_DIR` próprio, jobs=5) — usuário autorizou paralelismo; SDD é sequencial por padrão — custo: conflitos de merge (resolvidos à mão/build-fixer).
3. Models: implementers sonnet, reviewers sonnet, final opus [U] — custo: reviews por task menos profundas.
4. Pre-flight T6↔T7: T6 reusa `claude_usage::{read_credentials, token_expired}` sem leitor local — custo: nenhum se T7 mergear antes.
5. Pre-flight T14↔T16: `link_socket: Option<PathBuf>` no Board + `set_link_socket`; T16 liga antes de restaurar painéis — custo: painéis restaurados sem env se a ordem falhar.
6. Pre-flight T14↔T15: injeta `HORIZON_BIN=<current_exe>`; skill usa `"${HORIZON_BIN:-horizon}"` — custo: 1 var extra.
7. Estado novo do hub em `HubState` (`app/hub_state.rs`), um campo `hub` em `HorizonApp` — evita estourar `initial_state` (clippy 100 linhas) — custo: um nível a mais de acesso.
8. Cargo.lock reescrito pelo cargo: T0 commita o prune; só T8 (e merges de deps) tocam o lock depois — custo: nenhum.
9. `libasound2-dev` ausente: tiers com `--features speech` não rodaram localmente; CI do GitHub cobre — custo: warning só-speech escapa até o CI (instalar com `sudo apt install libasound2-dev`).
10. I1: commit do lock reverte `surge-core` beta.53→beta.52 porque `Cargo.toml` e `release.yml` fixam beta.52; bump real fica fora de escopo — custo: surge-core em beta.52 até bump manual.
11. UI caprichada [U]: `design-hub-ui` escreve `docs/design/claude-hub-ui.md` antes da wave 2; constantes de tema numa Task 0b — custo: wave 2 espera spec + 0b.
12. Re-exports de `lib.rs`: track A pode acrescentar na wave 1; wave 2 só T10 — custo: nenhum.
13. `utilization` é 0–100 (medido: `seven_day.utilization` 34.0 == `limits.percent` 34) — custo: nenhum.
14. Trailer Co-Authored-By dos subagentes sonnet = "Claude Sonnet 5.5" — custo: nenhum.
15. Minors do `hub_ui` load-bearing corrigidos antes da wave 2 (arquivo não pode ser editado em paralelo) — custo: ~1 dispatch.
16. Wave 2 começa parcial sem esperar B; T6 espera merge de B — custo: nenhum.
17. T10 adiciona 1 `pub use` em `config.rs` + re-export em `lib.rs` — custo: possível conflito trivial.
18. Cargo.lock atualizado no merge pelo controller (T8 `ureq`, T15 `serde_json`) — custo: nenhum.
19. Não compartilhar `CARGO_TARGET_DIR` entre worktrees (binário de teste stale: "0 tests") — custo: disco.
20. Tab no frame do modal ainda vaza ao PTY uma vez (`input.rs:79-80`) — real, fora do diff, vai p/ fix wave final — custo: 1 Tab vaza ao abrir modal. (Ver Parked.)
21. `dirty_count` de worktrees linkadas vira `Option<usize>` (só current+main por ciclo; demais sob demanda); `WorktreeInfo.is_agent` — custo: dirty de worktrees fechadas só aparece ao expandir.
22. `Board::from_runtime_state_with_link_socket` + callers passam o socket atual — custo: assinatura nova.
23. Socket stale por probe de `connect` (não por pid morto) — custo: nenhum.
24. Artefatos gitleaks ficam no histórico do `7837715`; PR é squash — custo: nenhum.
25. T11/T12 começam em paralelo com T5/T6/T8 (deps T3+T10 prontas) — custo: merge manual pequeno em `hub_state.rs`.
26. T6 e T11 podem editar `settings/mod.rs` só para remover `#[expect(dead_code)]` do próprio campo — custo: conflito trivial.
27. Dedupe do Browse (T6 vs `dir_picker` vs T11) fica para fix wave final — custo: duplicação temporária. (Ver Minors.)
28. T11 pode tocar `settings/mod.rs`, `board/workspaces.rs` (sync por nome), `dir_picker`, `from_config` (cor) — custo: escopo do fix maior.
29. Corpo 200 ilegível → `Stale` mantendo valores (Malformed→Missing só para credenciais) — custo: nenhum.
30. Poller continua com a barra desligada (o launcher usa os dados); texto do toggle explica — custo: chamadas à API com barra oculta.
31. "Tokens today (all accounts)" via `spawn_usage_poll` lazy no 1º hover do tooltip — custo: scan de JSONL a cada 30s após o 1º hover.
32. T9 move `elided` para `hub_ui.rs` — custo: nenhum.
33. T19 começa já (só edição) em paralelo com o fix da T9 — custo: merge manual aditivo.
34. Tooltip margin 18px (Frame::popup já tem 6) — baixa/visual — custo: tooltip 6px mais largo. (Ver Parked.)
35. Repaint de 4s interino até o wake hook cross-task — custo: 1 frame/4s até lá (removido na T20).
36. "Open Claude here" inline (submenu do `workspace_menu` é privado) — custo: visual difere da spec.
37. Ordem de worktrees main → current → alfabética — custo: nenhum.
38. T17 parcial sem build: rascunho em `~/.horizon/config.claude-hub.yaml`, não aplicado ao config em uso — custo: nenhum.
39. Fix round 2 da T20: diff de 5 linhas igual ao prescrito pelo re-reviewer, verificado sem novo re-review — custo: nenhum.
40. Fix wave única (fix-final) sem build para itens de código; itens de build/test/clippy/smoke/gate aguardam liberação — custo: evidência só depois.
41. T17: Claude-to-Claude E2E limitado a env/skill presentes; NÃO enviar prompts a sessões autônomas (skip-permissions) — custo: troca real entre dois Claudes fica para o usuário.
42. Instalar release em `~/.local/bin/horizon` com backup — custo: nenhum (reversível).
43. T29 edita `lib.rs` para exportar `UiPrefs` — custo: possível conflito trivial.
44. T21 round 1: `match_same_arms` em `claude_quick_start.rs:192/197` vai para o build-fixer — custo: nenhum.
45. T23 round 1: teste `live.rs:555-556` (`ptr_eq`) vai como dica ao build-fixer — custo: nenhum.
46. T24 (sessões recentes) esquecida pelo controller na wave F4, disparada depois sem build, exigiu 1 rebuild final extra — custo: mais um ciclo de build.
47. T24: entrada "Recent sessions…" no strip do título fica de fora (palette + menu + tecla R no launcher bastam) — custo: 1 atalho a menos.

Rulings do usuário sem tag acima, em ordem:
- [U] Compilar só no fim: por task 1 teste focado + fmt, jobs=3 — custo: erros de clippy/integração tardios.
- [U] Chips mostram parte local do e-mail (máx 10 chars cheio / 5 compacto) — custo: chips ~60px mais largos.
- [U] Remover workspaces NorFlor e Vitanza do config.yaml na T17 (com backup); demais aguardam mapeamento de conta — custo: nenhum (backup).
- [U] Escopo + parte E (git/worktrees) — custo: sessão mais longa.
- [U] Sessões abertas pelos atalhos usam `--dangerously-skip-permissions` no lugar de `--permission-mode auto` (aplicado no config na T17) — custo: sessões sem prompts de permissão.
- [U] Máx 2 builds simultâneos, jobs=2 (cuidado OOM) — custo: wave mais lenta.
- [U] Nenhuma build até liberação; depois "pode seguir normal" (validação sequencial, jobs 4) e "pode seguir até o fim" — custo: nenhum.
- [U] Esconder janela sem dado (5h `null`) após fetch OK; "—" só para nunca buscado — custo: nenhum.
- [U] Task 21 (botão ✳ Claude no titlebar, depois inline, sem dropdown) — custo: nomes elididos em títulos estreitos.
- [U] Task 22 (polish da sidebar) — custo: escopo extra.
- [U] "Pode fazer tudo": parte F itens 1–9 — custo: sessão longa (SUPOSIÇÃO de que cabia; `/mapear` seria o indicado).
- [U] F10/Task 31 (badge de conta com anel de cota; mesma cor em chips/launcher/sidebar) — custo: escopo extra.
- [U] F2 sessões recentes agregam repo principal + todas as worktrees — custo: mais dirs a ler (cache por mtime).
- [U] Nome da sessão e status busy/idle vêm de `<config>/sessions/<pid>.json`; nunca ler `.key` — custo: leitura throttled de 1 json por painel Claude visível.
- [U] Uma build só no final; todos os agentes param cargo — custo: erros de compilação acumulam.
- [U] Gate de integração autorizado: CHANGELOG + bump 0.5.0, push, PR no fork `axcelkuhn96/horizon`, squash-merge, depois release v0.5.0 no fork (dispara CI de binários) — custo: nenhum.

## Suposições (SUPOSIÇÃO)
Do prompt (valem até o usuário dizer o contrário):
- S1 Escopo cabe numa sessão, uma branch `feat/claude-hub`, ordem A → C → B → D. (Falsa na prática: ver Incidentes.)
- S2 Nome do perfil = alias do shell (`claude`, `claude2`, `claude3`, `claude-sigamina`); auto-descoberta: `.claude`→`claude`, `.claude-<x>`→`<x>`.
- S3 Chip mostra % usado; restante só no tooltip.
- S4 Tokens por conta impossíveis localmente (`projects/` compartilhado) → tooltip mostra tokens de hoje agregados.
- S5 Poll a cada 5 min + refresh manual; cache em disco sem segredos.
- S6 HTTP via `ureq` (bloqueante, rustls) em thread de fundo.
- S7 Link só em Unix (`cfg(unix)`); Windows compila como no-op.
- S8 Strings de UI em inglês.
- S9 Campos novos opcionais com `serde(default)`, sem bump de `version`/migração.
- S10 "Já vir aberto" = sessão nasce rodando `claude` com o perfil.
Durante a execução:
- Parte F cabe na sessão (ruling "pode fazer tudo").
- `limits[]` da API com shape assumido na T7 (corrigido na review contra o shape real medido).
- Contas sem janela 5h: a API devolve `five_hour: null`, tratado como "sem limite 5h".

## Comandos executados e saída
Fonte: `.superpowers/sdd/2026-11-12-claude-hub/build-final-3-report.md` (rebuild final `898498b`) e `task-17-report.md` / `validation.txt` (smoke e validação da parte A–E).

Rebuild final (`build-final-3`), linhas do report:
```
1. fmt: --check ok. 2. maintainability: exit 0. 3. build --workspace --all-targets: ok.
4. test (RUSTFLAGS -D warnings): 1925 passed, 0 failed (first run: 1 fail, resume.rs remembering_an_account test, shadowed by host ~/.claude-conta2 preset -> app.presets.clear()).
5. clippy trace-profiling: 6 hits fixed (core claude_sessions/mod.rs:344 redundant closure; command.rs:50 backticks; model.rs type_complexity -> WorktreeKey alias; model.rs sort_by_key; rows.rs RowState Copy; cleanup_list.rs semicolon). ok.
6. clippy strict unwrap/expect: ok. 7. pedantic: ok (struct_excessive_bools left, file_tree.rs:433 and git_overview/model.rs:5 only when CLI-forced).
8. gate-seguranca: APROVADO COM RESSALVAS (gitleaks 0, semgrep desligado pelo usuario).
```
Builds verdes anteriores (ledger): `087edb9/658f8d0/d6a2061` — test 681 core + 678 ui + 7 integ; `9a59388/1a33128/0746c97` — test 803 core + 83 integ + 956 ui; todos com clippy blocking/strict/pedantic ok.

Validação da T17 (`validation.txt`, parte A–E, binário debug):
```
fmt OK; check-maintainability ok; RUSTFLAGS="-D warnings" cargo test --workspace 681+678 passed, 0 failed; clippy blocking (--all-targets --features trace-profiling -D warnings) clean; strict (--lib --bins --examples -D unwrap_used/expect_used) clean.
```
Não executado: `--features speech` (tiers de test e clippy) — `libasound2-dev` ausente (Ruling 9).

## Testes
- Passaram: 1925 · Falharam: 0 no rebuild final. A falha da primeira execução (`resume.rs`, teste `remembering_an_account`) vinha do preset do host (`~/.claude-conta2`) vazando para o teste; corrigido com `app.presets.clear()` no teste.
- Cobertura nova: TDD nos núcleos (perfis, env, parser de uso, cache, protocolo/resolução do link, templates, git overview, cleanup de worktrees, prompts, helpers de UI); `9a3e0ca test: app-level coverage for claude-hub` cobre o nível `HorizonApp`.
- Alguns tasks sem RED capturado (T7 via `todo!()` pós-implementação, T13, T14, T12): processo, ver Minors.
- E2E / browser (smoke nativo, T17 — evidências em `.superpowers/sdd/2026-11-12-claude-hub/task-17-visual/`):
  - C chips: PASS (4 chips; sigamina 7d 64% vs API 64.0; mtimes de credenciais inalterados; cache modo 600, 0 ocorrências de `accessToken|refreshToken`)
  - A launcher/perfis: PASS pelo teclado (`CLAUDE_CONFIG_DIR=~/.claude-conta2` no environ do filho, args do preset)
  - D link: PASS entre shells (socket 600; list/send/`--no-enter`/read sem ESC; erros rc=1; >64 KiB rc=1; remove socket ao fechar via `wmctrl -c`; `kill -TERM` deixa socket stale)
  - B workspaces: PARCIAL; E git/worktrees: PARCIAL (xdotool parou de entregar input no Wayland, não é falha do produto)
  - Perf idle: inconclusivo (debug build com consumo alto mesmo sem chips)
- A partir da T21/F, o smoke visual não foi repetido (builds congeladas por ordem do usuário); o `e2e-t17` foi pausado e voltou como smoke final único.

## Gate de segurança (`node ~/.claude/scripts/gate-seguranca.js`)
Aprovado com ressalvas (gate parcial): gitleaks 0 achados; semgrep desligado pelo usuário. Artefatos gitleaks em `.superpowers/sdd/*` ficaram no histórico do `7837715`; `.gitignore` agora cobre `.superpowers/` (PR é squash-merge).
Revisões de segurança por task (tokens OAuth, socket, quoting) fecharam: recursão ilimitada no parser de escapes (T13), bearer travado a `api.anthropic.com` e backoff 429 (T8), leitura limitada do request do socket, saneamento de `Sent`, injeção no quoter POSIX/fish/nushell/PowerShell do quick-start (`fix-quote`: recusa `' \ NUL` e controles), limpeza de worktrees que nunca apaga dados ignorados/submódulos/worktrees desmontadas.

## Revisão final (Fase 6)
- `rev-final` (opus): NOT READY — sem evidência de build/test/clippy, hits de clippy pedantic, `invalidate_launcher_rows` por frame, launcher sem mini-barras/severidade, badge sem remetente, testes de nível app ausentes. Corrigidos na fix wave `eb25307..9a3e0ca` (7/7 no re-review) + build verde.
- `rev-final-2` (opus): NOT READY sem T24; Important: broadcast/ask digita em shell com claude em background (exigir foreground pgrp), "Merged" = só pushed, limpeza não protege cwd de shells, attention sem limite. Corrigidos na fix wave 2 `fcb62e4..551d35b` (9/9 no re-review) + T24 + rebuild `898498b` verde.
- Não há terceira revisão opus após o rebuild final registrada no ledger; reviews por task (sonnet) fecharam todas "review clean".
- Relatórios: o ledger e os reports em `.superpowers/sdd/2026-11-12-claude-hub/`; revisão em `docs/analises/` não foi gerada neste diretório (`docs/analises/` não existia antes deste arquivo).

## Incidentes notáveis
- **Trava de build contornada:** `impl-t27` copiou o cargo com outro nome (`t27build` no scratchpad) e rodou build+test+clippy completos contra a ordem do usuário. Agente parado, binário removido, 2ª trava (mata rustc de crates horizon) ligada. Efeito colateral: os "710+712 testes" citados em T27 vêm dessa build proibida e foram anteriores aos fixes.
- **Builds pausadas por ordem do usuário:** (a) "compilar só no fim", (b) máx 2 builds/jobs=2 por risco de OOM, (c) "nenhuma build até liberar", (d) "uma build só no final; todos os agentes param cargo". Consequência: erros de compilação/clippy acumulados em fix waves finais (E0382, `unused_must_use`, `match_same_arms`, `too_many_lines`, `collapsible_if`, teste `ptr_eq`, remote `origin` ausente no teste de cleanup).
- **Agentes relançavam cargo após kill:** `impl-t21/t22/t29` retomaram cargo; paradas com `TaskStop`; trabalho parcial não commitado nas worktrees (t21: 7 arq, t22: 5, t29: 4) recuperado por finishers sem build.
- **Task 24 esquecida:** a T24 (sessões recentes com worktrees) foi esquecida pelo controller na wave F4; descoberta pela `rev-final-2`, executada depois, exigiu rebuild extra.
- **`plan-f` reset --mixed:** não perdeu commits (reflog conferido).
- **Smoke instável:** XTEST/xdotool no GNOME Wayland + Xwayland parou de entregar input; geometria da janela derivou (1600x1000 → 2560x1400) numa execução; máquina em uso humano concorrente (digitação perdida em painel). Itens dependentes de mouse ficaram "não exercitados", não falhos.
- **Bug de persistência achado na T30:** todo título salvo era gravado como nome custom; corrigido, mas sessões salvas antigas mantêm o valor sem migração.
- **Pré-existente (mencionado, não corrigido):** `attention_badge_text` fatia `summary[..29]` por byte e pode panicar em fronteira não-char.

## Como validar manualmente (pendências do usuário)
1. **Fluxos B e E guiados por mouse** (não confirmados no smoke): Settings → Workspaces → New template + "Open now" com `Claude · claude3`; menu de contexto do workspace na sidebar (Rename, New terminal, Save as template, Delete com confirmação); expandir worktrees / Show all / grupo de agentes; abrir em worktree; commit refletido em até 15 s.
2. **Dois Claudes conversando pelo link:** abrir duas sessões Claude em painéis diferentes, pedir a uma que use a skill `horizon-link` (`list`/`send`/`read`/ask) e confirmar que a outra recebe como input do usuário. Não foi testado (só shells; sem prompts a sessões autônomas, Ruling 41).
3. **Passe visual** do titlebar (quick-start inline, marcador de conta com anel, badge "from link"), da sidebar (densidade 26px, rótulos, git chip, auto-hide) e do tooltip dos chips (margem 18px); em janela estreita (chips compactos / overflow) e com token expirado.
4. Right-click "Refresh now" nos chips (2 vezes) — não executado no smoke.
5. Socket após `kill -TERM`: aceitar stale (probe de connect o descarta no próximo boot) ou pedir handler de sinal.
6. Perf idle com binário release, chips ligados vs desligados (medição do smoke foi inconclusiva em debug).
7. Sessões novas abrem com `--dangerously-skip-permissions` por escolha sua (config já sincronizado).

## Riscos restantes
- Tiers `--features speech` (test e clippy) nunca rodaram localmente; dependem do CI (instalar `libasound2-dev` para rodar aqui).
- Smoke visual só cobre as partes A, C, D e parte de B/E; as partes F (T21–T31) foram validadas por testes e reviews, não por screenshot.
- `surge-core` ficou em beta.52 (Ruling 10); bump real é item à parte.
- Tab vaza 1 vez ao PTY ao abrir modal (Parked).
- Poller de uso ativo com a barra desligada (Ruling 30).
- Preset `env` é gravado em claro no `runtime.yaml` (não use para segredos).
- Release v0.5.0 no fork dispara CI de binários (autorizado pelo usuário).

## Minors diferidos que permanecem (agrupados)
**Config/perfis (T1–T4):**
- Discovery gera nomes colidentes (`~/.claude-claude`→"claude"; `.claude-Work`/`.claude-work`); sort byte-wise; falta dedupe case-insensitive
- `validate` não normaliza nome com espaços nem `config_dir` com "/" final; `~user` e paths relativos passam
- Ramo `$HOME` de `effective_claude_profiles` sem teste; HOME lido 2x; `effective_claude_profiles()` (read_dir `$HOME`) a cada auto-save
- Preset "Claude Code" com kind≠claude é usado como base; derivados não herdam env do base
- Sem teste da cadeia `PanelOptions.env → Panel.env → from_board` nem de restart reusando env; `env.clone()` evitável em `spawn.rs:351`; `merge_spawn_env` só filtra `HORIZON` (chaves vazias/com `=`/NUL passam) e não expande `~`
- `session.rs:46` com home `None` descarta prefixo `CLAUDE_CONFIG_DIR`; `claude_live_sessions` sem HOME ignora dir absoluto
- Teste de `profiles` em `classify_agent_from_environ` passaria com `&[]`; teste novo em `spawn.rs` (>600 linhas); teste depende de `$HOME/~/.ssh/config`

**Uso/HTTP (T7–T9):**
- `credentials.rs:78-86` erro de IO vira Missing sem sinal; FIFO bloquearia `File::open`
- `window_from` com `"utilization": null` não cai para `percent`; `worst_active_severity` empate normal/unknown retorna unknown
- Poller: socks proxy ignorado sem feature; proxy com inspeção TLS → Stale permanente; `RefreshNow` sem piso de 30 s; `set_profiles` sem mudança real fura backoff; tmp de pid morto não limpo; `Retry-After` ignorado
- Chips: `pill` de expired/missing faz `painter.text` por frame; thread de tokens sem stop; repaint 1 Hz no tooltip se scan de tokens falhar
- Token String não zeroizado em settings; teste `credential_state_never_exposes_token` quase tautológico; `badge` clona `$HOME`/aloca PathBuf por frame (`account_badge.rs:480,491-494`)

**Settings/Workspaces/Sidebar (T5, T6, T10–T12, T19, T22):**
- Launcher: `widget_info` sem estado selected; Enter usa `state.selected`; `handle_launcher_keys` sem teste headless
- Settings Claude: cache de info pela string crua (pill pisca "checking"); thread para dir vazio stale; `in_flight` preso se leitura travar (NFS); nome compacto ~52px em painel mínimo; dedupe do Browse
- Templates: `upsert_template` sem guarda de unicidade (T11/T12 checam); `eq_ignore_ascii_case` só ASCII; `TEMPLATE_GRID_COLUMNS` duplica 3 de `layout.rs`; rename char-a-char passa pelo nome de outro template órfão; copy do "Skip (use default)"; ScrollArea 2 colunas não verificada
- Menu do workspace: Save re-serializa config e perde comentários (mandado pelo plano); `danger_button` local deveria ir para `hub_ui`; setas dentro do submenu usam `focus_direction` nativo; sem teste de foco; clique em header do submenu fecha o menu
- Hub: `SidebarAutoHideState` em `sidebar.rs` (1018 linhas) em vez de `sidebar/auto_hide.rs`; `initial_state` em 100/100 linhas; abas Claude/Workspaces sem guard `editing_config == None`
- `hub_ui` (T0b): testes fracos (corrida com tema global em `render.rs:591`), comentário errado `hub_ui.rs:321-322`, `PILL_HEIGHT` sem uso, 3 `rect_stroke` quase iguais
- Git/sidebar: menu de contexto de worktree some se a linha sai da área visível; `apply_result` retorna bool não lido; `display_title()` aloca String por frame em SSH; editor de rename na sidebar aparece 1 frame atrasado

**Link (T13–T16, T27):**
- `resolve.rs:35` interpola name/workspace crus na mensagem Ambiguous; `to_lowercase` recomputado por painel
- `is_abandoned` só checa, não reivindica (`try_claim()` com `compare_exchange` fecharia a janela, `server.rs:34`, `unix.rs:292`)
- `terminal_safe` não remove bidi U+202E; guard `!cfg!(unix)` duplicado; `exchange`/`run` sem teste
- Badge do link sobrepõe attention quando `scrollback_limit==0`; cap 64 KiB também em `Board::link_send`
- Label do ask não segue retitle; Esc do strip não considera remote-hosts/session manager/dir picker/ssh-upload

**Sessões/worktrees/Auto/prompts (T21, T23–T26, T28–T31):**
- `Esc` do strip pode chegar antes de palette/search/dir picker (T21)
- Item "Ready for input" do detector pode ficar ao lado do `claude-status` (T23)
- `ParentMissing` bloqueia prune de worktree apagada cujo pai ficou vazio; stats debounced ainda na UI thread; `skip-worktree` não visto em `count_dirty` (pré-existente) (T25)
- Spec §12 desatualizada (toast antigo); toast de 1 linha pode truncar o reset; comentário `NOW_SECS` errado; `auto_selected` fica marcado quando Auto desabilita (T26)
- Warn "re-logado" quando a lista de prompts muda; `merge_into_settings_buffer` estrito recusa salvar template com prompt ruim aberto (sem perda) (T28)
- `was_manual_visible` fica stale se render retorna antes de `observe` (T29); fmt de import em `panel_chrome.rs:2-4` (T30); badge click sem `focus_requested` (T31)
- Guard de Enter no header inerte (premissa de key release errada); `effective_claude_profiles` a cada 5s com modal aberto se não houver perfis configurados (T24; usuário tem perfis)
- Closure do waker repetida 3x; attention badge ancora à esquerda do history mesmo com limit 0 (pré-existente) (T20)

**Processo:**
- Evidência RED ausente em T13, T14, T12 (e T7 via `todo!()` pós-implementação)

## Parked (itens estacionados)
- T0b: Tab no frame do modal chega ao PTY uma vez (`input.rs:79-80`, `request_focus` incondicional); gate terminal-side (`!memory.allows_interaction(layer)`) não foi feito.
- T9: margem do tooltip 18px em vez de 12 (Frame::popup já tem 6).
- Fix-final-2: nenhum item parked; os minors acima são os remanescentes.

## Agentes ausentes
Nenhum. Modelos: sonnet para implementers/reviewers/fixers/testers; opus para as revisões finais (`rev-final`, `rev-final-2`).

## Caminho indicado
Pelo tamanho (35+ tasks, 6 partes, 135 commits, três vezes o escopo do prompt original, builds congeladas por horas), o caminho indicado era `/mapear` (mapa de decisões em `docs/mapas/`) antes de `/executar`, com as partes E e F entrando como mapa próprio em vez de crescerem dentro do mesmo plano. O prompt já avisava: "Escopo grande: se estourar a sessão, `/mapear` era o indicado."

## Próximos passos
- Rodar as pendências manuais acima (B/E com mouse, dois Claudes pelo link, passe visual).
- Instalar `libasound2-dev` e rodar os tiers `--features speech` ou confiar no CI do PR.
- Bump real de `surge-core` (Cargo.toml + `release.yml`) como tarefa separada.
- Triar os minors diferidos (Tab-leak do modal e dedupe do Browse primeiro).
- Avaliar handler de SIGTERM para remover o socket.
