//! Claude usage chips in the toolbar: one chip per profile with its 5h / 7d
//! usage. Visual spec: `docs/design/claude-hub-ui.md` §1.
//!
//! This file holds the pure rules (labels, severity, widths), the snapshot
//! state fed by [`UsagePoller`] and the layout cache. Painting is in `paint`,
//! clock/duration text in `time`.

mod paint;
mod time;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Color32, Context, FontId, Galley, Id, LayerId, Order, Painter};
use horizon_core::claude_usage::{
    ProfileUsage, UsagePoller, UsageState, UsageWindow, parse_rfc3339_unix, used_percent_label,
};
use horizon_core::{ClaudeProfile, HorizonHome, UsageDashboard, Waker, format_tokens};

use super::HorizonApp;
use super::hub_state::HubState;
use super::hub_ui::{self, Severity};

pub(super) use paint::{show_chips, show_overflow_rows};

const IDENT_MAX_FULL: usize = 10;
const IDENT_MAX_COMPACT: usize = 5;
/// A fetch older than this makes an `Ok` profile read as stale.
const STALE_AFTER_MS: i64 = 15 * 60 * 1000;
const POLL_INTERVAL: Duration = Duration::from_secs(300);
const CACHE_FILE: &str = "claude-usage-cache.json";
/// How long a requested refresh counts as in flight before the menu offers "Refresh now" again.
const REFRESH_WINDOW: Duration = Duration::from_secs(30);
/// How long the tooltip keeps waiting for the first token stats before giving up.
const TOKENS_SCAN_PATIENCE: Duration = Duration::from_secs(30);

// Chip geometry (spec §1). Heights and gaps are fixed; widths come from the text.
pub(super) const CHIP_HEIGHT: f32 = 28.0;
pub(super) const CHIP_GAP: f32 = 6.0;
const CHIP_PAD: f32 = 8.0;
const DOT_DIAMETER: f32 = 6.0;
const DOT_GAP: f32 = 6.0;
const SECTION_GAP: f32 = 8.0;
const NAME_MAX_FULL: f32 = 84.0;
const NAME_MAX_COMPACT: f32 = 64.0;
const LABEL_WIDTH: f32 = 10.0;
const LABEL_GAP: f32 = 4.0;
const PCT_WIDTH: f32 = 26.0;
const BAR_WIDTH_FULL: f32 = 28.0;
const BAR_WIDTH_COMPACT: f32 = 20.0;
const GROUP_WIDTH_FULL: f32 = LABEL_WIDTH + LABEL_GAP + BAR_WIDTH_FULL + LABEL_GAP + PCT_WIDTH;
const GROUP_WIDTH_COMPACT: f32 = LABEL_WIDTH + LABEL_GAP + BAR_WIDTH_COMPACT + LABEL_GAP + PCT_WIDTH;
/// Per-chip widths used by layout tests (no font measurement available there).
#[cfg(test)]
const CHIP_FULL_ESTIMATE: f32 = 300.0;
#[cfg(test)]
const CHIP_COMPACT_ESTIMATE: f32 = 185.0;

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// Severity of a usage percentage; the API's own verdict (`"critical"` /
/// `"warning"`) wins. An unknown percentage is always normal.
#[must_use]
pub(super) fn severity_for(percent: Option<f32>, api: Option<&str>) -> Severity {
    let Some(percent) = percent else {
        return Severity::Normal;
    };
    let api = match api {
        Some("critical") => Some(Severity::Critical),
        Some("warning") => Some(Severity::Warning),
        _ => None,
    };
    hub_ui::severity_for(percent, api)
}

/// Account identifier shown after the profile name: the e-mail's local part,
/// at most `max_chars` characters followed by `…` when cut. `None` when the
/// e-mail is unknown or has no local part.
#[must_use]
pub(super) fn account_id(email: Option<&str>, max_chars: usize) -> Option<String> {
    let local: String = email?
        .split('@')
        .next()?
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    if local.is_empty() {
        return None;
    }
    if local.chars().count() <= max_chars {
        return Some(local);
    }
    let mut cut: String = local.chars().take(max_chars).collect();
    cut.push('\u{2026}');
    Some(cut)
}

/// `42%`, or `—` when the window (or the whole profile) has no value yet.
pub(super) fn window_text(window: Option<&UsageWindow>) -> String {
    match window {
        Some(window) if window.percent.is_some() => format!("{}%", used_percent_label(window)),
        _ => "\u{2014}".to_owned(),
    }
}

/// `("claude2 · gutosjc", "5h 37%  7d 12%")` — identity and usage halves of a chip label.
#[must_use]
fn chip_label_parts(name: &str, usage: Option<&ProfileUsage>, compact: bool) -> (String, String) {
    let id_max = if compact { IDENT_MAX_COMPACT } else { IDENT_MAX_FULL };
    let head = match account_id(usage.and_then(|u| u.email.as_deref()), id_max) {
        Some(id) => format!("{name} \u{b7} {id}"),
        None => name.to_owned(),
    };
    let tail = match usage {
        None => {
            if compact {
                "7d \u{2014}".to_owned()
            } else {
                "5h \u{2014}  7d \u{2014}".to_owned()
            }
        }
        Some(u) => {
            let seven = match u.state {
                UsageState::Expired => "expired".to_owned(),
                UsageState::Missing => "no login".to_owned(),
                UsageState::Ok | UsageState::Stale => format!("7d {}", window_text(Some(&u.seven_day))),
            };
            if compact {
                seven
            } else {
                format!("5h {}  {seven}", window_text(Some(&u.five_hour)))
            }
        }
    };
    (head, tail)
}

/// One-line chip text, for accessibility and the overflow menu:
/// `claude2 · gutosjc  5h 37%  7d 12%` (compact: `claude2 · gutos…  7d 12%`).
/// An expired or missing login replaces the 7d part by `expired` / `no login`.
#[must_use]
pub(super) fn chip_label(usage: &ProfileUsage, compact: bool) -> String {
    let (head, tail) = chip_label_parts(&usage.name, Some(usage), compact);
    format!("{head}  {tail}")
}

/// How a chip presents its profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChipStatus {
    /// No snapshot entry for the profile yet.
    NoData,
    Fresh,
    /// Data is old (older than 15 minutes, or the last refresh failed).
    Stale,
    Expired,
    Missing,
}

#[must_use]
pub(super) fn chip_status(usage: Option<&ProfileUsage>, now_ms: i64) -> ChipStatus {
    let Some(usage) = usage else {
        return ChipStatus::NoData;
    };
    match usage.state {
        UsageState::Expired => ChipStatus::Expired,
        UsageState::Missing => ChipStatus::Missing,
        UsageState::Stale => ChipStatus::Stale,
        UsageState::Ok => {
            if usage
                .fetched_at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) > STALE_AFTER_MS)
            {
                ChipStatus::Stale
            } else {
                ChipStatus::Fresh
            }
        }
    }
}

/// API severity applies to the window closest to its limit only, so a low
/// window does not turn red because the other one is critical.
pub(super) fn api_severity(usage: &ProfileUsage, five_hour: bool) -> Option<&str> {
    let api = usage.severity.as_deref()?;
    let (this, other) = if five_hour {
        (usage.five_hour.percent, usage.seven_day.percent)
    } else {
        (usage.seven_day.percent, usage.five_hour.percent)
    };
    (this.unwrap_or(-1.0) >= other.unwrap_or(-1.0)).then_some(api)
}

fn count_as_f32(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

/// Full chip: dot, name, id, 5h group and 7d group (or a status pill in place of 7d).
#[must_use]
fn full_chip_width(name_w: f32, id_w: f32, pill_w: Option<f32>) -> f32 {
    CHIP_PAD
        + DOT_DIAMETER
        + DOT_GAP
        + name_w
        + id_w
        + SECTION_GAP
        + GROUP_WIDTH_FULL
        + SECTION_GAP
        + pill_w.unwrap_or(GROUP_WIDTH_FULL)
        + CHIP_PAD
}

/// Compact chip: dot, name, id, then only the 7d group (or the status pill).
#[must_use]
fn compact_chip_width(name_w: f32, id_w: f32, pill_w: Option<f32>) -> f32 {
    CHIP_PAD + DOT_DIAMETER + DOT_GAP + name_w + id_w + SECTION_GAP + pill_w.unwrap_or(GROUP_WIDTH_COMPACT) + CHIP_PAD
}

/// Widths of every chip in both presentations, in profile order. Empty means
/// "no chips" (disabled, or no profiles).
#[derive(Clone, Debug, Default)]
pub(super) struct ChipWidths {
    pub(super) full: Vec<f32>,
    pub(super) compact: Vec<f32>,
}

static NO_CHIPS: ChipWidths = ChipWidths {
    full: Vec::new(),
    compact: Vec::new(),
};

impl ChipWidths {
    /// No chips at all.
    #[cfg(test)]
    #[must_use]
    pub(super) fn none() -> Self {
        Self::default()
    }

    /// `count` chips at a fixed width.
    #[cfg(test)]
    #[must_use]
    pub(super) fn estimated(count: usize) -> Self {
        Self {
            full: vec![CHIP_FULL_ESTIMATE; count],
            compact: vec![CHIP_COMPACT_ESTIMATE; count],
        }
    }

    #[must_use]
    pub(super) fn len(&self) -> usize {
        self.full.len()
    }

    /// Width of the first `visible` chips with the gaps between them.
    #[must_use]
    pub(super) fn total(&self, compact: bool, visible: usize) -> f32 {
        if visible == 0 {
            return 0.0;
        }
        let widths = if compact { &self.compact } else { &self.full };
        widths.iter().take(visible).sum::<f32>() + count_as_f32(visible - 1) * CHIP_GAP
    }
}

/// How the toolbar shows the chips: one presentation for all of them (never
/// full next to compact), the first `visible` of `total` inline and the rest in
/// the overflow menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ChipsPlan {
    pub(super) compact: bool,
    pub(super) visible: usize,
    pub(super) total: usize,
}

impl ChipsPlan {
    #[must_use]
    pub(super) fn overflowing(self) -> bool {
        self.visible < self.total
    }
}

// ---------------------------------------------------------------------------
// Snapshot state
// ---------------------------------------------------------------------------

/// Reset moment of a usage window: pre-formatted clock text plus the Unix time
/// for the "in 1h 12m" part computed while the tooltip is open.
pub(super) struct ResetInfo {
    pub(super) clock: String,
    pub(super) unix: i64,
}

/// Everything one chip needs, formatted when the snapshot arrives.
pub(super) struct ChipEntry {
    pub(super) name: String,
    pub(super) usage: Option<ProfileUsage>,
    id_full: Option<String>,
    id_compact: Option<String>,
    /// `Fulano Silva · fulano@exemplo.com`, whichever parts are known.
    pub(super) identity: Option<String>,
    pub(super) reset_five: Option<ResetInfo>,
    pub(super) reset_seven: Option<ResetInfo>,
}

fn reset_info(window: &UsageWindow, offset: Option<i64>, weekday: bool) -> Option<ResetInfo> {
    let unix = parse_rfc3339_unix(window.resets_at.as_deref()?)?;
    Some(ResetInfo {
        clock: time::format_clock(unix, offset, weekday),
        unix,
    })
}

fn identity_line(usage: &ProfileUsage) -> Option<String> {
    let clean = |text: &str| -> String { text.chars().filter(|ch| !ch.is_control()).collect() };
    let display = usage.display_name.as_deref().map(clean).filter(|text| !text.is_empty());
    let email = usage.email.as_deref().map(clean).filter(|text| !text.is_empty());
    match (display, email) {
        (Some(display), Some(email)) => Some(format!("{display} \u{b7} {email}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

impl ChipEntry {
    fn build(name: &str, usage: Option<&ProfileUsage>, offset: Option<i64>) -> Self {
        let email = usage.and_then(|u| u.email.as_deref());
        Self {
            name: name.to_owned(),
            usage: usage.cloned(),
            id_full: account_id(email, IDENT_MAX_FULL),
            id_compact: account_id(email, IDENT_MAX_COMPACT),
            identity: usage.and_then(identity_line),
            reset_five: usage.and_then(|u| reset_info(&u.five_hour, offset, false)),
            reset_seven: usage.and_then(|u| reset_info(&u.seven_day, offset, true)),
        }
    }

    /// Identifier text for the active presentation.
    fn id(&self, compact: bool) -> Option<&str> {
        if compact {
            self.id_compact.as_deref()
        } else {
            self.id_full.as_deref()
        }
    }

    /// Text of the status pill replacing the 7d group, if the login is unusable.
    fn pill_text(&self) -> Option<&'static str> {
        match self.usage.as_ref().map(|u| u.state) {
            Some(UsageState::Expired) => Some("expired"),
            Some(UsageState::Missing) => Some("no login"),
            _ => None,
        }
    }
}

/// Text layouts of one chip, valid for one `(usage version, pixels_per_point)`.
pub(super) struct ChipLayout {
    name_full: Arc<Galley>,
    name_compact: Arc<Galley>,
    id_full: Option<Arc<Galley>>,
    id_compact: Option<Arc<Galley>>,
    pill_width: Option<f32>,
    /// `5h` / `7d` captions and their percentages, laid out with
    /// `Color32::PLACEHOLDER`; the paint call supplies the colour.
    label_five: Arc<Galley>,
    label_seven: Arc<Galley>,
    pct_five: Arc<Galley>,
    pct_seven: Arc<Galley>,
    pub(super) accent_slot: usize,
}

impl ChipLayout {
    fn name(&self, compact: bool) -> &Arc<Galley> {
        if compact { &self.name_compact } else { &self.name_full }
    }

    fn id(&self, compact: bool) -> Option<&Arc<Galley>> {
        if compact {
            self.id_compact.as_ref()
        } else {
            self.id_full.as_ref()
        }
    }

    /// Caption and percentage galleys of the 5h or 7d window.
    fn window(&self, five_hour: bool) -> (&Arc<Galley>, &Arc<Galley>) {
        if five_hour {
            (&self.label_five, &self.pct_five)
        } else {
            (&self.label_seven, &self.pct_seven)
        }
    }
}

/// Galleys and widths of every chip. Rebuilt only when the snapshot, the
/// profile list or `pixels_per_point` change — never per frame.
#[derive(Default)]
pub(super) struct UsageChipsCache {
    key: Option<(u64, u32)>,
    pub(super) widths: ChipWidths,
    pub(super) chips: Vec<ChipLayout>,
}

fn id_galley(painter: &Painter, id: Option<&str>) -> Option<Arc<Galley>> {
    id.map(|id| hub_ui::elided(painter, &format!(" \u{b7} {id}"), FontId::proportional(10.5), 120.0))
}

fn window_galleys(painter: &Painter, caption: &str, window: Option<&UsageWindow>) -> (Arc<Galley>, Arc<Galley>) {
    (
        painter.layout_no_wrap(caption.to_owned(), FontId::proportional(8.5), Color32::PLACEHOLDER),
        painter.layout_no_wrap(window_text(window), FontId::monospace(10.5), Color32::PLACEHOLDER),
    )
}

fn build_cache(painter: &Painter, entries: &[ChipEntry], slots: &[usize], key: (u64, u32)) -> UsageChipsCache {
    let mut cache = UsageChipsCache {
        key: Some(key),
        ..UsageChipsCache::default()
    };
    for (index, entry) in entries.iter().enumerate() {
        let name_font = FontId::proportional(11.5);
        let name_full = hub_ui::elided(painter, &entry.name, name_font.clone(), NAME_MAX_FULL);
        let name_compact = hub_ui::elided(painter, &entry.name, name_font, NAME_MAX_COMPACT);
        let id_full = id_galley(painter, entry.id(false));
        let id_compact = id_galley(painter, entry.id(true));
        let pill_width = entry.pill_text().map(|text| hub_ui::pill_width(painter, text));
        let width_of = |galley: &Option<Arc<Galley>>| galley.as_ref().map_or(0.0, |g| g.size().x);
        cache
            .widths
            .full
            .push(full_chip_width(name_full.size().x, width_of(&id_full), pill_width));
        cache.widths.compact.push(compact_chip_width(
            name_compact.size().x,
            width_of(&id_compact),
            pill_width,
        ));
        let usage = entry.usage.as_ref();
        let (label_five, pct_five) = window_galleys(painter, "5h", usage.map(|u| &u.five_hour));
        let (label_seven, pct_seven) = window_galleys(painter, "7d", usage.map(|u| &u.seven_day));
        cache.chips.push(ChipLayout {
            name_full,
            name_compact,
            id_full,
            id_compact,
            pill_width,
            label_five,
            label_seven,
            pct_five,
            pct_seven,
            accent_slot: slots.get(index).copied().unwrap_or(0),
        });
    }
    cache
}

/// Where the poller reads credentials and keeps its cache. Absent until the app enables
/// polling, so tests never reach the network or the user's real credentials.
struct PollSetup {
    /// The user's home directory; profile directories resolve against it.
    user_home: PathBuf,
    cache_path: PathBuf,
}

/// Usage snapshot and poller of the Claude profiles; lives in [`HubState`].
#[derive(Default)]
pub(super) struct ClaudeUsageState {
    poller: Option<UsagePoller>,
    poll_setup: Option<PollSetup>,
    /// Profiles being polled; the chips follow this list, in order.
    profiles: Vec<ClaudeProfile>,
    snapshot: Vec<ProfileUsage>,
    entries: Vec<ChipEntry>,
    /// Bumped whenever `entries` change; keys the layout cache.
    version: u64,
    /// Set by a manual refresh or a profile change until fresh data arrives.
    refresh_started: Option<(Instant, i64)>,
    /// `HubState::rows_version` the profile list was last synced against.
    synced_rows_version: Option<u64>,
    /// Stats poll behind the "Tokens today" tooltip line; started by the first hover.
    tokens_poll: Option<UsageDashboard>,
    /// Claude tokens used today across all accounts, formatted on arrival.
    tokens_today: Option<String>,
    /// When the stats poll started and whether it already answered (even without data).
    tokens_scan: Option<(Instant, bool)>,
    pub(super) cache: UsageChipsCache,
}

impl ClaudeUsageState {
    fn rebuild_entries(&mut self) {
        let offset = time::local_offset_secs();
        self.entries = self
            .profiles
            .iter()
            .map(|profile| {
                let usage = self.snapshot.iter().find(|usage| usage.name == profile.name);
                ChipEntry::build(&profile.name, usage, offset)
            })
            .collect();
        self.version = self.version.wrapping_add(1);
    }

    /// Lets the profile list start a poller: credentials come from `user_home`, the cache lives
    /// under `horizon_home`.
    pub(super) fn enable_polling(&mut self, user_home: PathBuf, horizon_home: &HorizonHome) {
        self.poll_setup = Some(PollSetup {
            user_home,
            cache_path: horizon_home.root().join(CACHE_FILE),
        });
    }

    /// Replaces the polled profile list; returns whether it changed.
    fn set_profiles(&mut self, profiles: Vec<ClaudeProfile>) -> bool {
        if self.profiles == profiles {
            return false;
        }
        self.profiles = profiles;
        self.rebuild_entries();
        true
    }

    /// Takes a finished snapshot from the poller.
    fn apply_snapshot(&mut self, snapshot: Vec<ProfileUsage>) {
        if let Some((_, started_ms)) = self.refresh_started
            && refresh_answered(&snapshot, started_ms)
        {
            self.refresh_started = None;
        }
        self.snapshot = snapshot;
        self.rebuild_entries();
    }

    pub(super) fn entries(&self) -> &[ChipEntry] {
        &self.entries
    }

    /// Account e-mail of `name` from the snapshot, once known.
    #[must_use]
    pub(super) fn account_email(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)?
            .usage
            .as_ref()?
            .email
            .as_deref()
    }

    /// Latest usage of `name` from the snapshot, once known.
    #[must_use]
    pub(super) fn usage(&self, name: &str) -> Option<&ProfileUsage> {
        self.entries.iter().find(|entry| entry.name == name)?.usage.as_ref()
    }

    /// Starts the stats poll for the "Tokens today" line (once).
    pub(super) fn start_tokens_poll(&mut self) {
        if self.tokens_poll.is_none() {
            self.tokens_poll = Some(UsageDashboard::new());
            self.tokens_scan = Some((Instant::now(), false));
        }
    }

    /// Takes a finished stats snapshot; returns whether one arrived.
    fn poll_tokens(&mut self) -> bool {
        let Some(dashboard) = self.tokens_poll.as_mut() else {
            return false;
        };
        if !dashboard.poll() {
            return false;
        }
        if let Some((_, answered)) = self.tokens_scan.as_mut() {
            *answered = true;
        }
        let tokens = dashboard.snapshot.as_ref().map(|snapshot| snapshot.claude.today_tokens);
        if let Some(tokens) = tokens {
            self.set_tokens_today(tokens);
        }
        true
    }

    fn set_tokens_today(&mut self, tokens: u64) {
        self.tokens_today = Some(format_tokens(tokens));
    }

    /// `1.2M`-style tokens used today by all Claude accounts, once known.
    #[must_use]
    pub(super) fn tokens_today(&self) -> Option<&str> {
        self.tokens_today.as_deref()
    }

    /// The stats scan was started, has not answered yet and is still within its patience window.
    #[must_use]
    pub(super) fn tokens_scan_pending(&self, now: Instant) -> bool {
        self.tokens_scan.is_some_and(|(started, answered)| {
            !answered && now.saturating_duration_since(started) < TOKENS_SCAN_PATIENCE
        })
    }

    /// A refresh was requested and has not produced fresh data (or timed out).
    #[must_use]
    pub(super) fn is_refreshing(&self, now: Instant) -> bool {
        self.refresh_started
            .is_some_and(|(started, _)| now.saturating_duration_since(started) < REFRESH_WINDOW)
    }

    /// Asks the poller for an immediate refresh.
    pub(super) fn request_refresh(&mut self) {
        if let Some(poller) = &self.poller {
            poller.refresh_now();
            self.refresh_started = Some((Instant::now(), now_unix_ms()));
        }
    }

    /// Chip widths for the toolbar layout: none when the bar is disabled or
    /// there is no profile.
    #[must_use]
    pub(super) fn chip_widths(&self, enabled: bool) -> &ChipWidths {
        if enabled { &self.cache.widths } else { &NO_CHIPS }
    }
}

/// Whether `snapshot` answers a refresh that began at `started_ms`: some profile
/// was fetched since, or none can be (every profile is expired, missing or
/// failed, so no `fetched_at_ms` will ever advance).
fn refresh_answered(snapshot: &[ProfileUsage], started_ms: i64) -> bool {
    snapshot
        .iter()
        .any(|usage| usage.fetched_at_ms.is_some_and(|at| at >= started_ms))
        || (!snapshot.is_empty() && snapshot.iter().all(|usage| usage.state != UsageState::Ok))
}

pub(super) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

impl HubState {
    /// Rebuilds the chip layout cache when the snapshot, the profile list or
    /// the scale changed since it was built.
    pub(super) fn ensure_usage_chip_cache(&mut self, ctx: &Context) {
        let key = (self.usage.version, ctx.pixels_per_point().to_bits());
        if self.usage.cache.key == Some(key) {
            return;
        }
        let names: Vec<String> = self.usage.entries.iter().map(|entry| entry.name.clone()).collect();
        let slots = self.accent_slots(&names).to_vec();
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("usage_chips_measure")));
        self.usage.cache = build_cache(&painter, &self.usage.entries, &slots, key);
    }
}

impl HubState {
    /// Aligns the poller with `profiles` (the effective profiles of the config).
    fn sync_usage_profiles(&mut self, profiles: Vec<ClaudeProfile>, ctx: &Context) {
        let usage = &mut self.usage;
        let changed = usage.set_profiles(profiles.clone());
        if profiles.is_empty() {
            usage.poller = None;
            usage.refresh_started = None;
        } else if let Some(poller) = &usage.poller {
            if changed {
                poller.set_profiles(profiles);
                usage.refresh_started = Some((Instant::now(), now_unix_ms()));
            }
        } else if let Some(setup) = &usage.poll_setup {
            let waker: Waker = {
                let ctx = ctx.clone();
                Arc::new(move || ctx.request_repaint())
            };
            usage.poller = Some(UsagePoller::spawn_with_waker(
                profiles,
                setup.user_home.clone(),
                setup.cache_path.clone(),
                POLL_INTERVAL,
                Some(waker),
            ));
            usage.refresh_started = Some((Instant::now(), now_unix_ms()));
        }
        if changed {
            self.invalidate_launcher_rows();
        }
        // Our own bump must not trigger another profile sync.
        self.usage.synced_rows_version = Some(self.rows_version);
    }

    /// Applies a finished snapshot; the launcher shows the same data, so its rows rebuild once.
    fn apply_usage_snapshot(&mut self, snapshot: Vec<ProfileUsage>) {
        self.usage.apply_snapshot(snapshot);
        self.invalidate_launcher_rows();
        self.usage.synced_rows_version = Some(self.rows_version);
    }

    /// Takes the poller's latest snapshot, if one arrived.
    fn receive_usage_snapshot(&mut self) -> bool {
        let Some(snapshot) = self.usage.poller.as_ref().and_then(UsagePoller::try_recv) else {
            return false;
        };
        self.apply_usage_snapshot(snapshot);
        true
    }
}

impl HorizonApp {
    /// Per-frame work, cheap when idle: sync the polled profiles with the
    /// config, start the poller, and take a finished snapshot. Repaints only
    /// when a snapshot arrived; the poller's waker repaints an idle UI when it
    /// delivers one.
    pub(super) fn poll_claude_usage(&mut self, ctx: &Context) {
        if self.hub.usage.synced_rows_version != Some(self.hub.rows_version) {
            let profiles = self.template_config.effective_claude_profiles();
            self.hub.sync_usage_profiles(profiles, ctx);
        }
        let tokens = self.hub.usage.poll_tokens();
        if self.hub.receive_usage_snapshot() || tokens {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests;
