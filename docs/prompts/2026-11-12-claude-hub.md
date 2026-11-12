---
tipo_de_tarefa: misto — regra-de-negocio (perfis/derivação de presets, parser de uso, protocolo do link) + ui (toolbar, abas de settings, menus) + integracao (OAuth usage HTTP, socket local)
dominios_previstos: rust (fora da tabela de roteamento) + frontend (egui) | rust + integracao
modo: auto
models: implementer_backend:sonnet,implementer_frontend:sonnet,refactor:sonnet,tester:sonnet,reviewer:sonnet,final_reviewer:opus
branch: feat/claude-hub
review: seguranca (tokens OAuth lidos do disco; socket local que injeta texto em PTYs)
---

# Diagnóstico

Quatro entregas no Horizon (fork Rust/egui): (A) perfis Claude (4 contas via `CLAUDE_CONFIG_DIR`) com sessões abrindo já rodando por atalho; (C) chips no toolbar com % de uso 5h/7d por conta; (B) criar/editar workspaces por formulário, sem YAML; (D) link entre terminais (socket local + CLI + skill) para sessões conversarem. Pronto pra executar; todas as decisões vieram da grelha em modo auto (SUPOSIÇÃO). Escopo grande: se estourar a sessão, `/mapear` era o indicado.

# Contexto usado

- Memória CBS (Mem0) — NÃO ENCONTRADO: MCP CBS Memória ausente nesta sessão.
- Memória local `claude-multiconta-auth`: refresh concorrente de OAuth já zerou tokens de conta2/3/sigamina → Horizon nunca faz refresh nem grava credenciais — FATO (histórico).
- Aliases: `claude`→`~/.claude`, `claude2`→`~/.claude-conta2`, `claude3`→`~/.claude-conta3`, `claude-sigamina`→`~/.claude-sigamina` (`~/.zshrc:49-51`, `~/.bashrc:130-132`) — FATO.
- Cada dir tem `.credentials.json` (`claudeAiOauth.{accessToken, expiresAt(ms), refreshToken, subscriptionType, rateLimitTier}`) e `.claude.json` (`oauthAccount.{displayName, emailAddress, ...}`); `projects/` de conta2/3/sigamina é symlink pra `~/.claude/projects`; `sessions/` é próprio — FATO (inspeção local).
- `GET https://api.anthropic.com/api/oauth/usage` com `Authorization: Bearer <accessToken>` + `anthropic-beta: oauth-2025-04-20` responde `five_hour`/`seven_day` `{utilization, resets_at}` (podem ser `null`) e `limits[{kind, group, percent, severity, resets_at, is_active}]`; hoje 3 de 4 tokens expirados — FATO (chamada ao vivo).
- `PresetConfig` sem `env` (`crates/horizon-core/src/config.rs:86-100`); `TerminalConfig` (`:525-549`) e `PanelOptions` (`panel.rs:114-135`) sem `env`; `TerminalSpawnOptions.env` já chega ao PTY (`terminal/lifecycle.rs:44`); `agent_env` só injeta `HORIZON=1` (`panel/spawn.rs:911-917`, usado em `spawn.rs:360` e `panel.rs:544`) — FATO.
- Resume preso a `~/.claude`: `runtime_state/claude_live_sessions.rs:18-21,75-80`; `agent_detect.rs:33-43` só reconhece sufixo `.claude-conta2` — FATO.
- Workspaces de `config.yaml` são templates: só viram estado em sessão nova (`runtime_state.rs:53`, `session_store.rs:122`); edições da UI vão pra `~/.horizon/sessions` (`app/persistence.rs:22-49`); nada volta pro config; `sync_workspace_metadata` só nome/cwd (`board/workspaces.rs:270-281`) — FATO.
- Settings: abas General/Shortcuts/Presets/YAML; `SettingsEditor` (`app/settings/mod.rs:53`), `save_settings` (`:191`) → `atomic_write` (`app/util.rs:211`) → `apply_runtime_config` (`:237`); Presets não edita command/args/env — FATO.
- Toolbar raiz `render_toolbar` (`app/sidebar/toolbar.rs:14`), layout `RootToolbarLayout`/`ToolbarItem` (`app/root_chrome.rs`) — FATO.
- Usage atual: `usage_stats.rs` + `usage_stats/claude.rs` leem só `$HOME/.claude/projects`, poll 30s por painel Usage — FATO.
- Sem IPC/socket/MCP no repo; `Panel::write_input` (`panel.rs:403`), `Board::panel_mut` (`board.rs:355`), `Board::panel_id_by_local_id` (`board.rs:379`), `Panel.local_id` persistido (`panel.rs:164`) — FATO.
- CLI: parse manual em `crates/horizon-ui/src/main.rs:325` (sem clap) — FATO.
- Sem crate HTTP (só `tokio` rt+macros e `surge-core`) — FATO.
- Atalhos livres: Ctrl+Shift+L e Ctrl+Shift+1..8 — FATO (grep em config.rs/shortcuts.rs).
- `scripts/check-maintainability.sh`: cap de linhas removido no fork (guideline de split ~600 continua) — FATO.
- BMAD — NÃO ENCONTRADO (`_bmad/_config/manifest.yaml` ausente); recomendação: `/instalar-bmad` (não bloqueia).

# Dúvidas

## Bloqueantes — nenhuma (decididas na grelha em modo auto, abaixo como SUPOSIÇÃO)

## Não bloqueantes — assumidas (SUPOSIÇÃO)

- S1 Escopo cabe numa sessão, uma branch `feat/claude-hub`, ordem A → C → B → D.
- S2 Nome do perfil semeado igual ao alias do shell (`claude`, `claude2`, `claude3`, `claude-sigamina`); na auto-descoberta, nome derivado do dir (`.claude`→`claude`, `.claude-<x>`→`<x>`).
- S3 Chip mostra % **usado** (mesma semântica do `/usage` do Claude); % restante só no tooltip.
- S4 Tokens por conta são impossíveis localmente (`projects/` compartilhado) → tooltip mostra tokens de hoje agregados.
- S5 Poll de uso a cada 5 min + refresh manual; cache em disco sem segredos.
- S6 HTTP via `ureq` (bloqueante, rustls) numa thread de fundo, coerente com o modelo de threads atual (std thread + mpsc).
- S7 Link só em Unix (`cfg(unix)`); no Windows compila como no-op documentado.
- S8 Strings de UI em inglês (padrão do fork).
- S9 Campos novos de config opcionais com `serde(default)`, sem bump de `version`/migração, salvo se o implementador provar necessidade.
- S10 "já vir aberto" = a sessão nasce rodando `claude` com o perfil (não um shell onde se digita `claude`).

# Prompt final premium

<role>
Agente sênior no Claude Code operando com BMAD 6.12 + Superpowers 7.0 via executor-bmad-superpowers. Não implemente inline; siga o executor.
</role>

<project_context>
- Repo: `/home/axcel/horizon-fork` (fork pessoal `axcelkuhn96/horizon` do Horizon; remote `fork` = push, `origin` = upstream peters/horizon). Versão 0.4.0.
- Stack: Rust edition 2024 · eframe/egui (wgpu) · alacritty_terminal. Crates: `horizon-core` (domínio: config, board, panel, PTY, sessões, usage) e `horizon-ui` (binário egui: app/, settings/, sidebar/, command_palette/, dir_picker/).
- Regras do projeto (AGENTS.md é a fonte de verdade — ler antes de começar): erros tipados com thiserror; sem `unwrap`/`expect` em código de runtime; `tracing`; `#![forbid(unsafe_code)]`; lógica de domínio e estado em `horizon-core`, UI só renderiza/coleta ações; módulos pequenos de um propósito (split em ~600 linhas; arquivos já grandes como `config.rs` 1112, `panel.rs` 1241, `panel/spawn.rs` 1178, `app/sidebar.rs` 1011 devem ser divididos ao serem tocados em vez de crescer); `#[cfg(test)]` no fim do arquivo; defaults centralizados; sem alocação no hot path de render; checklist de perf para overlay sempre visível (sem repaint contínuo, cache de layout/strings).
- Mudou config/presets/atalhos → sincronizar `~/.horizon/config.yaml` do usuário (backup antes).
- Dependência nova: versão estável mais recente do crates.io, no `[workspace.dependencies]`, com justificativa no commit.
- Contas Claude do usuário: `claude`→`~/.claude`, `claude2`→`~/.claude-conta2`, `claude3`→`~/.claude-conta3`, `claude-sigamina`→`~/.claude-sigamina`; credenciais em `<dir>/.credentials.json`, conta em `<dir>/.claude.json`; `projects/` é compartilhado por symlink.
- Memória CBS não disponível nesta sessão. BMAD não instalado (`/instalar-bmad` recomendado, não bloqueia).
</project_context>

<task>
Entregar, na branch `feat/claude-hub`, quatro partes, nesta ordem:

**A. Perfis Claude e sessões que já abrem rodando**
1. Seção nova `claude_profiles: [{ name, config_dir }]` no config (`horizon-core`). Ausente/vazia → auto-descoberta de `~/.claude` e `~/.claude-*` que tenham `.credentials.json` (ordem estável: `~/.claude` primeiro, depois alfabética; nome derivado do dir, S2).
2. `PresetConfig` ganha `env: BTreeMap<String,String>` genérico (`serde(default, skip_serializing_if = "is_empty")`), levado por `PanelOptions` até `TerminalSpawnOptions.env`, mesclado com `agent_env` (env do preset vence, exceto `HORIZON`); env guardado no `Panel` e no estado persistido para restart e restore da sessão.
3. `resolved_presets` gera um preset derivado por perfil: nome `Claude · <perfil>`, alias = nome do perfil, kind `claude`, `env.CLAUDE_CONFIG_DIR=<config_dir expandido>`, args herdados do preset `Claude Code` base do usuário. Presets derivados aparecem no preset picker e na command palette (via `command_palette_preset_entries` existente) e não são gravados no YAML como presets.
4. Resume e detecção cientes do config dir: `live_claude_session_ids` / `claude_session_transcript_exists` (`runtime_state/claude_live_sessions.rs`) recebem o config dir do painel (default `~/.claude`); `agent_detect` classifica pelo `CLAUDE_CONFIG_DIR` comparando com os perfis configurados (não mais só o sufixo `.claude-conta2`).
5. Atalho novo `claude_launcher` (default `Ctrl+Shift+L`, editável na aba Shortcuts, seguindo o molde de `toggle_scroll_pan`): abre um picker pequeno com os perfis (nome + % usado 5h/7d quando houver), teclas `1`–`9` escolhem, Enter/clique confirmam, Esc fecha; a escolha abre uma sessão Claude rodando no workspace ativo pelo fluxo existente `add_panel_to_workspace` (dir picker se o workspace não tem cwd).
6. Aba Settings nova **Claude**: lista os perfis (nome, config_dir com dir picker, estado da credencial: ok/expirada/ausente — sem exibir token), adicionar/remover/reordenar, botão "Detect accounts" que roda a auto-descoberta.

**C. Barra de uso no topo**
1. Módulo de domínio em `horizon-core` (ex.: `claude_usage/`): lê `accessToken` e `expiresAt` de `<config_dir>/.credentials.json` **somente leitura**; se válido, chama `GET https://api.anthropic.com/api/oauth/usage` (headers `Authorization: Bearer …`, `anthropic-beta: oauth-2025-04-20`, timeout 10s) e extrai 5h e 7d (`five_hour`/`seven_day.utilization` e `resets_at`, com fallback em `limits[]` por `group`); lê `oauthAccount.displayName`/`emailAddress` de `<config_dir>/.claude.json`.
2. **Nunca** faz refresh do OAuth nem escreve em `.credentials.json`/`.claude.json`. Token expirado/ausente/401 → estado `Expired`/`Missing` com o último valor conhecido.
3. Poller único (std thread + mpsc, dono = app, não por painel): a cada 5 min + refresh manual; resultados gravados em `~/.horizon/claude-usage-cache.json` (sem segredos: só nome, %, resets_at, fetched_at, estado) e carregados no boot.
4. UI: novo `ToolbarItem` no toolbar raiz existente (sem segunda faixa): um chip por perfil com nome + mini-barras/porcentagens `5h NN%` e `7d NN%`, cor por severidade (normal/aviso ≥75%/crítico ≥90% ou `severity` da API); chip esmaecido com "expired" quando sem token válido; tooltip com displayName/e-mail, % usado e % restante, horário de reset local, idade do dado, tokens de hoje agregados (reuso do `usage_stats`, S4) e dica "open `<perfil>` in a terminal to renew login". Clique esquerdo = abre sessão Claude daquele perfil (igual ao launcher); clique direito = "Refresh now". Layout responsivo: com pouco espaço, chips compactam (só nome + 7d) e o excesso vai pro overflow menu existente.
5. Toggle `features.claude_usage_bar` (default `true`) na aba General.

**B. Workspaces sem editar YAML**
1. Aba Settings nova **Workspaces** (formulário sobre `editing_config.workspaces`): lista de templates com adicionar, renomear, remover (com confirmação), reordenar; campos nome, cwd (dir picker reaproveitando `dir_picker`) e cor; por template, lista de terminais com adicionar por preset (inclui os perfis Claude), nome, kind (somente leitura, vem do preset), cwd opcional, remover, reordenar; posições/tamanhos novos em layout automático (grade simples, lógica em `horizon-core`); botão "Open now" instancia o template no board da sessão atual. Salva pelo fluxo existente (`editing_config` → `to_yaml` → `atomic_write` → `apply_runtime_config`) e convive com a aba YAML (alternar abas preserva edições).
2. Menu de contexto do workspace na sidebar ganha: **Rename**, **New terminal ▸** (presets, incluindo perfis Claude), **Save as template** (grava o layout atual do workspace — nome, cwd, cor, terminais com kind/nome/cwd/posição/tamanho/preset env — em `config.yaml`, substituindo o template de mesmo vínculo ou criando um novo) e **Delete** (com confirmação; usa `remove_workspace` existente).

**D. Link entre terminais**
1. Servidor local em `horizon-core` (`cfg(unix)`): socket Unix em `$XDG_RUNTIME_DIR/horizon/<pid>.sock` (fallback `~/.horizon/run/<pid>.sock`), dir 0700, socket 0600, removido no shutdown; protocolo JSON por linha com versão; requisições encaminhadas por canal para a thread de UI e respondidas após o processamento no frame (sem tocar o `Board` fora da thread de UI).
2. Todo painel recebe env `HORIZON_PANEL_ID` (local_id), `HORIZON_PANEL_NAME` (título atual no spawn) e `HORIZON_SOCKET`.
3. Subcomando no mesmo binário (parse manual em `main.rs`, antes de subir o eframe; sai sem abrir janela): `horizon link list` (id, nome, workspace, kind, se é agente), `horizon link send <nome|id> <texto> [--no-enter]` (escreve o texto no PTY alvo + Enter por padrão; usa bracketed paste quando o alvo pedir), `horizon link read <nome|id> [--lines N]` (últimas N linhas visíveis/scrollback em texto puro, default 50, máx 500). Resolução por id exato, depois nome exato, depois nome sem distinção de caixa; ambiguidade → erro listando candidatos. Sem `HORIZON_SOCKET` → erro claro.
4. Skill `horizon-link` no plugin Claude que o Horizon já injeta via `--plugin-dir` (`HorizonHome::claude_plugin_dir()`), ensinando quando e como usar `horizon link list/send/read` (e que a mensagem chega ao outro agente como input do usuário, então deve se identificar com `HORIZON_PANEL_NAME`).
5. UI: menu de contexto do painel ganha **Send to panel…** (escolhe painel alvo; envia a seleção atual ou um texto digitado num campo) e **Copy panel address** (copia o nome/id); o painel que recebe mensagem mostra um indicador breve no titlebar (badge que some após alguns segundos, sem repaint contínuo além da animação curta).

**NÃO inclui:** refresh/renovação de login OAuth; contagem de tokens por conta; MCP server; sincronização entre máquinas; editor visual de posição arrastando dentro da aba Workspaces (o "Save as template" cobre isso); alterar o comportamento de sessões/templates existentes além do descrito; release/tag; mudanças no upstream `peters/horizon`.
</task>

<tipo_de_tarefa>
- A1–A4, C1–C3, D1–D3: `regra-de-negocio` (descoberta/derivação de perfis, merge de env, parser de uso e estados, cache, protocolo e resolução de alvo do link) → TDD estrito.
- A5–A6, C4–C5, B1–B2, D5: `ui` → validação visual nativa (o app é desktop egui, não web): `target/debug/horizon` com `DISPLAY=:1`, screenshot da janela pelo PID sob teste (não por nome), golden path + 2 estados (ex.: token expirado, toolbar estreita) — evidências em `.superpowers/sdd/<plano>/task-N-visual/`; teste unitário para lógica extraída da view.
- D4 e integração socket/HTTP ponta a ponta: `operacional` → E2E (`horizon link list/send/read` contra uma instância real; chamada real ao endpoint com a conta de token válido).
</tipo_de_tarefa>

<dominios_previstos>
- A, B: rust (fora da tabela) + frontend (egui)
- C: rust + integracao (OAuth usage HTTP) / frontend na parte do toolbar
- D: rust + integracao (socket local, CLI) / frontend na parte de menus
</dominios_previstos>

<business_rules>
- Perfil = `{name, config_dir}`; `name` único (case-insensitive) e não vazio; `config_dir` aceita `~`, é expandido no uso e persistido como digitado. SUPOSIÇÃO S2 para nomes.
- Seed do `~/.horizon/config.yaml` do usuário: `claude`→`~/.claude`, `claude2`→`~/.claude-conta2`, `claude3`→`~/.claude-conta3`, `claude-sigamina`→`~/.claude-sigamina` (backup do arquivo antes; preservar todo o resto, inclusive workspaces e presets existentes).
- Merge de env: `HORIZON=1` sempre presente para agentes; env do preset sobrepõe as demais chaves; `HORIZON_PANEL_*`/`HORIZON_SOCKET` injetados pelo Horizon e não sobrescrevíveis pelo preset.
- Uso: % exibido = usado (S3), arredondado a inteiro; restante = 100 − usado (mín. 0). Token considerado expirado quando `expiresAt` (ms) ≤ agora + 60s ou `accessToken` vazio. 401/403 → `Expired`; erro de rede/timeout → `Stale` mantendo último valor; `five_hour`/`seven_day` `null` → aquela janela mostra "—".
- Segurança: tokens nunca são logados, exibidos, copiados para cache ou enviados para outro host além de `api.anthropic.com`; arquivos de credenciais só abertos em leitura.
- Link: só o mesmo usuário (permissões do socket); mensagens limitadas (ex.: 64 KiB por requisição); `send` não executa nada além de escrever no PTY do alvo; `read` devolve texto puro sem sequências de escape; painel inexistente/encerrado → erro tipado.
- Workspaces: "Save as template" nunca apaga outros templates; "Delete" de workspace vivo pede confirmação; remover template na aba Workspaces não fecha workspaces já abertos.
- SUPOSIÇÕES S1–S10 valem como regra até o usuário dizer o contrário.
</business_rules>

<technical_context>
- Config/presets: `crates/horizon-core/src/config.rs` (`PresetConfig` :86-100, `to_panel_options` :105-115, `requires_workspace_cwd` :118, defaults ~200-320, `ShortcutsConfig` :367/396/435/462, `WorkspaceConfig` :508-522, `TerminalConfig` :525-549, `resolved_presets` :701); `config_migration.rs` só se S9 cair.
- Spawn/env: `crates/horizon-core/src/panel.rs` (`PanelOptions` :114-135, `local_id` :164, `session_binding` :172, restart :544, `write_input` :403); `panel/spawn.rs` (`spawn_panel` :152, `spawn_terminal` :275, `agent_env` :911, `horizon_claude_plugin_args` :919, `resolve_agent_launch_command` :704); `terminal/lifecycle.rs:44,92`; `horizon_home.rs` (`claude_plugin_dir`).
- Resume/detecção: `runtime_state/claude_live_sessions.rs`, `runtime_state.rs:267-340,497`, `agent_detect.rs`.
- Atalhos/palette: `horizon-core/src/shortcuts.rs` (:230, :258-284), `horizon-ui/src/command_registry.rs`, `app/actions/command_palette.rs` (:112, :131, :197-205, :218, :259-293), `app/actions/support.rs`, `app/actions/panels.rs:111-127`, `app/actions/pickers.rs:57-190`, `app/settings/shortcuts.rs`, `app/shortcut_inventory.rs`.
- Settings: `horizon-ui/src/app/settings/` (`mod.rs` :25-44 abas, :53, :146, :191, :237, :347-349, :384; `presets.rs`, `general.rs`, `yaml_editor.rs`) → novos `settings/claude.rs`, `settings/workspaces.rs`.
- Workspaces: `horizon-core/src/board/workspaces.rs` (:12, :34, :189, :251, :270-281), `runtime_state.rs:53,631`, `session_store.rs:122`, `app/sidebar.rs:500-543`, `app/sidebar/menu.rs`, `app/view.rs:24,31`, `app/panels.rs:771`, `dir_picker/{local,modal}.rs`.
- Toolbar/uso: `app/sidebar/toolbar.rs` (:14, :85-91), `app/root_chrome.rs`, `usage_stats.rs`, `usage_stats/claude.rs`, `usage_dashboard.rs`, `usage_widget.rs`, `app/mod.rs` (estado do app, :242).
- Link: novo módulo `horizon-core/src/link/` (servidor, protocolo, resolução), drenagem no loop do app (`app/lifecycle.rs`), subcomando em `horizon-ui/src/main.rs` (`parse_cli_args` :325), `board.rs:355,379`, `terminal/events.rs` (OSC existente, referência).
- Dependência nova provável: `ureq` (rustls) — checar versão estável no crates.io; sem tokio novo.
- Validação (AGENTS.md, todas devem passar na branch final):
  `cargo fmt --all -- --check` · `./scripts/check-maintainability.sh` · `RUSTFLAGS="-D warnings" cargo test --workspace` · `RUSTFLAGS="-D warnings" cargo test --workspace --features speech` · `cargo clippy --all-targets --features speech,trace-profiling -- -D warnings` · `cargo clippy --workspace --lib --bins --examples --features speech -- -D warnings -D clippy::unwrap_used -D clippy::expect_used` (pedantic é advisory).
- Smoke: plano temporário em `docs/testing/2026-11-12-claude-hub-smoke.md` (baseline, fluxos A/B/C/D, bordas, persistência/restore, regressão visual) — apagar após a validação; screenshots ao lançar e após resize.
</technical_context>

<acceptance_criteria>
- Com o `config.yaml` semeado, preset picker, command palette e `Ctrl+Shift+L` listam `claude`, `claude2`, `claude3`, `claude-sigamina`; escolher `claude2` abre painel rodando `claude` com `CLAUDE_CONFIG_DIR=~/.claude-conta2` (verificado em `/proc/<pid>/environ` do filho), já com os args do preset Claude Code base.
- Restart do painel e reabertura do Horizon mantêm o env do perfil; resume de sessão Claude de um perfil ≠ `~/.claude` decide `--resume`/`--session-id` olhando o dir certo (teste unitário com dirs temporários).
- `agent_detect` classifica corretamente os 4 perfis (teste com environ sintético).
- Sem `claude_profiles` no YAML, a auto-descoberta acha os dirs com `.credentials.json` (teste com HOME temporário).
- Toolbar mostra 4 chips; a conta com token válido exibe `5h`/`7d` iguais (±1 p.p.) aos do endpoint no mesmo minuto; contas expiradas aparecem esmaecidas com "expired" e último valor de cache; nenhum arquivo de credencial tem mtime alterado após 2 ciclos de poll (verificado); o cache não contém `accessToken`/`refreshToken` (grep).
- Idle com toolbar visível: sem repaint contínuo atribuível aos chips (trace de perf idle antes/depois, workload registrado no commit).
- Aba Workspaces cria um template com 2 terminais (1 shell + 1 `Claude · claude3`) sem tocar a aba YAML; o YAML gerado é válido, recarrega no hot-reload, e "Open now" materializa o workspace no board.
- "Save as template" de um workspace vivo grava nome/cwd/terminais/posições no `config.yaml`; "Rename" e "Delete" pela sidebar funcionam e persistem na sessão.
- `horizon link list` em um shell dentro do Horizon lista os painéis; `horizon link send <outro> "echo oi"` faz o texto aparecer e executar no painel alvo; `--no-enter` não executa; `horizon link read <outro> --lines 20` devolve as últimas linhas em texto puro; alvo ambíguo/inexistente retorna erro e exit code ≠ 0; socket com modo 0600 e removido ao fechar.
- Uma sessão Claude no Horizon enxerga a skill `horizon-link` e consegue mandar mensagem para outra sessão Claude em outro painel (E2E manual com evidência no report).
- Todos os comandos de validação do AGENTS.md verdes na branch final; screenshots do smoke anexados; `~/.horizon/config.yaml` do usuário sincronizado com backup preservado.
</acceptance_criteria>

<constraints>
- Não refatorar fora do escopo. Não alterar contratos públicos sem necessidade. Não alterar secrets.
- Não assumir regra de negócio crítica sem marcar SUPOSIÇÃO. Seguir padrões existentes do projeto (AGENTS.md).
- **Nunca** renovar OAuth nem escrever em `~/.claude*/.credentials.json` ou `.claude.json`; nunca logar/exibir token.
- Não mexer em `~/Downloads`; não alterar os aliases do shell do usuário.
- Mudanças cirúrgicas: dividir arquivos grandes quando tocados (regra do AGENTS.md), sem "melhorar" código não relacionado; dead code pré-existente é mencionado, não apagado.
- Lógica de domínio em `horizon-core`; UI só render/coleta; sem trabalho por frame não cacheado nos chips/badges.
- Windows/macOS continuam compilando (link `cfg(unix)`, no-op no Windows).
- Sem push/PR/merge sem passar pelo gate de integração.
</constraints>
