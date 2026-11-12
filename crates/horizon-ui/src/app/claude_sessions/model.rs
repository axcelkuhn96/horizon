//! Pure logic of the recent-sessions modal: which sessions are listed, how a row reads, and how
//! a resume picks its account. Nothing here touches egui or the disk, so all of it is tested.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use horizon_core::PanelId;
use horizon_core::claude_sessions::{SessionAccount, SessionSummary, sanitize_text};
use horizon_core::git_overview::RepoOverview;

/// Sessions listed before "Show all".
pub(super) const WINDOW_LIMIT: usize = 30;
const TITLE_CHARS: usize = 120;
const CHIP_CHARS: usize = 18;
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const TWO_DAYS: u64 = 2 * DAY;

/// How many of the (filtered) sessions the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ListWindow {
    pub(super) limit: usize,
}

impl Default for ListWindow {
    fn default() -> Self {
        Self { limit: WINDOW_LIMIT }
    }
}

impl ListWindow {
    /// The window after "Show all": everything that was scanned.
    pub(super) fn all() -> Self {
        Self { limit: usize::MAX }
    }
}

/// One session line, with every string already built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ListRow {
    /// Index into the modal's session list.
    pub(super) session: usize,
    /// The session's id, to find the row again after a rebuild.
    pub(super) id: String,
    pub(super) title: String,
    /// `None` when the account is not known (or no longer a configured profile).
    pub(super) account: Option<String>,
    pub(super) age: String,
    pub(super) size: String,
    /// Branch chip text; `None` for the main checkout.
    pub(super) chip: Option<String>,
    pub(super) tooltip: String,
    /// Set when the session is live right now (see [`ListView::mark_running`]).
    pub(super) running: Option<Running>,
}

/// A session that is live right now, so a second Claude must not resume it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Running {
    /// The Horizon panel running it; `None` for a Claude outside Horizon (or in another instance).
    pub(super) panel: Option<PanelId>,
}

/// Which sessions are live: those the scanner saw in the profiles' live-session files and those
/// of Horizon's own panels. Ids are lowercase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LiveSessions {
    external: BTreeSet<String>,
    panels: BTreeMap<String, PanelId>,
}

impl LiveSessions {
    pub(super) fn new(external: BTreeSet<String>, panels: BTreeMap<String, PanelId>) -> Self {
        Self { external, panels }
    }

    /// `Some` when `session_id` is live; the panel is known when a Horizon panel runs it.
    pub(super) fn running(&self, session_id: &str) -> Option<Running> {
        let id = session_id.to_ascii_lowercase();
        let panel = self.panels.get(&id).copied();
        (panel.is_some() || self.external.contains(&id)).then_some(Running { panel })
    }
}

/// An entry of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Row(ListRow),
    /// `▸ Agent worktrees (7)` above the agent rows.
    AgentHeader {
        count: usize,
        expanded: bool,
    },
}

/// The list as shown: entries in order plus the counts of its footer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ListView {
    pub(super) items: Vec<Item>,
    /// Sessions in the window.
    pub(super) shown: usize,
    /// Sessions that match the filter.
    pub(super) matching: usize,
}

impl ListView {
    /// Indices into `items` of the rows the keyboard can select (agent rows only when expanded).
    pub(super) fn selectable(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| matches!(item, Item::Row(_)).then_some(index))
            .collect()
    }

    pub(super) fn row(&self, item: usize) -> Option<&ListRow> {
        match self.items.get(item)? {
            Item::Row(row) => Some(row),
            Item::AgentHeader { .. } => None,
        }
    }

    /// Flags the rows whose session is live.
    pub(super) fn mark_running(&mut self, live: &LiveSessions) {
        for item in &mut self.items {
            if let Item::Row(row) = item {
                row.running = live.running(&row.id);
            }
        }
    }
}

/// Everything a list build depends on; the cached view is rebuilt only when this changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ViewKey {
    pub(super) scan_version: u64,
    pub(super) filter: String,
    pub(super) window: ListWindow,
    pub(super) agents_expanded: bool,
    pub(super) accounts_version: u64,
    pub(super) live_version: u64,
}

/// The list for `sessions` (newest first): the `window.limit` most recent that match `filter`,
/// worktree sessions of agents grouped after the others.
pub(super) fn build_view(
    sessions: &[SessionSummary],
    filter: &str,
    window: ListWindow,
    agents_expanded: bool,
    now: SystemTime,
    known_profiles: &[String],
) -> ListView {
    let needle = filter.trim().to_lowercase();
    let matching: Vec<usize> = (0..sessions.len())
        .filter(|&index| needle.is_empty() || matches_filter(&sessions[index], &needle))
        .collect();
    let shown: Vec<usize> = matching.iter().copied().take(window.limit).collect();
    let row_of = |index: usize| ListRow::new(index, &sessions[index], now, known_profiles);
    let mut items: Vec<Item> = shown
        .iter()
        .filter(|&&index| !sessions[index].origin.is_agent)
        .map(|&index| Item::Row(row_of(index)))
        .collect();
    let agents: Vec<usize> = shown
        .iter()
        .copied()
        .filter(|&index| sessions[index].origin.is_agent)
        .collect();
    if !agents.is_empty() {
        items.push(Item::AgentHeader {
            count: agents.len(),
            expanded: agents_expanded,
        });
        if agents_expanded {
            items.extend(agents.into_iter().map(|index| Item::Row(row_of(index))));
        }
    }
    ListView {
        items,
        shown: shown.len(),
        matching: matching.len(),
    }
}

fn matches_filter(session: &SessionSummary, needle: &str) -> bool {
    let account = session.account.profile().unwrap_or("");
    let branch = session.origin.branch.as_deref().unwrap_or("");
    let folder = session
        .origin
        .worktree
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    [
        session.title.as_str(),
        session.first_prompt.as_str(),
        account,
        branch,
        folder,
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(needle))
}

impl ListRow {
    fn new(index: usize, session: &SessionSummary, now: SystemTime, known_profiles: &[String]) -> Self {
        let account = session
            .account
            .profile()
            .filter(|name| known_profiles.iter().any(|known| known.as_str() == *name))
            .map(str::to_owned);
        let chip = (!session.origin.is_main).then(|| {
            let name = session
                .origin
                .branch
                .as_deref()
                .or_else(|| session.origin.worktree.file_name().and_then(|name| name.to_str()))
                .unwrap_or("worktree");
            middle_truncate(&sanitize_text(name, 80), CHIP_CHARS)
        });
        Self {
            session: index,
            id: session.id.clone(),
            title: row_title(&session.title),
            account,
            age: relative_date_label(session.updated, now),
            size: size_label(session.size),
            chip,
            tooltip: row_tooltip(session),
            running: None,
        }
    }
}

/// What the header says about the number of sessions: the count, or how many are shown of at
/// least how many exist when the scan stopped at its caps.
pub(super) fn header_count_label(shown: usize, total_found: usize) -> String {
    if total_found > shown {
        return format!("{shown} shown (of \u{2265}{total_found})");
    }
    let noun = if shown == 1 { "session" } else { "sessions" };
    format!("{shown} {noun}")
}

/// What the directories to scan depend on, taken from data the app already holds (no disk): the
/// workspace folder and, once its git overview has arrived, the worktrees of its repository.
/// The open modal rescans when this changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ScanSignature {
    folder: Option<PathBuf>,
    /// `(path, branch, is main, prunable)` of each worktree; `None` until the overview arrives.
    worktrees: Option<Vec<(PathBuf, Option<String>, bool, bool)>>,
}

pub(super) fn scan_signature(folder: Option<&Path>, overview: Option<&RepoOverview>) -> ScanSignature {
    ScanSignature {
        folder: folder.map(Path::to_path_buf),
        worktrees: overview.map(|overview| {
            overview
                .worktrees
                .iter()
                .map(|worktree| {
                    (
                        worktree.path.clone(),
                        worktree.branch.clone(),
                        worktree.is_main,
                        worktree.prunable,
                    )
                })
                .collect()
        }),
    }
}

/// Title as drawn: control characters removed, bounded, never empty.
pub(super) fn row_title(raw: &str) -> String {
    let title = sanitize_text(raw, TITLE_CHARS);
    if title.is_empty() {
        "Untitled session".to_owned()
    } else {
        title
    }
}

fn row_tooltip(session: &SessionSummary) -> String {
    let folder = sanitize_text(&session.origin.worktree.to_string_lossy(), 240);
    format!("{folder}\nResumes in {folder}")
}

/// `text` cut to `max` characters with the middle replaced by an ellipsis.
pub(super) fn middle_truncate(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max || max < 3 {
        return text.to_owned();
    }
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// `just now`, `5 min ago`, `2 h ago`, `yesterday`, then `Nov 8` (UTC date).
pub(super) fn relative_date_label(updated: SystemTime, now: SystemTime) -> String {
    let age = now.duration_since(updated).map_or(0, |elapsed| elapsed.as_secs());
    match age {
        0..MINUTE => "just now".to_owned(),
        MINUTE..HOUR => format!("{} min ago", age / MINUTE),
        HOUR..DAY => format!("{} h ago", age / HOUR),
        DAY..TWO_DAYS => "yesterday".to_owned(),
        _ => month_day(updated),
    }
}

fn month_day(time: SystemTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = i64::try_from(secs / DAY).unwrap_or(0) + 719_468;
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let name = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .copied()
        .unwrap_or("?");
    format!("{name} {day}")
}

/// `12 B`, `340 KB`, `1.2 MB`.
pub(super) fn size_label(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = KB * KB;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", bytes / KB)
    } else {
        let tenths = bytes * 10 / MB;
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

/// What a resume needs to know about the account before it can start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ResumeAccount {
    /// Resume under this profile.
    Use(String),
    /// The session's account is unknown (or no longer configured): ask first.
    Ask,
}

/// The account a resume uses: the session's own when it is still a configured profile.
pub(super) fn resolve_resume_account(account: &SessionAccount, configured: &[String]) -> ResumeAccount {
    match account {
        SessionAccount::Profile(name) if configured.iter().any(|known| known == name) => {
            ResumeAccount::Use(name.clone())
        }
        SessionAccount::Profile(_) | SessionAccount::Unknown => ResumeAccount::Ask,
    }
}

/// Directory a resume starts in: the folder the session ran in, never the workspace's own.
pub(super) fn resume_cwd(session: &SessionSummary) -> &Path {
    &session.origin.worktree
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use horizon_core::claude_sessions::SessionOrigin;

    use super::*;

    fn session(
        n: u64,
        title: &str,
        branch: Option<&str>,
        main: bool,
        agent: bool,
        account: SessionAccount,
    ) -> SessionSummary {
        let worktree = PathBuf::from(if agent {
            format!("/p/.claude/worktrees/agent-{n}")
        } else if main {
            "/p".to_owned()
        } else {
            format!("/p-wt{n}")
        });
        SessionSummary {
            origin: SessionOrigin {
                worktree: worktree.clone(),
                branch: branch.map(str::to_owned),
                is_main: main,
                is_agent: agent,
            },
            id: format!("00000000-0000-4000-8000-{n:012}"),
            title: title.to_owned(),
            first_prompt: String::new(),
            updated: SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000 - n),
            size: 1_300_000,
            cwd: worktree,
            account,
        }
    }

    fn many(count: u64, agent: bool) -> Vec<SessionSummary> {
        (1..=count)
            .map(|n| {
                session(
                    n,
                    &format!("task {n}"),
                    Some("main"),
                    !agent,
                    agent,
                    SessionAccount::Unknown,
                )
            })
            .collect()
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000)
    }

    #[test]
    fn row_label_sanitizes_control_chars_and_truncates() {
        assert_eq!(row_title("fix\u{1b}[31m the\nbug"), "fix[31m the bug");
        assert_eq!(row_title("\u{7}\n"), "Untitled session");
        assert_eq!(row_title(&"x".repeat(500)).chars().count(), TITLE_CHARS);
    }

    #[test]
    fn middle_truncate_keeps_both_ends() {
        assert_eq!(middle_truncate("short", 18), "short");
        let cut = middle_truncate("feature/a-very-long-branch-name", 18);
        assert_eq!(cut.chars().count(), 18);
        assert!(cut.starts_with("feature/"));
        assert!(cut.ends_with("name"));
        assert!(cut.contains('\u{2026}'));
    }

    #[test]
    fn relative_date_label_buckets() {
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000 - secs);
        assert_eq!(relative_date_label(at(10), now()), "just now");
        assert_eq!(relative_date_label(at(5 * 60), now()), "5 min ago");
        assert_eq!(relative_date_label(at(2 * 3_600 + 5), now()), "2 h ago");
        assert_eq!(relative_date_label(at(30 * 3_600), now()), "yesterday");
        // 2_000_000_000 is 2033-05-18; ten days earlier reads as a date.
        assert_eq!(relative_date_label(at(10 * 86_400), now()), "May 8");
        // A modification time in the future is "just now", not a panic.
        assert_eq!(relative_date_label(now() + Duration::from_secs(60), now()), "just now");
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(12), "12 B");
        assert_eq!(size_label(340 * 1024), "340 KB");
        assert_eq!(size_label(1_300_000), "1.2 MB");
    }

    #[test]
    fn list_window_caps_at_30_and_show_all_expands() {
        let sessions = many(112, false);

        let capped = build_view(&sessions, "", ListWindow::default(), false, now(), &[]);
        let all = build_view(&sessions, "", ListWindow::all(), false, now(), &[]);

        assert_eq!((capped.shown, capped.matching), (30, 112));
        assert_eq!(capped.items.len(), 30);
        assert_eq!((all.shown, all.matching), (112, 112));
        assert_eq!(ListWindow::all().limit, usize::MAX);
        assert_eq!(ListWindow::default().limit, WINDOW_LIMIT);
    }

    #[test]
    fn agent_group_collapsed_by_default_and_counts_hidden() {
        let mut sessions = many(3, false);
        sessions.extend((10..17).map(|n| session(n, "agent work", Some("wt"), false, true, SessionAccount::Unknown)));
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated));

        let collapsed = build_view(&sessions, "", ListWindow::default(), false, now(), &[]);
        let expanded = build_view(&sessions, "", ListWindow::default(), true, now(), &[]);

        assert_eq!(collapsed.items.len(), 3 + 1, "three rows and the header");
        assert_eq!(
            collapsed.items.last(),
            Some(&Item::AgentHeader {
                count: 7,
                expanded: false
            })
        );
        assert_eq!(collapsed.selectable().len(), 3);
        assert_eq!(expanded.items.len(), 3 + 1 + 7);
        assert_eq!(expanded.selectable().len(), 10);
        assert_eq!(expanded.row(4).map(|row| row.title.as_str()), Some("agent work"));
    }

    #[test]
    fn filter_matches_title_branch_account_and_folder() {
        let sessions = vec![
            session(
                1,
                "Fix login redirect",
                Some("feat/login"),
                false,
                false,
                SessionAccount::Profile("conta2".into()),
            ),
            session(
                2,
                "Refactor billing",
                Some("main"),
                true,
                false,
                SessionAccount::Unknown,
            ),
        ];
        let count = |filter: &str| build_view(&sessions, filter, ListWindow::default(), false, now(), &[]).matching;

        assert_eq!(count(""), 2);
        assert_eq!(count("LOGIN"), 1);
        assert_eq!(count("conta2"), 1);
        assert_eq!(count("billing"), 1);
        assert_eq!(count("wt1"), 1, "folder name");
        assert_eq!(count("nothing"), 0);
    }

    #[test]
    fn branch_chip_only_off_the_main_checkout() {
        let sessions = vec![
            session(1, "a", Some("feat/login"), false, false, SessionAccount::Unknown),
            session(2, "b", Some("main"), true, false, SessionAccount::Unknown),
        ];

        let view = build_view(&sessions, "", ListWindow::default(), false, now(), &[]);

        assert_eq!(view.row(0).and_then(|row| row.chip.as_deref()), Some("feat/login"));
        assert_eq!(view.row(1).and_then(|row| row.chip.as_deref()), None);
        assert!(view.row(0).is_some_and(|row| row.tooltip.contains("Resumes in /p-wt1")));
    }

    #[test]
    fn unknown_account_requires_account_pick_before_resume() {
        let configured = vec!["conta1".to_owned(), "conta2".to_owned()];

        assert_eq!(
            resolve_resume_account(&SessionAccount::Unknown, &configured),
            ResumeAccount::Ask
        );
        assert_eq!(
            resolve_resume_account(&SessionAccount::Profile("gone".into()), &configured),
            ResumeAccount::Ask,
            "a profile that is no longer configured cannot be resumed under"
        );
        assert_eq!(
            resolve_resume_account(&SessionAccount::Profile("conta2".into()), &configured),
            ResumeAccount::Use("conta2".to_owned())
        );
    }

    #[test]
    fn an_account_that_is_no_longer_configured_is_shown_as_unknown() {
        let sessions = vec![session(
            1,
            "a",
            None,
            true,
            false,
            SessionAccount::Profile("gone".into()),
        )];

        let view = build_view(
            &sessions,
            "",
            ListWindow::default(),
            false,
            now(),
            &["conta1".to_owned()],
        );

        assert_eq!(view.row(0).and_then(|row| row.account.clone()), None);
    }

    #[test]
    fn resume_cwd_is_session_worktree_not_workspace_cwd() {
        let wt = session(1, "a", Some("feat"), false, false, SessionAccount::Unknown);

        assert_eq!(resume_cwd(&wt), Path::new("/p-wt1"));
    }

    #[test]
    fn titles_with_markup_stay_text() {
        let sessions = vec![session(
            1,
            "<b>bold</b>\u{202e}evil",
            None,
            true,
            false,
            SessionAccount::Unknown,
        )];

        let view = build_view(&sessions, "", ListWindow::default(), false, now(), &[]);

        assert_eq!(view.row(0).map(|row| row.title.as_str()), Some("<b>bold</b>evil"));
    }

    fn worktree_info(
        path: &str,
        branch: Option<&str>,
        dirty: Option<usize>,
    ) -> horizon_core::git_overview::WorktreeInfo {
        horizon_core::git_overview::WorktreeInfo {
            path: PathBuf::from(path),
            branch: branch.map(str::to_owned),
            head_short: None,
            is_main: false,
            is_current: false,
            locked: false,
            prunable: false,
            dirty_count: dirty,
            is_agent: false,
        }
    }

    fn overview_of(worktrees: Vec<horizon_core::git_overview::WorktreeInfo>) -> RepoOverview {
        RepoOverview {
            repo_root: PathBuf::from("/p"),
            branch: Some("main".to_owned()),
            detached: false,
            head_short: None,
            dirty_count: 0,
            upstream: None,
            ahead: 0,
            behind: 0,
            worktrees,
        }
    }

    #[test]
    fn scan_signature_changes_when_the_overview_arrives_and_ignores_dirty_counts() {
        let folder = Path::new("/p");
        let before = scan_signature(Some(folder), None);
        let clean = overview_of(vec![worktree_info("/p", Some("main"), Some(0))]);
        let dirty = overview_of(vec![worktree_info("/p", Some("main"), Some(7))]);
        let more = overview_of(vec![
            worktree_info("/p", Some("main"), None),
            worktree_info("/p-wt", Some("feat"), None),
        ]);

        let arrived = scan_signature(Some(folder), Some(&clean));

        assert_ne!(before, arrived, "the worktrees now count");
        assert_eq!(
            arrived,
            scan_signature(Some(folder), Some(&dirty)),
            "uncommitted files do not"
        );
        assert_ne!(arrived, scan_signature(Some(folder), Some(&more)));
        assert_ne!(
            arrived,
            scan_signature(Some(Path::new("/q")), Some(&clean)),
            "nor does a new folder"
        );
    }

    #[test]
    fn header_count_says_shown_of_at_least_when_the_scan_was_capped() {
        assert_eq!(header_count_label(1, 1), "1 session");
        assert_eq!(header_count_label(12, 12), "12 sessions");
        assert_eq!(header_count_label(0, 0), "0 sessions");
        assert_eq!(header_count_label(1_000, 1_342), "1000 shown (of \u{2265}1342)");
    }

    #[test]
    fn running_sessions_are_marked_and_know_their_panel() {
        let sessions = many(3, false);
        let mut view = build_view(&sessions, "", ListWindow::default(), false, now(), &[]);
        let live = LiveSessions::new(
            BTreeSet::from([sessions[0].id.clone()]),
            BTreeMap::from([(sessions[1].id.clone(), PanelId(9))]),
        );

        view.mark_running(&live);

        assert_eq!(view.row(0).and_then(|row| row.running), Some(Running { panel: None }));
        assert_eq!(
            view.row(1).and_then(|row| row.running),
            Some(Running {
                panel: Some(PanelId(9))
            })
        );
        assert_eq!(view.row(2).and_then(|row| row.running), None);
        assert_eq!(
            live.running(&sessions[0].id.to_uppercase()),
            Some(Running { panel: None })
        );
    }
}
