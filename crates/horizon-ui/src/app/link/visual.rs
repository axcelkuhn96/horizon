//! The transient line drawn between two panels when they talk over the link: geometry
//! (pure) and painting. Visual spec: `docs/design/claude-hub-ui.md` §13.

use std::time::{Duration, Instant};

use egui::emath::TSTransform;
use egui::{Color32, Context, Id, Order, Painter, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use horizon_core::{PanelId, WorkspaceId};

use super::super::HorizonApp;
use super::{BADGE_FULL, badge_opacity, badge_wake};
use crate::theme;

/// Lines painted at once; the rest are dropped.
const MAX_LINES: usize = 8;
/// Transient lines kept (one per recent message); older ones drop first.
const MAX_TRANSIENT: usize = 32;
const STROKE_WIDTH: f32 = 1.5;
const LINE_ALPHA: u8 = 170;
const ARROW_SIZE: f32 = 6.0;
const DASH: f32 = 6.0;
const GAP: f32 = 4.0;
const CURVE_SAMPLES: u8 = 24;
/// Share of the horizontal distance the Bezier controls are pulled by.
const CONTROL_PULL: f32 = 0.4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum LinkVisualKind {
    Send,
    Ask,
    Reply,
}

/// A message that just went from one panel to another. Ask lines that are still waiting
/// are not stored: they come from the pending-ask table.
#[derive(Debug, Clone, Copy)]
pub(in crate::app) struct LinkVisual {
    pub(in crate::app) from: PanelId,
    pub(in crate::app) to: PanelId,
    pub(in crate::app) started: Instant,
    pub(in crate::app) kind: LinkVisualKind,
}

impl LinkVisual {
    /// A line that is already in its fade (a resolved ask goes away in 0.7 s).
    pub(in crate::app) fn fading(from: PanelId, to: PanelId, now: Instant) -> Self {
        Self {
            from,
            to,
            started: now.checked_sub(BADGE_FULL).unwrap_or(now),
            kind: LinkVisualKind::Ask,
        }
    }
}

/// Opacity of a transient line: the badge timeline (opaque 2.3 s, fade to 3 s).
#[must_use]
fn visual_alpha(visual: &LinkVisual, now: Instant) -> f32 {
    badge_opacity(visual.started, now)
}

/// Pushes `visual`, dropping the oldest beyond [`MAX_TRANSIENT`].
pub(in crate::app) fn push_visual(visuals: &mut Vec<LinkVisual>, visual: LinkVisual) {
    if visuals.len() >= MAX_TRANSIENT {
        visuals.remove(0);
    }
    visuals.push(visual);
}

/// Drops finished lines; returns when the UI must wake next for the fades.
pub(in crate::app) fn prune_visuals(visuals: &mut Vec<LinkVisual>, now: Instant) -> Option<Duration> {
    visuals.retain(|visual| badge_wake(visual.started, now).is_some());
    visuals
        .iter()
        .filter_map(|visual| badge_wake(visual.started, now))
        .min()
}

/// One line to paint: panel pair, opacity, and whether it is a waiting ask.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LineSpec {
    pub(super) from: PanelId,
    pub(super) to: PanelId,
    pub(super) alpha: f32,
    pub(super) kind: LinkVisualKind,
    pub(super) pending: bool,
}

/// Waiting asks first (they stay until resolved), then the newest transient lines, at most
/// [`MAX_LINES`] in all.
#[must_use]
pub(super) fn select_lines(
    transient: &[LinkVisual],
    pending: impl Iterator<Item = (PanelId, PanelId)>,
    now: Instant,
) -> Vec<LineSpec> {
    let mut lines: Vec<LineSpec> = pending
        .map(|(from, to)| LineSpec {
            from,
            to,
            alpha: 1.0,
            kind: LinkVisualKind::Ask,
            pending: true,
        })
        .take(MAX_LINES)
        .collect();
    for visual in transient.iter().rev() {
        if lines.len() >= MAX_LINES {
            break;
        }
        let alpha = visual_alpha(visual, now);
        let duplicates_pending = lines
            .iter()
            .any(|line| line.pending && line.from == visual.from && line.to == visual.to);
        if alpha > 0.0 && !duplicates_pending {
            lines.push(LineSpec {
                from: visual.from,
                to: visual.to,
                alpha,
                kind: visual.kind,
                pending: false,
            });
        }
    }
    lines
}

/// Where the segment from the centre of `rect` towards `toward` leaves `rect`.
#[must_use]
pub(super) fn edge_point(rect: Rect, toward: Pos2) -> Pos2 {
    let center = rect.center();
    let delta = toward - center;
    if delta == Vec2::ZERO {
        return center;
    }
    let half = rect.size() / 2.0;
    let scale_x = if delta.x == 0.0 {
        f32::INFINITY
    } else {
        half.x / delta.x.abs()
    };
    let scale_y = if delta.y == 0.0 {
        f32::INFINITY
    } else {
        half.y / delta.y.abs()
    };
    center + delta * scale_x.min(scale_y)
}

/// Curve between the facing edges of two panels: `(start, control1, control2, end)`.
/// `None` when the panels overlap (there is no gap to draw in).
#[must_use]
pub(super) fn link_curve(from: Rect, to: Rect) -> Option<[Pos2; 4]> {
    if from.intersects(to) {
        return None;
    }
    let start = edge_point(from, to.center());
    let end = edge_point(to, from.center());
    let pull = (end.x - start.x) * CONTROL_PULL;
    Some([
        start,
        Pos2::new(start.x + pull, start.y),
        Pos2::new(end.x - pull, end.y),
        end,
    ])
}

fn bezier_point(curve: &[Pos2; 4], t: f32) -> Pos2 {
    let inverse = 1.0 - t;
    let weights = [
        inverse * inverse * inverse,
        3.0 * inverse * inverse * t,
        3.0 * inverse * t * t,
        t * t * t,
    ];
    let mut point = Vec2::ZERO;
    for (control, weight) in curve.iter().zip(weights) {
        point += control.to_vec2() * weight;
    }
    point.to_pos2()
}

fn curve_points(curve: &[Pos2; 4]) -> Vec<Pos2> {
    (0..=CURVE_SAMPLES)
        .map(|step| bezier_point(curve, f32::from(step) / f32::from(CURVE_SAMPLES)))
        .collect()
}

fn line_color(kind: LinkVisualKind, alpha: f32) -> Color32 {
    let base = match kind {
        LinkVisualKind::Reply => theme::PALETTE_GREEN(),
        LinkVisualKind::Send | LinkVisualKind::Ask => theme::ACCENT(),
    };
    theme::alpha(base, LINE_ALPHA).gamma_multiply(alpha.clamp(0.0, 1.0))
}

fn paint_line(painter: &Painter, curve: &[Pos2; 4], spec: &LineSpec) {
    let color = line_color(spec.kind, spec.alpha);
    let stroke = Stroke::new(STROKE_WIDTH, color);
    let points = curve_points(curve);
    if spec.pending {
        painter.extend(Shape::dashed_line(&points, stroke, DASH, GAP));
    } else {
        painter.add(Shape::line(points, stroke));
    }
    let end = curve[3];
    let tangent = end - curve[2];
    let direction = if tangent == Vec2::ZERO {
        Vec2::X
    } else {
        tangent.normalized()
    };
    let side = Vec2::new(-direction.y, direction.x);
    let back = end - direction * ARROW_SIZE;
    painter.add(Shape::convex_polygon(
        vec![end, back + side * (ARROW_SIZE * 0.5), back - side * (ARROW_SIZE * 0.5)],
        color,
        Stroke::NONE,
    ));
}

impl HorizonApp {
    /// Paints the link lines of the main canvas, above the workspace backgrounds and below
    /// the panels. Both ends must be in the same workspace; other pairs are not drawn.
    pub(in crate::app) fn render_link_visuals(
        &self,
        ctx: &Context,
        canvas_transform: TSTransform,
        canvas_clip_rect: Rect,
    ) {
        if self.hub.link_visuals.is_empty() && self.hub.link_asks.is_empty() {
            return;
        }
        let specs = select_lines(&self.hub.link_visuals, self.hub.link_asks.pairs(), Instant::now());
        let curves: Vec<([Pos2; 4], LineSpec)> = specs
            .into_iter()
            .filter_map(|spec| {
                let (from, workspace) = self.panel_canvas_rect(spec.from)?;
                let (to, other) = self.panel_canvas_rect(spec.to)?;
                (workspace == other && !self.workspace_is_detached(workspace))
                    .then(|| link_curve(from, to))
                    .flatten()
                    .map(|curve| (curve, spec))
            })
            .collect();
        if curves.is_empty() {
            return;
        }
        egui::Area::new(Id::new("link_visuals"))
            .fixed_pos(Pos2::ZERO)
            .constrain(false)
            .interactable(false)
            .order(Order::Background)
            .show(ctx, |ui| {
                ui.ctx().set_transform_layer(ui.layer_id(), canvas_transform);
                ui.set_clip_rect(canvas_clip_rect);
                let _ = ui.allocate_exact_size(Vec2::ZERO, Sense::hover());
                for (curve, spec) in &curves {
                    paint_line(ui.painter(), curve, spec);
                }
            });
    }

    fn panel_canvas_rect(&self, id: PanelId) -> Option<(Rect, WorkspaceId)> {
        let panel = self.board.panel(id)?;
        let layout = panel.layout;
        let min = Pos2::new(layout.position[0], layout.position[1]);
        Some((
            Rect::from_min_size(min, Vec2::new(layout.size[0], layout.size[1])),
            panel.workspace_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
    }

    #[test]
    fn link_visual_endpoints_clip_to_panel_edges() {
        let left = rect(0.0, 0.0, 100.0, 100.0);
        let right = rect(300.0, 0.0, 100.0, 100.0);
        let [start, _, _, end] = link_curve(left, right).expect("gap between panels");
        assert!(
            (start.x - 100.0).abs() < 1e-3 && (start.y - 50.0).abs() < 1e-3,
            "{start:?}"
        );
        assert!((end.x - 300.0).abs() < 1e-3 && (end.y - 50.0).abs() < 1e-3, "{end:?}");

        // Diagonal neighbour: the point lies on the border, not at the centre.
        let below = rect(300.0, 300.0, 100.0, 100.0);
        let [start, _, _, end] = link_curve(left, below).expect("curve");
        assert!(
            left.expand(1e-3).contains(start) && !left.shrink(1.0).contains(start),
            "{start:?}"
        );
        assert!(
            below.expand(1e-3).contains(end) && !below.shrink(1.0).contains(end),
            "{end:?}"
        );

        // Stacked vertically: leaves through the bottom edge, enters through the top.
        let above = rect(0.0, 0.0, 100.0, 100.0);
        let under = rect(0.0, 250.0, 100.0, 100.0);
        let [start, _, _, end] = link_curve(above, under).expect("curve");
        assert!((start.y - 100.0).abs() < 1e-3 && (end.y - 250.0).abs() < 1e-3);
    }

    #[test]
    fn overlapping_panels_have_no_line() {
        assert!(link_curve(rect(0.0, 0.0, 100.0, 100.0), rect(50.0, 50.0, 100.0, 100.0)).is_none());
    }

    #[test]
    fn controls_are_pulled_horizontally_by_forty_percent() {
        let [start, c1, c2, end] = link_curve(rect(0.0, 0.0, 100.0, 100.0), rect(300.0, 0.0, 100.0, 100.0)).unwrap();
        assert!((c1.x - (start.x + 0.4 * (end.x - start.x))).abs() < 1e-3);
        assert!((c2.x - (end.x - 0.4 * (end.x - start.x))).abs() < 1e-3);
        assert!((c1.y - start.y).abs() < 1e-3 && (c2.y - end.y).abs() < 1e-3);
    }

    fn visual(kind: LinkVisualKind, started: Instant) -> LinkVisual {
        LinkVisual {
            from: PanelId(1),
            to: PanelId(2),
            started,
            kind,
        }
    }

    #[test]
    fn link_visual_alpha_timeline() {
        let start = Instant::now();
        let line = visual(LinkVisualKind::Send, start);
        assert!((visual_alpha(&line, start) - 1.0).abs() < 1e-6);
        assert!((visual_alpha(&line, start + Duration::from_millis(2300)) - 1.0).abs() < 1e-6);
        let mid = visual_alpha(&line, start + Duration::from_millis(2650));
        assert!((mid - 0.5).abs() < 1e-3, "{mid}");
        assert!(visual_alpha(&line, start + Duration::from_secs(3)).abs() < 1e-6);
    }

    #[test]
    fn a_resolved_ask_fades_out_in_under_a_second() {
        let now = Instant::now();
        let line = LinkVisual::fading(PanelId(1), PanelId(2), now);
        assert!((visual_alpha(&line, now) - 1.0).abs() < 1e-6);
        assert!(visual_alpha(&line, now + Duration::from_millis(700)).abs() < 1e-3);
    }

    #[test]
    fn pending_asks_stay_opaque_and_come_first() {
        let now = Instant::now();
        let old = visual(
            LinkVisualKind::Send,
            now.checked_sub(Duration::from_secs(10)).unwrap_or(now),
        );
        let lines = select_lines(
            &[old],
            [(PanelId(7), PanelId(8))].into_iter(),
            now + Duration::from_secs(100),
        );
        assert_eq!(lines.len(), 1, "the finished transient line is gone");
        assert!(lines[0].pending && (lines[0].alpha - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lines_are_capped_at_eight_with_pending_first() {
        let now = Instant::now();
        let transient: Vec<LinkVisual> = (0..20)
            .map(|index| LinkVisual {
                from: PanelId(100 + index),
                to: PanelId(200),
                started: now,
                kind: LinkVisualKind::Send,
            })
            .collect();
        let pending = (0..3).map(|index| (PanelId(index), PanelId(50)));
        let lines = select_lines(&transient, pending, now);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines.iter().filter(|line| line.pending).count(), 3);
        assert_eq!(lines[3].from, PanelId(119), "newest transient first");
    }

    #[test]
    fn a_pending_ask_hides_the_same_pair_transient() {
        let now = Instant::now();
        let lines = select_lines(
            &[visual(LinkVisualKind::Ask, now)],
            [(PanelId(1), PanelId(2))].into_iter(),
            now,
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn prune_drops_finished_lines_and_reports_the_next_wake() {
        let start = Instant::now();
        let mut lines = vec![visual(LinkVisualKind::Send, start)];
        push_visual(
            &mut lines,
            visual(LinkVisualKind::Reply, start + Duration::from_secs(2)),
        );
        let wake = prune_visuals(&mut lines, start + Duration::from_millis(3100));
        assert_eq!(lines.len(), 1);
        assert_eq!(wake, Some(Duration::from_millis(1200)));
        assert_eq!(prune_visuals(&mut lines, start + Duration::from_secs(10)), None);
        assert!(lines.is_empty());
    }

    #[test]
    fn transient_lines_are_bounded() {
        let mut lines = Vec::new();
        let now = Instant::now();
        for _ in 0..(MAX_TRANSIENT + 5) {
            push_visual(&mut lines, visual(LinkVisualKind::Send, now));
        }
        assert_eq!(lines.len(), MAX_TRANSIENT);
    }
}
