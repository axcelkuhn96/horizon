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

use egui::{Align2, Color32, FontId, Frame, Galley, Margin, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use horizon_core::claude_usage::{ProfileUsage, UsageState, remaining_percent};
use horizon_core::{ClaudeProfile, PanelId, PanelKind, default_claude_config_dir, expand_config_dir};

use super::usage_chips::{format_relative, now_unix_ms};
use super::util::usize_to_f32;
use super::{HorizonApp, hub_ui};
use crate::theme;

/// Variable the launcher sets for a non-default account.
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

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

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

/// What the tooltip says, formatted into text only while it is open.
pub(super) struct TooltipInfo {
    name: String,
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
    let login_unusable = inputs
        .usage
        .is_some_and(|usage| matches!(usage.state, UsageState::Expired | UsageState::Missing));
    let accessible = match used_seven {
        Some(used) if remaining.is_some() => {
            format!("{name}, {}% of weekly quota remaining", remaining_percent(used))
        }
        _ => format!("{name}, weekly quota unknown"),
    };
    BadgeLayout {
        name: hub_ui::elided(painter, name, FontId::proportional(NAME_FONT_SIZE), NAME_MAX_WIDTH),
        ring: RingGeometry::new(state, remaining, inputs.pixels_per_point),
        state,
        accent: inputs.accent,
        tooltip: TooltipInfo {
            name: name.clone(),
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
    /// `$HOME`, read once.
    home: Option<Option<PathBuf>>,
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
        self.home
            .get_or_insert_with(|| std::env::var_os("HOME").map(PathBuf::from))
            .clone()
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
        let Some(panel) = self.board.panel(panel_id) else {
            return;
        };
        let kind = panel.kind;
        let detected = self.hub.status_ui.live(panel_id).map(|live| live.config_dir.as_path());
        if kind != PanelKind::Claude && detected.is_none() {
            self.hub.account_badges.forget(panel_id);
            return;
        }
        let env_dir = (kind == PanelKind::Claude)
            .then(|| panel.env.get(CONFIG_DIR_ENV))
            .flatten()
            .map(|raw| Path::new(raw.as_str()));
        let pixels_per_point = painter.ctx().pixels_per_point();
        let key = BadgeKey {
            dir: detected.or(env_dir),
            version: self.hub.usage.version(),
            ppp_bits: pixels_per_point.to_bits(),
        };
        if self.hub.account_badges.is_fresh(panel_id, &key) {
            return;
        }
        let (version, ppp_bits) = (key.version, key.ppp_bits);
        let dir = key.dir.map(Path::to_path_buf);
        let detected = detected.map(Path::to_path_buf);
        let env_dir = env_dir.map(Path::to_path_buf);

        let home = self.hub.account_badges.home();
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

/// Paints the badge inside `rect`: hover fill, ring (track and arc, or dashes when dimmed), disc
/// and name.
pub(super) fn paint_account_badge(painter: &Painter, rect: Rect, layout: &BadgeLayout, show_name: bool, hovered: bool) {
    if hovered {
        painter.rect_filled(
            rect.expand2(Vec2::new(3.0, 0.0)),
            egui::CornerRadius::same(HOVER_RADIUS),
            theme::alpha(theme::PANEL_BG_ALT(), HOVER_FILL_ALPHA),
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

/// Paints a placed badge and gives it a hover-only tooltip (account, e-mail, quota, reset).
pub(super) fn show_account_badge(ui: &mut Ui, painter: &Painter, panel_id: PanelId, badge: &PlacedBadge<'_>) {
    let layout = badge.layout;
    let response = ui.interact(
        badge.rect,
        ui.make_persistent_id(("panel_account_badge", panel_id.0)),
        Sense::hover(),
    );
    paint_account_badge(painter, badge.rect, layout, badge.show_name, response.hovered());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &layout.accessible));
    response.on_hover_ui_at_pointer(|ui| {
        Frame::NONE
            .inner_margin(Margin::same(TOOLTIP_MARGIN - TOOLTIP_FRAME_MARGIN))
            .show(ui, |ui| tooltip_body(ui, layout, now_unix_ms() / 1000));
    });
}

fn tooltip_body(ui: &mut Ui, layout: &BadgeLayout, now_secs: i64) {
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
    let pill_width = hub_ui::pill_width(painter, "active");
    let pill = hub_ui::pill_rect(
        Pos2::new(header.max.x - pill_width, header.center().y - 9.0),
        pill_width,
    );
    hub_ui::pill(painter, pill, "active", theme::PALETTE_GREEN());

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
mod tests {
    use std::cell::Cell;

    use horizon_core::claude_usage::UsageWindow;

    use super::*;
    use crate::app::hub_ui::Severity;

    fn profile(name: &str, dir: &str) -> ClaudeProfile {
        ClaudeProfile {
            name: name.to_owned(),
            config_dir: dir.to_owned(),
        }
    }

    fn two_profiles() -> Vec<ClaudeProfile> {
        vec![profile("default", "~/.claude"), profile("work", "~/.claude-work")]
    }

    fn usage(state: UsageState, seven: Option<f32>) -> ProfileUsage {
        ProfileUsage {
            name: "work".to_owned(),
            display_name: None,
            email: Some("fulano@exemplo.com".to_owned()),
            five_hour: UsageWindow {
                percent: Some(12.0),
                resets_at: None,
            },
            seven_day: UsageWindow {
                percent: seven,
                resets_at: None,
            },
            severity: None,
            state,
            fetched_at_ms: Some(1_000),
        }
    }

    fn with_painter(check: impl FnOnce(&Painter)) {
        let ctx = egui::Context::default();
        let check = Cell::new(Some(check));
        // Text layout needs fonts, which exist inside a pass.
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
            if let Some(check) = check.take() {
                check(&painter);
            }
        });
    }

    #[test]
    fn remaining_is_100_minus_used_clamped() {
        assert_eq!(remaining_pct(Some(66.0)), Some(34.0));
        assert_eq!(remaining_pct(Some(0.0)), Some(100.0));
        assert_eq!(remaining_pct(Some(100.0)), Some(0.0));
        assert_eq!(remaining_pct(Some(130.0)), Some(0.0));
        assert_eq!(remaining_pct(Some(-5.0)), Some(100.0));
    }

    #[test]
    fn remaining_none_when_usage_unknown() {
        assert_eq!(remaining_pct(None), None);
        assert_eq!(remaining_pct(Some(f32::NAN)), None);
    }

    #[test]
    fn ring_sweep_zero_and_full() {
        assert!(ring_sweep(0.0).abs() < f32::EPSILON);
        assert!(ring_sweep(-3.0).abs() < f32::EPSILON);
        assert!((ring_sweep(100.0) - 1.0).abs() < f32::EPSILON);
        assert!((ring_sweep(250.0) - 1.0).abs() < f32::EPSILON);
        assert!((ring_sweep(50.0) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn ring_sweep_min_visible_slice_when_nonzero() {
        assert!((ring_sweep(0.1) - MIN_VISIBLE_SWEEP).abs() < f32::EPSILON);
        assert!((ring_sweep(2.0) - 0.02).abs() < f32::EPSILON);
        assert!(ring_sweep(3.0) > MIN_VISIBLE_SWEEP);
    }

    #[test]
    fn ring_points_count_bounded_and_on_circle() {
        let center = Pos2::new(30.0, 12.0);
        let points = ring_points(center, 7.5, START_ANGLE, 0.34, 17);
        assert_eq!(points.len(), 18);
        for point in &points {
            assert!(
                ((*point - center).length() - 7.5).abs() < 1e-3,
                "{point:?} off the circle"
            );
        }
        // Starts at 12 o'clock and sweeps clockwise (towards +x on screen).
        assert!((points[0] - Pos2::new(30.0, 4.5)).length() < 1e-3);
        assert!(points[1].x > points[0].x);

        assert_eq!(ring_points(center, 7.5, START_ANGLE, 1.0, 500).len(), MAX_SEGMENTS + 1);
        assert_eq!(ring_points(center, 7.5, START_ANGLE, 1.0, 0).len(), 2);
        for ppp in [1.0, 1.5, 2.0, 4.0] {
            assert!(arc_segments(1.0, ppp) <= MAX_SEGMENTS);
            assert!(arc_segments(0.02, ppp) >= 1);
        }
    }

    #[test]
    fn badge_state_dim_for_expired_missing_nologin() {
        assert_eq!(badge_state(UsageState::Expired, Some(80.0)), BadgeState::Dim);
        assert_eq!(badge_state(UsageState::Missing, Some(80.0)), BadgeState::Dim);
        assert_eq!(badge_state(UsageState::Ok, None), BadgeState::Dim);
        assert_eq!(badge_state(UsageState::Stale, None), BadgeState::Dim);
    }

    #[test]
    fn badge_state_severity_follows_used_pct() {
        assert_eq!(
            badge_state(UsageState::Ok, Some(80.0)),
            BadgeState::Ok(Severity::Normal)
        );
        assert_eq!(
            badge_state(UsageState::Ok, Some(20.0)),
            BadgeState::Ok(Severity::Warning)
        );
        assert_eq!(
            badge_state(UsageState::Ok, Some(5.0)),
            BadgeState::Ok(Severity::Critical)
        );
        assert_eq!(
            badge_state(UsageState::Stale, Some(5.0)),
            BadgeState::Ok(Severity::Critical)
        );
    }

    #[test]
    fn panel_profile_matches_normalized_config_dir() {
        let home = Path::new("/home/u");
        let profiles = two_profiles();
        for dir in [
            "/home/u/.claude-work",
            "/home/u/.claude-work/",
            "~/.claude-work",
            "~/.claude-work/",
        ] {
            assert_eq!(
                panel_profile(PanelKind::Shell, None, Some(Path::new(dir)), &profiles, home),
                Some(1),
                "detected {dir}"
            );
            assert_eq!(
                panel_profile(PanelKind::Claude, Some(Path::new(dir)), None, &profiles, home),
                Some(1),
                "env {dir}"
            );
        }
        // A running Claude beats the env the panel was started with.
        assert_eq!(
            panel_profile(
                PanelKind::Claude,
                Some(Path::new("~/.claude-work")),
                Some(Path::new("/home/u/.claude")),
                &profiles,
                home
            ),
            Some(0)
        );
        // A profile written with a trailing slash still matches.
        let slashed = vec![profile("work", "~/.claude-work/")];
        assert_eq!(
            panel_profile(
                PanelKind::Shell,
                None,
                Some(Path::new("/home/u/.claude-work")),
                &slashed,
                home
            ),
            Some(0)
        );
    }

    #[test]
    fn panel_profile_default_dir_maps_to_default_profile() {
        let home = Path::new("/home/u");
        let profiles = two_profiles();
        assert_eq!(panel_profile(PanelKind::Claude, None, None, &profiles, home), Some(0));
        assert_eq!(
            panel_profile(PanelKind::Claude, Some(Path::new("")), None, &profiles, home),
            Some(0)
        );
        assert_eq!(
            panel_profile(PanelKind::Claude, Some(Path::new("~/.claude/")), None, &profiles, home),
            Some(0)
        );
        assert_eq!(
            panel_profile(
                PanelKind::Shell,
                None,
                Some(Path::new("/home/u/.claude")),
                &profiles,
                home
            ),
            Some(0)
        );
        // The default profile is found wherever it sits in the list.
        let reordered = vec![profile("work", "~/.claude-work"), profile("default", "~/.claude")];
        assert_eq!(panel_profile(PanelKind::Claude, None, None, &reordered, home), Some(1));
        // No default profile configured: no account.
        let only_work = vec![profile("work", "~/.claude-work")];
        assert_eq!(panel_profile(PanelKind::Claude, None, None, &only_work, home), None);
    }

    #[test]
    fn panel_profile_none_for_non_claude_shell() {
        let home = Path::new("/home/u");
        let profiles = two_profiles();
        assert_eq!(panel_profile(PanelKind::Shell, None, None, &profiles, home), None);
        // The env of a shell says nothing about a Claude that is not running.
        assert_eq!(
            panel_profile(
                PanelKind::Shell,
                Some(Path::new("~/.claude-work")),
                None,
                &profiles,
                home
            ),
            None
        );
        assert_eq!(panel_profile(PanelKind::Editor, None, None, &profiles, home), None);
    }

    #[test]
    fn panel_profile_unknown_dir_is_none() {
        let home = Path::new("/home/u");
        let profiles = two_profiles();
        assert_eq!(
            panel_profile(PanelKind::Shell, None, Some(Path::new("/srv/other")), &profiles, home),
            None
        );
        assert_eq!(
            panel_profile(PanelKind::Claude, Some(Path::new("/srv/other")), None, &profiles, home),
            None
        );
        assert_eq!(panel_profile(PanelKind::Claude, None, None, &[], home), None);
    }

    #[test]
    fn cache_rebuilds_only_when_key_changes() {
        let panel = PanelId(7);
        let mut cache = AccountBadgeCache::default();
        let dir = PathBuf::from("/home/u/.claude-work");
        let key = |dir: Option<&Path>, version, ppp_bits| BadgeKey { dir, version, ppp_bits };
        let entry = |dir: Option<&Path>, version, ppp_bits| Entry {
            dir: dir.map(Path::to_path_buf),
            version,
            ppp_bits,
            layout: None,
        };

        assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 1)), "nothing cached yet");
        cache.store(panel, entry(Some(&dir), 1, 1));
        assert!(
            cache.is_fresh(panel, &key(Some(&dir), 1, 1)),
            "a panel without an account is cached too"
        );
        assert!(!cache.is_fresh(panel, &key(Some(&dir), 2, 1)), "new usage snapshot");
        assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 2)), "new scale");
        assert!(!cache.is_fresh(panel, &key(None, 1, 1)), "Claude gone");
        assert!(
            !cache.is_fresh(panel, &key(Some(Path::new("/home/u/.claude")), 1, 1)),
            "other account"
        );
        assert!(!cache.is_fresh(PanelId(8), &key(Some(&dir), 1, 1)), "other panel");

        cache.forget(panel);
        assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 1)));
    }

    #[test]
    fn cache_evicts_closed_panels_when_the_count_moves() {
        let mut cache = AccountBadgeCache::default();
        for id in [1, 2] {
            cache.store(
                PanelId(id),
                Entry {
                    dir: None,
                    version: 0,
                    ppp_bits: 0,
                    layout: None,
                },
            );
        }
        cache.evict_closed(1, |id| id == PanelId(1));
        assert!(cache.entries.contains_key(&PanelId(1)));
        assert!(!cache.entries.contains_key(&PanelId(2)));
    }

    #[test]
    fn badge_width_fits_name_ellipsis_and_ring() {
        assert!((badge_width(None) - RING_SLOT).abs() < f32::EPSILON);
        assert!((badge_width(Some(40.0)) - (RING_SLOT + NAME_GAP + 40.0)).abs() < f32::EPSILON);

        with_painter(|painter| {
            let long = profile(&"x".repeat(80), "~/.claude-long");
            let short = profile("a", "~/.claude-a");
            let layout = |profile: &ClaudeProfile| {
                build_layout(
                    painter,
                    &LayoutInputs {
                        profile,
                        usage: None,
                        reset_seven: None,
                        accent: Color32::WHITE,
                        pixels_per_point: 1.0,
                    },
                )
            };
            let long = layout(&long);
            let short = layout(&short);
            assert!(long.width(true) <= MAX_WIDTH + 0.5, "{}", long.width(true));
            assert!(long.width(true) > short.width(true), "a longer name widens the badge");
            assert!(short.width(true) > RING_SLOT);
            assert!((short.width(false) - RING_SLOT).abs() < f32::EPSILON);
        });
    }

    #[test]
    fn layout_follows_usage_state() {
        with_painter(|painter| {
            let work = profile("work", "~/.claude-work");
            let layout = |usage: Option<&ProfileUsage>| {
                build_layout(
                    painter,
                    &LayoutInputs {
                        profile: &work,
                        usage,
                        reset_seven: None,
                        accent: Color32::WHITE,
                        pixels_per_point: 2.0,
                    },
                )
            };

            let fresh = usage(UsageState::Ok, Some(66.0));
            let ok = layout(Some(&fresh));
            assert_eq!(ok.state, BadgeState::Ok(Severity::Normal));
            assert_eq!(ok.accessible, "work, 34% of weekly quota remaining");
            assert!(!ok.ring.arc.is_empty() && ok.ring.dashes.is_empty() && !ok.ring.full);
            assert!(ok.ring.arc.len() <= MAX_SEGMENTS + 1);

            let untouched = usage(UsageState::Ok, Some(0.0));
            assert!(layout(Some(&untouched)).ring.full, "all quota left is a whole circle");

            let expired = usage(UsageState::Expired, Some(10.0));
            let dim = layout(Some(&expired));
            assert_eq!(dim.state, BadgeState::Dim);
            assert!(dim.ring.arc.is_empty() && !dim.ring.dashes.is_empty());
            assert!(dim.tooltip.login_note.as_deref().is_some_and(|note| {
                note.contains("claude /login") && note.contains("CLAUDE_CONFIG_DIR=~/.claude-work")
            }));

            let unknown = layout(None);
            assert_eq!(unknown.state, BadgeState::Dim);
            assert_eq!(unknown.accessible, "work, weekly quota unknown");
            assert!(unknown.tooltip.login_note.is_none());
        });
    }

    #[test]
    fn tooltip_text_has_email_used_remaining_and_reset() {
        let info = TooltipInfo {
            name: "work".to_owned(),
            email: Some("fulano@exemplo.com".to_owned()),
            used_seven: Some(66.0),
            used_five: Some(12.0),
            reset: Some(("Mon 14:00".to_owned(), 1_000_000 + 2 * 86_400 + 4 * 3_600)),
            login_note: None,
        };
        let lines = info.lines(1_000_000);
        assert_eq!(lines.email, Some("fulano@exemplo.com"));
        assert_eq!(lines.weekly, "7d: 66% used \u{b7} 34% left");
        assert_eq!(lines.five.as_deref(), Some("5h: 12% used"));
        assert_eq!(lines.reset.as_deref(), Some("Resets Mon 14:00 (in 2d 4h)"));
        assert_eq!(lines.note, None);

        let unknown = TooltipInfo {
            name: "work".to_owned(),
            email: None,
            used_seven: None,
            used_five: None,
            reset: None,
            login_note: Some("Not logged in".to_owned()),
        };
        let lines = unknown.lines(0);
        assert_eq!(lines.weekly, "7d: no data");
        assert!(lines.five.is_none() && lines.reset.is_none() && lines.email.is_none());
        assert_eq!(lines.note, Some("Not logged in"));
    }
}
