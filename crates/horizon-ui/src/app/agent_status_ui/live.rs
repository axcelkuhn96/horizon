//! Which Claude runs in which panel, and what its live-session file says.
//!
//! Claude often runs inside a shell panel, so the process walk (`detect_claude_process`) is
//! profile-aware, throttled per panel and cached here; the session file is only re-read when its
//! modification time moves. Nothing runs per frame for panels that are not due.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::{Galley, Painter};

use horizon_core::agent_detect::{ClaudeProcess, detect_claude_process};
use horizon_core::agent_status::{
    AgentStatus, LiveSessionInfo, LiveSessionRead, LiveSessionTracker, StatusEvent, StatusSource,
    read_live_session_if_changed,
};
use horizon_core::{ClaudeProfile, Config, PanelId, PanelKind, expand_config_dir};

use super::toast::{StatusToast, toast_for_notice};
use crate::app::HorizonApp;
use crate::app::claude_quick_start::titlebar::shell_is_busy;
use crate::app::hub_toast::Toast;

/// Re-check interval of a panel whose Claude process is known.
const KNOWN_INTERVAL: Duration = Duration::from_secs(2);
/// Re-check interval of a panel without a known Claude (one `/proc/<pid>/stat` read when idle).
const PROBE_INTERVAL: Duration = Duration::from_secs(3);
/// Panels refreshed per frame at most.
const MAX_REFRESHES_PER_FRAME: usize = 4;
/// Longest wait between two process walks of a busy shell that hosts no Claude.
const MAX_WALK_BACKOFF_SHIFT: u32 = 3;
/// How long the profile list is reused.
const PROFILES_TTL: Duration = Duration::from_secs(30);
/// Toast requests kept while nobody takes them.
const MAX_QUEUED_TOASTS: usize = 16;

/// The Claude process found in a panel and what its session file last said. Shared with the
/// account badge of the titlebar (task 31), which needs the same panel to profile mapping.
pub(in crate::app) struct PanelLiveSession {
    /// Pid of the `claude` process.
    pub(in crate::app) pid: u32,
    /// Its `CLAUDE_CONFIG_DIR` (the default `~/.claude` when unset).
    pub(in crate::app) config_dir: PathBuf,
    /// Name of the configured profile that owns `config_dir`.
    pub(in crate::app) profile: Option<String>,
    /// The latest readable session file; carries the session name.
    pub(in crate::app) info: Option<LiveSessionInfo>,
    modified: Option<SystemTime>,
    tracker: LiveSessionTracker,
}

impl PanelLiveSession {
    fn new(process: &ClaudeProcess, profile: Option<String>) -> Self {
        Self {
            pid: process.pid,
            config_dir: process.agent.config_dir.clone(),
            profile,
            info: None,
            modified: None,
            tracker: LiveSessionTracker::default(),
        }
    }

    /// The session name as `ListAgents` shows it.
    pub(in crate::app) fn session_name(&self) -> Option<&str> {
        self.info.as_ref().and_then(|info| info.name.as_deref())
    }

    /// Reads the session file when it changed. Returns the status events it produced, and
    /// `true` when the session is gone (process dead or file removed).
    fn read_step(&mut self) -> (Vec<StatusEvent>, bool) {
        if !process_alive(self.pid) {
            return (Vec::new(), true);
        }
        match read_live_session_if_changed(&self.config_dir, self.pid, self.modified) {
            LiveSessionRead::Unchanged => (Vec::new(), false),
            LiveSessionRead::Gone => (Vec::new(), true),
            LiveSessionRead::Fresh { info, modified } => {
                self.modified = modified;
                let events = self.tracker.observe(&info).into_iter().collect();
                self.info = Some(info);
                (events, false)
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

struct Entry {
    next_check: Instant,
    live: Option<PanelLiveSession>,
    /// Process walks in a row that found no Claude in a busy shell (drives the back-off).
    fruitless_walks: u32,
}

/// The session name of a titlebar chip laid out for a scale; relaid only when either moves.
struct CachedName {
    text: String,
    ppp_bits: u32,
    galley: Arc<Galley>,
}

/// Agent-status state owned by `HubState`.
#[derive(Default)]
pub(in crate::app) struct StatusUi {
    entries: HashMap<PanelId, Entry>,
    /// Galleys of the chips' session names, one per panel.
    name_galleys: HashMap<PanelId, CachedName>,
    /// Panel count at the last eviction sweep.
    swept_for: usize,
    profiles: Option<(Instant, Vec<ClaudeProfile>)>,
    /// Toast requests waiting for the hub toast queue.
    pub(super) toasts: Vec<StatusToast>,
}

impl StatusUi {
    /// The Claude found in `panel_id`, once detected.
    pub(in crate::app) fn live(&self, panel_id: PanelId) -> Option<&PanelLiveSession> {
        self.entries.get(&panel_id)?.live.as_ref()
    }

    /// Forgets the Claude of a panel whose process exited. Returns whether there was one.
    fn forget_live(&mut self, panel_id: PanelId) -> bool {
        self.entries
            .get_mut(&panel_id)
            .is_some_and(|entry| entry.live.take().is_some())
    }

    fn is_due(&self, panel_id: PanelId, now: Instant) -> bool {
        self.entries.get(&panel_id).is_none_or(|entry| now >= entry.next_check)
    }

    /// Drops the entries of closed panels; sweeps only when the panel count moved.
    pub(in crate::app) fn evict_closed(&mut self, panel_count: usize, live: impl Fn(PanelId) -> bool) {
        if self.swept_for == panel_count {
            return;
        }
        self.swept_for = panel_count;
        self.entries.retain(|panel_id, _| live(*panel_id));
        self.name_galleys.retain(|panel_id, _| live(*panel_id));
    }

    /// The galley of `panel_id`'s session name for its titlebar chip: laid out when the name or
    /// the scale changed, shared from the cache otherwise. `None` while the name is unknown or
    /// `visible` is false (narrow panel).
    pub(in crate::app) fn name_galley(
        &mut self,
        painter: &Painter,
        panel_id: PanelId,
        visible: bool,
    ) -> Option<Arc<Galley>> {
        let name = self
            .entries
            .get(&panel_id)
            .and_then(|entry| entry.live.as_ref())
            .and_then(PanelLiveSession::session_name)
            .filter(|_| visible);
        let Some(name) = name else {
            self.name_galleys.remove(&panel_id);
            return None;
        };
        let ppp_bits = painter.ctx().pixels_per_point().to_bits();
        if let Some(cached) = self.name_galleys.get(&panel_id)
            && cached.ppp_bits == ppp_bits
            && cached.text == name
        {
            return Some(Arc::clone(&cached.galley));
        }
        let galley = super::elided_name(painter, name);
        self.name_galleys.insert(
            panel_id,
            CachedName {
                text: name.to_owned(),
                ppp_bits,
                galley: Arc::clone(&galley),
            },
        );
        Some(galley)
    }

    pub(super) fn queue_capped(&mut self, toast: StatusToast) {
        if self.toasts.len() >= MAX_QUEUED_TOASTS {
            self.toasts.remove(0);
        }
        self.toasts.push(toast);
    }

    /// Configured profiles, or the discovered ones; re-read at most every [`PROFILES_TTL`].
    fn profiles(&mut self, config: &Config, now: Instant) -> Vec<ClaudeProfile> {
        if let Some((at, profiles)) = &self.profiles
            && now.saturating_duration_since(*at) < PROFILES_TTL
        {
            return profiles.clone();
        }
        let profiles = config.effective_claude_profiles();
        self.profiles = Some((now, profiles.clone()));
        profiles
    }

    /// Looks at one panel: reads its session file, or looks for a Claude process. Returns the
    /// status events to apply (an `Unknown` one when the Claude that was there is gone).
    fn refresh(
        &mut self,
        panel_id: PanelId,
        kind: PanelKind,
        child_pid: i32,
        profiles: &[ClaudeProfile],
        home: Option<&Path>,
        now: Instant,
    ) -> Vec<StatusEvent> {
        let entry = self.entries.entry(panel_id).or_insert_with(|| Entry {
            next_check: now,
            live: None,
            fruitless_walks: 0,
        });
        let mut events = Vec::new();
        if let Some(live) = entry.live.as_mut() {
            let (read_events, gone) = live.read_step();
            events = read_events;
            if gone {
                entry.live = None;
                events.push(StatusEvent {
                    state: AgentStatus::Unknown,
                    session_id: None,
                });
            }
        }
        if entry.live.is_none() {
            // A shell only has a Claude when something holds its terminal's foreground.
            let may_host_claude = kind == PanelKind::Claude || shell_is_busy(child_pid);
            if may_host_claude {
                if let Some(process) = detect_claude_process(child_pid, profiles) {
                    let profile = profile_name_for(profiles, home, &process.agent.config_dir);
                    let mut live = PanelLiveSession::new(&process, profile);
                    let (read_events, gone) = live.read_step();
                    // Without a readable session file there is nothing to follow (the hooks, if
                    // installed, still report); try the walk again next time.
                    if !gone && live.info.is_some() {
                        events.extend(read_events);
                        entry.live = Some(live);
                    }
                }
                if entry.live.is_none() && kind != PanelKind::Claude {
                    entry.fruitless_walks = entry.fruitless_walks.saturating_add(1);
                }
            } else {
                entry.fruitless_walks = 0;
            }
        } else {
            entry.fruitless_walks = 0;
        }
        entry.next_check = now + next_interval(entry.live.is_some(), entry.fruitless_walks);
        events
    }
}

/// Wait before the next check: short while a Claude is followed, [`PROBE_INTERVAL`] while none
/// is expected, and doubling (up to 8x) for a busy shell whose process walks keep finding none
/// (a long-running build), so that shell is not walked every few seconds.
fn next_interval(has_live: bool, fruitless_walks: u32) -> Duration {
    if has_live {
        KNOWN_INTERVAL
    } else {
        PROBE_INTERVAL * (1_u32 << fruitless_walks.saturating_sub(1).min(MAX_WALK_BACKOFF_SHIFT))
    }
}

/// Name of the profile whose expanded config dir is `config_dir`.
fn profile_name_for(profiles: &[ClaudeProfile], home: Option<&Path>, config_dir: &Path) -> Option<String> {
    let home = home?;
    profiles
        .iter()
        .find(|profile| expand_config_dir(&profile.config_dir, home) == config_dir)
        .map(|profile| profile.name.clone())
}

impl HorizonApp {
    /// Once per frame: turns the board's status notices into toast requests, then refreshes the
    /// few panels whose check is due (see the module doc).
    pub(in crate::app) fn poll_agent_status(&mut self) {
        self.drain_status_notices();
        let now = Instant::now();
        self.hub
            .status_ui
            .evict_closed(self.board.panels.len(), |panel_id| self.board.panel(panel_id).is_some());

        let mut due: Vec<(PanelId, PanelKind, i32)> = Vec::new();
        let mut exited: Vec<PanelId> = Vec::new();
        for panel in &self.board.panels {
            if !matches!(panel.kind, PanelKind::Shell | PanelKind::Command | PanelKind::Claude) {
                continue;
            }
            if panel.child_exited() {
                if self.hub.status_ui.live(panel.id).is_some() {
                    exited.push(panel.id);
                }
                continue;
            }
            let Some(child_pid) = panel.child_pid() else {
                continue;
            };
            if due.len() < MAX_REFRESHES_PER_FRAME && self.hub.status_ui.is_due(panel.id, now) {
                due.push((panel.id, panel.kind, child_pid));
            }
        }
        // A panel whose process exited has no Claude and no session name any more.
        for panel_id in exited {
            self.hub.status_ui.forget_live(panel_id);
            if let Some(panel) = self.board.panel_mut(panel_id) {
                panel.set_agent_session_name(None);
            }
        }
        if due.is_empty() {
            return;
        }
        let profiles = self.hub.status_ui.profiles(&self.template_config, now);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for (panel_id, kind, child_pid) in due {
            let events = self
                .hub
                .status_ui
                .refresh(panel_id, kind, child_pid, &profiles, home.as_deref(), now);
            for event in events {
                self.board
                    .apply_agent_status_from(panel_id, event, StatusSource::SessionFile);
            }
            // The session name also lives on the panel, where the link list and labels read it.
            let name = self
                .hub
                .status_ui
                .live(panel_id)
                .and_then(PanelLiveSession::session_name);
            if let Some(panel) = self.board.panel_mut(panel_id) {
                panel.set_agent_session_name(name);
            }
        }
    }

    fn drain_status_notices(&mut self) {
        for notice in self.board.take_status_notices() {
            let toast = {
                let live = self.hub.status_ui.live(notice.panel_id);
                let account = live.and_then(|live| live.profile.as_deref());
                let subject = live
                    .and_then(PanelLiveSession::session_name)
                    .or_else(|| self.board.panel(notice.panel_id).map(|panel| panel.title.as_str()))
                    .unwrap_or("Claude");
                toast_for_notice(&notice, account, subject)
            };
            self.hub.status_ui.queue_capped(toast);
        }
        for toast in self.hub.status_ui.take_toasts() {
            self.hub.toasts.push(Toast {
                kind: toast.kind.hub_kind(),
                text: toast.text,
                panel: Some(toast.panel),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::agent_detect::RunningAgent;

    use super::*;

    const SESSION: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

    fn process(pid: u32, config_dir: &Path) -> ClaudeProcess {
        ClaudeProcess {
            pid,
            agent: RunningAgent {
                binary: "claude".to_owned(),
                config_dir: config_dir.to_path_buf(),
                session_id: None,
            },
        }
    }

    fn write_session(dir: &Path, pid: u32, status: &str) {
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::write(
            sessions.join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"{SESSION}","name":"expia-11","status":"{status}"}}"#),
        )
        .expect("write session");
    }

    #[test]
    fn profile_is_matched_by_expanded_config_dir() {
        let profiles = vec![
            ClaudeProfile {
                name: "conta1".to_owned(),
                config_dir: "~/.claude".to_owned(),
            },
            ClaudeProfile {
                name: "conta2".to_owned(),
                config_dir: "~/.claude-work".to_owned(),
            },
        ];
        let home = Path::new("/home/x");

        assert_eq!(
            profile_name_for(&profiles, Some(home), Path::new("/home/x/.claude-work")).as_deref(),
            Some("conta2")
        );
        assert_eq!(profile_name_for(&profiles, Some(home), Path::new("/elsewhere")), None);
        assert_eq!(profile_name_for(&profiles, None, Path::new("/home/x/.claude")), None);
    }

    #[test]
    fn read_step_emits_one_event_per_status_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();
        let mut live = PanelLiveSession::new(&process(pid, dir.path()), Some("conta2".to_owned()));

        write_session(dir.path(), pid, "busy");
        let (events, gone) = live.read_step();
        assert!(!gone);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].state, AgentStatus::Working);
        assert_eq!(live.session_name(), Some("expia-11"));

        let (events, gone) = live.read_step();
        assert!(!gone && events.is_empty(), "unchanged file: no read, no event");

        std::fs::remove_file(dir.path().join(format!("sessions/{pid}.json"))).expect("remove");
        let (_, gone) = live.read_step();
        assert!(gone, "a removed file means the session ended");
    }

    #[test]
    fn a_dead_process_is_gone_even_if_its_file_is_left_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A pid far above any real pid_max.
        let dead_pid = u32::MAX - 1;
        write_session(dir.path(), dead_pid, "busy");
        let mut live = PanelLiveSession::new(&process(dead_pid, dir.path()), None);

        let (events, gone) = live.read_step();

        if cfg!(target_os = "linux") {
            assert!(gone);
            assert!(events.is_empty());
        }
    }

    #[test]
    fn entries_are_due_until_refreshed_and_evicted_with_their_panel() {
        let mut ui = StatusUi::default();
        let now = Instant::now();
        assert!(ui.is_due(PanelId(1), now));

        ui.entries.insert(
            PanelId(1),
            Entry {
                next_check: now + KNOWN_INTERVAL,
                live: None,
                fruitless_walks: 0,
            },
        );
        assert!(!ui.is_due(PanelId(1), now));
        assert!(ui.is_due(PanelId(1), now + KNOWN_INTERVAL));

        ui.evict_closed(1, |_| false);
        assert!(ui.live(PanelId(1)).is_none());
        assert!(ui.entries.is_empty());
        ui.entries.insert(
            PanelId(2),
            Entry {
                next_check: now,
                live: None,
                fruitless_walks: 0,
            },
        );
        ui.evict_closed(1, |_| false);
        assert_eq!(ui.entries.len(), 1, "no sweep while the panel count is unchanged");
    }

    #[test]
    fn busy_shells_without_a_claude_are_walked_less_and_less_often() {
        assert_eq!(next_interval(true, 0), KNOWN_INTERVAL);
        assert_eq!(next_interval(false, 0), PROBE_INTERVAL);
        assert_eq!(next_interval(false, 1), PROBE_INTERVAL);
        assert_eq!(next_interval(false, 2), PROBE_INTERVAL * 2);
        assert_eq!(next_interval(false, 3), PROBE_INTERVAL * 4);
        assert_eq!(next_interval(false, 4), PROBE_INTERVAL * 8);
        assert_eq!(next_interval(false, 500), PROBE_INTERVAL * 8, "the wait is capped");
        assert_eq!(
            next_interval(true, 9),
            KNOWN_INTERVAL,
            "a followed Claude keeps its pace"
        );
    }

    #[test]
    fn forgetting_the_live_session_of_an_exited_panel_reports_it_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ui = StatusUi::default();
        ui.entries.insert(
            PanelId(1),
            Entry {
                next_check: Instant::now(),
                live: Some(PanelLiveSession::new(&process(42, dir.path()), None)),
                fruitless_walks: 0,
            },
        );

        assert!(ui.live(PanelId(1)).is_some());
        assert!(ui.forget_live(PanelId(1)));
        assert!(ui.live(PanelId(1)).is_none());
        assert!(!ui.forget_live(PanelId(1)));
        assert!(!ui.forget_live(PanelId(2)));
    }

    #[test]
    fn the_name_galley_is_laid_out_once_per_name_and_scale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();
        write_session(dir.path(), pid, "busy");
        let mut live = PanelLiveSession::new(&process(pid, dir.path()), None);
        let _ = live.read_step();
        let mut ui = StatusUi::default();
        ui.entries.insert(
            PanelId(1),
            Entry {
                next_check: Instant::now(),
                live: Some(live),
                fruitless_walks: 0,
            },
        );
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = Painter::new(ctx.clone(), egui::LayerId::background(), egui::Rect::EVERYTHING);

            let first = ui.name_galley(&painter, PanelId(1), true).expect("galley");
            let again = ui.name_galley(&painter, PanelId(1), true).expect("galley");
            assert!(Arc::ptr_eq(&first, &again), "same name and scale: no new layout");

            assert!(ui.name_galley(&painter, PanelId(1), false).is_none(), "narrow panel");
            assert!(
                !ui.name_galleys.contains_key(&PanelId(1)),
                "hiding the name drops its cache"
            );
            assert!(ui.name_galley(&painter, PanelId(2), true).is_none(), "unknown panel");
            assert!(ui.name_galley(&painter, PanelId(1), true).is_some(), "shown again");
            assert!(ui.name_galleys.contains_key(&PanelId(1)));
        });
    }

    #[test]
    fn queued_toasts_are_capped() {
        use super::super::toast::StatusToastKind;

        let mut ui = StatusUi::default();
        for index in 0..(MAX_QUEUED_TOASTS + 5) {
            ui.queue_capped(StatusToast {
                kind: StatusToastKind::Info,
                text: index.to_string(),
                panel: PanelId(1),
            });
        }

        assert_eq!(ui.toasts.len(), MAX_QUEUED_TOASTS);
        assert_eq!(ui.toasts[0].text, "5");
    }
}
