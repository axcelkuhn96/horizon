use horizon_core::ClaudeProfile;
use horizon_core::claude_usage::{ProfileUsage, UsageState, UsageWindow};

use super::*;

fn window(percent: Option<f32>) -> UsageWindow {
    UsageWindow {
        percent,
        resets_at: None,
    }
}

fn usage(name: &str, email: Option<&str>, five: Option<f32>, seven: Option<f32>) -> ProfileUsage {
    ProfileUsage {
        name: name.to_owned(),
        display_name: None,
        email: email.map(str::to_owned),
        five_hour: window(five),
        seven_day: window(seven),
        severity: None,
        state: UsageState::Ok,
        fetched_at_ms: Some(1_000),
    }
}

fn profile(name: &str) -> ClaudeProfile {
    ClaudeProfile {
        name: name.to_owned(),
        config_dir: format!("~/.claude-{name}"),
    }
}

#[test]
fn severity_thresholds_75_90() {
    assert_eq!(severity_for(Some(74.9), None), Severity::Normal);
    assert_eq!(severity_for(Some(75.0), None), Severity::Warning);
    assert_eq!(severity_for(Some(89.9), None), Severity::Warning);
    assert_eq!(severity_for(Some(90.0), None), Severity::Critical);
    assert_eq!(severity_for(None, None), Severity::Normal);
}

#[test]
fn severity_api_critical_overrides() {
    assert_eq!(severity_for(Some(10.0), Some("critical")), Severity::Critical);
    assert_eq!(severity_for(Some(10.0), Some("warning")), Severity::Warning);
    assert_eq!(severity_for(Some(95.0), Some("unknown")), Severity::Critical);
    assert_eq!(severity_for(None, Some("critical")), Severity::Normal);
}

#[test]
fn chip_label_shows_email_local_part() {
    let u = usage("claude2", Some("gutosjc@example.com"), Some(37.0), Some(12.0));

    assert_eq!(chip_label(&u, false), "claude2 \u{b7} gutosjc  5h 37%  7d 12%");
}

#[test]
fn chip_label_truncates_identifier() {
    let u = usage(
        "claude2",
        Some("averylongaccountname@example.com"),
        Some(37.0),
        Some(12.0),
    );

    assert_eq!(
        chip_label(&u, false),
        "claude2 \u{b7} averylonga\u{2026}  5h 37%  7d 12%"
    );
    assert_eq!(chip_label(&u, true), "claude2 \u{b7} avery\u{2026}  7d 12%");
}

#[test]
fn chip_label_without_email_omits_identifier() {
    let u = usage("claude2", None, Some(37.0), Some(12.0));

    assert_eq!(chip_label(&u, false), "claude2  5h 37%  7d 12%");
    assert_eq!(chip_label(&u, true), "claude2  7d 12%");
    let blank = usage("claude2", Some("@example.com"), Some(1.0), Some(2.0));
    assert_eq!(chip_label(&blank, true), "claude2  7d 2%");
}

#[test]
fn chip_label_dash_for_null_window() {
    let u = usage("claude2", None, None, Some(12.0));

    assert_eq!(chip_label(&u, false), "claude2  5h \u{2014}  7d 12%");
    let both = usage("claude2", None, None, None);
    assert_eq!(chip_label(&both, true), "claude2  7d \u{2014}");
}

#[test]
fn chip_label_expired_marker() {
    let mut u = usage("claude2", Some("gutosjc@example.com"), Some(37.0), Some(12.0));
    u.state = UsageState::Expired;

    assert_eq!(chip_label(&u, false), "claude2 \u{b7} gutosjc  5h 37%  expired");
    assert_eq!(chip_label(&u, true), "claude2 \u{b7} gutos\u{2026}  expired");
    u.state = UsageState::Missing;
    assert_eq!(chip_label(&u, true), "claude2 \u{b7} gutos\u{2026}  no login");
}

#[test]
fn account_id_strips_control_characters_and_keeps_short_names() {
    assert_eq!(account_id(Some("a\u{1b}[31mb@x.io"), 10).as_deref(), Some("a[31mb"));
    assert_eq!(account_id(Some("ana@x.io"), 10).as_deref(), Some("ana"));
    assert_eq!(account_id(Some("exactly10c@x.io"), 10).as_deref(), Some("exactly10c"));
    assert_eq!(account_id(None, 10), None);
    assert_eq!(account_id(Some(""), 10), None);
}

#[test]
fn chip_status_goes_stale_after_fifteen_minutes() {
    let u = usage("a", None, Some(1.0), Some(1.0));

    assert_eq!(chip_status(None, 0), ChipStatus::NoData);
    assert_eq!(chip_status(Some(&u), 1_000 + STALE_AFTER_MS), ChipStatus::Fresh);
    assert_eq!(chip_status(Some(&u), 1_001 + STALE_AFTER_MS), ChipStatus::Stale);
    let mut expired = u.clone();
    expired.state = UsageState::Expired;
    assert_eq!(chip_status(Some(&expired), 0), ChipStatus::Expired);
    let mut failed = u;
    failed.state = UsageState::Stale;
    assert_eq!(chip_status(Some(&failed), 0), ChipStatus::Stale);
}

#[test]
fn widths_scale_with_count_and_compact_is_narrower() {
    assert!(chips_width(0, false).abs() <= f32::EPSILON);
    assert!(chips_width(2, false) > chips_width(1, false));
    assert!(chips_width(3, true) < chips_width(3, false));
    let widths = ChipWidths::estimated(3);
    assert!((widths.total(false, 3) - chips_width(3, false)).abs() <= f32::EPSILON);
    assert!((widths.total(true, 2) - chips_width(2, true)).abs() <= f32::EPSILON);
    assert!(widths.total(true, 0).abs() <= f32::EPSILON);
    assert!(full_chip_width(40.0, 40.0, None) <= 340.0);
    assert!(compact_chip_width(40.0, 30.0, None) < full_chip_width(40.0, 40.0, None));
}

#[test]
fn snapshot_follows_the_profile_list_and_feeds_the_launcher_seams() {
    let mut state = ClaudeUsageState::default();
    assert!(state.set_profiles(vec![profile("conta1"), profile("conta2")]));
    assert!(!state.set_profiles(vec![profile("conta1"), profile("conta2")]));
    assert_eq!(state.entries().len(), 2);
    assert_eq!(state.account_email("conta1"), None);
    assert_eq!(state.usage_summary("conta1"), None);

    let before = state.version;
    state.apply_snapshot(vec![
        usage("conta2", Some("outra@example.com"), Some(91.0), Some(77.0)),
        usage("gone", Some("x@example.com"), Some(1.0), Some(1.0)),
    ]);

    assert_ne!(state.version, before);
    assert_eq!(state.account_email("conta2"), Some("outra@example.com"));
    assert_eq!(state.usage_summary("conta2"), Some("5h 91%  7d 77%"));
    assert_eq!(state.account_email("gone"), None);
    assert_eq!(state.account_email("conta1"), None);
}

#[test]
fn fresh_data_ends_a_refresh_but_older_data_does_not() {
    let mut state = ClaudeUsageState::default();
    state.set_profiles(vec![profile("a")]);
    state.refresh_started = Some((Instant::now(), 5_000));
    let mut old = usage("a", None, Some(1.0), Some(1.0));
    old.fetched_at_ms = Some(4_000);

    state.apply_snapshot(vec![old.clone()]);
    assert!(state.is_refreshing(Instant::now()));

    old.fetched_at_ms = Some(5_000);
    state.apply_snapshot(vec![old]);
    assert!(!state.is_refreshing(Instant::now()));
}

#[test]
fn a_refresh_that_never_answers_times_out() {
    let mut state = ClaudeUsageState::default();
    let long_ago = Instant::now().checked_sub(REFRESH_WINDOW + Duration::from_secs(1));
    state.refresh_started = long_ago.map(|at| (at, 0));

    assert!(!state.is_refreshing(Instant::now()));
}

#[test]
fn chip_widths_are_empty_when_disabled_or_without_profiles() {
    let mut state = ClaudeUsageState::default();
    assert_eq!(state.chip_widths(true).len(), 0);
    state.cache.widths = ChipWidths::estimated(2);

    assert_eq!(state.chip_widths(true).len(), 2);
    assert_eq!(state.chip_widths(false).len(), 0);
}

#[test]
fn identity_line_joins_display_name_and_email() {
    let mut u = usage("a", Some("fulano@example.com"), None, None);
    assert_eq!(identity_line(&u).as_deref(), Some("fulano@example.com"));
    u.display_name = Some("Fulano Silva".to_owned());
    assert_eq!(
        identity_line(&u).as_deref(),
        Some("Fulano Silva \u{b7} fulano@example.com")
    );
    u.email = None;
    assert_eq!(identity_line(&u).as_deref(), Some("Fulano Silva"));
    u.display_name = None;
    assert_eq!(identity_line(&u), None);
}

#[test]
fn measured_cache_makes_compact_chips_narrower_than_full_ones() {
    let ctx = egui::Context::default();
    let entries = vec![ChipEntry::build(
        "claude2",
        Some(&usage("claude2", Some("gutosjc@example.com"), Some(37.0), Some(12.0))),
        Some(0),
    )];
    let mut cache = None;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("usage_chips_test")));
        cache = Some(build_cache(&painter, &entries, &[0], (1, 0)));
    });
    let cache = cache.expect("cache built inside the frame");

    assert_eq!(cache.chips.len(), 1);
    assert!(cache.widths.compact[0] < cache.widths.full[0]);
    assert!(cache.widths.full[0] < 340.0);
}
