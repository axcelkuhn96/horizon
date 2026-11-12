//! Recent Claude sessions of a workspace: a hub modal listing the transcripts of the workspace's
//! folder and of every worktree of its repository, with the account that ran each one and a
//! one-click resume. Visual spec: `docs/design/claude-hub-ui.md` section 10.
//!
//! Transcripts are listed and read on the `horizon-core` scanner thread; the UI only receives
//! finished results, so nothing here reads the disk per frame. The account of a session comes
//! from the Horizon-owned index (see [`horizon_core::claude_sessions`]), fed by [`record`].
//! The pure list logic is in [`model`], painting in [`view`] and [`rows`], the resume itself in [`resume`].

mod model;
mod record;
mod resume;
mod rows;
mod view;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::{Color32, Context};
use horizon_core::agent_detect::default_claude_config_dir;
use horizon_core::claude_sessions::{
    AccountIndex, AccountIndexWriter, ScanDir, ScanLimits, ScanRequest, ScanResult, SessionScanner, SessionSummary,
    attribute_accounts, is_session_uuid, project_dirs_for,
};
use horizon_core::git_overview::RepoOverview;
use horizon_core::{HorizonHome, PanelId, PanelKind, Waker, WorkspaceId, expand_config_dir, live_claude_session_ids};

use self::model::{ListRow, ListView, ListWindow, LiveSessions, Running, ScanSignature, ViewKey, scan_signature};
use super::HorizonApp;
use super::claude_launcher::{CachedGalley, claude_profile_names};
use super::hub_toast::{Toast, ToastKind};
use super::hub_ui::{self, ModalPlacement};

const MODAL_WIDTH: f32 = 560.0;
const INDEX_FILE: &str = "claude-session-accounts.json";
/// An open modal rescans this often, so transcripts and running sessions stay current.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

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
    /// Bring the Horizon panel that runs the session to the front.
    FocusPanel(PanelId),
}

/// The open modal.
struct SessionsModal {
    workspace: WorkspaceId,
    workspace_name: String,
    /// Whether the workspace has a folder to look sessions up for.
    has_folder: bool,
    folder: Option<PathBuf>,
    generation: u64,
    phase: Phase,
    /// Newest first.
    sessions: Vec<SessionSummary>,
    /// Transcripts that exist for the workspace, which may be more than `sessions` holds.
    total_found: usize,
    /// Live sessions the last scan saw in the profiles' live-session files.
    scan_live: BTreeSet<String>,
    live: LiveSessions,
    /// Bumped when `live` changes, so the rows are flagged again.
    live_version: u64,
    /// What the last scan's directory list was built from.
    signature: ScanSignature,
    scanned_at: Instant,
    /// The active workspace as of the last frame, to follow it when it changes.
    active_seen: Option<WorkspaceId>,
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
            folder: None,
            generation,
            phase: Phase::Loading,
            sessions: Vec::new(),
            total_found: 0,
            scan_live: BTreeSet::new(),
            live: LiveSessions::default(),
            live_version: 0,
            signature: ScanSignature::default(),
            scanned_at: Instant::now(),
            active_seen: None,
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

    /// The selected row.
    fn selected_row(&self) -> Option<&ListRow> {
        let item = *self.selectable.get(self.selected)?;
        self.view.row(item)
    }

    /// Takes in which sessions run in Horizon's panels now; the rows are flagged again on a change.
    fn update_live(&mut self, panels: BTreeMap<String, PanelId>) {
        let live = LiveSessions::new(self.scan_live.clone(), panels);
        if live != self.live {
            self.live = live;
            self.live_version = self.live_version.wrapping_add(1);
        }
    }

    /// Points the modal at another workspace: the list empties until its scan arrives.
    fn retarget(&mut self, workspace: WorkspaceId, name: String, folder: Option<PathBuf>) {
        self.workspace = workspace;
        self.workspace_name = name;
        self.has_folder = folder.is_some();
        self.folder = folder;
        self.phase = Phase::Loading;
        self.sessions.clear();
        self.total_found = 0;
        self.scan_live.clear();
        self.scan_version = self.scan_version.wrapping_add(1);
        self.selected = 0;
        self.reveal_selected = true;
        self.pick = None;
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
            live_version: self.live_version,
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
        self.view.mark_running(&self.live);
        self.selectable = self.view.selectable();
        let found = kept.and_then(|id| {
            self.selectable
                .iter()
                .position(|&item| self.view.row(item).is_some_and(|row| row.id == id))
        });
        self.selected = found.unwrap_or(0);
        self.title_galleys = (0..self.view.items.len()).map(|_| None).collect();
        self.header_galley = None;
        // A periodic rescan must not yank the scroll back: reveal only when the user changed the
        // query or the selected session is gone.
        let query_changed = self
            .view_key
            .as_ref()
            .is_none_or(|old| old.filter != key.filter || old.window != key.window);
        self.reveal_selected |= query_changed || found.is_none();
        self.view_key = Some(key);
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
        let live: BTreeSet<String> = result.discovered.iter().map(|found| found.session_id.clone()).collect();
        let ScanResult {
            generation,
            mut sessions,
            total_found,
            failed,
            ..
        } = result;
        let Some(modal) = self.modal.as_mut().filter(|modal| modal.generation == generation) else {
            return;
        };
        // A rescan keeps the selection (the rebuild finds the selected session again by id).
        if modal.phase == Phase::Loading {
            modal.selected = 0;
        }
        attribute_accounts(&mut sessions, &self.index);
        let phase = if failed { Phase::Failed } else { Phase::Ready };
        // A rescan that found nothing new leaves the built list (and its scroll) alone.
        let changed = modal.phase != phase || modal.total_found != total_found || modal.sessions != sessions;
        modal.sessions = sessions;
        modal.total_found = total_found;
        modal.scan_live = live;
        modal.phase = phase;
        if changed {
            modal.scan_version = modal.scan_version.wrapping_add(1);
        }
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
        let Some((workspace_name, folder)) = self.workspace_session_folder(workspace) else {
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

        let mut modal = SessionsModal::new(workspace, workspace_name, folder.is_some(), 0, profiles);
        modal.folder = folder;
        modal.active_seen = self.board.active_workspace;
        self.request_sessions_scan(ctx, &mut modal);
        self.hub.sessions.modal = Some(modal);
    }

    /// Name and folder of `workspace`: its own folder, else the folder of its first panel.
    fn workspace_session_folder(&self, workspace: WorkspaceId) -> Option<(String, Option<PathBuf>)> {
        self.board.workspace(workspace).map(|found| {
            let folder = found.cwd.clone().or_else(|| {
                found
                    .panels
                    .iter()
                    .find_map(|id| self.board.panel(*id).and_then(|panel| panel.launch_cwd.clone()))
            });
            (found.name.clone(), folder)
        })
    }

    /// Asks the scanner for the sessions of the modal's workspace with what is known now (its
    /// git overview may not have arrived yet; the modal rescans when it does).
    fn request_sessions_scan(&mut self, ctx: &Context, modal: &mut SessionsModal) {
        let profile_dirs = self.profile_config_dirs();
        let (dirs, signature) = {
            let overview = self.hub.git.overview_for(modal.workspace);
            let dirs = modal
                .folder
                .as_deref()
                .map(|cwd| session_scan_dirs(overview, cwd, &profile_dirs))
                .unwrap_or_default();
            (dirs, scan_signature(modal.folder.as_deref(), overview))
        };
        let sessions = &mut self.hub.sessions;
        sessions.generation = sessions.generation.wrapping_add(1);
        modal.generation = sessions.generation;
        modal.signature = signature;
        modal.scanned_at = Instant::now();
        let request = ScanRequest {
            generation: modal.generation,
            dirs,
            index: sessions.index.clone(),
            profiles: profile_dirs,
        };
        sessions.ensure_writer(ctx);
        sessions.ensure_scanner(ctx).request(request);
    }

    /// Per frame while the modal is open: follows the active workspace, and rescans when the
    /// workspace's git overview changed what to look at or the list is due a refresh.
    fn keep_sessions_scan_current(&mut self, ctx: &Context, modal: &mut SessionsModal) {
        let active = self.board.active_workspace;
        let mut retargeted = false;
        if active != modal.active_seen {
            modal.active_seen = active;
            if let Some(active) = active.filter(|active| *active != modal.workspace)
                && let Some((name, folder)) = self.workspace_session_folder(active)
            {
                modal.retarget(active, name, folder);
                retargeted = true;
            }
        }
        let current = scan_signature(modal.folder.as_deref(), self.hub.git.overview_for(modal.workspace));
        if retargeted || current != modal.signature || modal.scanned_at.elapsed() >= RESCAN_INTERVAL {
            self.request_sessions_scan(ctx, modal);
        }
        ctx.request_repaint_after(RESCAN_INTERVAL);
    }

    /// The sessions Horizon's own panels run right now, by lowercase session id.
    fn live_panel_sessions(&self) -> BTreeMap<String, PanelId> {
        self.board
            .panels
            .iter()
            .filter(|panel| !panel.process_exited())
            .filter_map(|panel| {
                let live = self
                    .hub
                    .status_ui
                    .live(panel.id)
                    .and_then(|live| live.info.as_ref())
                    .map(|info| info.session_id.as_str());
                // A hook-reported id is trusted only for a Claude panel: a shell that Claude left
                // may still carry the id of the session that ended.
                let hooked = matches!(panel.kind, PanelKind::Claude)
                    .then(|| panel.agent_session_id())
                    .flatten();
                let id = live.or(hooked)?;
                is_session_uuid(id).then(|| (id.to_ascii_lowercase(), panel.id))
            })
            .collect()
    }

    /// Whether `session_id` is live, from the modal's view and, at the moment of a click, from the
    /// live-session files themselves.
    fn running_now(&self, modal: &SessionsModal, session_id: &str) -> Option<Running> {
        modal.live.running(session_id).or_else(|| {
            let id = session_id.to_ascii_lowercase();
            live_ids_now(&self.profile_config_dirs())
                .contains(&id)
                .then_some(Running { panel: None })
        })
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
        self.keep_sessions_scan_current(ctx, &mut modal);
        modal.update_live(self.live_panel_sessions());
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

/// Ids of the sessions running now under any profile's config dir or the default one.
fn live_ids_now(profiles: &[(String, PathBuf)]) -> BTreeSet<String> {
    let mut dirs: Vec<PathBuf> = profiles.iter().map(|(_, dir)| dir.clone()).collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(default_claude_config_dir(&home));
    }
    dirs.iter()
        .flat_map(|dir| live_claude_session_ids(dir))
        .map(|id| id.to_ascii_lowercase())
        .collect()
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

    #[test]
    fn live_sessions_flag_their_rows_and_bump_the_version_only_on_a_change() {
        let mut modal = modal_with(vec![
            session(1, SessionAccount::Unknown),
            session(2, SessionAccount::Unknown),
        ]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_100);
        modal.refresh_view(0, now);
        assert!(modal.selected_row().is_some_and(|row| row.running.is_none()));

        modal.scan_live.insert(modal.sessions[0].id.clone());
        modal.update_live(BTreeMap::new());
        let version = modal.live_version;
        modal.update_live(BTreeMap::new());
        modal.refresh_view(0, now);

        assert_eq!(modal.live_version, version, "the same live set is not a change");
        assert!(modal.selected_row().is_some_and(|row| row.running.is_some()));
        assert!(modal.view.row(1).is_some_and(|row| row.running.is_none()));

        modal.update_live(BTreeMap::from([(modal.sessions[1].id.clone(), PanelId(4))]));
        modal.refresh_view(0, now);
        assert_ne!(modal.live_version, version);
        assert_eq!(
            modal.view.row(1).and_then(|row| row.running),
            Some(Running {
                panel: Some(PanelId(4))
            })
        );
    }

    #[test]
    fn a_rescan_that_changes_nothing_does_not_move_the_scroll() {
        let mut modal = modal_with(vec![
            session(1, SessionAccount::Unknown),
            session(2, SessionAccount::Unknown),
        ]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_100);
        modal.refresh_view(0, now);
        modal.reveal_selected = false;

        modal.update_live(BTreeMap::new());
        modal.refresh_view(0, now);
        assert!(!modal.reveal_selected, "same query, same selection: no scroll");

        modal.filter = "task 2".to_owned();
        modal.refresh_view(0, now);
        assert!(modal.reveal_selected, "a new query reveals the selection");
    }

    #[test]
    fn retargeting_empties_the_list_until_the_new_scan_arrives() {
        let mut modal = modal_with(vec![session(1, SessionAccount::Unknown)]);
        modal.total_found = 5;
        modal.pick = Some(PendingPick {
            id: modal.sessions[0].id.clone(),
            here: false,
        });

        modal.retarget(WorkspaceId(2), "web".to_owned(), None);

        assert_eq!(modal.workspace, WorkspaceId(2));
        assert_eq!(modal.workspace_name, "web");
        assert!(!modal.has_folder);
        assert_eq!(modal.phase, Phase::Loading);
        assert!(modal.sessions.is_empty());
        assert_eq!(modal.total_found, 0);
        assert_eq!(modal.pick, None);
    }
}
