//! Pure decision logic for the auto-hiding workspace sidebar.
//!
//! Kept egui-free so the reveal/collapse policy can be unit-tested in isolation
//! from rendering. The runtime rendering in [`super`] reads these constants and
//! calls [`sidebar_reveal_state`].

use std::time::Duration;

/// Delay before the auto-hiding sidebar collapses after the pointer leaves it.
pub(super) const SIDEBAR_AUTO_HIDE_DELAY: Duration = Duration::from_millis(1500);
/// Width of the thin handle shown when the auto-hiding sidebar is collapsed.
pub(super) const SIDEBAR_STRIP_WIDTH: f32 = 6.0;
/// Width of the pointer-reachable region used to re-reveal a collapsed sidebar.
pub(super) const SIDEBAR_STRIP_HOVER_WIDTH: f32 = 10.0;

/// Visibility outcome for the auto-hiding sidebar. Pure, egui-free for testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarReveal {
    /// Render the sidebar at full width with its normal contents.
    FullVisible,
    /// Render only the thin edge handle.
    ThinStrip,
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
mod tests {
    use std::time::Duration;

    use super::{SIDEBAR_AUTO_HIDE_DELAY, SidebarReveal, sidebar_reveal_state};

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
