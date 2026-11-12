//! Pure decision logic for the auto-hiding minimap.
//!
//! Egui-free so the reveal policy can be unit-tested in isolation from
//! rendering. Mirrors the shape of the sidebar auto-hide (`SidebarAutoHideState`
//! plus a pure decision), with the minimap's own timings.

use std::time::{Duration, Instant};

/// Delay before the auto-hiding minimap fades out after the pointer leaves its zone.
pub(super) const MINIMAP_AUTO_HIDE_DELAY: Duration = Duration::from_millis(600);
/// How far past the minimap rectangle the pointer still counts as "near it".
pub(super) const MINIMAP_REVEAL_ZONE_PAD: f32 = 16.0;
/// Fade-in duration, seconds.
pub(super) const MINIMAP_FADE_IN_SECS: f32 = 0.12;
/// Fade-out duration, seconds.
pub(super) const MINIMAP_FADE_OUT_SECS: f32 = 0.16;
/// Length of the hit-zone marker drawn in the corner while the minimap is hidden.
pub(super) const MINIMAP_HINT_LENGTH: f32 = 48.0;
/// Thickness of the hit-zone marker.
pub(super) const MINIMAP_HINT_THICKNESS: f32 = 2.0;

/// Auto-hide bookkeeping for one minimap overlay.
#[derive(Debug, Default)]
pub(in crate::app) struct MinimapAutoHide {
    revealed: bool,
    last_hover: Option<Instant>,
    /// Whether the pointer was in the zone on the previous frame, so the exit
    /// frame can stamp `last_hover` and start the grace period on departure.
    was_in_zone: bool,
    /// Manual visibility on the previous frame; `None` before the first frame.
    was_manual_visible: Option<bool>,
}

impl MinimapAutoHide {
    /// Whether the minimap was shown on the previous frame.
    pub(super) fn revealed(&self) -> bool {
        self.revealed
    }

    /// Advances the state for one frame and returns whether the minimap should be shown.
    ///
    /// `pointer_in_zone` must already include "dragging or clicking inside the
    /// minimap" so interaction keeps it open.
    pub(super) fn observe(&mut self, manual_visible: bool, enabled: bool, pointer_in_zone: bool, now: Instant) -> bool {
        // Manually showing the minimap flashes it for the grace period so the
        // user sees what the toggle did even with the pointer elsewhere.
        if manual_visible && enabled && self.was_manual_visible == Some(false) {
            self.revealed = true;
            self.last_hover = Some(now);
        }
        self.was_manual_visible = Some(manual_visible);
        // Stamp on the exit frame too: the grace period runs from the moment
        // the pointer leaves, not from the last frame it was seen inside.
        if (pointer_in_zone || self.was_in_zone) && manual_visible {
            self.last_hover = Some(now);
        }
        self.was_in_zone = pointer_in_zone;
        let reveal = minimap_shown(manual_visible, enabled, self, pointer_in_zone, now);
        self.revealed = reveal;
        reveal
    }

    /// Time left before a revealed minimap hides, `None` when no timer is running.
    pub(super) fn hide_in(&self, pointer_in_zone: bool, now: Instant) -> Option<Duration> {
        if !self.revealed || pointer_in_zone {
            return None;
        }
        let elapsed = now.duration_since(self.last_hover?);
        Some(MINIMAP_AUTO_HIDE_DELAY.saturating_sub(elapsed))
    }
}

/// Reveal decision for the auto-hide policy alone.
///
/// - disabled: always shown;
/// - pointer in the zone: shown;
/// - otherwise shown only while it was shown last frame and the pointer left
///   less than [`MINIMAP_AUTO_HIDE_DELAY`] ago.
pub(super) fn should_reveal(enabled: bool, state: &MinimapAutoHide, pointer_in_zone: bool, now: Instant) -> bool {
    if !enabled || pointer_in_zone {
        return true;
    }
    state.revealed
        && state
            .last_hover
            .is_some_and(|hover| now.duration_since(hover) < MINIMAP_AUTO_HIDE_DELAY)
}

/// Whether the minimap is drawn: the manual toggle (`toggle_minimap`) is the
/// master switch, so a manually hidden minimap never reveals on hover.
pub(super) fn minimap_shown(
    manual_visible: bool,
    enabled: bool,
    state: &MinimapAutoHide,
    pointer_in_zone: bool,
    now: Instant,
) -> bool {
    manual_visible && should_reveal(enabled, state, pointer_in_zone, now)
}

/// Opacity the fade animates toward.
pub(super) fn minimap_alpha_target(reveal: bool) -> f32 {
    if reveal { 1.0 } else { 0.0 }
}

/// Fade duration in seconds for the given direction.
pub(super) fn minimap_fade_secs(reveal: bool) -> f32 {
    if reveal {
        MINIMAP_FADE_IN_SECS
    } else {
        MINIMAP_FADE_OUT_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revealed_state(last_hover: Instant) -> MinimapAutoHide {
        MinimapAutoHide {
            revealed: true,
            last_hover: Some(last_hover),
            ..MinimapAutoHide::default()
        }
    }

    #[test]
    fn should_reveal_always_true_when_disabled() {
        let now = Instant::now();

        assert!(should_reveal(false, &MinimapAutoHide::default(), false, now));
    }

    #[test]
    fn should_reveal_on_hover_zone() {
        let now = Instant::now();

        assert!(should_reveal(true, &MinimapAutoHide::default(), true, now));
    }

    #[test]
    fn should_stay_hidden_without_hover() {
        let now = Instant::now();

        assert!(!should_reveal(true, &MinimapAutoHide::default(), false, now));
    }

    #[test]
    fn should_hide_after_600ms_outside_zone() {
        let left_at = Instant::now();
        let state = revealed_state(left_at);

        assert!(should_reveal(true, &state, false, left_at + Duration::from_millis(599)));
        assert!(!should_reveal(
            true,
            &state,
            false,
            left_at + Duration::from_millis(600)
        ));
    }

    #[test]
    fn manual_hidden_overrides_hover() {
        let now = Instant::now();

        assert!(!minimap_shown(false, true, &MinimapAutoHide::default(), true, now));
        assert!(!minimap_shown(false, false, &MinimapAutoHide::default(), false, now));
        assert!(minimap_shown(true, true, &MinimapAutoHide::default(), true, now));
    }

    #[test]
    fn observe_never_reveals_a_manually_hidden_minimap() {
        let now = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(!state.observe(false, true, true, now));
        assert!(!state.revealed());
        assert_eq!(state.hide_in(true, now), None);
    }

    #[test]
    fn observe_tracks_reveal_and_hide_timer() {
        let start = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(state.observe(true, true, true, start));
        assert!(state.revealed());
        assert_eq!(state.hide_in(true, start), None);

        // The exit frame stamps the hover, so the grace period starts here.
        let left = start + Duration::from_millis(200);
        assert!(state.observe(true, true, false, left));
        assert_eq!(state.hide_in(false, left), Some(MINIMAP_AUTO_HIDE_DELAY));

        let mid = left + Duration::from_millis(200);
        assert!(state.observe(true, true, false, mid));
        assert_eq!(state.hide_in(false, mid), Some(Duration::from_millis(400)));

        let expired = left + Duration::from_millis(700);
        assert!(!state.observe(true, true, false, expired));
        assert_eq!(state.hide_in(false, expired), None);
    }

    #[test]
    fn grace_period_starts_when_the_pointer_leaves_not_at_the_last_frame_inside() {
        let start = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(state.observe(true, true, true, start));
        // A long frame gap while hovering must not eat into the grace period.
        let left = start + Duration::from_millis(5000);
        assert!(state.observe(true, true, false, left));
        assert!(state.observe(true, true, false, left + Duration::from_millis(599)));
        assert!(!state.observe(true, true, false, left + Duration::from_millis(600)));
    }

    #[test]
    fn manual_show_flashes_the_minimap_for_the_grace_period() {
        let start = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(!state.observe(false, true, false, start));
        let shown = start + Duration::from_millis(100);
        assert!(state.observe(true, true, false, shown));
        assert!(state.revealed());
        assert_eq!(state.hide_in(false, shown), Some(MINIMAP_AUTO_HIDE_DELAY));
        assert!(state.observe(true, true, false, shown + Duration::from_millis(599)));
        assert!(!state.observe(true, true, false, shown + Duration::from_millis(600)));
    }

    #[test]
    fn first_frame_with_a_visible_minimap_does_not_flash() {
        let now = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(!state.observe(true, true, false, now));
    }

    #[test]
    fn disabled_auto_hide_has_no_timer() {
        let now = Instant::now();
        let mut state = MinimapAutoHide::default();

        assert!(state.observe(true, false, false, now));
        assert_eq!(state.hide_in(false, now), None);
    }

    #[test]
    fn alpha_target_and_fade_follow_direction() {
        assert!((minimap_alpha_target(true) - 1.0).abs() < f32::EPSILON);
        assert!(minimap_alpha_target(false).abs() < f32::EPSILON);
        assert!(minimap_fade_secs(true) < minimap_fade_secs(false));
    }
}
