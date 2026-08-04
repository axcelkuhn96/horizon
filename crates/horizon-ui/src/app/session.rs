use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::Context;
use horizon_core::{
    AgentSessionBinding, AgentSessionCatalog, Board, PanelId, PanelKind, PanelResume, PanelState,
    live_claude_session_ids,
};

/// Build a `<binary> --resume <session_id>` command string.
///
/// Returns `None` if either input fails the safety check:
/// - `binary` must be exactly `"claude"` or `"claude2"` (allowlist)
/// - `session_id` must be between 32 and 128 characters (UUID v4 is 36; 128 is generous)
/// - `session_id` must contain only hex digits and hyphens (`[0-9a-fA-F-]`)
///
/// The length bounds prevent both empty/short ids and multi-megabyte all-hex
/// ids (denial-of-service / terminal-corruption) being written to the PTY; the
/// charset check prevents shell-metacharacter injection while still accepting
/// standard UUID v4 identifiers used by Claude.
pub(crate) fn resume_command(binary: &str, session_id: &str) -> Option<String> {
    if binary != "claude" && binary != "claude2" {
        return None;
    }
    if session_id.len() < 32 || session_id.len() > 128 {
        return None;
    }
    if !session_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    Some(format!("{binary} --resume {session_id}"))
}

use crate::{loading_spinner, theme};

use super::util::{empty_string_as_none, short_session_id, truncate_session_label};
use super::{ActiveSession, DetachedWorkspaceViewportState, HorizonApp, ResolvedSession, StartupBootstrap};

const SESSION_BINDING_ACTIVITY_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct DynamicPanelBindingState {
    panel_id: PanelId,
    kind: PanelKind,
    cwd: String,
    launched_at_millis: i64,
    session_binding: Option<AgentSessionBinding>,
    recent_output: bool,
}

fn collect_dynamic_binding_updates(
    dynamic_panels: &[DynamicPanelBindingState],
    reserved_session_ids: &HashSet<String>,
    recent_for: impl Fn(PanelKind, Option<&str>) -> Vec<horizon_core::AgentSessionRecord>,
) -> Vec<(PanelId, AgentSessionBinding)> {
    let mut used_session_ids = reserved_session_ids.clone();
    used_session_ids.extend(
        dynamic_panels
            .iter()
            .filter_map(|panel| panel.session_binding.as_ref().map(|binding| binding.session_id.clone())),
    );

    let mut grouped_panels: HashMap<(PanelKind, String), Vec<&DynamicPanelBindingState>> = HashMap::new();
    for panel in dynamic_panels {
        grouped_panels
            .entry((panel.kind, panel.cwd.clone()))
            .or_default()
            .push(panel);
    }

    let mut assignments = Vec::new();
    for ((kind, cwd), panels) in grouped_panels {
        if kind == PanelKind::Claude {
            continue;
        }

        let mut candidates = recent_for(kind, empty_string_as_none(&cwd));
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.updated_at));

        let active_bound_panels: Vec<_> = panels
            .iter()
            .filter(|panel| panel.recent_output && panel.session_binding.is_some())
            .collect();
        if active_bound_panels.len() == 1 {
            let panel = active_bound_panels[0];
            let Some(current_binding) = panel.session_binding.as_ref() else {
                continue;
            };
            if let Some(candidate) = candidates.iter().find(|candidate| {
                candidate.session_id != current_binding.session_id
                    && candidate.updated_at > current_binding.updated_at.unwrap_or(0)
                    && !used_session_ids.contains(&candidate.session_id)
            }) {
                used_session_ids.insert(candidate.session_id.clone());
                assignments.push((panel.panel_id, candidate.clone().into_binding()));
            }
        }

        let mut unbound_panels: Vec<_> = panels
            .iter()
            .filter(|panel| panel.session_binding.is_none())
            .copied()
            .collect();
        if unbound_panels.is_empty() {
            continue;
        }

        unbound_panels.sort_by_key(|panel| std::cmp::Reverse(panel.launched_at_millis));
        let oldest_launch = unbound_panels
            .iter()
            .map(|panel| panel.launched_at_millis)
            .min()
            .unwrap_or(0);
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| {
                !used_session_ids.contains(&candidate.session_id)
                    && candidate.updated_at >= oldest_launch.saturating_sub(300_000)
            })
            .collect();
        for (panel, candidate) in unbound_panels.into_iter().zip(candidates) {
            used_session_ids.insert(candidate.session_id.clone());
            assignments.push((panel.panel_id, candidate.into_binding()));
        }
    }

    assignments
}

impl HorizonApp {
    pub(super) fn activate_persistent_session(&mut self, session: &ResolvedSession) {
        self.release_active_session_lease();
        self.transcript_root = Some(session.transcript_root.clone());
        self.startup_chooser = None;
        self.active_session = Some(ActiveSession {
            session_id: session.session_id.clone(),
            lease: match self.session_store.acquire_lease(&session.session_id) {
                Ok(lease) => Some(lease),
                Err(error) => {
                    tracing::warn!("failed to acquire session lease: {error}");
                    None
                }
            },
            last_lease_refresh: Some(Instant::now()),
            persistent: true,
        });
        self.apply_runtime_state(&session.runtime_state);
    }

    pub(super) fn activate_ephemeral_session(&mut self, runtime_state: &horizon_core::RuntimeState) {
        self.release_active_session_lease();
        self.active_session = Some(ActiveSession {
            session_id: "ephemeral".to_string(),
            lease: None,
            last_lease_refresh: None,
            persistent: false,
        });
        self.transcript_root = None;
        self.startup_chooser = None;
        self.apply_runtime_state(runtime_state);
    }

    pub(super) fn apply_runtime_state(&mut self, runtime_state: &horizon_core::RuntimeState) {
        self.window_config = runtime_state.window_or(&self.template_config.window).clone();
        self.detached_workspaces = runtime_state
            .detached_workspaces
            .iter()
            .filter(|workspace| !workspace.workspace_local_id.is_empty())
            .map(|workspace| {
                (
                    workspace.workspace_local_id.clone(),
                    DetachedWorkspaceViewportState::new(workspace.window.clone()),
                )
            })
            .collect();
        self.pending_detached_window_position_restore = self.detached_workspaces.keys().cloned().collect();
        self.pending_detached_reattach.clear();
        self.canvas_view = runtime_state.canvas_view_or_default();
        self.pan_target = None;
        self.initial_pan_done = runtime_state.has_persisted_canvas_view();
        self.runtime_dirty_since = None;
        self.git_watchers.clear();
        self.startup_receiver = Self::runtime_state_needs_session_bootstrap(runtime_state)
            .then(|| Self::spawn_startup_bootstrap(runtime_state.clone()));
        self.board = if self.startup_receiver.is_some() {
            Board::new()
        } else {
            Board::from_runtime_state_with_transcripts(runtime_state, self.transcript_root.as_deref()).unwrap_or_else(
                |error| {
                    tracing::error!("failed to restore runtime state: {error}");
                    Board::new()
                },
            )
        };
        self.board.attention_enabled = self.template_config.features.attention_feed;
    }

    pub(super) fn runtime_state_needs_session_bootstrap(runtime_state: &horizon_core::RuntimeState) -> bool {
        runtime_state
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.panels)
            .any(Self::panel_state_needs_bootstrap)
    }

    /// A panel triggers the startup bootstrap when either:
    /// - it is an agent panel needing its session recovered (Story-6 binding), or
    /// - it is a Shell panel that had a Claude agent running at save time AND that
    ///   agent has a captured `session_id` (Task 3/4 shell-resume injection).
    ///
    /// The Shell arm requires `session_id.is_some()` so we don't spin up bootstrap
    /// for a Shell whose claude had no resumable id — there would be nothing to inject.
    ///
    /// The Shell arm is additive: it only ADDS a reason to spawn bootstrap and
    /// does not affect the per-panel `needs_session_bootstrap()` invariant shared
    /// by the agent binding worker (which still gates on `supports_session_binding()`).
    fn panel_state_needs_bootstrap(panel: &PanelState) -> bool {
        panel.needs_session_bootstrap()
            || panel
                .running_agent
                .as_ref()
                .is_some_and(|agent| panel.kind == PanelKind::Shell && agent.session_id.is_some())
    }

    pub(super) fn spawn_startup_bootstrap(mut runtime_state: horizon_core::RuntimeState) -> Receiver<StartupBootstrap> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let session_catalog = AgentSessionCatalog::load().unwrap_or_else(|error| {
                tracing::warn!("failed to load agent session catalog: {error}");
                AgentSessionCatalog::default()
            });
            let busy_session_ids = live_claude_session_ids();
            runtime_state.bootstrap_missing_agent_bindings(&session_catalog, &busy_session_ids);
            let _ = tx.send(StartupBootstrap {
                runtime_state,
                session_catalog,
            });
        });
        rx
    }

    fn spawn_session_catalog_refresh() -> Receiver<horizon_core::Result<AgentSessionCatalog>> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(AgentSessionCatalog::load());
        });
        rx
    }

    pub(super) fn poll_startup_bootstrap(&mut self) -> bool {
        let Some(receiver) = self.startup_receiver.take() else {
            return true;
        };

        match receiver.try_recv() {
            Ok(bootstrap) => {
                self.session_catalog = bootstrap.session_catalog;
                self.last_session_catalog_refresh = Some(Instant::now());
                self.board = Board::from_runtime_state_with_transcripts(
                    &bootstrap.runtime_state,
                    self.transcript_root.as_deref(),
                )
                .unwrap_or_else(|error| {
                    tracing::error!("failed to restore runtime state: {error}");
                    Board::new()
                });
                self.board.attention_enabled = self.template_config.features.attention_feed;

                // Inject `<binary> --resume <session_id>\n` into every restored Shell
                // panel that had a running Claude agent at save time.  This re-launches
                // claude/claude2 inside the fresh zsh that the restored panel opened.
                //
                // The session_id comes exclusively from the agent process's argv
                // (`--resume <uuid>` captured at save time).  If no id was captured
                // (agent.session_id is None) we inject nothing — never fall back to a
                // cwd-newest scan which would cause two shells in the same directory to
                // resume the same session.
                //
                // Inject-once is guaranteed structurally: this `Ok(bootstrap)` arm fires
                // exactly once (the receiver is taken and never put back), so the loop
                // below can never run a second time.
                for workspace in &bootstrap.runtime_state.workspaces {
                    for panel_state in &workspace.panels {
                        if panel_state.kind != PanelKind::Shell {
                            continue;
                        }
                        let Some(ref agent) = panel_state.running_agent else {
                            continue;
                        };
                        let Some(ref session_id) = agent.session_id else {
                            continue;
                        };
                        let Some(cmd) = resume_command(&agent.binary, session_id) else {
                            tracing::warn!(
                                binary = %agent.binary,
                                session_id = %session_id,
                                "skipping shell resume: session_id failed safety check"
                            );
                            continue;
                        };
                        let Some(panel_id) = self.board.panel_id_by_local_id(&panel_state.local_id) else {
                            continue;
                        };
                        if let Some(panel) = self.board.panel_mut(panel_id) {
                            panel.write_input(format!("{cmd}\n").as_bytes());
                        }
                    }
                }

                true
            }
            Err(TryRecvError::Empty) => {
                self.startup_receiver = Some(receiver);
                false
            }
            Err(TryRecvError::Disconnected) => {
                tracing::warn!("startup bootstrap worker disconnected before sending runtime state");
                true
            }
        }
    }

    pub(super) fn refresh_active_session_lease(&mut self) {
        const LEASE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

        let Some(active_session) = self.active_session.as_mut() else {
            return;
        };
        if !active_session.persistent {
            return;
        }

        let Some(lease) = active_session.lease.as_mut() else {
            return;
        };
        if active_session
            .last_lease_refresh
            .is_some_and(|last_refresh| last_refresh.elapsed() < LEASE_REFRESH_INTERVAL)
        {
            return;
        }

        match self.session_store.refresh_lease(lease) {
            Ok(()) => active_session.last_lease_refresh = Some(Instant::now()),
            Err(error) => tracing::warn!("failed to refresh session lease: {error}"),
        }
    }

    pub(super) fn release_active_session_lease(&mut self) {
        let Some(active_session) = self.active_session.as_mut() else {
            return;
        };
        if !active_session.persistent {
            return;
        }

        if let Err(error) = self.session_store.release_lease(&active_session.session_id) {
            tracing::warn!("failed to release session lease: {error}");
        }
        active_session.lease = None;
        active_session.last_lease_refresh = None;
    }

    pub(super) fn maybe_refresh_session_catalog(&mut self) {
        const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

        if let Some(receiver) = self.session_catalog_refresh.take() {
            match receiver.try_recv() {
                Ok(Ok(catalog)) => {
                    self.session_catalog = catalog;
                    self.last_session_catalog_refresh = Some(Instant::now());
                    self.capture_new_agent_bindings();
                }
                Ok(Err(error)) => {
                    tracing::warn!("failed to refresh agent session catalog: {error}");
                    self.last_session_catalog_refresh = Some(Instant::now());
                }
                Err(TryRecvError::Empty) => {
                    self.session_catalog_refresh = Some(receiver);
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    tracing::warn!("session catalog refresh worker disconnected");
                }
            }
        }

        let has_dynamic_agent =
            self.board.panels.iter().any(|panel| {
                panel.kind.supports_session_binding() && !matches!(panel.resume, PanelResume::Session { .. })
            });
        if !has_dynamic_agent {
            return;
        }

        let has_unbound_agent = self.board.panels.iter().any(|panel| {
            panel.kind.supports_session_binding()
                && !matches!(panel.resume, PanelResume::Session { .. })
                && panel.session_binding.is_none()
        });
        let has_recent_dynamic_output = self.board.panels.iter().any(|panel| {
            panel.kind.supports_session_binding()
                && !matches!(panel.resume, PanelResume::Session { .. })
                && panel.had_recent_output_within(SESSION_BINDING_ACTIVITY_WINDOW)
        });
        if !has_unbound_agent && !has_recent_dynamic_output {
            return;
        }

        let should_refresh = self
            .last_session_catalog_refresh
            .is_none_or(|last_refresh| last_refresh.elapsed() >= REFRESH_INTERVAL);

        if should_refresh && self.session_catalog_refresh.is_none() {
            self.session_catalog_refresh = Some(Self::spawn_session_catalog_refresh());
        }
    }

    fn capture_new_agent_bindings(&mut self) {
        let reserved_session_ids: HashSet<String> = self
            .board
            .panels
            .iter()
            .filter(|panel| matches!(panel.resume, PanelResume::Session { .. }))
            .filter_map(|panel| panel.session_binding.as_ref().map(|binding| binding.session_id.clone()))
            .collect();
        let dynamic_panels: Vec<_> = self
            .board
            .panels
            .iter()
            .filter(|panel| {
                panel.kind.supports_session_binding() && !matches!(panel.resume, PanelResume::Session { .. })
            })
            .map(|panel| DynamicPanelBindingState {
                panel_id: panel.id,
                kind: panel.kind,
                cwd: panel
                    .launch_cwd
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                launched_at_millis: panel.launched_at_millis,
                session_binding: panel.session_binding.clone(),
                recent_output: panel.had_recent_output_within(SESSION_BINDING_ACTIVITY_WINDOW),
            })
            .collect();
        let assignments = collect_dynamic_binding_updates(&dynamic_panels, &reserved_session_ids, |kind, cwd| {
            self.session_catalog.recent_for(kind, cwd)
        });

        if assignments.is_empty() {
            return;
        }

        for (panel_id, binding) in assignments {
            if let Some(panel) = self.board.panel_mut(panel_id) {
                panel.set_session_binding(Some(binding));
            }
        }
        self.mark_runtime_dirty();
    }

    pub(super) fn session_rebind_options(&self, panel_id: PanelId) -> Vec<(String, AgentSessionBinding)> {
        let Some(panel) = self.board.panel(panel_id) else {
            return Vec::new();
        };
        if !panel.kind.supports_session_binding() {
            return Vec::new();
        }

        let cwd = panel.launch_cwd.as_ref().map(|path| path.display().to_string());
        let current_session_id = panel
            .session_binding
            .as_ref()
            .map(|binding| binding.session_id.as_str());
        self.session_catalog
            .recent_for(panel.kind, cwd.as_deref())
            .into_iter()
            .filter(|session| Some(session.session_id.as_str()) != current_session_id)
            .take(8)
            .map(|session| {
                let short_id = short_session_id(&session.session_id);
                let label = truncate_session_label(
                    &session
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("{} session", panel.kind.display_name())),
                );
                (format!("{label} · {short_id}"), session.into_binding())
            })
            .collect()
    }

    pub(super) fn rebind_panel_session(&mut self, panel_id: PanelId, binding: AgentSessionBinding) -> bool {
        let Some(panel) = self.board.panel_mut(panel_id) else {
            return false;
        };

        panel.resume = PanelResume::Session {
            session_id: binding.session_id.clone(),
        };
        panel.set_session_binding(Some(binding));
        true
    }
}

pub(super) fn render_loading_view(ctx: &Context) {
    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(theme::BG()))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.28);
                ui.label(egui::RichText::new("Horizon").size(26.0).strong().color(theme::FG()));
                ui.add_space(16.0);
            });
            loading_spinner::show(
                ui,
                egui::Id::new("startup_loading_spinner"),
                Some("Resolving saved sessions\u{2026}"),
            );
        });
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{DynamicPanelBindingState, HorizonApp, collect_dynamic_binding_updates, resume_command};

    // ── resume_command ────────────────────────────────────────────────────────

    const VALID_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn resume_command_returns_command_for_valid_uuid_claude() {
        let result = resume_command("claude", VALID_UUID);
        assert_eq!(result, Some(format!("claude --resume {VALID_UUID}")));
    }

    #[test]
    fn resume_command_returns_command_for_valid_uuid_claude2() {
        let result = resume_command("claude2", VALID_UUID);
        assert_eq!(result, Some(format!("claude2 --resume {VALID_UUID}")));
    }

    #[test]
    fn resume_command_returns_none_for_empty_session_id() {
        assert_eq!(resume_command("claude", ""), None);
    }

    #[test]
    fn resume_command_returns_none_for_too_short_session_id() {
        // 31 characters — just below the 32-char minimum
        assert_eq!(resume_command("claude", "550e8400e29b41d4a71644665544000"), None);
    }

    #[test]
    fn resume_command_returns_none_for_over_long_session_id() {
        // 200 chars, all valid charset — must still be rejected by the upper bound
        let id = "a".repeat(200);
        assert_eq!(resume_command("claude", &id), None);
    }

    #[test]
    fn resume_command_returns_none_for_disallowed_binary() {
        // Only "claude"/"claude2" are allowed; anything else is rejected even with a valid id.
        assert_eq!(resume_command("evilbin", VALID_UUID), None);
    }

    #[test]
    fn resume_command_returns_none_for_session_id_with_semicolon() {
        let id = "550e8400-e29b-41d4-a716-446655440000;rm -rf /";
        assert_eq!(resume_command("claude", id), None);
    }

    #[test]
    fn resume_command_returns_none_for_session_id_with_dollar_paren() {
        let id = "550e8400-e29b-41d4-a716-44665544$(whoami)";
        assert_eq!(resume_command("claude", id), None);
    }

    #[test]
    fn resume_command_returns_none_for_session_id_with_space() {
        let id = "550e8400-e29b-41d4-a716-44665544 0000";
        assert_eq!(resume_command("claude", id), None);
    }

    #[test]
    fn resume_command_returns_none_for_session_id_with_backtick() {
        let id = "550e8400-e29b-41d4-a716-4466554`id`0";
        assert_eq!(resume_command("claude", id), None);
    }

    use horizon_core::{PanelId, PanelKind, PanelResume, PanelState, RuntimeState, WorkspaceState};

    #[test]
    fn runtime_state_needs_bootstrap_for_unbound_last_agent_panel() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "panel".to_string(),
                    name: "Claude".to_string(),
                    kind: PanelKind::Claude,
                    resume: PanelResume::Last,
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(HorizonApp::runtime_state_needs_session_bootstrap(&state));
    }

    #[test]
    fn runtime_state_needs_bootstrap_for_unbound_last_opencode_panel() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "panel".to_string(),
                    name: "OpenCode".to_string(),
                    kind: PanelKind::OpenCode,
                    resume: PanelResume::Last,
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(HorizonApp::runtime_state_needs_session_bootstrap(&state));
    }

    /// Task 3/4: a Shell panel with a running agent that HAS a captured `session_id`
    /// must trigger bootstrap so the Ok-arm inject loop can write the resume command.
    #[test]
    fn runtime_state_needs_bootstrap_for_shell_with_running_agent_and_session_id() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "shell".to_string(),
                    name: "Shell".to_string(),
                    kind: PanelKind::Shell,
                    resume: PanelResume::Fresh,
                    running_agent: Some(horizon_core::agent_detect::RunningAgent {
                        binary: "claude".to_string(),
                        config_dir: std::path::PathBuf::from("/home/user/.claude"),
                        session_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
                    }),
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(
            HorizonApp::runtime_state_needs_session_bootstrap(&state),
            "Shell panel with running_agent.session_id=Some must trigger bootstrap"
        );
    }

    /// A Shell panel with a running agent but NO captured `session_id` must NOT
    /// trigger bootstrap — there is nothing to inject, so bootstrap is wasteful.
    #[test]
    fn runtime_state_skips_bootstrap_for_shell_with_running_agent_but_no_session_id() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "shell".to_string(),
                    name: "Shell".to_string(),
                    kind: PanelKind::Shell,
                    resume: PanelResume::Fresh,
                    running_agent: Some(horizon_core::agent_detect::RunningAgent {
                        binary: "claude".to_string(),
                        config_dir: std::path::PathBuf::from("/home/user/.claude"),
                        session_id: None,
                    }),
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(
            !HorizonApp::runtime_state_needs_session_bootstrap(&state),
            "Shell panel with running_agent.session_id=None must not trigger bootstrap"
        );
    }

    /// A Shell panel with no detected agent must NOT trigger bootstrap (unchanged).
    #[test]
    fn runtime_state_skips_bootstrap_for_shell_without_running_agent() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "shell".to_string(),
                    name: "Shell".to_string(),
                    kind: PanelKind::Shell,
                    resume: PanelResume::Fresh,
                    running_agent: None,
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(
            !HorizonApp::runtime_state_needs_session_bootstrap(&state),
            "Shell panel with running_agent=None must not trigger bootstrap"
        );
    }

    #[test]
    fn runtime_state_needs_bootstrap_for_unbound_last_pi_panel() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "panel".to_string(),
                    name: "Pi".to_string(),
                    kind: PanelKind::Pi,
                    resume: PanelResume::Last,
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(HorizonApp::runtime_state_needs_session_bootstrap(&state));
    }

    /// A `Fresh` unbound agent panel that had real prior output must trigger
    /// bootstrap so the previous session can be recovered on restart.
    #[test]
    fn runtime_state_needs_bootstrap_for_unbound_fresh_agent_panel_with_activity() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![PanelState {
                    local_id: "claude-active".to_string(),
                    name: "Claude".to_string(),
                    kind: PanelKind::Claude,
                    resume: PanelResume::Fresh,
                    had_session_activity: true,
                    ..PanelState::default()
                }],
            }],
            ..RuntimeState::default()
        };

        assert!(
            HorizonApp::runtime_state_needs_session_bootstrap(&state),
            "unbound Fresh agent panel with had_session_activity=true must trigger bootstrap"
        );
    }

    /// Bound panels (any resume) must always skip bootstrap (binding already known).
    /// Truly-fresh panels with no prior activity must also skip — nothing to recover.
    #[test]
    fn runtime_state_skips_bootstrap_for_bound_panels_and_truly_fresh_panels() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![
                    // Non-agent panel — never bootstrap-eligible.
                    PanelState {
                        local_id: "shell".to_string(),
                        name: "Shell".to_string(),
                        kind: PanelKind::Shell,
                        resume: PanelResume::Fresh,
                        ..PanelState::default()
                    },
                    // Bound agent — already has a session_id, no bootstrap needed.
                    PanelState {
                        local_id: "bound".to_string(),
                        name: "Codex".to_string(),
                        kind: PanelKind::Codex,
                        resume: PanelResume::Last,
                        session_binding: Some(horizon_core::AgentSessionBinding::new(
                            PanelKind::Codex,
                            "session-9".to_string(),
                            None,
                            None,
                            None,
                        )),
                        ..PanelState::default()
                    },
                    // Fresh agent with NO prior activity — skip to avoid spurious resume.
                    PanelState {
                        local_id: "truly-fresh".to_string(),
                        name: "Claude".to_string(),
                        kind: PanelKind::Claude,
                        resume: PanelResume::Fresh,
                        had_session_activity: false,
                        ..PanelState::default()
                    },
                ],
            }],
            ..RuntimeState::default()
        };

        assert!(
            !HorizonApp::runtime_state_needs_session_bootstrap(&state),
            "bound and truly-fresh panels must not trigger bootstrap"
        );
    }

    #[test]
    fn runtime_state_skips_bootstrap_for_agents_without_exact_session_catalogs() {
        let state = RuntimeState {
            workspaces: vec![WorkspaceState {
                local_id: "workspace".to_string(),
                name: "alpha".to_string(),
                cwd: None,
                position: None,
                template: None,
                layout: None,
                sidebar_collapsed: false,
                panels: vec![
                    PanelState {
                        local_id: "gemini".to_string(),
                        name: "Gemini".to_string(),
                        kind: PanelKind::Gemini,
                        resume: PanelResume::Last,
                        ..PanelState::default()
                    },
                    PanelState {
                        local_id: "kilo".to_string(),
                        name: "KiloCode".to_string(),
                        kind: PanelKind::KiloCode,
                        resume: PanelResume::Last,
                        ..PanelState::default()
                    },
                ],
            }],
            ..RuntimeState::default()
        };

        assert!(!HorizonApp::runtime_state_needs_session_bootstrap(&state));
    }

    #[test]
    fn collect_dynamic_binding_updates_assigns_unbound_panels() {
        let panels = vec![DynamicPanelBindingState {
            panel_id: PanelId(7),
            kind: PanelKind::Codex,
            cwd: "/repo".to_string(),
            launched_at_millis: 10,
            session_binding: None,
            recent_output: false,
        }];
        let updates = collect_dynamic_binding_updates(&panels, &HashSet::new(), |kind, cwd| {
            assert_eq!(kind, PanelKind::Codex);
            assert_eq!(cwd, Some("/repo"));
            vec![horizon_core::AgentSessionRecord {
                kind: PanelKind::Codex,
                session_id: "session-1".to_string(),
                cwd: Some("/repo".to_string()),
                label: None,
                updated_at: 12,
            }]
        });

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, PanelId(7));
        assert_eq!(updates[0].1.session_id, "session-1");
    }

    #[test]
    fn collect_dynamic_binding_updates_refreshes_single_recently_active_panel() {
        let panels = vec![DynamicPanelBindingState {
            panel_id: PanelId(7),
            kind: PanelKind::Codex,
            cwd: "/repo".to_string(),
            launched_at_millis: 10,
            session_binding: Some(horizon_core::AgentSessionBinding::new(
                PanelKind::Codex,
                "session-old".to_string(),
                Some("/repo".to_string()),
                None,
                Some(12),
            )),
            recent_output: true,
        }];
        let updates = collect_dynamic_binding_updates(&panels, &HashSet::new(), |kind, cwd| {
            assert_eq!(kind, PanelKind::Codex);
            assert_eq!(cwd, Some("/repo"));
            vec![
                horizon_core::AgentSessionRecord {
                    kind: PanelKind::Codex,
                    session_id: "session-new".to_string(),
                    cwd: Some("/repo".to_string()),
                    label: None,
                    updated_at: 20,
                },
                horizon_core::AgentSessionRecord {
                    kind: PanelKind::Codex,
                    session_id: "session-old".to_string(),
                    cwd: Some("/repo".to_string()),
                    label: None,
                    updated_at: 12,
                },
            ]
        });

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, PanelId(7));
        assert_eq!(updates[0].1.session_id, "session-new");
    }

    #[test]
    fn collect_dynamic_binding_updates_does_not_reassign_ambiguous_recent_group() {
        let panels = vec![
            DynamicPanelBindingState {
                panel_id: PanelId(7),
                kind: PanelKind::Codex,
                cwd: "/repo".to_string(),
                launched_at_millis: 10,
                session_binding: Some(horizon_core::AgentSessionBinding::new(
                    PanelKind::Codex,
                    "session-a".to_string(),
                    Some("/repo".to_string()),
                    None,
                    Some(12),
                )),
                recent_output: true,
            },
            DynamicPanelBindingState {
                panel_id: PanelId(8),
                kind: PanelKind::Codex,
                cwd: "/repo".to_string(),
                launched_at_millis: 11,
                session_binding: Some(horizon_core::AgentSessionBinding::new(
                    PanelKind::Codex,
                    "session-b".to_string(),
                    Some("/repo".to_string()),
                    None,
                    Some(13),
                )),
                recent_output: true,
            },
        ];
        let updates = collect_dynamic_binding_updates(&panels, &HashSet::new(), |kind, cwd| {
            assert_eq!(kind, PanelKind::Codex);
            assert_eq!(cwd, Some("/repo"));
            vec![horizon_core::AgentSessionRecord {
                kind: PanelKind::Codex,
                session_id: "session-c".to_string(),
                cwd: Some("/repo".to_string()),
                label: None,
                updated_at: 20,
            }]
        });

        assert!(updates.is_empty());
    }

    #[test]
    fn collect_dynamic_binding_updates_does_not_reassign_claude_bindings() {
        let panels = vec![DynamicPanelBindingState {
            panel_id: PanelId(7),
            kind: PanelKind::Claude,
            cwd: "/repo".to_string(),
            launched_at_millis: 10,
            session_binding: Some(horizon_core::AgentSessionBinding::new(
                PanelKind::Claude,
                "preassigned-session".to_string(),
                Some("/repo".to_string()),
                None,
                Some(12),
            )),
            recent_output: true,
        }];
        let updates = collect_dynamic_binding_updates(&panels, &HashSet::new(), |kind, cwd| {
            assert_eq!(kind, PanelKind::Claude);
            assert_eq!(cwd, Some("/repo"));
            vec![horizon_core::AgentSessionRecord {
                kind: PanelKind::Claude,
                session_id: "external-newer-session".to_string(),
                cwd: Some("/repo".to_string()),
                label: None,
                updated_at: 20,
            }]
        });

        assert!(updates.is_empty());
    }
}
