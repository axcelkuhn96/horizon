//! Weekly-usage alert toasts: an account crossing 80% / 90% of its weekly limit raises one toast
//! per crossing (the rules are `horizon_core::claude_usage::AlertTracker`). Fed from the place
//! that already takes the poller's snapshots; there is no extra poller or timer.
//! Visual spec: `docs/design/claude-hub-ui.md` §12.

use std::time::SystemTime;

use horizon_core::claude_usage::{AlertLevel, ProfileUsage, ProfileUsageView, UsageState};

use super::hub_state::HubState;
use super::hub_toast::{Toast, ToastKind};
use super::usage_chips::format_relative;

/// The toast for `name` having reached `level`. `pct` is the used share of the weekly window and
/// `resets_in_secs` the time left until it resets, when known. 80% is a warning, 90% an error.
#[must_use]
pub(super) fn alert_toast(level: AlertLevel, name: &str, pct: f32, resets_in_secs: Option<i64>) -> Toast {
    let name: String = name.chars().filter(|ch| !ch.is_control()).collect();
    let shown = pct.clamp(0.0, 100.0);
    let detail = match resets_in_secs {
        Some(secs) => format!("Resets in {} \u{b7} switch to Auto?", format_relative(secs)),
        None => "Switch to Auto?".to_owned(),
    };
    Toast {
        kind: match level {
            AlertLevel::Warn80 => ToastKind::Warn,
            AlertLevel::Crit90 => ToastKind::Error,
        },
        text: format!("{name} \u{b7} {shown:.0}% of weekly limit\n{detail}"),
        panel: None,
    }
}

impl HubState {
    /// Looks at a fresh snapshot and queues a toast for every account that just crossed a
    /// threshold. Accounts whose login is not usable are ignored (neither alert nor re-arm).
    pub(super) fn observe_usage_alerts(&mut self, snapshot: &[ProfileUsage], now: SystemTime) {
        for usage in snapshot {
            let view = ProfileUsageView::new(&usage.name, 0, Some(usage));
            let Some(pct) = view.weekly_pct.filter(|_| view.state == UsageState::Ok) else {
                continue;
            };
            if let Some(level) = self.alerts.observe(&usage.name, Some(pct), view.resets_at) {
                let resets_in = view
                    .resets_at
                    .and_then(|at| at.duration_since(now).ok())
                    .and_then(|left| i64::try_from(left.as_secs()).ok())
                    .filter(|secs| *secs > 0);
                self.toasts.push(alert_toast(level, &usage.name, pct, resets_in));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use horizon_core::claude_usage::UsageWindow;

    use super::*;

    const DAY: i64 = 24 * 3600;

    #[test]
    fn alert_toast_text_formats_level_and_reset() {
        let warn = alert_toast(AlertLevel::Warn80, "conta1", 80.4, Some(2 * DAY + 4 * 3600));
        assert_eq!(warn.kind, ToastKind::Warn);
        assert_eq!(
            warn.text,
            "conta1 \u{b7} 80% of weekly limit\nResets in 2d 4h \u{b7} switch to Auto?"
        );
        assert_eq!(warn.panel, None);

        let crit = alert_toast(AlertLevel::Crit90, "conta2", 93.0, Some(3 * 3600 + 120));
        assert_eq!(crit.kind, ToastKind::Error);
        assert_eq!(
            crit.text,
            "conta2 \u{b7} 93% of weekly limit\nResets in 3h 2m \u{b7} switch to Auto?"
        );

        let unknown = alert_toast(AlertLevel::Warn80, "conta1", 82.0, None);
        assert_eq!(unknown.text, "conta1 \u{b7} 82% of weekly limit\nSwitch to Auto?");
    }

    #[test]
    fn alert_toast_title_never_breaks_on_control_characters() {
        let toast = alert_toast(AlertLevel::Warn80, "a\nb", 85.0, None);

        assert_eq!(toast.text.lines().count(), 2);
        assert!(toast.text.starts_with("ab \u{b7} 85%"));
    }

    fn snapshot(weekly: f32, state: UsageState) -> Vec<ProfileUsage> {
        vec![ProfileUsage {
            name: "conta1".to_owned(),
            display_name: None,
            email: None,
            five_hour: UsageWindow::default(),
            seven_day: UsageWindow {
                percent: Some(weekly),
                resets_at: Some("2026-11-15T00:00:00Z".to_owned()),
            },
            severity: None,
            state,
            fetched_at_ms: Some(1),
        }]
    }

    fn queued(hub: &mut HubState) -> Vec<Toast> {
        hub.toasts.drain_visible(Instant::now())
    }

    #[test]
    fn crossings_raise_one_toast_each_and_rerenders_stay_quiet() {
        let now = UNIX_EPOCH + Duration::from_secs(1_794_700_800 - 2 * 24 * 3600);
        let mut hub = HubState::default();

        hub.observe_usage_alerts(&snapshot(70.0, UsageState::Ok), now);
        assert!(queued(&mut hub).is_empty(), "the first reading is only a baseline");

        hub.observe_usage_alerts(&snapshot(82.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(82.0, UsageState::Ok), now);
        let toasts = queued(&mut hub);
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].kind, ToastKind::Warn);
        assert!(toasts[0].text.contains("Resets in 2d 0h"));

        hub.observe_usage_alerts(&snapshot(91.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(91.0, UsageState::Ok), now);
        let toasts = queued(&mut hub);
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts[1].kind, ToastKind::Error);
    }

    #[test]
    fn unusable_logins_raise_nothing_and_do_not_rearm() {
        let now = UNIX_EPOCH;
        let mut hub = HubState::default();
        hub.observe_usage_alerts(&snapshot(50.0, UsageState::Ok), now);
        hub.observe_usage_alerts(&snapshot(85.0, UsageState::Ok), now);
        assert_eq!(queued(&mut hub).len(), 1);

        hub.observe_usage_alerts(&snapshot(0.0, UsageState::Expired), now);
        hub.observe_usage_alerts(&snapshot(85.0, UsageState::Stale), now);
        hub.observe_usage_alerts(&snapshot(85.0, UsageState::Ok), now);

        assert_eq!(queued(&mut hub).len(), 1, "no new toast");
    }
}
