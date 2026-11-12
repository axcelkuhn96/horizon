//! Weekly-usage alert toasts: an account crossing 80% / 90% of its weekly limit raises one toast
//! per crossing (the rules are `horizon_core::claude_usage::AlertTracker`). Fed from the place
//! that already takes the poller's snapshots; there is no extra poller or timer.
//! Visual spec: `docs/design/claude-hub-ui.md` §12.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use horizon_core::claude_usage::{AlertLevel, ProfileUsage, ProfileUsageView, UsageState};

use super::hub_state::HubState;
use super::hub_toast::{Toast, ToastKind};
use super::usage_chips::{format_clock, local_offset_secs};

/// The toast for `name` having reached `level`. `pct` is the used share of the weekly window and
/// `resets_at` the already formatted local reset time (`Mon 14:00`), when known. 80% is a
/// warning, 90% an error.
#[must_use]
pub(super) fn alert_toast(level: AlertLevel, name: &str, pct: f32, resets_at: Option<&str>) -> Toast {
    let name: String = name.chars().filter(|ch| !ch.is_control()).collect();
    let shown = pct.clamp(0.0, 100.0);
    let mut text = format!("{name} at {shown:.0}% of the weekly limit");
    if let Some(clock) = resets_at {
        let _ = write!(text, " \u{2014} resets {clock}");
    }
    Toast {
        kind: match level {
            AlertLevel::Warn80 => ToastKind::Warn,
            AlertLevel::Crit90 => ToastKind::Error,
        },
        text,
        panel: None,
    }
}

impl HubState {
    /// Looks at a fresh snapshot and queues a toast for every account that just crossed a
    /// threshold. Accounts whose login is not usable are ignored (neither alert nor re-arm), as
    /// are weekly figures of a window that already reset.
    pub(super) fn observe_usage_alerts(&mut self, snapshot: &[ProfileUsage], now: SystemTime) {
        for usage in snapshot {
            let view = ProfileUsageView::new(&usage.name, 0, Some(usage), now);
            let Some(pct) = view.weekly_pct.filter(|_| view.state == UsageState::Ok) else {
                continue;
            };
            if let Some(level) = self.alerts.observe(&usage.name, Some(pct), view.resets_at) {
                let clock = view
                    .resets_at
                    .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                    .and_then(|since| i64::try_from(since.as_secs()).ok())
                    .map(|unix| format_clock(unix, local_offset_secs(), true));
                self.toasts.push(alert_toast(level, &usage.name, pct, clock.as_deref()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use horizon_core::claude_usage::UsageWindow;

    use super::*;

    #[test]
    fn alert_toast_text_formats_level_and_reset() {
        let warn = alert_toast(AlertLevel::Warn80, "conta1", 80.4, Some("Mon 14:00"));
        assert_eq!(warn.kind, ToastKind::Warn);
        assert_eq!(warn.text, "conta1 at 80% of the weekly limit \u{2014} resets Mon 14:00");
        assert_eq!(warn.panel, None);

        let crit = alert_toast(AlertLevel::Crit90, "conta2", 93.0, Some("Fri 09:30 UTC"));
        assert_eq!(crit.kind, ToastKind::Error);
        assert_eq!(
            crit.text,
            "conta2 at 93% of the weekly limit \u{2014} resets Fri 09:30 UTC"
        );

        let unknown = alert_toast(AlertLevel::Warn80, "conta1", 82.0, None);
        assert_eq!(unknown.text, "conta1 at 82% of the weekly limit");
        assert!(
            !unknown.text.contains("Auto"),
            "toasts have no action to switch accounts"
        );
    }

    #[test]
    fn alert_toast_title_never_breaks_on_control_characters() {
        let toast = alert_toast(AlertLevel::Warn80, "a\nb", 85.0, None);

        assert_eq!(toast.text.lines().count(), 1);
        assert!(toast.text.starts_with("ab at 85%"));
    }

    /// A weekly window that resets far in the future (relative to any real clock).
    const RESETS: &str = "2099-01-01T00:00:00Z";

    fn snapshot(weekly: f32, state: UsageState) -> Vec<ProfileUsage> {
        vec![ProfileUsage {
            name: "conta1".to_owned(),
            display_name: None,
            email: None,
            five_hour: UsageWindow::default(),
            seven_day: UsageWindow {
                percent: Some(weekly),
                resets_at: Some(RESETS.to_owned()),
            },
            severity: None,
            state,
            fetched_at_ms: Some(1),
        }]
    }

    fn queued(hub: &mut HubState) -> Vec<Toast> {
        hub.toasts.drain_visible(Instant::now())
    }

    /// Every reading below differs in percentage from the previous one, so a toast the tracker
    /// wrongly re-fired would have different text and not be swallowed by the toast dedupe.
    #[test]
    fn crossings_raise_one_toast_each_and_rerenders_stay_quiet() {
        let now = UNIX_EPOCH + Duration::from_secs(1_794_700_800 - 2 * 24 * 3600);
        let mut hub = HubState::default();

        hub.observe_usage_alerts(&snapshot(70.0, UsageState::Ok), now);
        assert!(queued(&mut hub).is_empty(), "the first reading is only a baseline");

        hub.observe_usage_alerts(&snapshot(82.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(83.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(84.0, UsageState::Ok), now);
        let toasts = queued(&mut hub);
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].kind, ToastKind::Warn);
        assert!(toasts[0].text.contains("82%"));

        hub.observe_usage_alerts(&snapshot(91.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(92.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(93.0, UsageState::Ok), now);
        let toasts = queued(&mut hub);
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts[1].kind, ToastKind::Error);
        assert!(toasts[1].text.contains("91%"));
    }

    #[test]
    fn unusable_logins_raise_nothing_and_do_not_rearm() {
        let now = UNIX_EPOCH;
        let mut hub = HubState::default();
        hub.observe_usage_alerts(&snapshot(50.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(85.0, UsageState::Ok), now);
        assert_eq!(queued(&mut hub).len(), 1);

        hub.observe_usage_alerts(&snapshot(0.0, UsageState::Expired), now);
        hub.observe_usage_alerts(&snapshot(86.0, UsageState::Stale), now);
        hub.observe_usage_alerts(&snapshot(87.0, UsageState::Ok), now);

        assert_eq!(queued(&mut hub).len(), 1, "no new toast");
    }

    #[test]
    fn a_weekly_figure_of_a_window_that_already_reset_raises_nothing() {
        let after_reset = UNIX_EPOCH + Duration::from_secs(4_102_444_800 + 3600);
        let mut hub = HubState::default();
        hub.observe_usage_alerts(&snapshot(50.0, UsageState::Ok), UNIX_EPOCH);

        hub.observe_usage_alerts(&snapshot(95.0, UsageState::Ok), after_reset);

        assert!(queued(&mut hub).is_empty());
    }
}
