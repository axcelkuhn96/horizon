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
fn chip_label_hides_a_window_the_api_returned_null() {
    let u = usage("claude2", None, None, Some(12.0));

    assert_eq!(chip_label(&u, false), "claude2  7d 12%");
    assert_eq!(chip_label(&u, true), "claude2  7d 12%");
    let both = usage("claude2", None, None, None);
    assert_eq!(chip_label(&both, true), "claude2");
    assert_eq!(chip_label(&both, false), "claude2");
    let (head, tail) = chip_label_parts("claude2", Some(&u), false);
    assert_eq!((head.as_str(), tail.as_str()), ("claude2", "7d 12%"));
}

#[test]
fn chip_label_keeps_dashes_before_the_first_fetch() {
    let mut u = usage("claude2", None, None, None);
    u.fetched_at_ms = None;

    assert_eq!(chip_label(&u, false), "claude2  5h \u{2014}  7d \u{2014}");
    assert_eq!(chip_label(&u, true), "claude2  7d \u{2014}");
    assert_eq!(chip_label_parts("claude2", None, false).1, "5h \u{2014}  7d \u{2014}");
}

#[test]
fn chip_label_expired_hides_missing_five_hour_but_keeps_the_marker() {
    let mut u = usage("claude2", None, None, Some(12.0));
    u.state = UsageState::Expired;

    assert_eq!(chip_label(&u, false), "claude2  expired");
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
fn widths_total_adds_gaps_and_compact_is_narrower() {
    let widths = ChipWidths::estimated(3);

    assert!(widths.total(false, 0).abs() <= f32::EPSILON);
    assert!((widths.total(false, 1) - CHIP_FULL_ESTIMATE).abs() <= f32::EPSILON);
    assert!((widths.total(false, 3) - (3.0 * CHIP_FULL_ESTIMATE + 2.0 * CHIP_GAP)).abs() <= f32::EPSILON);
    assert!(widths.total(true, 3) < widths.total(false, 3));
    let both = ShownWindows::of(None);
    assert!(full_chip_width(40.0, 40.0, None, both) <= 340.0);
    assert!(compact_chip_width(40.0, 30.0, None, both) < full_chip_width(40.0, 40.0, None, both));
}

#[test]
fn hidden_five_hour_window_frees_its_width() {
    let both = ShownWindows::of(None);
    let seven_only = ShownWindows {
        five: false,
        seven: true,
    };
    let none = ShownWindows {
        five: false,
        seven: false,
    };

    let freed = full_chip_width(40.0, 40.0, None, both) - full_chip_width(40.0, 40.0, None, seven_only);
    assert!((freed - (SECTION_GAP + GROUP_WIDTH_FULL)).abs() <= f32::EPSILON);
    // Compact never had a 5h group, so hiding it changes nothing there.
    assert!(
        (compact_chip_width(40.0, 30.0, None, both) - compact_chip_width(40.0, 30.0, None, seven_only)).abs()
            <= f32::EPSILON
    );
    // A status pill takes the 7d slot whatever the windows say.
    assert!(compact_chip_width(40.0, 30.0, Some(50.0), none) > compact_chip_width(40.0, 30.0, None, none));
    assert!(
        (full_chip_width(40.0, 0.0, None, none) - (CHIP_PAD * 2.0 + DOT_DIAMETER + DOT_GAP + 40.0)).abs()
            <= f32::EPSILON
    );
}

#[test]
fn shown_windows_hide_a_window_without_percent_only_once_fetched() {
    let never = ShownWindows::of(None);
    assert!(never.five && never.seven);

    let mut u = usage("a", None, None, Some(12.0));
    assert_eq!(
        ShownWindows::of(Some(&u)),
        ShownWindows {
            five: false,
            seven: true
        }
    );
    u.state = UsageState::Stale;
    assert_eq!(
        ShownWindows::of(Some(&u)),
        ShownWindows {
            five: false,
            seven: true
        }
    );
    u.fetched_at_ms = None;
    assert_eq!(
        ShownWindows::of(Some(&u)),
        ShownWindows {
            five: true,
            seven: true
        }
    );
    let full = usage("a", None, Some(1.0), Some(2.0));
    assert!(ShownWindows::of(Some(&full)).shows(true) && ShownWindows::of(Some(&full)).shows(false));
}

#[test]
fn measured_chip_without_five_hour_is_narrower_than_with_it() {
    let ctx = egui::Context::default();
    let entries = vec![
        ChipEntry::build("a", Some(&usage("a", None, Some(37.0), Some(12.0))), Some(0)),
        ChipEntry::build("a", Some(&usage("a", None, None, Some(12.0))), Some(0)),
    ];
    let mut cache = None;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("usage_chips_test_hidden")));
        cache = Some(build_cache(&painter, &entries, &[0, 1], (1, 0)));
    });
    let cache = cache.expect("cache built inside the frame");

    assert!(cache.widths.full[1] < cache.widths.full[0]);
    assert!((cache.widths.compact[1] - cache.widths.compact[0]).abs() <= f32::EPSILON);
}

#[test]
fn window_text_shows_percent_or_dash() {
    let some = window(Some(42.0));

    assert_eq!(window_text(Some(&some)), "42%");
    assert_eq!(window_text(Some(&window(None))), "\u{2014}");
    assert_eq!(window_text(None), "\u{2014}");
}

#[test]
fn a_snapshot_with_no_fetchable_profile_ends_the_refresh() {
    let mut state = ClaudeUsageState::default();
    state.set_profiles(vec![profile("a"), profile("b")]);
    state.refresh_started = Some((Instant::now(), 5_000));
    let mut expired = usage("a", None, Some(1.0), Some(1.0));
    expired.state = UsageState::Expired;
    expired.fetched_at_ms = Some(1_000);
    let mut missing = usage("b", None, None, None);
    missing.state = UsageState::Missing;
    missing.fetched_at_ms = None;

    state.apply_snapshot(vec![expired, missing]);

    assert!(!state.is_refreshing(Instant::now()));
}

#[test]
fn an_old_ok_snapshot_does_not_end_the_refresh() {
    let mut old = usage("a", None, Some(1.0), Some(1.0));
    old.fetched_at_ms = Some(1_000);
    let mut failed = usage("b", None, Some(1.0), Some(1.0));
    failed.state = UsageState::Stale;

    assert!(!refresh_answered(&[old.clone()], 5_000));
    assert!(refresh_answered(&[old, failed.clone()], 1_000));
    assert!(refresh_answered(&[failed], 5_000));
    assert!(!refresh_answered(&[], 0));
}

#[test]
fn tokens_today_is_formatted_once_when_the_stats_arrive() {
    let mut state = ClaudeUsageState::default();
    assert_eq!(state.tokens_today(), None);
    assert!(!state.poll_tokens());

    state.set_tokens_today(1_240_000);

    assert_eq!(state.tokens_today(), Some("1.2M"));
}

#[test]
fn tokens_scan_stops_being_pending_once_answered_or_after_the_patience_window() {
    let mut state = ClaudeUsageState::default();
    let now = Instant::now();
    assert!(!state.tokens_scan_pending(now));

    state.tokens_scan = Some((now, false));
    assert!(state.tokens_scan_pending(now));
    assert!(!state.tokens_scan_pending(now + TOKENS_SCAN_PATIENCE));

    state.tokens_scan = Some((now, true));
    assert!(!state.tokens_scan_pending(now));
}

#[test]
fn snapshot_follows_the_profile_list_and_feeds_the_launcher_seams() {
    let mut state = ClaudeUsageState::default();
    assert!(state.set_profiles(vec![profile("conta1"), profile("conta2")]));
    assert!(!state.set_profiles(vec![profile("conta1"), profile("conta2")]));
    assert_eq!(state.entries().len(), 2);
    assert_eq!(state.account_email("conta1"), None);
    assert!(state.usage("conta1").is_none());

    let before = state.version;
    state.apply_snapshot(vec![
        usage("conta2", Some("outra@example.com"), Some(91.0), Some(77.0)),
        usage("gone", Some("x@example.com"), Some(1.0), Some(1.0)),
    ]);

    assert_ne!(state.version, before);
    assert_eq!(state.account_email("conta2"), Some("outra@example.com"));
    assert_eq!(
        state
            .usage("conta2")
            .map(|u| (u.five_hour.percent, u.seven_day.percent)),
        Some((Some(91.0), Some(77.0)))
    );
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

#[test]
fn cache_holds_pre_laid_out_window_text() {
    let ctx = egui::Context::default();
    let entries = vec![
        ChipEntry::build("a", Some(&usage("a", None, Some(37.0), None)), Some(0)),
        ChipEntry::build("b", None, Some(0)),
    ];
    let mut cache = None;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("usage_chips_test_text")));
        cache = Some(build_cache(&painter, &entries, &[0, 1], (1, 0)));
    });
    let cache = cache.expect("cache built inside the frame");

    assert_eq!(cache.chips[0].window(true).1.text(), "37%");
    assert_eq!(cache.chips[0].window(false).1.text(), "\u{2014}");
    assert_eq!(cache.chips[0].window(true).0.text(), "5h");
    assert_eq!(cache.chips[1].window(false).0.text(), "7d");
    assert_eq!(cache.chips[1].window(true).1.text(), "\u{2014}");
}

#[test]
fn profile_change_syncs_the_poller_list_and_invalidates_launcher_rows() {
    let mut hub = HubState::default();
    let rows = hub.rows_version;

    hub.sync_usage_profiles(vec![profile("a")], &Context::default());

    assert_eq!(hub.usage.entries().len(), 1);
    assert_ne!(hub.rows_version, rows);
    assert_eq!(hub.usage.synced_rows_version, Some(hub.rows_version));

    let rows = hub.rows_version;
    hub.sync_usage_profiles(vec![profile("a")], &Context::default());
    assert_eq!(hub.rows_version, rows);

    hub.sync_usage_profiles(vec![profile("a"), profile("b")], &Context::default());
    assert_eq!(hub.usage.entries().len(), 2);
    assert_ne!(hub.rows_version, rows);
    assert_eq!(hub.usage.synced_rows_version, Some(hub.rows_version));
}

#[test]
fn the_poller_only_starts_once_polling_is_enabled_with_the_apps_homes() {
    let mut hub = HubState::default();
    hub.sync_usage_profiles(vec![profile("a")], &Context::default());
    assert!(hub.usage.poller.is_none(), "no poll setup: nothing may start");

    let dir = tempfile::tempdir().expect("tempdir");
    let horizon_home = HorizonHome::from_root(dir.path().join(".horizon"));
    hub.usage.enable_polling(dir.path().to_path_buf(), &horizon_home);
    hub.sync_usage_profiles(vec![profile("a"), profile("b")], &Context::default());

    assert!(hub.usage.poller.is_some());
    let setup = hub.usage.poll_setup.as_ref().expect("setup");
    assert_eq!(setup.user_home, dir.path());
    assert_eq!(setup.cache_path, dir.path().join(".horizon").join(CACHE_FILE));
}

#[test]
fn emptying_the_profiles_stops_the_refresh_and_the_poller() {
    let mut hub = HubState::default();
    hub.sync_usage_profiles(vec![profile("a")], &Context::default());
    hub.usage.refresh_started = Some((Instant::now(), 1));

    hub.sync_usage_profiles(Vec::new(), &Context::default());

    assert!(hub.usage.entries().is_empty());
    assert!(hub.usage.poller.is_none());
    assert!(!hub.usage.is_refreshing(Instant::now()));
}

#[test]
fn a_snapshot_invalidates_launcher_rows_without_asking_for_another_profile_sync() {
    let mut hub = HubState::default();
    hub.sync_usage_profiles(vec![profile("a")], &Context::default());
    let rows = hub.rows_version;

    hub.apply_usage_snapshot(vec![usage("a", Some("fulano@example.com"), Some(10.0), Some(20.0))]);

    assert_ne!(hub.rows_version, rows);
    assert_eq!(hub.usage.synced_rows_version, Some(hub.rows_version));
    assert_eq!(
        hub.usage.usage("a").map(|u| (u.five_hour.percent, u.seven_day.percent)),
        Some((Some(10.0), Some(20.0)))
    );
    assert!(!hub.receive_usage_snapshot(), "no poller in tests: nothing to receive");
}
