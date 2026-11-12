//! The "Claude" strip in the terminal titlebar: geometry, the star glyph and the busy probe.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Painter, Pos2, Rect, Stroke, Vec2};
use horizon_core::PanelId;

/// Width of the collapsed "Claude" button.
pub(in crate::app) const BUTTON_WIDTH: f32 = 72.0;
pub(super) const STRIP_HEIGHT: f32 = 20.0;
/// Gap between the strip and the history meter (or controls) to its right, and between the strip
/// and the titlebar badges/title to its left.
pub(in crate::app) const STRIP_GAP: f32 = 6.0;
/// Narrower panels would leave the title no room, so they get no button.
const MIN_PANEL_WIDTH: f32 = 440.0;
/// Room kept for the title (and the workspace accent) when the strip is open.
const TITLE_RESERVE: f32 = 64.0;
/// The strip is laid out for at least this much room, even in a very narrow panel.
const MIN_STRIP_ROOM: f32 = 150.0;
/// How long a busy probe stays valid; /proc is not read per frame.
const BUSY_TTL: Duration = Duration::from_secs(1);

/// Tooltips of the collapsed button, by availability.
pub(super) const BUSY_TOOLTIP: &str = "Busy \u{2014} finish the running command first";
pub(super) const EXITED_TOOLTIP: &str = "Shell exited \u{2014} nothing to type into";
pub(super) const READY_TOOLTIP: &str = "Start Claude in this terminal";

/// `true` when a panel this wide has room for the button.
#[must_use]
pub(in crate::app) fn panel_is_wide_enough(panel_width: f32) -> bool {
    panel_width >= MIN_PANEL_WIDTH
}

/// X of the strip's right edge: just left of the history meter, or of the leftmost control (mic,
/// collapse caret, close) when there is none; `controls_left` is that control's left edge.
#[must_use]
pub(in crate::app) fn strip_right(history_badge: Rect, has_history_meter: bool, controls_left: f32) -> f32 {
    if has_history_meter {
        history_badge.min.x - STRIP_GAP
    } else {
        controls_left - 8.0
    }
}

/// Width the open strip may use: from the title's reserve to its right edge.
#[must_use]
pub(super) fn strip_room(titlebar_rect: Rect, right: f32) -> f32 {
    (right - titlebar_rect.min.x - TITLE_RESERVE).max(MIN_STRIP_ROOM)
}

/// Strip rect: `width` wide, ending at `right`, centered in the titlebar.
#[must_use]
pub(super) fn strip_rect(titlebar_rect: Rect, right: f32, width: f32) -> Rect {
    Rect::from_min_size(
        Pos2::new(right - width, titlebar_rect.center().y - STRIP_HEIGHT * 0.5),
        Vec2::new(width, STRIP_HEIGHT),
    )
}

/// Six-spoke star, drawn as segments so it does not depend on a font glyph.
pub(super) fn paint_asterisk(painter: &Painter, center: Pos2, color: egui::Color32, radius: f32) {
    let stroke = Stroke::new(1.5_f32, color);
    for angle_deg in [0.0_f32, 60.0, 120.0] {
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let offset = Vec2::new(cos, sin) * radius;
        painter.line_segment([center - offset, center + offset], stroke);
    }
}

/// Busy probes per panel, refreshed at most once per [`BUSY_TTL`] and only for panels that ask.
#[derive(Default)]
pub(in crate::app) struct BusyCache {
    probes: HashMap<PanelId, (Instant, bool)>,
    /// Panel count at the last eviction sweep.
    swept_for: usize,
}

impl BusyCache {
    /// Whether something other than the shell holds the terminal's foreground. A fresh answer is
    /// reused; a stale one is re-probed only when `may_probe` (the panel is hovered or its strip
    /// is open), otherwise the last known answer stands (`false` when there is none).
    pub(in crate::app) fn is_busy(
        &mut self,
        panel_id: PanelId,
        now: Instant,
        may_probe: bool,
        probe: impl FnOnce() -> bool,
    ) -> bool {
        let cached = self.probes.get(&panel_id).copied();
        if let Some((at, busy)) = cached
            && now.saturating_duration_since(at) < BUSY_TTL
        {
            return busy;
        }
        if !may_probe {
            return cached.is_some_and(|(_, busy)| busy);
        }
        let busy = probe();
        self.probes.insert(panel_id, (now, busy));
        busy
    }

    /// Forgets a panel's probe (it just got a command; the next look must be fresh).
    pub(in crate::app) fn forget(&mut self, panel_id: PanelId) {
        self.probes.remove(&panel_id);
    }

    /// Drops the probes of panels that were closed; sweeps only when the panel count moved.
    pub(super) fn evict_closed(&mut self, panel_count: usize, live: impl Fn(PanelId) -> bool) {
        if self.swept_for == panel_count {
            return;
        }
        self.swept_for = panel_count;
        self.probes.retain(|panel_id, _| live(*panel_id));
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.probes.len()
    }
}

pub(in crate::app) use horizon_core::shell_is_busy;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn busy_probe_is_reused_inside_the_ttl_and_rerun_after_it() {
        let mut cache = BusyCache::default();
        let calls = Cell::new(0);
        let start = Instant::now();
        let mut ask = |at: Instant, answer: bool| {
            cache.is_busy(PanelId(1), at, true, || {
                calls.set(calls.get() + 1);
                answer
            })
        };

        assert!(ask(start, true));
        assert!(ask(start + Duration::from_millis(500), false), "cached answer wins");
        assert_eq!(calls.get(), 1);
        assert!(!ask(start + BUSY_TTL, false));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_panel_that_is_not_hovered_or_open_is_never_probed() {
        let mut cache = BusyCache::default();
        let start = Instant::now();

        assert!(!cache.is_busy(PanelId(1), start, false, || panic!("probed an idle panel")));
        assert_eq!(cache.len(), 0);
        assert!(cache.is_busy(PanelId(1), start, true, || true));
        assert!(
            cache.is_busy(PanelId(1), start + BUSY_TTL * 5, false, || panic!(
                "probed an idle panel"
            )),
            "the last known answer stands"
        );
    }

    #[test]
    fn forgetting_a_panel_forces_a_fresh_probe() {
        let mut cache = BusyCache::default();
        let now = Instant::now();
        assert!(cache.is_busy(PanelId(1), now, true, || true));

        cache.forget(PanelId(1));

        assert!(!cache.is_busy(PanelId(1), now, true, || false));
    }

    #[test]
    fn probes_of_closed_panels_are_swept_when_the_panel_count_moves() {
        let mut cache = BusyCache::default();
        let now = Instant::now();
        for id in 1..=3 {
            cache.is_busy(PanelId(id), now, true, || false);
        }

        cache.evict_closed(3, |id| id != PanelId(2));
        assert_eq!(cache.len(), 2);

        cache.is_busy(PanelId(2), now, true, || false);
        cache.evict_closed(3, |id| id != PanelId(2));
        assert_eq!(cache.len(), 3, "no sweep while the count is unchanged");
        cache.evict_closed(2, |id| id != PanelId(2));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn strip_ends_left_of_the_history_meter_inside_the_titlebar() {
        let titlebar = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(600.0, 34.0));
        let history = Rect::from_min_size(Pos2::new(400.0, 7.0), Vec2::new(96.0, 20.0));
        let controls = Rect::from_min_size(Pos2::new(560.0, 9.0), Vec2::splat(16.0));

        let right = strip_right(history, true, controls.min.x);
        assert!((right - (history.min.x - STRIP_GAP)).abs() < 1e-4);
        let rect = strip_rect(titlebar, right, 200.0);
        assert!((rect.max.x - right).abs() < 1e-4);
        assert!((rect.width() - 200.0).abs() < 1e-4);
        assert!((rect.center().y - titlebar.center().y).abs() < 1e-4);
        assert!(titlebar.contains_rect(rect));

        let without = strip_right(history, false, controls.min.x);
        assert!((without - (controls.min.x - 8.0)).abs() < 1e-4);
    }

    #[test]
    fn open_strip_leaves_the_title_its_reserve_but_never_less_than_the_floor() {
        let titlebar = Rect::from_min_max(Pos2::new(100.0, 0.0), Pos2::new(700.0, 34.0));

        assert!((strip_room(titlebar, 500.0) - (500.0 - 100.0 - TITLE_RESERVE)).abs() < 1e-4);
        assert!((strip_room(titlebar, 150.0) - MIN_STRIP_ROOM).abs() < 1e-4);
    }

    #[test]
    fn narrow_panels_get_no_button() {
        assert!(panel_is_wide_enough(440.0));
        assert!(!panel_is_wide_enough(439.0));
    }
}
