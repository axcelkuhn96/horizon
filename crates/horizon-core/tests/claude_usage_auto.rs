//! Auto account selection and weekly usage alerts (public API only).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horizon_core::claude_usage::{
    AlertLevel, AlertTracker, AutoReason, ProfileUsage, ProfileUsageView, SkipReason, UsageState, UsageWindow,
    pick_auto,
};

fn view(name: &str, order: usize, state: UsageState, weekly: Option<f32>, five: Option<f32>) -> ProfileUsageView {
    ProfileUsageView {
        name: name.to_owned(),
        state,
        weekly_pct: weekly,
        five_hour_pct: five,
        resets_at: None,
        order,
    }
}

fn ok(name: &str, order: usize, weekly: f32) -> ProfileUsageView {
    view(name, order, UsageState::Ok, Some(weekly), None)
}

fn picked(views: &[ProfileUsageView]) -> Option<String> {
    pick_auto(views).map(|pick| pick.name)
}

#[test]
fn auto_picks_most_remaining_weekly() {
    let views = [ok("a", 0, 34.0), ok("b", 1, 12.0), ok("c", 2, 60.0)];

    let pick = pick_auto(&views).expect("a usable profile");

    assert_eq!(pick.name, "b");
    assert_eq!(pick.reason, AutoReason::MostRemaining);
    assert!((pick.remaining_pct - 88.0).abs() < 0.01);
}

#[test]
fn auto_skips_expired_missing_and_no_login() {
    let views = [
        view("expired", 0, UsageState::Expired, Some(1.0), None),
        view("missing", 1, UsageState::Missing, None, None),
        ok("fine", 2, 70.0),
    ];

    assert_eq!(picked(&views).as_deref(), Some("fine"));
    assert_eq!(views[0].skip_reason(), Some(SkipReason::Expired));
    assert_eq!(views[1].skip_reason(), Some(SkipReason::NoLogin));
    assert_eq!(views[2].skip_reason(), None);
}

#[test]
fn auto_tie_breaks_on_five_hour_then_config_order() {
    let tied_on_week = [
        view("a", 0, UsageState::Ok, Some(40.0), Some(50.0)),
        view("b", 1, UsageState::Ok, Some(40.0), Some(10.0)),
    ];
    assert_eq!(picked(&tied_on_week).as_deref(), Some("b"));

    let no_five_hour_window = [
        view("a", 0, UsageState::Ok, Some(40.0), Some(5.0)),
        view("b", 1, UsageState::Ok, Some(40.0), None),
    ];
    assert_eq!(picked(&no_five_hour_window).as_deref(), Some("b"));

    let fully_tied = [ok("z", 1, 40.0), ok("y", 0, 40.0)];
    assert_eq!(picked(&fully_tied).as_deref(), Some("y"));

    let rounds_to_tie = [ok("a", 0, 40.2), ok("b", 1, 39.8)];
    assert_eq!(picked(&rounds_to_tie).as_deref(), Some("a"));
}

#[test]
fn auto_without_usage_data_picks_first_logged_in() {
    let views = [
        view("gone", 0, UsageState::Missing, None, None),
        view("first", 1, UsageState::Ok, None, None),
        view("second", 2, UsageState::Stale, None, None),
    ];

    let pick = pick_auto(&views).expect("a logged-in profile");

    assert_eq!(pick.name, "first");
    assert_eq!(pick.reason, AutoReason::NoUsageData);
}

#[test]
fn auto_prefers_a_profile_with_data_over_one_without() {
    let views = [view("blank", 0, UsageState::Ok, None, None), ok("known", 1, 90.0)];

    assert_eq!(picked(&views).as_deref(), Some("known"));
}

#[test]
fn auto_none_when_nothing_usable() {
    assert_eq!(pick_auto(&[]), None);
    let views = [
        view("a", 0, UsageState::Expired, None, None),
        view("b", 1, UsageState::Missing, None, None),
    ];
    assert_eq!(pick_auto(&views), None);
}

#[test]
fn auto_ignores_profile_with_weekly_100() {
    let views = [ok("spent", 0, 100.0), ok("left", 1, 95.0)];
    assert_eq!(picked(&views).as_deref(), Some("left"));
    assert_eq!(views[0].skip_reason(), Some(SkipReason::LimitReached));

    assert_eq!(pick_auto(&[ok("spent", 0, 100.0)]), None);
}

#[test]
fn auto_ignores_profile_with_five_hour_100() {
    let views = [
        view("blocked", 0, UsageState::Ok, Some(5.0), Some(100.0)),
        view("open", 1, UsageState::Ok, Some(50.0), Some(0.0)),
    ];
    assert_eq!(picked(&views).as_deref(), Some("open"));
}

#[test]
fn view_projects_a_snapshot_entry_without_secrets() {
    let usage = ProfileUsage {
        name: "conta1".to_owned(),
        display_name: Some("Ada".to_owned()),
        email: Some("ada@example.com".to_owned()),
        five_hour: UsageWindow {
            percent: Some(10.0),
            resets_at: None,
        },
        seven_day: UsageWindow {
            percent: Some(66.0),
            resets_at: Some("2026-11-15T00:00:00Z".to_owned()),
        },
        severity: None,
        state: UsageState::Ok,
        fetched_at_ms: Some(1),
    };

    let projected = ProfileUsageView::new("conta1", 3, Some(&usage));

    assert_eq!(projected.weekly_pct, Some(66.0));
    assert_eq!(projected.five_hour_pct, Some(10.0));
    assert_eq!(projected.order, 3);
    assert_eq!(
        projected.resets_at,
        Some(UNIX_EPOCH + Duration::from_secs(1_794_700_800))
    );
    let unknown = ProfileUsageView::new("conta2", 0, None);
    assert_eq!(unknown.state, UsageState::Stale);
    assert_eq!(unknown.weekly_pct, None);
}

fn at(secs: u64) -> Option<SystemTime> {
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

const WEEK: u64 = 7 * 24 * 3600;

#[test]
fn alert_fires_once_at_80() {
    let mut tracker = AlertTracker::default();
    assert_eq!(tracker.observe("a", Some(70.0), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(79.9), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(80.0), at(WEEK)), Some(AlertLevel::Warn80));
}

#[test]
fn alert_fires_crit_when_jumping_to_90() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(93.0), at(WEEK)), Some(AlertLevel::Crit90));
    assert_eq!(tracker.observe("a", Some(95.0), at(WEEK)), None);
}

#[test]
fn alert_80_then_90_fires_both_once() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(82.0), at(WEEK)), Some(AlertLevel::Warn80));
    assert_eq!(tracker.observe("a", Some(91.0), at(WEEK)), Some(AlertLevel::Crit90));
    assert_eq!(tracker.observe("a", Some(92.0), at(WEEK)), None);
}

#[test]
fn alert_does_not_refire_while_above() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(81.0), at(WEEK)), Some(AlertLevel::Warn80));
    for pct in [81.0, 84.0, 79.0, 76.0, 85.0] {
        assert_eq!(tracker.observe("a", Some(pct), at(WEEK)), None, "{pct}");
    }
}

#[test]
fn alert_rearms_below_75() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), Some(AlertLevel::Warn80));
    assert_eq!(tracker.observe("a", Some(74.0), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(80.0), at(WEEK)), Some(AlertLevel::Warn80));
}

#[test]
fn alert_rearms_on_new_window() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), Some(AlertLevel::Warn80));
    // Same window, sub-second jitter in the reset time: still the same crossing.
    assert_eq!(tracker.observe("a", Some(86.0), at(WEEK + 1)), None);
    // A new window opens while the reading is already high (e.g. a stale first answer).
    assert_eq!(tracker.observe("a", Some(85.0), at(2 * WEEK)), Some(AlertLevel::Warn80));
}

#[test]
fn alert_first_observation_is_baseline_only() {
    let mut tracker = AlertTracker::default();
    assert_eq!(tracker.observe("a", Some(95.0), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(96.0), at(WEEK)), None);

    let mut tracker = AlertTracker::default();
    assert_eq!(tracker.observe("a", Some(82.0), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(91.0), at(WEEK)), Some(AlertLevel::Crit90));
}

#[test]
fn alert_ignores_unknown_pct() {
    let mut tracker = AlertTracker::default();
    assert_eq!(tracker.observe("a", None, None), None);
    assert_eq!(tracker.observe("a", Some(f32::NAN), None), None);
    // Unknown readings set no baseline: the first real one still only becomes the baseline.
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), None);
    // ... and they never re-arm.
    assert_eq!(tracker.observe("a", Some(50.0), at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), Some(AlertLevel::Warn80));
    assert_eq!(tracker.observe("a", None, at(WEEK)), None);
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), None);
}

#[test]
fn alert_state_is_per_profile() {
    let mut tracker = AlertTracker::default();
    tracker.observe("a", Some(50.0), at(WEEK));
    tracker.observe("b", Some(50.0), at(WEEK));
    assert_eq!(tracker.observe("a", Some(85.0), at(WEEK)), Some(AlertLevel::Warn80));
    assert_eq!(tracker.observe("b", Some(85.0), at(WEEK)), Some(AlertLevel::Warn80));
    assert_eq!(tracker.observe("a", Some(86.0), at(WEEK)), None);
}
