//! Pure decision logic for the auto-hiding workspace sidebar.
//!
//! Kept egui-free so the reveal/collapse policy can be unit-tested in isolation
//! from rendering. The runtime rendering in [`super`] reads these constants and
//! calls [`sidebar_reveal_state`].

use std::time::{Duration, Instant};

/// Delay before the auto-hiding sidebar collapses after the pointer leaves it.
pub(super) const SIDEBAR_AUTO_HIDE_DELAY: Duration = Duration::from_millis(1500);
/// Width of the thin handle shown when the auto-hiding sidebar is collapsed.
pub(super) const SIDEBAR_STRIP_WIDTH: f32 = 6.0;
/// Width of the pointer-reachable region used to re-reveal a collapsed sidebar.
pub(super) const SIDEBAR_STRIP_HOVER_WIDTH: f32 = 10.0;

/// How long a rename hosted by the sidebar keeps it open without any pointer on the sidebar or
/// typing. A rename nobody finishes (label scrolled away, user walked off) must not pin the
/// sidebar for good: past this the rename is abandoned and auto-hide resumes.
pub(super) const RENAME_PIN_IDLE: Duration = Duration::from_secs(30);

/// Auto-hide bookkeeping: last pointer hover and whether the sidebar was fully revealed.
#[derive(Default)]
pub(in crate::app) struct SidebarAutoHideState {
    pub(super) last_hover: Option<Instant>,
    pub(super) revealed: bool,
    /// Last moment the sidebar-hosted rename saw activity (pointer on the sidebar or typing);
    /// `None` while no rename is hosted there.
    pub(super) rename_pin_anchor: Option<Instant>,
}

/// What to do with a sidebar-hosted rename this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenamePin {
    /// Keep the sidebar open; the new activity anchor.
    Hold(Instant),
    /// Idle for too long: cancel the rename so the sidebar can hide again.
    Abandon,
}

/// Pure step of the rename pin: `activity` (pointer on the sidebar, typing) renews it, a first
/// frame starts it, and [`RENAME_PIN_IDLE`] without activity abandons the rename.
pub(super) fn rename_pin_step(anchor: Option<Instant>, activity: bool, now: Instant) -> RenamePin {
    match anchor {
        Some(at) if !activity => {
            if now.saturating_duration_since(at) >= RENAME_PIN_IDLE {
                RenamePin::Abandon
            } else {
                RenamePin::Hold(at)
            }
        }
        _ => RenamePin::Hold(now),
    }
}

/// Visibility outcome for the auto-hiding sidebar. Pure, egui-free for testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarReveal {
    /// Render the sidebar at full width with its normal contents.
    FullVisible,
    /// Render only the thin edge handle.
    ThinStrip,
}

/// Horizontal space the sidebar reserves from the canvas on the left edge.
///
/// Pure, egui-free so the canvas-layout seam can be unit-tested:
/// - sidebar hidden entirely => `0.0` (canvas owns the full viewport);
/// - auto-hide off, or auto-hide on but currently revealed => the full
///   `sidebar_width` (canvas starts after the sidebar, unchanged behavior);
/// - auto-hide on and collapsed to the strip => only [`SIDEBAR_STRIP_WIDTH`],
///   so the reclaimed band becomes usable canvas.
pub(super) fn reserved_sidebar_width(visible: bool, auto_hide: bool, revealed: bool, sidebar_width: f32) -> f32 {
    if !visible {
        return 0.0;
    }
    if auto_hide && !revealed {
        return SIDEBAR_STRIP_WIDTH;
    }
    sidebar_width
}

/// Decide whether the sidebar should be fully visible or collapsed to a strip.
///
/// Pure decision used by both runtime rendering and unit tests:
/// - auto-hide off => always [`SidebarReveal::FullVisible`] (unchanged behavior);
/// - pointer currently over the sidebar/strip => full;
/// - pointer left less than `delay` ago => full (grace period still revealed);
/// - pointer left `delay` or more ago (or never hovered) => thin strip.
pub(super) fn sidebar_reveal_state(
    auto_hide: bool,
    pointer_over: bool,
    since_left: Option<Duration>,
    delay: Duration,
) -> SidebarReveal {
    if !auto_hide {
        return SidebarReveal::FullVisible;
    }
    if pointer_over {
        return SidebarReveal::FullVisible;
    }
    match since_left {
        Some(elapsed) if elapsed < delay => SidebarReveal::FullVisible,
        _ => SidebarReveal::ThinStrip,
    }
}

#[cfg(test)]
// Exact float equality is intended here: the widths asserted are constants
// passed through `reserved_sidebar_width` unchanged, with no arithmetic that
// could introduce rounding error.
#[allow(clippy::float_cmp)]
mod tests {
    use std::time::Duration;

    use std::time::Instant;

    use super::{
        RENAME_PIN_IDLE, RenamePin, SIDEBAR_AUTO_HIDE_DELAY, SIDEBAR_STRIP_WIDTH, SidebarReveal, rename_pin_step,
        reserved_sidebar_width, sidebar_reveal_state,
    };

    #[test]
    fn rename_pin_starts_on_the_first_frame_and_holds_while_idle_is_short() {
        let start = Instant::now();

        assert_eq!(rename_pin_step(None, false, start), RenamePin::Hold(start));
        let later = start + RENAME_PIN_IDLE / 2;
        assert_eq!(rename_pin_step(Some(start), false, later), RenamePin::Hold(start));
    }

    #[test]
    fn rename_pin_activity_renews_the_anchor() {
        let start = Instant::now();
        let later = start + RENAME_PIN_IDLE * 2;

        assert_eq!(rename_pin_step(Some(start), true, later), RenamePin::Hold(later));
    }

    #[test]
    fn rename_pin_is_abandoned_after_the_idle_limit_without_activity() {
        let start = Instant::now();

        assert_eq!(
            rename_pin_step(
                Some(start),
                false,
                start + RENAME_PIN_IDLE - std::time::Duration::from_millis(1)
            ),
            RenamePin::Hold(start)
        );
        assert_eq!(
            rename_pin_step(Some(start), false, start + RENAME_PIN_IDLE),
            RenamePin::Abandon
        );
    }

    #[test]
    fn reserved_width_zero_when_sidebar_hidden() {
        assert_eq!(reserved_sidebar_width(false, false, false, 280.0), 0.0);
        assert_eq!(reserved_sidebar_width(false, true, false, 280.0), 0.0);
    }

    #[test]
    fn reserved_width_full_when_auto_hide_off() {
        // Auto-hide off keeps the full sidebar reserved regardless of `revealed`.
        assert_eq!(reserved_sidebar_width(true, false, false, 280.0), 280.0);
        assert_eq!(reserved_sidebar_width(true, false, true, 280.0), 280.0);
    }

    #[test]
    fn reserved_width_full_when_auto_hide_revealed() {
        assert_eq!(reserved_sidebar_width(true, true, true, 280.0), 280.0);
    }

    #[test]
    fn reserved_width_strip_when_auto_hide_collapsed() {
        assert_eq!(reserved_sidebar_width(true, true, false, 280.0), SIDEBAR_STRIP_WIDTH);
    }

    #[test]
    fn sidebar_reveal_always_full_when_auto_hide_off() {
        assert_eq!(
            sidebar_reveal_state(false, false, None, SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::FullVisible
        );
        // Even with a long-elapsed timer, auto-hide off stays full.
        assert_eq!(
            sidebar_reveal_state(false, false, Some(Duration::from_secs(60)), SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::FullVisible
        );
    }

    #[test]
    fn sidebar_reveal_full_while_pointer_over() {
        assert_eq!(
            sidebar_reveal_state(true, true, None, SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::FullVisible
        );
    }

    #[test]
    fn sidebar_reveal_full_during_grace_period() {
        assert_eq!(
            sidebar_reveal_state(true, false, Some(Duration::from_millis(500)), SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::FullVisible
        );
    }

    #[test]
    fn sidebar_reveal_collapses_after_delay() {
        assert_eq!(
            sidebar_reveal_state(true, false, Some(SIDEBAR_AUTO_HIDE_DELAY), SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::ThinStrip
        );
        assert_eq!(
            sidebar_reveal_state(true, false, Some(Duration::from_secs(10)), SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::ThinStrip
        );
    }

    #[test]
    fn sidebar_reveal_collapses_when_never_hovered() {
        assert_eq!(
            sidebar_reveal_state(true, false, None, SIDEBAR_AUTO_HIDE_DELAY),
            SidebarReveal::ThinStrip
        );
    }
}
