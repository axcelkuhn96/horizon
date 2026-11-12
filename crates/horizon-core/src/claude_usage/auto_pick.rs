//! The "Auto" account: which profile has the most weekly quota left. Pure rules over a
//! token-free projection of the usage snapshot; the UI decides when to evaluate (on click).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{ProfileUsage, UsageState, parse_rfc3339_unix, remaining_percent};

/// Usage figures older than this rank below fresh ones (they stay usable when nothing is fresh).
const FRESH_FOR: Duration = Duration::from_secs(15 * 60);

/// What Auto needs to know about one profile; carries no credential.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileUsageView {
    pub name: String,
    pub state: UsageState,
    /// Used percentage of the weekly (7d) window; `None` when the account has none or it is unknown.
    pub weekly_pct: Option<f32>,
    /// Used percentage of the 5h window; `None` for accounts without that limit.
    pub five_hour_pct: Option<f32>,
    /// When the weekly window resets.
    pub resets_at: Option<SystemTime>,
    /// When the figures were fetched; `None` when never.
    pub fetched_at: Option<SystemTime>,
    /// Position in the configured profile list; the last tie-breaker.
    pub order: usize,
}

impl ProfileUsageView {
    /// Projects a snapshot entry as of `now`. A profile without an entry yet counts as logged in
    /// with no usage data (its credentials are only judged once the poller has looked at them).
    /// A weekly figure whose window already reset is a leftover of the previous window (typically
    /// a cache read after a restart) and is dropped as unknown.
    #[must_use]
    pub fn new(name: &str, order: usize, usage: Option<&ProfileUsage>, now: SystemTime) -> Self {
        let Some(usage) = usage else {
            return Self {
                name: name.to_owned(),
                state: UsageState::Stale,
                weekly_pct: None,
                five_hour_pct: None,
                resets_at: None,
                fetched_at: None,
                order,
            };
        };
        let resets_at = usage
            .seven_day
            .resets_at
            .as_deref()
            .and_then(parse_rfc3339_unix)
            .and_then(|secs| u64::try_from(secs).ok())
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        let window_over = resets_at.is_some_and(|at| at <= now);
        let fetched_at = usage
            .fetched_at_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms));
        Self {
            name: name.to_owned(),
            state: usage.state,
            weekly_pct: usage.seven_day.percent.filter(|_| !window_over),
            five_hour_pct: usage.five_hour.percent,
            resets_at: resets_at.filter(|_| !window_over),
            fetched_at,
            order,
        }
    }

    /// The figures were fetched recently enough to rank on.
    fn is_fresh(&self, now: SystemTime) -> bool {
        // A fetch time in the future (clock skew) counts as fresh.
        self.fetched_at
            .is_some_and(|at| now.duration_since(at).unwrap_or_default() <= FRESH_FOR)
    }

    /// Why Auto never picks this profile, if so.
    #[must_use]
    pub fn skip_reason(&self) -> Option<SkipReason> {
        match self.state {
            UsageState::Expired => Some(SkipReason::Expired),
            UsageState::Missing => Some(SkipReason::NoLogin),
            UsageState::Ok | UsageState::Stale => {
                let spent = |pct: Option<f32>| pct.is_some_and(|pct| remaining_percent(pct) == 0);
                (spent(self.weekly_pct) || spent(self.five_hour_pct)).then_some(SkipReason::LimitReached)
            }
        }
    }
}

/// Why a profile is left out of Auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Expired,
    NoLogin,
    LimitReached,
}

impl SkipReason {
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::NoLogin => "no login",
            Self::LimitReached => "limit reached",
        }
    }
}

/// Why Auto chose a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReason {
    /// The most weekly quota left.
    MostRemaining,
    /// No usable profile reported usage; the first logged-in one stands in.
    NoUsageData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoPick {
    pub name: String,
    /// Weekly quota left (0-100); not meaningful for [`AutoReason::NoUsageData`] (100).
    pub remaining_pct: f32,
    pub reason: AutoReason,
}

/// Lower five-hour usage first; an account without that window sorts before any other.
fn five_hour_key(view: &ProfileUsageView) -> f32 {
    view.five_hour_pct.unwrap_or(-1.0)
}

/// The usable view with the most weekly quota left among `views` (rounded remaining, then lower
/// 5h usage, then config order), with its rounded remaining and used percentages.
fn best_by_weekly<'a>(views: impl Iterator<Item = &'a ProfileUsageView>) -> Option<(&'a ProfileUsageView, f32)> {
    views
        .filter_map(|view| view.weekly_pct.map(|used| (view, remaining_percent(used), used)))
        .min_by(|(a, rem_a, _), (b, rem_b, _)| {
            rem_b
                .cmp(rem_a)
                .then_with(|| five_hour_key(a).total_cmp(&five_hour_key(b)))
                .then_with(|| a.order.cmp(&b.order))
        })
        .map(|(view, _, used)| (view, used))
}

/// The profile with the most weekly quota left, as of `now`. Expired, missing and exhausted
/// profiles are skipped. Ties (on the rounded remaining percentage) go to the lower 5h usage,
/// then to the config order. Figures older than 15 minutes rank below fresh ones but still count
/// when nothing is fresh. Without any weekly figure the first usable profile is returned; `None`
/// when no profile is usable.
#[must_use]
pub fn pick_auto(views: &[ProfileUsageView], now: SystemTime) -> Option<AutoPick> {
    let usable = || views.iter().filter(|view| view.skip_reason().is_none());
    let best = best_by_weekly(usable().filter(|view| view.is_fresh(now))).or_else(|| best_by_weekly(usable()));
    if let Some((view, used)) = best {
        return Some(AutoPick {
            name: view.name.clone(),
            remaining_pct: 100.0 - used.clamp(0.0, 100.0),
            reason: AutoReason::MostRemaining,
        });
    }
    usable().min_by_key(|view| view.order).map(|view| AutoPick {
        name: view.name.clone(),
        remaining_pct: 100.0,
        reason: AutoReason::NoUsageData,
    })
}
