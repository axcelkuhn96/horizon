//! Recent Claude sessions of a workspace: a hub modal listing the transcripts of the workspace's
//! folder and of every worktree of its repository, with the account that ran each one and a
//! one-click resume. Visual spec: `docs/design/claude-hub-ui.md` section 10.
//!
//! Transcripts are listed and read on the `horizon-core` scanner thread; the UI only receives
//! finished results, so nothing here reads the disk per frame. The account of a session comes
//! from the Horizon-owned index (see [`horizon_core::claude_sessions`]), fed by [`record`].
//! The pure list logic is in [`model`], painting in [`view`], the resume itself in [`resume`].

mod model;
mod record;
mod resume;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use egui::{Color32, Context};
use horizon_core::agent_detect::default_claude_config_dir;
use horizon_core::claude_sessions::{
    AccountIndex, AccountIndexWriter, ScanDir, ScanLimits, ScanRequest, ScanResult, SessionScanner, SessionSummary,
    attribute_accounts, project_dirs_for,
};
use horizon_core::git_overview::RepoOverview;
use horizon_core::{HorizonHome, Waker, WorkspaceId, expand_config_dir};

use self::model::{ListView, ListWindow, ViewKey};
use super::HorizonApp;
use super::claude_launcher::{CachedGalley, claude_profile_names};
use super::hub_toast::{Toast, ToastKind};
use super::hub_ui::{self, ModalPlacement};

const MODAL_WIDTH: f32 = 560.0;
const INDEX_FILE: &str = "claude-session-accounts.json";

/// What the list is waiting for or showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
    /// The scanner could not run; shown without any detail from the transcripts.
    Failed,
}

/// A resume waiting for the user to say which account ran the session.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingPick {
    /// Id of the session to resume.
    id: String,
    /// Resume in the active shell instead of a new panel.
    here: bool,
}

/// An account chip of the "Which account?" step.
struct ProfileChip {
    name: String,
    accent: Color32,
}

/// What the user did this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionsAction {
    None,
    Resume {
        session: usize,
        here: bool,
    },
    PickAccount(usize),
    CancelPick,
    ToggleAgents,
    ShowAll,
    /// Position in the selectable rows.
    Select(usize),
}

/// The open modal.
struct SessionsModal {
    workspace: WorkspaceId,
    workspace_name: String,
    /// Whether the workspace has a folder to look sessions up for.
    has_folder: bool,
    generation: u64,
    phase: Phase,
    /// Newest first.
    sessions: Vec<SessionSummary>,
    /// Bumped on every scan result: list strings and galleys are rebuilt only when it moves.
    scan_version: u64,
    filter: String,
    window: ListWindow,
    agents_expanded: bool,
    view: ListView,
    view_key: Option<ViewKey>,
    /// Item indices of `view` the keyboard can select.
    selectable: Vec<usize>,
    /// Position in `selectable`.
    selected: usize,
    /// Keyboard moved the selection: scroll it into view once.
    reveal_selected: bool,
    pick: Option<PendingPick>,
    profiles: Vec<ProfileChip>,
    title_galleys: Vec<Option<CachedGalley>>,
    header_galley: Option<CachedGalley>,
}

impl SessionsModal {
    fn new(
        workspace: WorkspaceId,
        workspace_name: String,
        has_folder: bool,
        generation: u64,
        profiles: Vec<ProfileChip>,
    ) -> Self {
        Self {
            workspace,
            workspace_name,
            has_folder,
            generation,
            phase: Phase::Loading,
            sessions: Vec::new(),
            scan_version: 0,
            filter: String::new(),
            window: ListWindow::default(),
            agents_expanded: false,
            view: ListView::default(),
            view_key: None,
            selectable: Vec::new(),
            selected: 0,
            reveal_selected: true,
            pick: None,
            profiles,
            title_galleys: Vec::new(),
            header_galley: None,
        }
    }

    /// Session index of the selected row.
    fn selected_session(&self) -> Option<usize> {
        let item = *self.selectable.get(self.selected)?;
        self.view.row(item).map(|row| row.session)
    }

    fn profile_names(&self) -> Vec<String> {
        self.profiles.iter().map(|chip| chip.name.clone()).collect()
    }

    /// Rebuilds the list when anything it depends on moved, keeping the selected session.
    fn refresh_view(&mut self, accounts_version: u64, now: SystemTime) {
        let key = ViewKey {
            scan_version: self.scan_version,
            filter: self.filter.clone(),
            window: self.window,
            agents_expanded: self.agents_expanded,
            accounts_version,
        };
        if self.view_key.as_ref() == Some(&key) {
            return;
        }
        let kept = self
            .selectable
            .get(self.selected)
            .and_then(|&item| self.view.row(item))
            .map(|row| row.id.clone());
        self.view = model::build_view(
            &self.sessions,
            &self.filter,
            self.window,
            self.agents_expanded,
            now,
            &self.profile_names(),
        );
        self.selectable = self.view.selectable();
        self.selected = kept
            .and_then(|id| {
                self.selectable
                    .iter()
                    .position(|&item| self.view.row(item).is_some_and(|row| row.id == id))
            })
            .unwrap_or(0);
        self.title_galleys = (0..self.view.items.len()).map(|_| None).collect();
        self.header_galley = None;
        self.view_key = Some(key);
        self.reveal_selected = true;
    }
}

/// State of the recent-sessions feature, owned by `HubState`.
pub(in crate::app) struct SessionsUi {
    scanner: Option<SessionScanner>,
    writer: Option<AccountIndexWriter>,
    /// The index as this process knows it (the file's entries once loaded, plus what was recorded).
    index: AccountIndex,
    index_loaded: bool,
    /// Bumped when the index changes, so an open list refreshes the accounts it shows.
    accounts_version: u64,
    generation: u64,
    modal: Option<SessionsModal>,
    last_panel_scan: Option<Instant>,
}

impl Default for SessionsUi {
    fn default() -> Self {
        Self {
            scanner: None,
            writer: None,
            index: AccountIndex::empty(HorizonHome::resolve().root().join(INDEX_FILE)),
            index_loaded: false,
            accounts_version: 0,
            generation: 0,
            modal: None,
            last_panel_scan: None,
        }
    }
}

impl SessionsUi {
    fn waker(ctx: &Context) -> Waker {
        let ctx = ctx.clone();
        Arc::new(move || ctx.request_repaint())
    }

    fn ensure_scanner(&mut self, ctx: &Context) -> &SessionScanner {
        self.scanner
            .get_or_insert_with(|| SessionScanner::start(ScanLimits::default(), Some(Self::waker(ctx))))
    }

    fn ensure_writer(&mut self, ctx: &Context) -> &AccountIndexWriter {
        let path = self.index.path().to_path_buf();
        self.writer
            .get_or_insert_with(|| AccountIndexWriter::start(path, Some(Self::waker(ctx))))
    }

    /// Remembers `session_id` ran under `profile`; saved off the UI thread. A repeat is free.
    fn record(&mut self, ctx: &Context, session_id: &str, profile: &str, config_dir: &str) {
        let ts = unix_now();
        if self.index.record(session_id, profile, config_dir, ts) {
            self.accounts_version = self.accounts_version.wrapping_add(1);
            self.ensure_writer(ctx).record(session_id, profile, config_dir, ts);
        }
    }

    /// The modal's session list with the accounts of the current index.
    fn attribute_open_modal(&mut self) {
        if let Some(modal) = self.modal.as_mut() {
            attribute_accounts(&mut modal.sessions, &self.index);
        }
    }

    /// Takes in what the worker threads have delivered.
    fn poll(&mut self, ctx: &Context) {
        if !self.index_loaded
            && let Some(loaded) = self.writer.as_ref().and_then(AccountIndexWriter::try_loaded)
        {
            self.index_loaded = true;
            self.index.merge(&loaded);
            self.accounts_version = self.accounts_version.wrapping_add(1);
            self.attribute_open_modal();
        }
        while let Some(result) = self.scanner.as_ref().and_then(SessionScanner::try_recv) {
            self.take_scan_result(ctx, result);
        }
    }

    fn take_scan_result(&mut self, ctx: &Context, result: ScanResult) {
        for found in &result.discovered {
            self.record(
                ctx,
                &found.session_id,
                &found.profile,
                &found.config_dir.to_string_lossy(),
            );
        }
        let ScanResult {
            generation,
            sessions,
            failed,
            ..
        } = result;
        let Some(modal) = self.modal.as_mut().filter(|modal| modal.generation == generation) else {
            return;
        };
        modal.sessions = sessions;
        attribute_accounts(&mut modal.sessions, &self.index);
        modal.phase = if failed { Phase::Failed } else { Phase::Ready };
        modal.scan_version = modal.scan_version.wrapping_add(1);
        modal.selected = 0;
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

impl HorizonApp {
    /// Opens the modal for the active workspace (palette and launcher entry points).
    pub(in crate::app) fn open_claude_sessions_for_active(&mut self, ctx: &Context) {
        match self.board.active_workspace {
            Some(workspace) => self.open_claude_sessions(ctx, workspace),
            None => self.hub.toasts.push(Toast {
                kind: ToastKind::Info,
                text: "No workspace\nOpen a workspace to see its Claude sessions".to_owned(),
                panel: None,
            }),
        }
    }

    /// Opens the modal for `workspace` and asks the scanner for its sessions. Nothing is read
    /// here: the folder list is built from data the app already holds.
    pub(in crate::app) fn open_claude_sessions(&mut self, ctx: &Context, workspace: WorkspaceId) {
        let Some((workspace_name, folder)) = self.board.workspace(workspace).map(|found| {
            let folder = found.cwd.clone().or_else(|| {
                found
                    .panels
                    .iter()
                    .find_map(|id| self.board.panel(*id).and_then(|panel| panel.launch_cwd.clone()))
            });
            (found.name.clone(), folder)
        }) else {
            return;
        };
        // One modal at a time: the palette, launcher, remote hosts and sessions close.
        self.command_palette = None;
        self.hub.claude_launcher = None;
        self.remote_hosts_overlay = None;
        self.session_manager = None;

        let names = claude_profile_names(&self.presets);
        let accents: Vec<Color32> = self
            .hub
            .accent_slots(&names)
            .iter()
            .map(|slot| hub_ui::accent_color(*slot))
            .collect();
        let profiles: Vec<ProfileChip> = names
            .into_iter()
            .zip(accents)
            .map(|(name, accent)| ProfileChip { name, accent })
            .collect();

        let profile_dirs = self.profile_config_dirs();
        let dirs = folder
            .as_deref()
            .map(|cwd| {
                let overview = self.hub.git.overview_for(workspace);
                session_scan_dirs(overview, cwd, &profile_dirs)
            })
            .unwrap_or_default();
        self.hub.sessions.generation = self.hub.sessions.generation.wrapping_add(1);
        let generation = self.hub.sessions.generation;
        let request = ScanRequest {
            generation,
            dirs,
            index: self.hub.sessions.index.clone(),
            profiles: profile_dirs,
        };
        self.hub.sessions.ensure_writer(ctx);
        self.hub.sessions.ensure_scanner(ctx).request(request);
        self.hub.sessions.modal = Some(SessionsModal::new(
            workspace,
            workspace_name,
            folder.is_some(),
            generation,
            profiles,
        ));
    }

    /// `(profile, expanded config dir)` of every configured profile.
    fn profile_config_dirs(&self) -> Vec<(String, PathBuf)> {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Vec::new();
        };
        self.template_config
            .effective_claude_profiles()
            .into_iter()
            .map(|profile| {
                let dir = expand_config_dir(&profile.config_dir, &home);
                (profile.name, dir)
            })
            .collect()
    }

    /// Once per frame: takes in scan results, then draws the modal when it is open.
    pub(in crate::app) fn render_claude_sessions(&mut self, ctx: &Context) {
        self.hub.sessions.poll(ctx);
        let Some(mut modal) = self.hub.sessions.modal.take() else {
            return;
        };
        modal.refresh_view(self.hub.sessions.accounts_version, SystemTime::now());

        let mut action = view::handle_keys(ctx, &mut modal);
        let outcome = hub_ui::show_modal(
            ctx,
            egui::Id::new("claude_sessions"),
            MODAL_WIDTH,
            ModalPlacement::TopFraction(0.12),
            Some(view::filter_id()),
            |ui| {
                let from_ui = view::show_contents(ui, &mut modal);
                if action == SessionsAction::None {
                    action = from_ui;
                }
            },
        );

        match self.apply_sessions_action(ctx, &mut modal, action) {
            SessionsFlow::Close => {}
            SessionsFlow::Stay if outcome.dismissed => {}
            SessionsFlow::Stay => self.hub.sessions.modal = Some(modal),
        }
    }
}

/// Transcript directories for `cwd`: the shared projects directory of the default config dir,
/// plus the projects directory of any profile that keeps its own.
fn session_scan_dirs(overview: Option<&RepoOverview>, cwd: &Path, profiles: &[(String, PathBuf)]) -> Vec<ScanDir> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let default_root = default_claude_config_dir(&home).join("projects");
    let canonical = |root: &Path| std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut seen = vec![canonical(&default_root)];
    let mut roots: Vec<(PathBuf, Option<String>)> = vec![(default_root, None)];
    for (name, dir) in profiles {
        let root = dir.join("projects");
        let resolved = canonical(&root);
        if !seen.contains(&resolved) {
            seen.push(resolved);
            roots.push((root, Some(name.clone())));
        }
    }
    roots
        .into_iter()
        .flat_map(|(root, implied_profile)| {
            project_dirs_for(overview, cwd, &root)
                .into_iter()
                .map(move |(dir, origin)| ScanDir {
                    dir,
                    origin,
                    implied_profile: implied_profile.clone(),
                })
        })
        .collect()
}

/// Whether the modal stays open after an action.
enum SessionsFlow {
    Stay,
    Close,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use horizon_core::claude_sessions::{SessionAccount, SessionOrigin};

    use super::*;
    use crate::app::test_support::test_app;

    fn session(n: u64, account: SessionAccount) -> SessionSummary {
        let origin = SessionOrigin {
            worktree: PathBuf::from("/p"),
            branch: Some("main".to_owned()),
            is_main: true,
            is_agent: false,
        };
        SessionSummary {
            cwd: origin.worktree.clone(),
            origin,
            id: format!("00000000-0000-4000-8000-{n:012}"),
            title: format!("task {n}"),
            first_prompt: String::new(),
            updated: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - n),
            size: 10,
            account,
        }
    }

    fn modal_with(sessions: Vec<SessionSummary>) -> SessionsModal {
        let profile = |name: &str| ProfileChip {
            name: name.to_owned(),
            accent: Color32::WHITE,
        };
        let mut modal = SessionsModal::new(
            WorkspaceId(1),
            "api".to_owned(),
            true,
            1,
            vec![profile("conta1"), profile("conta2")],
        );
        modal.sessions = sessions;
        modal.phase = Phase::Ready;
        modal
    }

    #[test]
    fn sessions_list_cache_rebuilds_only_on_scan_version() {
        let mut modal = modal_with(vec![
            session(1, SessionAccount::Unknown),
            session(2, SessionAccount::Unknown),
        ]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_100);
        modal.refresh_view(0, now);
        assert_eq!(modal.view.shown, 2);
        // A marker only a rebuild would erase.
        modal.view.shown = 99;

        modal.refresh_view(0, now);
        assert_eq!(modal.view.shown, 99, "same inputs reuse the built list");

        modal.scan_version += 1;
        modal.refresh_view(0, now);
        assert_eq!(modal.view.shown, 2, "a new scan rebuilds it");

        modal.view.shown = 99;
        modal.refresh_view(1, now);
        assert_eq!(modal.view.shown, 2, "so does a change of accounts");
    }

    #[test]
    fn rebuild_keeps_the_selected_session_when_the_filter_narrows_the_list() {
        let mut modal = modal_with(vec![
            session(1, SessionAccount::Unknown),
            session(2, SessionAccount::Unknown),
            session(3, SessionAccount::Unknown),
        ]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_100);
        modal.refresh_view(0, now);
        modal.selected = 2;

        modal.filter = "task 3".to_owned();
        modal.refresh_view(0, now);

        assert_eq!(modal.selectable.len(), 1);
        assert_eq!(modal.selected_session(), Some(2));
    }

    #[test]
    fn unknown_account_requires_account_pick_before_resume() {
        let mut app = test_app();
        let mut modal = modal_with(vec![session(1, SessionAccount::Unknown)]);

        let flow = app.apply_sessions_action(
            &Context::default(),
            &mut modal,
            SessionsAction::Resume { session: 0, here: true },
        );

        assert!(matches!(flow, SessionsFlow::Stay));
        assert_eq!(
            modal.pick,
            Some(PendingPick {
                id: modal.sessions[0].id.clone(),
                here: true,
            })
        );
    }

    #[test]
    fn cancelling_the_account_step_keeps_the_list() {
        let mut app = test_app();
        let mut modal = modal_with(vec![session(1, SessionAccount::Unknown)]);
        modal.pick = Some(PendingPick {
            id: modal.sessions[0].id.clone(),
            here: false,
        });

        let flow = app.apply_sessions_action(&Context::default(), &mut modal, SessionsAction::CancelPick);

        assert!(matches!(flow, SessionsFlow::Stay));
        assert_eq!(modal.pick, None);
    }
}
