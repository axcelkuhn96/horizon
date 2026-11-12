//! Account marker in the panel titlebar: a disc in the profile's colour inside a ring that shows
//! how much of the weekly quota is left, plus the profile name.
//!
//! Visual spec: `docs/design/claude-hub-ui.md` §17. The panel to profile mapping reuses what the
//! agent-status poller already keeps per panel (`StatusUi::live`), so nothing reads `/proc` here.
//! Layouts (name galley, ring geometry, tooltip numbers) are cached per panel by
//! `(config dir, usage version, pixels_per_point)` and rebuilt only when that key moves; painting
//! reuses the cached geometry and allocates nothing.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Align2, Color32, CursorIcon, FontId, Frame, Galley, Margin, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
use horizon_core::agent_detect::default_claude_config_dir;
use horizon_core::claude_usage::{ProfileUsage, UsageState, remaining_percent};
use horizon_core::{ClaudeProfile, PanelId, PanelKind, claude_config_dir_for_env, expand_config_dir};

use super::usage_chips::{format_relative, now_unix_ms};
use super::util::usize_to_f32;
use super::{HorizonApp, hub_ui};
use crate::theme;

/// Room the ring takes at the left of the badge.
const RING_SLOT: f32 = 16.0;
const RING_RADIUS: f32 = 7.5;
const DISC_RADIUS: f32 = 3.5;
const TRACK_WIDTH: f32 = 1.8;
const ARC_WIDTH: f32 = 2.4;
const HOLLOW_DISC_WIDTH: f32 = 1.2;
/// Gap between the ring and the name.
const NAME_GAP: f32 = 5.0;
const NAME_FONT_SIZE: f32 = 10.5;
/// The badge never grows past this, name included.
const MAX_WIDTH: f32 = 120.0;
const NAME_MAX_WIDTH: f32 = MAX_WIDTH - RING_SLOT - NAME_GAP;
/// Smallest visible arc, as a fraction of the circle, while any quota is left.
const MIN_VISIBLE_SWEEP: f32 = 0.02;
/// Most segments an arc is drawn with.
const MAX_SEGMENTS: usize = 48;
/// Fewest segments of a full circle, however small it is on screen.
const MIN_FULL_SEGMENTS: f32 = 16.0;
/// On-screen length (in physical pixels) one segment of the arc covers.
const SEGMENT_PIXELS: f32 = 4.0;
/// Where the arc starts: 12 o'clock.
const START_ANGLE: f32 = -FRAC_PI_2;
/// Dashes of the track of a dimmed badge and the part of each period they fill (6 on, 4 off).
const DASH_COUNT: u8 = 5;
const DASH_FILL: f32 = 0.6;
const DIM_NAME_ALPHA: u8 = 140;
const TRACK_ALPHA: u8 = 70;
const HOVER_FILL_ALPHA: u8 = 200;
const HOVER_RADIUS: u8 = 4;

const TOOLTIP_WIDTH: f32 = 280.0;
const TOOLTIP_MARGIN: i8 = 12;
/// Inner margin of egui's tooltip frame, already part of the card.
const TOOLTIP_FRAME_MARGIN: i8 = 6;
const TOOLTIP_BAR: Vec2 = Vec2::new(120.0, 4.0);

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// Weekly quota left, `100 - used`, clamped to 0..=100. `None` while the usage is unknown.
#[must_use]
pub(super) fn remaining_pct(used_7d: Option<f32>) -> Option<f32> {
    let used = used_7d.filter(|used| used.is_finite())?;
    Some(100.0 - used.clamp(0.0, 100.0))
}

/// Fraction of the circle the arc sweeps for `remaining` percent: 0 for nothing left, at least
/// [`MIN_VISIBLE_SWEEP`] while any is left, 1 for everything.
#[must_use]
pub(super) fn ring_sweep(remaining: f32) -> f32 {
    if !remaining.is_finite() || remaining <= 0.0 {
        return 0.0;
    }
    (remaining / 100.0).clamp(MIN_VISIBLE_SWEEP, 1.0)
}

/// Polyline of an arc of `radius` around `center`: from angle `start` (radians, clockwise on
/// screen), `sweep` of the full circle (0..=1), in `segments` pieces (1..=48), so
/// `segments + 1` points.
#[must_use]
pub(super) fn ring_points(center: Pos2, radius: f32, start: f32, sweep: f32, segments: usize) -> Vec<Pos2> {
    let segments = segments.clamp(1, MAX_SEGMENTS);
    let span = sweep.clamp(0.0, 1.0) * TAU;
    let steps = usize_to_f32(segments);
    (0..=segments)
        .map(|index| {
            let (sin, cos) = (start + span * usize_to_f32(index) / steps).sin_cos();
            center + Vec2::new(cos, sin) * radius
        })
        .collect()
}

/// Segments an arc sweeping `sweep` of the circle is drawn with at `pixels_per_point`: enough to
/// look round, never more than [`MAX_SEGMENTS`] for the full circle.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=48 first
fn arc_segments(sweep: f32, pixels_per_point: f32) -> usize {
    let circumference = TAU * RING_RADIUS * pixels_per_point.max(1.0);
    let full = (circumference / SEGMENT_PIXELS).clamp(MIN_FULL_SEGMENTS, usize_to_f32(MAX_SEGMENTS));
    (full * sweep.clamp(0.0, 1.0))
        .ceil()
        .clamp(1.0, usize_to_f32(MAX_SEGMENTS)) as usize
}

/// How the badge presents the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BadgeState {
    /// Quota known and the login usable; the arc takes the colour of `Severity`.
    Ok(hub_ui::Severity),
    /// Expired, missing or unknown: hollow disc, dashed track, no arc.
    Dim,
}

/// State for an account whose login is in `usage` and whose weekly quota left is `remaining`.
/// The severity follows what is used (`100 - remaining`), the same scale as the usage chips.
#[must_use]
pub(super) fn badge_state(usage: UsageState, remaining: Option<f32>) -> BadgeState {
    match (usage, remaining) {
        (UsageState::Expired | UsageState::Missing, _) | (_, None) => BadgeState::Dim,
        (UsageState::Ok | UsageState::Stale, Some(remaining)) => {
            BadgeState::Ok(hub_ui::severity_for(100.0 - remaining, None))
        }
    }
}

fn arc_color(severity: hub_ui::Severity) -> Color32 {
    match severity {
        hub_ui::Severity::Normal => theme::PALETTE_GREEN(),
        hub_ui::Severity::Warning => theme::PALETTE_YELLOW(),
        hub_ui::Severity::Critical => theme::PALETTE_RED(),
    }
}

/// Index in `profiles` of the account a panel runs under, or `None` when it runs none (or one that
/// is not a configured profile).
///
/// The running Claude's own config dir (`detected_config_dir`) wins; a Claude panel without a
/// running Claude falls back to the `CLAUDE_CONFIG_DIR` it was started with (`env_config_dir`).
/// A missing or empty dir means the default `~/.claude`. Dirs are compared after `~` expansion and
/// ignoring a trailing slash. Any other panel without a detected Claude has no account.
#[must_use]
pub(super) fn panel_profile(
    kind: PanelKind,
    env_config_dir: Option<&Path>,
    detected_config_dir: Option<&Path>,
    profiles: &[ClaudeProfile],
    home: &Path,
) -> Option<usize> {
    let normalized = |dir: &Path| match dir.to_str() {
        Some("") => default_claude_config_dir(home),
        Some(text) => expand_config_dir(text, home),
        None => dir.to_path_buf(),
    };
    let wanted = match (detected_config_dir, kind) {
        (Some(dir), _) => normalized(dir),
        (None, PanelKind::Claude) => env_config_dir.map_or_else(|| default_claude_config_dir(home), normalized),
        (None, _) => return None,
    };
    profiles
        .iter()
        .position(|profile| expand_config_dir(&profile.config_dir, home) == wanted)
}

/// Width of the badge for a name galley of `name_width` (`None`: ring only, as in narrow panels).
#[must_use]
fn badge_width(name_width: Option<f32>) -> f32 {
    RING_SLOT + name_width.map_or(0.0, |width| NAME_GAP + width)
}

/// Percent used, rounded the way the usage chips round it.
fn used_label(used: f32) -> u32 {
    100 - remaining_percent(used)
}

/// What the login of the account allows, as the tooltip and the screen-reader label tell it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountStatus {
    Active,
    Stale,
    Expired,
    NoLogin,
    /// No usage snapshot for the account yet.
    Unknown,
}

impl AccountStatus {
    fn of(usage: Option<UsageState>) -> Self {
        match usage {
            Some(UsageState::Ok) => Self::Active,
            Some(UsageState::Stale) => Self::Stale,
            Some(UsageState::Expired) => Self::Expired,
            Some(UsageState::Missing) => Self::NoLogin,
            None => Self::Unknown,
        }
    }

    /// Text of the pill in the tooltip header; none while the status is unknown.
    fn pill(self) -> Option<(&'static str, Color32)> {
        match self {
            Self::Active => Some(("active", theme::PALETTE_GREEN())),
            Self::Stale => Some(("stale", theme::PALETTE_YELLOW())),
            Self::Expired => Some(("expired", theme::PALETTE_RED())),
            Self::NoLogin => Some(("no login", theme::PALETTE_RED())),
            Self::Unknown => None,
        }
    }

    /// `conta2, 34% of weekly quota remaining`; an unusable login is told instead of a quota.
    fn accessible(self, name: &str, used_seven: Option<f32>) -> String {
        match (self, used_seven) {
            (Self::Expired, _) => format!("{name}, login expired"),
            (Self::NoLogin, _) => format!("{name}, not logged in"),
            (_, Some(used)) if remaining_pct(Some(used)).is_some() => {
                format!("{name}, {}% of weekly quota remaining", remaining_percent(used))
            }
            _ => format!("{name}, weekly quota unknown"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

/// What the tooltip says, formatted into text only while it is open.
pub(super) struct TooltipInfo {
    name: String,
    status: AccountStatus,
    email: Option<String>,
    used_seven: Option<f32>,
    used_five: Option<f32>,
    /// Reset of the weekly window: clock text and Unix time.
    reset: Option<(String, i64)>,
    login_note: Option<String>,
}

/// The text lines of the tooltip.
struct TooltipLines<'a> {
    email: Option<&'a str>,
    weekly: String,
    five: Option<String>,
    reset: Option<String>,
    note: Option<&'a str>,
}

impl TooltipInfo {
    fn lines(&self, now_secs: i64) -> TooltipLines<'_> {
        TooltipLines {
            email: self.email.as_deref(),
            weekly: self.used_seven.map_or_else(
                || "7d: no data".to_owned(),
                |used| {
                    format!(
                        "7d: {}% used \u{b7} {}% left",
                        used_label(used),
                        remaining_percent(used)
                    )
                },
            ),
            five: self.used_five.map(|used| format!("5h: {}% used", used_label(used))),
            reset: self
                .reset
                .as_ref()
                .map(|(clock, unix)| format!("Resets {clock} (in {})", format_relative(unix - now_secs))),
            note: self.login_note.as_deref(),
        }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Ring geometry relative to the ring's centre, so one cached copy serves any position.
pub(super) struct RingGeometry {
    /// The arc of quota left; empty when dimmed or when nothing is left.
    arc: Vec<Pos2>,
    /// The arc is the whole circle: drawn as a circle.
    full: bool,
    /// The dashes of the dimmed track.
    dashes: Vec<[Pos2; 2]>,
}

impl RingGeometry {
    fn new(state: BadgeState, remaining: Option<f32>, pixels_per_point: f32) -> Self {
        let sweep = match (state, remaining) {
            (BadgeState::Ok(_), Some(remaining)) => ring_sweep(remaining),
            _ => 0.0,
        };
        let full = sweep >= 1.0;
        let arc = if sweep > 0.0 && !full {
            ring_points(
                Pos2::ZERO,
                RING_RADIUS,
                START_ANGLE,
                sweep,
                arc_segments(sweep, pixels_per_point),
            )
        } else {
            Vec::new()
        };
        let dashes = if state == BadgeState::Dim {
            let period = TAU / f32::from(DASH_COUNT);
            (0..DASH_COUNT)
                .flat_map(|index| {
                    let start = START_ANGLE + period * f32::from(index);
                    ring_points(Pos2::ZERO, RING_RADIUS, start, DASH_FILL / f32::from(DASH_COUNT), 2)
                        .windows(2)
                        .map(|pair| [pair[0], pair[1]])
                        .collect::<Vec<_>>()
                })
                .collect()
        } else {
            Vec::new()
        };
        Self { arc, full, dashes }
    }
}

/// Everything the badge of one panel needs to paint and explain itself.
pub(super) struct BadgeLayout {
    name: Arc<Galley>,
    ring: RingGeometry,
    state: BadgeState,
    accent: Color32,
    tooltip: TooltipInfo,
    /// `conta2, 34% of weekly quota remaining`.
    accessible: String,
}

impl BadgeLayout {
    /// Width of the badge; `show_name` is false in narrow panels (ring only).
    pub(super) fn width(&self, show_name: bool) -> f32 {
        badge_width(show_name.then(|| self.name.size().x))
    }
}

/// What [`build_layout`] needs about one account.
pub(super) struct LayoutInputs<'a> {
    pub(super) profile: &'a ClaudeProfile,
    pub(super) usage: Option<&'a ProfileUsage>,
    /// Clock text and Unix time of the weekly reset, formatted when the snapshot arrived.
    pub(super) reset_seven: Option<(&'a str, i64)>,
    pub(super) accent: Color32,
    pub(super) pixels_per_point: f32,
}

fn printable(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

/// Lays out the badge of one account.
#[must_use]
pub(super) fn build_layout(painter: &Painter, inputs: &LayoutInputs<'_>) -> BadgeLayout {
    let name = &inputs.profile.name;
    let used_seven = inputs.usage.and_then(|usage| usage.seven_day.percent);
    let remaining = remaining_pct(used_seven);
    let state = inputs
        .usage
        .map_or(BadgeState::Dim, |usage| badge_state(usage.state, remaining));
    let status = AccountStatus::of(inputs.usage.map(|usage| usage.state));
    let login_unusable = matches!(status, AccountStatus::Expired | AccountStatus::NoLogin);
    let accessible = status.accessible(name, used_seven);
    BadgeLayout {
        name: hub_ui::elided(painter, name, FontId::proportional(NAME_FONT_SIZE), NAME_MAX_WIDTH),
        ring: RingGeometry::new(state, remaining, inputs.pixels_per_point),
        state,
        accent: inputs.accent,
        tooltip: TooltipInfo {
            name: name.clone(),
            status,
            email: inputs
                .usage
                .and_then(|usage| usage.email.as_deref())
                .map(printable)
                .filter(|email| !email.is_empty()),
            used_seven,
            used_five: inputs.usage.and_then(|usage| usage.five_hour.percent),
            reset: inputs.reset_seven.map(|(clock, unix)| (clock.to_owned(), unix)),
            login_note: login_unusable.then(|| {
                format!(
                    "Not logged in \u{2014} run claude /login with CLAUDE_CONFIG_DIR={}",
                    printable(&inputs.profile.config_dir)
                )
            }),
        },
        accessible,
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// What a cached layout was built from; it is rebuilt when any part moves.
struct BadgeKey<'a> {
    /// Config dir the panel's Claude runs under (detected, or from the panel's env).
    dir: Option<&'a Path>,
    /// Version of the usage snapshot (it also moves with the profile list).
    version: u64,
    ppp_bits: u32,
}

struct Entry {
    dir: Option<PathBuf>,
    version: u64,
    ppp_bits: u32,
    layout: Option<BadgeLayout>,
}

/// Badge layouts per panel. `None` is cached too: a panel without a known account is not
/// re-resolved every frame.
#[derive(Default)]
pub(super) struct AccountBadgeCache {
    entries: HashMap<PanelId, Entry>,
    /// Panel count at the last eviction sweep.
    swept_for: usize,
    /// `$HOME`, read once (`home_read` says whether).
    home: Option<PathBuf>,
    home_read: bool,
}

impl AccountBadgeCache {
    /// The cached layout of `panel_id`, if it has an account.
    pub(super) fn get(&self, panel_id: PanelId) -> Option<&BadgeLayout> {
        self.entries.get(&panel_id)?.layout.as_ref()
    }

    fn is_fresh(&self, panel_id: PanelId, key: &BadgeKey<'_>) -> bool {
        self.entries.get(&panel_id).is_some_and(|entry| {
            entry.dir.as_deref() == key.dir && entry.version == key.version && entry.ppp_bits == key.ppp_bits
        })
    }

    fn store(&mut self, panel_id: PanelId, key: Entry) {
        self.entries.insert(panel_id, key);
    }

    fn forget(&mut self, panel_id: PanelId) {
        self.entries.remove(&panel_id);
    }

    /// Drops the entries of closed panels; sweeps only when the panel count moved.
    pub(super) fn evict_closed(&mut self, panel_count: usize, live: impl Fn(PanelId) -> bool) {
        if self.swept_for == panel_count {
            return;
        }
        self.swept_for = panel_count;
        self.entries.retain(|panel_id, _| live(*panel_id));
    }

    /// The user's home directory (read from the environment on first use).
    fn home(&mut self) -> Option<PathBuf> {
        if !self.home_read {
            self.home = std::env::var_os("HOME").map(PathBuf::from);
            self.home_read = true;
        }
        self.home.clone()
    }
}

impl HorizonApp {
    /// Brings the badge layout of `panel_id` up to date: resolves the panel's account and lays the
    /// badge out, but only when the panel's config dir, the usage snapshot or the scale changed.
    /// Panels that are not Claude panels and run no Claude have no badge.
    pub(super) fn refresh_account_badge(&mut self, painter: &Painter, panel_id: PanelId) {
        let panel_count = self.board.panels.len();
        self.hub
            .account_badges
            .evict_closed(panel_count, |id| self.board.panel(id).is_some());
        let home = self.hub.account_badges.home();
        let Some(panel) = self.board.panel(panel_id) else {
            return;
        };
        let kind = panel.kind;
        let detected = self.hub.status_ui.live(panel_id).map(|live| live.config_dir.as_path());
        if kind != PanelKind::Claude && detected.is_none() {
            self.hub.account_badges.forget(panel_id);
            return;
        }
        // The dir the panel was started with (blank or unset means the default `~/.claude`).
        let env_dir = home
            .as_deref()
            .filter(|_| kind == PanelKind::Claude)
            .map(|home| claude_config_dir_for_env(&panel.env, home));
        let pixels_per_point = painter.ctx().pixels_per_point();
        let key = BadgeKey {
            dir: detected.or(env_dir.as_deref()),
            version: self.hub.usage.version(),
            ppp_bits: pixels_per_point.to_bits(),
        };
        if self.hub.account_badges.is_fresh(panel_id, &key) {
            return;
        }
        let (version, ppp_bits) = (key.version, key.ppp_bits);
        let dir = key.dir.map(Path::to_path_buf);
        let detected = detected.map(Path::to_path_buf);

        let names: Vec<String> = self
            .hub
            .usage
            .profiles()
            .iter()
            .map(|profile| profile.name.clone())
            .collect();
        let slots = self.hub.accent_slots(&names).to_vec();
        let layout = home.as_deref().and_then(|home| {
            let profiles = self.hub.usage.profiles();
            let profile_index = panel_profile(kind, env_dir.as_deref(), detected.as_deref(), profiles, home)?;
            let entry = self.hub.usage.entries().get(profile_index);
            Some(build_layout(
                painter,
                &LayoutInputs {
                    profile: profiles.get(profile_index)?,
                    usage: entry.and_then(|entry| entry.usage.as_ref()),
                    reset_seven: entry
                        .and_then(|entry| entry.reset_seven.as_ref())
                        .map(|reset| (reset.clock.as_str(), reset.unix)),
                    accent: hub_ui::accent_color(slots.get(profile_index).copied().unwrap_or(0)),
                    pixels_per_point,
                },
            ))
        });
        self.hub.account_badges.store(
            panel_id,
            Entry {
                dir,
                version,
                ppp_bits,
                layout,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// A badge placed in a titlebar.
pub(super) struct PlacedBadge<'a> {
    pub(super) layout: &'a BadgeLayout,
    /// The name is drawn next to the ring (wide panels only).
    pub(super) show_name: bool,
    pub(super) rect: Rect,
    /// A click opens the quick-start strip at its accounts (shell panels that can start Claude).
    pub(super) clickable: bool,
}

impl PlacedBadge<'_> {
    /// Titlebar width the badge takes when it stands alone, gap to the next control included.
    pub(super) fn slot(&self) -> f32 {
        self.rect.width() + super::claude_quick_start::titlebar::STRIP_GAP
    }
}

/// Left edge of the name inside the badge's rect.
const fn name_offset() -> f32 {
    RING_SLOT + NAME_GAP
}

/// How the pointer and the keyboard currently treat the badge.
#[derive(Clone, Copy, Default)]
struct BadgeInteraction {
    hovered: bool,
    /// Keyboard focus (only a clickable badge takes it): draws the focus ring.
    focused: bool,
}

/// Paints the badge inside `rect`: hover fill, focus ring, ring (track and arc, or dashes when
/// dimmed), disc and name.
fn paint_account_badge(painter: &Painter, rect: Rect, layout: &BadgeLayout, show_name: bool, state: BadgeInteraction) {
    if state.hovered {
        painter.rect_filled(
            rect.expand2(Vec2::new(3.0, 0.0)),
            egui::CornerRadius::same(HOVER_RADIUS),
            theme::alpha(theme::PANEL_BG_ALT(), HOVER_FILL_ALPHA),
        );
    }
    if state.focused {
        painter.rect_stroke(
            rect.expand2(Vec2::new(3.0, 1.0)),
            egui::CornerRadius::same(HOVER_RADIUS),
            Stroke::new(1.5, theme::ACCENT()),
            StrokeKind::Outside,
        );
    }
    let center = Pos2::new(rect.min.x + RING_SLOT / 2.0, rect.center().y);
    let track = Stroke::new(TRACK_WIDTH, theme::alpha(theme::FG_DIM(), TRACK_ALPHA));
    let name_color = match layout.state {
        BadgeState::Ok(severity) => {
            painter.circle_stroke(center, RING_RADIUS, track);
            paint_arc(painter, center, &layout.ring, arc_color(severity));
            painter.circle_filled(center, DISC_RADIUS, layout.accent);
            theme::FG_SOFT()
        }
        BadgeState::Dim => {
            for dash in &layout.ring.dashes {
                painter.line_segment([center + dash[0].to_vec2(), center + dash[1].to_vec2()], track);
            }
            painter.circle_stroke(center, DISC_RADIUS, Stroke::new(HOLLOW_DISC_WIDTH, layout.accent));
            theme::alpha(theme::FG_SOFT(), DIM_NAME_ALPHA)
        }
    };
    if show_name {
        painter.galley(
            Pos2::new(rect.min.x + name_offset(), rect.center().y - layout.name.size().y / 2.0),
            Arc::clone(&layout.name),
            name_color,
        );
    }
}

/// The arc of quota left, as short segments with round ends (a polyline would need a fresh
/// vector every frame).
fn paint_arc(painter: &Painter, center: Pos2, ring: &RingGeometry, color: Color32) {
    if ring.full {
        painter.circle_stroke(center, RING_RADIUS, Stroke::new(ARC_WIDTH, color));
        return;
    }
    let stroke = Stroke::new(ARC_WIDTH, color);
    for pair in ring.arc.windows(2) {
        painter.line_segment([center + pair[0].to_vec2(), center + pair[1].to_vec2()], stroke);
    }
    for end in [ring.arc.first(), ring.arc.last()].into_iter().flatten() {
        painter.circle_filled(center + end.to_vec2(), ARC_WIDTH / 2.0, color);
    }
}

/// Paints a placed badge and gives it a tooltip (account, e-mail, quota, reset). Returns whether
/// it was clicked this frame; only a clickable badge senses clicks, shows the pointer cursor and
/// can take keyboard focus.
pub(super) fn show_account_badge(ui: &mut Ui, painter: &Painter, panel_id: PanelId, badge: &PlacedBadge<'_>) -> bool {
    let layout = badge.layout;
    let clickable = badge.clickable;
    let response = ui.interact(
        badge.rect,
        ui.make_persistent_id(("panel_account_badge", panel_id.0)),
        if clickable { Sense::click() } else { Sense::hover() },
    );
    if clickable && response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    paint_account_badge(
        painter,
        badge.rect,
        layout,
        badge.show_name,
        BadgeInteraction {
            hovered: response.hovered(),
            focused: clickable && response.has_focus(),
        },
    );
    let widget = if clickable {
        egui::WidgetType::Button
    } else {
        egui::WidgetType::Label
    };
    response.widget_info(|| egui::WidgetInfo::labeled(widget, true, &layout.accessible));
    let clicked = clickable && response.clicked();
    response.on_hover_ui_at_pointer(|ui| {
        Frame::NONE
            .inner_margin(Margin::same(TOOLTIP_MARGIN - TOOLTIP_FRAME_MARGIN))
            .show(ui, |ui| tooltip_body(ui, layout, clickable, now_unix_ms() / 1000));
    });
    clicked
}

fn tooltip_body(ui: &mut Ui, layout: &BadgeLayout, clickable: bool, now_secs: i64) {
    ui.set_width(TOOLTIP_WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;
    let info = &layout.tooltip;
    let lines = info.lines(now_secs);

    let (header, _) = ui.allocate_exact_size(Vec2::new(TOOLTIP_WIDTH, 20.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(Pos2::new(header.min.x + 4.0, header.center().y), 4.0, layout.accent);
    painter.text(
        Pos2::new(header.min.x + 14.0, header.center().y),
        Align2::LEFT_CENTER,
        &info.name,
        FontId::proportional(13.0),
        theme::FG(),
    );
    if let Some((label, color)) = info.status.pill() {
        let pill_width = hub_ui::pill_width(painter, label);
        let pill = hub_ui::pill_rect(
            Pos2::new(header.max.x - pill_width, header.center().y - 9.0),
            pill_width,
        );
        hub_ui::pill(painter, pill, label, color);
    }

    if let Some(email) = lines.email {
        ui.label(egui::RichText::new(email).size(11.0).color(theme::FG_DIM()));
    }
    ui.add_space(4.0);
    weekly_row(ui, layout, &lines.weekly);
    if let Some(five) = &lines.five {
        ui.label(egui::RichText::new(five).size(11.0).color(theme::FG_SOFT()));
    }
    if let Some(reset) = &lines.reset {
        ui.label(egui::RichText::new(reset).size(10.5).color(theme::FG_DIM()));
    }
    if let Some(note) = lines.note {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(note).size(10.5).color(theme::PALETTE_YELLOW()));
    }
    if clickable {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Click to start Claude with an account")
                .size(10.5)
                .color(theme::FG_DIM()),
        );
    }
}

/// `7d: 66% used · 34% left` with a thin bar in the colour of the ring.
fn weekly_row(ui: &mut Ui, layout: &BadgeLayout, text: &str) {
    let (row, _) = ui.allocate_exact_size(Vec2::new(TOOLTIP_WIDTH, 16.0), Sense::hover());
    let painter = ui.painter();
    let cy = row.center().y;
    let bar = Rect::from_min_size(Pos2::new(row.min.x, cy - TOOLTIP_BAR.y / 2.0), TOOLTIP_BAR);
    let used_frac = layout
        .tooltip
        .used_seven
        .map_or(0.0, |used| used.clamp(0.0, 100.0) / 100.0);
    let fill = match layout.state {
        BadgeState::Ok(severity) => arc_color(severity),
        BadgeState::Dim => theme::alpha(theme::FG_DIM(), TRACK_ALPHA),
    };
    hub_ui::mini_bar(painter, bar, used_frac, fill);
    painter.text(
        Pos2::new(bar.max.x + 8.0, cy),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(11.0),
        theme::FG(),
    );
}

#[cfg(test)]
mod tests;
