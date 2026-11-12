use std::cell::Cell;

use horizon_core::claude_usage::UsageWindow;

use super::*;
use crate::app::hub_ui::Severity;

fn profile(name: &str, dir: &str) -> ClaudeProfile {
    ClaudeProfile {
        name: name.to_owned(),
        config_dir: dir.to_owned(),
    }
}

fn two_profiles() -> Vec<ClaudeProfile> {
    vec![profile("default", "~/.claude"), profile("work", "~/.claude-work")]
}

fn usage(state: UsageState, seven: Option<f32>) -> ProfileUsage {
    ProfileUsage {
        name: "work".to_owned(),
        display_name: None,
        email: Some("fulano@exemplo.com".to_owned()),
        five_hour: UsageWindow {
            percent: Some(12.0),
            resets_at: None,
        },
        seven_day: UsageWindow {
            percent: seven,
            resets_at: None,
        },
        severity: None,
        state,
        fetched_at_ms: Some(1_000),
    }
}

fn with_painter(check: impl FnOnce(&Painter)) {
    let ctx = egui::Context::default();
    let check = Cell::new(Some(check));
    // Text layout needs fonts, which exist inside a pass.
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        if let Some(check) = check.take() {
            check(&painter);
        }
    });
}

#[test]
fn remaining_is_100_minus_used_clamped() {
    assert_eq!(remaining_pct(Some(66.0)), Some(34.0));
    assert_eq!(remaining_pct(Some(0.0)), Some(100.0));
    assert_eq!(remaining_pct(Some(100.0)), Some(0.0));
    assert_eq!(remaining_pct(Some(130.0)), Some(0.0));
    assert_eq!(remaining_pct(Some(-5.0)), Some(100.0));
}

#[test]
fn remaining_none_when_usage_unknown() {
    assert_eq!(remaining_pct(None), None);
    assert_eq!(remaining_pct(Some(f32::NAN)), None);
}

#[test]
fn ring_sweep_zero_and_full() {
    assert!(ring_sweep(0.0).abs() < f32::EPSILON);
    assert!(ring_sweep(-3.0).abs() < f32::EPSILON);
    assert!((ring_sweep(100.0) - 1.0).abs() < f32::EPSILON);
    assert!((ring_sweep(250.0) - 1.0).abs() < f32::EPSILON);
    assert!((ring_sweep(50.0) - 0.5).abs() < f32::EPSILON);
}

#[test]
fn ring_sweep_min_visible_slice_when_nonzero() {
    assert!((ring_sweep(0.1) - MIN_VISIBLE_SWEEP).abs() < f32::EPSILON);
    assert!((ring_sweep(2.0) - 0.02).abs() < f32::EPSILON);
    assert!(ring_sweep(3.0) > MIN_VISIBLE_SWEEP);
}

#[test]
fn ring_points_count_bounded_and_on_circle() {
    let center = Pos2::new(30.0, 12.0);
    let points = ring_points(center, 7.5, START_ANGLE, 0.34, 17);
    assert_eq!(points.len(), 18);
    for point in &points {
        assert!(
            ((*point - center).length() - 7.5).abs() < 1e-3,
            "{point:?} off the circle"
        );
    }
    // Starts at 12 o'clock and sweeps clockwise (towards +x on screen).
    assert!((points[0] - Pos2::new(30.0, 4.5)).length() < 1e-3);
    assert!(points[1].x > points[0].x);

    assert_eq!(ring_points(center, 7.5, START_ANGLE, 1.0, 500).len(), MAX_SEGMENTS + 1);
    assert_eq!(ring_points(center, 7.5, START_ANGLE, 1.0, 0).len(), 2);
    for ppp in [1.0, 1.5, 2.0, 4.0] {
        assert!(arc_segments(1.0, ppp) <= MAX_SEGMENTS);
        assert!(arc_segments(0.02, ppp) >= 1);
    }
}

#[test]
fn badge_state_dim_for_expired_missing_nologin() {
    assert_eq!(badge_state(UsageState::Expired, Some(80.0)), BadgeState::Dim);
    assert_eq!(badge_state(UsageState::Missing, Some(80.0)), BadgeState::Dim);
    assert_eq!(badge_state(UsageState::Ok, None), BadgeState::Dim);
    assert_eq!(badge_state(UsageState::Stale, None), BadgeState::Dim);
}

#[test]
fn badge_state_severity_follows_used_pct() {
    assert_eq!(
        badge_state(UsageState::Ok, Some(80.0)),
        BadgeState::Ok(Severity::Normal)
    );
    assert_eq!(
        badge_state(UsageState::Ok, Some(20.0)),
        BadgeState::Ok(Severity::Warning)
    );
    assert_eq!(
        badge_state(UsageState::Ok, Some(5.0)),
        BadgeState::Ok(Severity::Critical)
    );
    assert_eq!(
        badge_state(UsageState::Stale, Some(5.0)),
        BadgeState::Ok(Severity::Critical)
    );
}

#[test]
fn panel_profile_matches_normalized_config_dir() {
    let home = Path::new("/home/u");
    let profiles = two_profiles();
    for dir in [
        "/home/u/.claude-work",
        "/home/u/.claude-work/",
        "~/.claude-work",
        "~/.claude-work/",
    ] {
        assert_eq!(
            panel_profile(PanelKind::Shell, None, Some(Path::new(dir)), &profiles, home),
            Some(1),
            "detected {dir}"
        );
        assert_eq!(
            panel_profile(PanelKind::Claude, Some(Path::new(dir)), None, &profiles, home),
            Some(1),
            "env {dir}"
        );
    }
    // A running Claude beats the env the panel was started with.
    assert_eq!(
        panel_profile(
            PanelKind::Claude,
            Some(Path::new("~/.claude-work")),
            Some(Path::new("/home/u/.claude")),
            &profiles,
            home
        ),
        Some(0)
    );
    // A profile written with a trailing slash still matches.
    let slashed = vec![profile("work", "~/.claude-work/")];
    assert_eq!(
        panel_profile(
            PanelKind::Shell,
            None,
            Some(Path::new("/home/u/.claude-work")),
            &slashed,
            home
        ),
        Some(0)
    );
}

#[test]
fn panel_profile_default_dir_maps_to_default_profile() {
    let home = Path::new("/home/u");
    let profiles = two_profiles();
    assert_eq!(panel_profile(PanelKind::Claude, None, None, &profiles, home), Some(0));
    assert_eq!(
        panel_profile(PanelKind::Claude, Some(Path::new("")), None, &profiles, home),
        Some(0)
    );
    assert_eq!(
        panel_profile(PanelKind::Claude, Some(Path::new("~/.claude/")), None, &profiles, home),
        Some(0)
    );
    assert_eq!(
        panel_profile(
            PanelKind::Shell,
            None,
            Some(Path::new("/home/u/.claude")),
            &profiles,
            home
        ),
        Some(0)
    );
    // The default profile is found wherever it sits in the list.
    let reordered = vec![profile("work", "~/.claude-work"), profile("default", "~/.claude")];
    assert_eq!(panel_profile(PanelKind::Claude, None, None, &reordered, home), Some(1));
    // No default profile configured: no account.
    let only_work = vec![profile("work", "~/.claude-work")];
    assert_eq!(panel_profile(PanelKind::Claude, None, None, &only_work, home), None);
}

#[test]
fn panel_profile_none_for_non_claude_shell() {
    let home = Path::new("/home/u");
    let profiles = two_profiles();
    assert_eq!(panel_profile(PanelKind::Shell, None, None, &profiles, home), None);
    // The env of a shell says nothing about a Claude that is not running.
    assert_eq!(
        panel_profile(
            PanelKind::Shell,
            Some(Path::new("~/.claude-work")),
            None,
            &profiles,
            home
        ),
        None
    );
    assert_eq!(panel_profile(PanelKind::Editor, None, None, &profiles, home), None);
}

#[test]
fn panel_profile_unknown_dir_is_none() {
    let home = Path::new("/home/u");
    let profiles = two_profiles();
    assert_eq!(
        panel_profile(PanelKind::Shell, None, Some(Path::new("/srv/other")), &profiles, home),
        None
    );
    assert_eq!(
        panel_profile(PanelKind::Claude, Some(Path::new("/srv/other")), None, &profiles, home),
        None
    );
    assert_eq!(panel_profile(PanelKind::Claude, None, None, &[], home), None);
}

fn key(dir: Option<&Path>, version: u64, ppp_bits: u32) -> BadgeKey<'_> {
    BadgeKey { dir, version, ppp_bits }
}

fn entry(dir: Option<&Path>, version: u64, ppp_bits: u32) -> Entry {
    Entry {
        dir: dir.map(Path::to_path_buf),
        version,
        ppp_bits,
        layout: None,
    }
}

#[test]
fn cache_rebuilds_only_when_key_changes() {
    let panel = PanelId(7);
    let mut cache = AccountBadgeCache::default();
    let dir = PathBuf::from("/home/u/.claude-work");
    assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 1)), "nothing cached yet");
    cache.store(panel, entry(Some(&dir), 1, 1));
    assert!(
        cache.is_fresh(panel, &key(Some(&dir), 1, 1)),
        "a panel without an account is cached too"
    );
    assert!(!cache.is_fresh(panel, &key(Some(&dir), 2, 1)), "new usage snapshot");
    assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 2)), "new scale");
    assert!(!cache.is_fresh(panel, &key(None, 1, 1)), "Claude gone");
    assert!(
        !cache.is_fresh(panel, &key(Some(Path::new("/home/u/.claude")), 1, 1)),
        "other account"
    );
    assert!(!cache.is_fresh(PanelId(8), &key(Some(&dir), 1, 1)), "other panel");

    cache.forget(panel);
    assert!(!cache.is_fresh(panel, &key(Some(&dir), 1, 1)));
}

#[test]
fn env_dir_is_resolved_once_per_env() {
    let mut cache = AccountBadgeCache::default();
    let home = Path::new("/home/u");
    let panel = PanelId(3);
    let mut env = BTreeMap::new();
    env.insert("CLAUDE_CONFIG_DIR".to_owned(), "/home/u/.claude-work".to_owned());

    let first = cache.env_dir(panel, &env, home);
    let again = cache.env_dir(panel, &env, home);
    assert_eq!(&*first, Path::new("/home/u/.claude-work"));
    assert!(Arc::ptr_eq(&first, &again), "an unchanged env reuses the resolved dir");

    env.insert("CLAUDE_CONFIG_DIR".to_owned(), "/home/u/.claude-other".to_owned());
    let changed = cache.env_dir(panel, &env, home);
    assert_eq!(&*changed, Path::new("/home/u/.claude-other"));

    cache.forget(panel);
    assert!(cache.env_dirs.is_empty());
}

#[test]
fn cache_evicts_closed_panels_when_the_count_moves() {
    let mut cache = AccountBadgeCache::default();
    for id in [1, 2] {
        cache.store(
            PanelId(id),
            Entry {
                dir: None,
                version: 0,
                ppp_bits: 0,
                layout: None,
            },
        );
    }
    cache.evict_closed(1, |id| id == PanelId(1));
    assert!(cache.entries.contains_key(&PanelId(1)));
    assert!(!cache.entries.contains_key(&PanelId(2)));
}

#[test]
fn badge_width_fits_name_ellipsis_and_ring() {
    assert!((badge_width(None) - RING_SLOT).abs() < f32::EPSILON);
    assert!((badge_width(Some(40.0)) - (RING_SLOT + NAME_GAP + 40.0)).abs() < f32::EPSILON);

    with_painter(|painter| {
        let long = profile(&"x".repeat(80), "~/.claude-long");
        let short = profile("a", "~/.claude-a");
        let layout = |profile: &ClaudeProfile| {
            build_layout(
                painter,
                &LayoutInputs {
                    profile,
                    usage: None,
                    reset_seven: None,
                    accent: Color32::WHITE,
                    pixels_per_point: 1.0,
                },
            )
        };
        let long = layout(&long);
        let short = layout(&short);
        assert!(long.width(true) <= MAX_WIDTH + 0.5, "{}", long.width(true));
        assert!(long.width(true) > short.width(true), "a longer name widens the badge");
        assert!(short.width(true) > RING_SLOT);
        assert!((short.width(false) - RING_SLOT).abs() < f32::EPSILON);
    });
}

#[test]
fn layout_follows_usage_state() {
    with_painter(|painter| {
        let work = profile("work", "~/.claude-work");
        let layout = |usage: Option<&ProfileUsage>| {
            build_layout(
                painter,
                &LayoutInputs {
                    profile: &work,
                    usage,
                    reset_seven: None,
                    accent: Color32::WHITE,
                    pixels_per_point: 2.0,
                },
            )
        };

        let fresh = usage(UsageState::Ok, Some(66.0));
        let ok = layout(Some(&fresh));
        assert_eq!(ok.state, BadgeState::Ok(Severity::Normal));
        assert_eq!(ok.accessible, "work, 34% of weekly quota remaining");
        assert_eq!(ok.tooltip.status, AccountStatus::Active);
        assert!(!ok.ring.arc.is_empty() && ok.ring.dashes.is_empty() && !ok.ring.full);
        assert!(ok.ring.arc.len() <= MAX_SEGMENTS + 1);

        let untouched = usage(UsageState::Ok, Some(0.0));
        assert!(layout(Some(&untouched)).ring.full, "all quota left is a whole circle");

        let expired = usage(UsageState::Expired, Some(10.0));
        let dim = layout(Some(&expired));
        assert_eq!(dim.state, BadgeState::Dim);
        assert_eq!(dim.tooltip.status, AccountStatus::Expired);
        assert_eq!(dim.accessible, "work, login expired");
        assert!(dim.ring.arc.is_empty() && !dim.ring.dashes.is_empty());
        assert!(dim.tooltip.login_note.as_deref().is_some_and(|note| {
            note.contains("claude /login") && note.contains("CLAUDE_CONFIG_DIR=~/.claude-work")
        }));

        let unknown = layout(None);
        assert_eq!(unknown.state, BadgeState::Dim);
        assert_eq!(unknown.accessible, "work, weekly quota unknown");
        assert_eq!(unknown.tooltip.status, AccountStatus::Unknown);
        assert!(unknown.tooltip.login_note.is_none());
    });
}

#[test]
fn tooltip_text_has_email_used_remaining_and_reset() {
    let info = TooltipInfo {
        name: "work".to_owned(),
        status: AccountStatus::Active,
        email: Some("fulano@exemplo.com".to_owned()),
        used_seven: Some(66.0),
        used_five: Some(12.0),
        reset: Some(("Mon 14:00".to_owned(), 1_000_000 + 2 * 86_400 + 4 * 3_600)),
        login_note: None,
    };
    let lines = info.lines(1_000_000);
    assert_eq!(lines.email, Some("fulano@exemplo.com"));
    assert_eq!(lines.weekly, "7d: 66% used \u{b7} 34% left");
    assert_eq!(lines.five.as_deref(), Some("5h: 12% used"));
    assert_eq!(lines.reset.as_deref(), Some("Resets Mon 14:00 (in 2d 4h)"));
    assert_eq!(lines.note, None);

    let unknown = TooltipInfo {
        name: "work".to_owned(),
        status: AccountStatus::NoLogin,
        email: None,
        used_seven: None,
        used_five: None,
        reset: None,
        login_note: Some("Not logged in".to_owned()),
    };
    let lines = unknown.lines(0);
    assert_eq!(lines.weekly, "7d: no data");
    assert!(lines.five.is_none() && lines.reset.is_none() && lines.email.is_none());
    assert_eq!(lines.note, Some("Not logged in"));
}

#[test]
fn account_status_pill_and_label_follow_the_login() {
    assert_eq!(AccountStatus::of(Some(UsageState::Ok)), AccountStatus::Active);
    assert_eq!(AccountStatus::of(Some(UsageState::Missing)), AccountStatus::NoLogin);
    assert_eq!(AccountStatus::of(None), AccountStatus::Unknown);
    assert_eq!(AccountStatus::Active.pill().map(|pill| pill.0), Some("active"));
    assert_eq!(AccountStatus::Expired.pill().map(|pill| pill.0), Some("expired"));
    assert!(AccountStatus::Unknown.pill().is_none());
    assert_eq!(
        AccountStatus::Expired.accessible("work", Some(10.0)),
        "work, login expired"
    );
    assert_eq!(AccountStatus::NoLogin.accessible("work", None), "work, not logged in");
    assert_eq!(
        AccountStatus::Stale.accessible("work", Some(90.0)),
        "work, 10% of weekly quota remaining"
    );
}
