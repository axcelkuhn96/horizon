//! Agent status in the interface: the status dot (titlebar and sidebar), the titlebar chip with
//! the Claude session name, and the per-panel live-session cache behind them.
//!
//! The status itself lives on the panel (`Panel::agent_status`); this module only paints it.
//! Visual spec: `docs/design/claude-hub-ui.md` §8.

mod live;
mod toast;

use std::sync::Arc;

use egui::{Color32, FontId, Galley, Painter, Pos2, Rect, Stroke, Vec2};
use horizon_core::PanelId;
use horizon_core::agent_status::AgentStatus;

use super::{HorizonApp, hub_ui};
use crate::theme;

pub(super) use live::StatusUi;

/// Dot radius in a panel titlebar.
pub(super) const TITLEBAR_DOT_RADIUS: f32 = 4.5;
/// Dot radius in a sidebar row.
pub(super) const SIDEBAR_DOT_RADIUS: f32 = 3.5;
/// Width of the fixed status column of the sidebar rows of a workspace.
pub(super) const SIDEBAR_STATUS_COLUMN: f32 = 12.0;
/// Hit rect of the titlebar dot (tooltip only).
const DOT_SLOT: f32 = 16.0;
const CHIP_HEIGHT: f32 = 18.0;
const NAME_GAP: f32 = 2.0;
const NAME_MAX_WIDTH: f32 = 110.0;
const NAME_FONT_SIZE: f32 = 10.5;

/// The four shapes: status is told apart by form as well as colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DotShape {
    /// Ring with a small centre dot.
    Ring,
    /// Filled dot with a halo.
    DotHalo,
    /// Filled dot with a tick.
    DotTick,
    /// Filled dot with a cross.
    DotCross,
}

/// Shape for `status`; `None` for `Unknown` (nothing is painted).
#[must_use]
pub(super) fn dot_shape(status: AgentStatus) -> Option<DotShape> {
    match status {
        AgentStatus::Unknown => None,
        AgentStatus::Working => Some(DotShape::Ring),
        AgentStatus::Waiting => Some(DotShape::DotHalo),
        AgentStatus::Done => Some(DotShape::DotTick),
        AgentStatus::Error => Some(DotShape::DotCross),
    }
}

/// Tooltip and accessibility text of a status; empty for `Unknown`.
#[must_use]
pub(super) fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Unknown => "",
        AgentStatus::Working => "Working\u{2026}",
        AgentStatus::Waiting => "Waiting for you",
        AgentStatus::Done => "Done \u{2014} ready for next prompt",
        AgentStatus::Error => "Stopped with an error",
    }
}

/// Colour of the dot: a seen `Done` fades to alpha 0.55.
fn dot_color(status: AgentStatus, seen: bool) -> Color32 {
    match status {
        AgentStatus::Unknown => Color32::TRANSPARENT,
        AgentStatus::Working => theme::ACCENT(),
        AgentStatus::Waiting => theme::PALETTE_YELLOW(),
        AgentStatus::Done if seen => theme::alpha(theme::PALETTE_GREEN(), 140),
        AgentStatus::Done => theme::PALETTE_GREEN(),
        AgentStatus::Error => theme::PALETTE_RED(),
    }
}

/// The two segments of the tick inside a dot of `radius` at `center`.
#[must_use]
fn tick_segments(center: Pos2, radius: f32) -> [[Pos2; 2]; 2] {
    let unit = radius * 0.5;
    let low = center + Vec2::new(-unit * 0.3, unit * 0.7);
    [
        [center + Vec2::new(-unit, unit * 0.05), low],
        [low, center + Vec2::new(unit, -unit * 0.7)],
    ]
}

/// The two segments of the cross inside a dot of `radius` at `center`.
#[must_use]
fn cross_segments(center: Pos2, radius: f32) -> [[Pos2; 2]; 2] {
    let unit = radius * 0.4;
    [
        [center + Vec2::new(-unit, -unit), center + Vec2::new(unit, unit)],
        [center + Vec2::new(-unit, unit), center + Vec2::new(unit, -unit)],
    ]
}

/// Paints the status dot of `radius` at `center`. `Unknown` paints nothing. `seen` only matters
/// for `Done` (a finished turn nobody looked at yet is drawn at full strength).
pub(super) fn paint_status_dot(painter: &Painter, center: Pos2, radius: f32, status: AgentStatus, seen: bool) {
    let Some(shape) = dot_shape(status) else {
        return;
    };
    let color = dot_color(status, seen);
    match shape {
        DotShape::Ring => {
            painter.circle_stroke(center, radius - 0.75, Stroke::new(1.5_f32, color));
            painter.circle_filled(center, radius / 3.0, color);
        }
        DotShape::DotHalo => {
            painter.circle_stroke(center, radius + 2.0, Stroke::new(1.0_f32, theme::alpha(color, 115)));
            painter.circle_filled(center, radius, color);
        }
        DotShape::DotTick => {
            painter.circle_filled(center, radius, color);
            let stroke = Stroke::new(1.2_f32, Color32::WHITE);
            for segment in tick_segments(center, radius) {
                painter.line_segment(segment, stroke);
            }
        }
        DotShape::DotCross => {
            painter.circle_filled(center, radius, color);
            let stroke = Stroke::new(1.2_f32, Color32::WHITE);
            for segment in cross_segments(center, radius) {
                painter.line_segment(segment, stroke);
            }
        }
    }
}

/// Tooltip of the titlebar chip: the status, then the session name, its id and the account.
#[must_use]
fn chip_tooltip(status: AgentStatus, name: Option<&str>, session_id: Option<&str>, account: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let label = status_label(status);
    if !label.is_empty() {
        lines.push(label.to_owned());
    }
    if let Some(name) = name {
        lines.push(format!("Session: {name}"));
    }
    if let Some(session_id) = session_id {
        lines.push(format!("Id: {session_id}"));
    }
    if let Some(account) = account {
        lines.push(format!("Account: {account}"));
    }
    lines.join("\n")
}

/// What the titlebar shows for the agent of a panel: the status dot and the session name.
pub(super) struct AgentChip<'a> {
    pub(super) status: AgentStatus,
    pub(super) seen: bool,
    pub(super) session_name: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    pub(super) account: Option<&'a str>,
    name: Option<Arc<Galley>>,
    width: f32,
    /// Where the chip sits in the titlebar; set by [`Self::placed`].
    pub(super) rect: Rect,
}

/// Inputs of [`AgentChip::build`].
pub(super) struct ChipInputs<'a> {
    pub(super) status: AgentStatus,
    pub(super) seen: bool,
    pub(super) session_name: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    pub(super) account: Option<&'a str>,
    /// `false` in narrow panels: only the dot is drawn.
    pub(super) show_name: bool,
}

impl<'a> AgentChip<'a> {
    /// The chip for `inputs`, or `None` when there is nothing to show (no status and no name).
    pub(super) fn build(painter: &Painter, inputs: &ChipInputs<'a>) -> Option<Self> {
        let name_text = inputs.session_name.filter(|_| inputs.show_name);
        let has_dot = inputs.status != AgentStatus::Unknown;
        if !has_dot && name_text.is_none() {
            return None;
        }
        let name =
            name_text.map(|text| hub_ui::elided(painter, text, FontId::proportional(NAME_FONT_SIZE), NAME_MAX_WIDTH));
        let name_width = name.as_ref().map_or(0.0, |galley| galley.size().x);
        let width = chip_width(has_dot, name.is_some().then_some(name_width));
        Some(Self {
            status: inputs.status,
            seen: inputs.seen,
            session_name: inputs.session_name,
            session_id: inputs.session_id,
            account: inputs.account,
            name,
            width,
            rect: Rect::NOTHING,
        })
    }

    /// Width the chip takes in the titlebar (gap not included).
    pub(super) fn width(&self) -> f32 {
        self.width
    }

    /// The chip placed at `rect` (as computed from [`Self::width`]).
    pub(super) fn placed(mut self, rect: Rect) -> Self {
        self.rect = rect;
        self
    }

    /// Titlebar width the chip takes, gap included.
    pub(super) fn slot(&self) -> f32 {
        self.width + super::claude_quick_start::titlebar::STRIP_GAP
    }

    pub(super) fn tooltip(&self) -> String {
        chip_tooltip(self.status, self.session_name, self.session_id, self.account)
    }

    /// Height of the chip's rect.
    pub(super) const fn height() -> f32 {
        CHIP_HEIGHT
    }

    /// Paints the dot and the session name inside `self.rect`.
    pub(super) fn paint(&self, painter: &Painter, focused: bool) {
        let mut x = self.rect.min.x;
        if self.status != AgentStatus::Unknown {
            let center = Pos2::new(x + DOT_SLOT / 2.0, self.rect.center().y);
            paint_status_dot(painter, center, TITLEBAR_DOT_RADIUS, self.status, self.seen);
            x += DOT_SLOT + NAME_GAP;
        }
        if let Some(galley) = &self.name {
            let color = if focused { theme::FG_SOFT() } else { theme::FG_DIM() };
            painter.galley(
                Pos2::new(x, self.rect.center().y - galley.size().y / 2.0),
                Arc::clone(galley),
                color,
            );
        }
    }
}

/// Panels narrower than this show the status dot but not the session name.
const NAME_MIN_PANEL_WIDTH: f32 = 520.0;

impl HorizonApp {
    /// The agent chip of `panel_id` (not placed yet), or `None` when the panel has no agent
    /// status and no known session name.
    pub(super) fn agent_chip(&self, painter: &Painter, panel_id: PanelId, panel_width: f32) -> Option<AgentChip<'_>> {
        let panel = self.board.panel(panel_id)?;
        let live = self.hub.status_ui.live(panel_id);
        let info = live.and_then(|live| live.info.as_ref());
        AgentChip::build(
            painter,
            &ChipInputs {
                status: panel.agent_status(),
                seen: panel.agent_status_seen(),
                session_name: info.and_then(|info| info.name.as_deref()),
                session_id: panel
                    .agent_session_id()
                    .or_else(|| info.map(|info| info.session_id.as_str())),
                account: live.and_then(|live| live.profile.as_deref()),
                show_name: panel_width >= NAME_MIN_PANEL_WIDTH,
            },
        )
    }
}

/// Chip width for a dot slot (`has_dot`) and a name of `name_width`.
#[must_use]
fn chip_width(has_dot: bool, name_width: Option<f32>) -> f32 {
    let dot = if has_dot { DOT_SLOT } else { 0.0 };
    match name_width {
        Some(width) if has_dot => dot + NAME_GAP + width,
        Some(width) => width,
        None => dot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_dot_shape_per_state_is_distinct() {
        let shapes = [
            dot_shape(AgentStatus::Working),
            dot_shape(AgentStatus::Waiting),
            dot_shape(AgentStatus::Done),
            dot_shape(AgentStatus::Error),
        ];
        assert_eq!(
            shapes,
            [
                Some(DotShape::Ring),
                Some(DotShape::DotHalo),
                Some(DotShape::DotTick),
                Some(DotShape::DotCross),
            ]
        );
        for (index, shape) in shapes.iter().enumerate() {
            assert!(
                !shapes[index + 1..].contains(shape),
                "shapes must differ by form, not only colour"
            );
        }
        assert_eq!(dot_shape(AgentStatus::Unknown), None, "no agent paints nothing");
    }

    #[test]
    fn tick_and_cross_stay_inside_the_dot() {
        let center = Pos2::new(40.0, 20.0);
        for radius in [SIDEBAR_DOT_RADIUS, TITLEBAR_DOT_RADIUS] {
            for segment in tick_segments(center, radius)
                .into_iter()
                .chain(cross_segments(center, radius))
            {
                for point in segment {
                    assert!((point - center).length() < radius, "{point:?} outside radius {radius}");
                }
            }
        }
    }

    #[test]
    fn labels_follow_the_spec_and_unknown_is_empty() {
        assert_eq!(status_label(AgentStatus::Working), "Working\u{2026}");
        assert_eq!(status_label(AgentStatus::Waiting), "Waiting for you");
        assert_eq!(status_label(AgentStatus::Done), "Done \u{2014} ready for next prompt");
        assert_eq!(status_label(AgentStatus::Error), "Stopped with an error");
        assert_eq!(status_label(AgentStatus::Unknown), "");
    }

    #[test]
    fn seen_done_fades_and_unseen_done_does_not() {
        assert!(dot_color(AgentStatus::Done, true).a() < dot_color(AgentStatus::Done, false).a());
        assert_eq!(
            dot_color(AgentStatus::Waiting, true),
            dot_color(AgentStatus::Waiting, false)
        );
    }

    #[test]
    fn chip_width_adds_the_dot_slot_gap_and_name() {
        assert!((chip_width(true, None) - DOT_SLOT).abs() < f32::EPSILON);
        assert!((chip_width(false, Some(50.0)) - 50.0).abs() < f32::EPSILON);
        assert!((chip_width(true, Some(50.0)) - (DOT_SLOT + NAME_GAP + 50.0)).abs() < f32::EPSILON);
        assert!(chip_width(false, None).abs() < f32::EPSILON);
    }

    #[test]
    fn tooltip_lists_status_name_id_and_account() {
        assert_eq!(
            chip_tooltip(
                AgentStatus::Waiting,
                Some("expia-11"),
                Some("f054f32b"),
                Some("claude2")
            ),
            "Waiting for you\nSession: expia-11\nId: f054f32b\nAccount: claude2"
        );
        assert_eq!(chip_tooltip(AgentStatus::Unknown, Some("x"), None, None), "Session: x");
        assert_eq!(chip_tooltip(AgentStatus::Unknown, None, None, None), "");
    }

    #[test]
    fn chip_is_absent_without_status_or_name() {
        let ctx = egui::Context::default();
        let painter = egui::Painter::new(ctx, egui::LayerId::background(), Rect::EVERYTHING);
        let inputs = |status, name: Option<&'static str>, show_name| ChipInputs {
            status,
            seen: true,
            session_name: name,
            session_id: None,
            account: None,
            show_name,
        };

        assert!(AgentChip::build(&painter, &inputs(AgentStatus::Unknown, None, true)).is_none());
        assert!(AgentChip::build(&painter, &inputs(AgentStatus::Unknown, Some("n"), false)).is_none());
        let dot_only = AgentChip::build(&painter, &inputs(AgentStatus::Working, Some("n"), false)).expect("dot");
        assert!((dot_only.width() - DOT_SLOT).abs() < f32::EPSILON);
    }
}
