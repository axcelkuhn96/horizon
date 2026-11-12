//! Weekly-usage alerts: one per crossing of 80% / 90%. State lives in memory; the first
//! observation of a profile is only a baseline, so reopening the app above a threshold stays
//! quiet (the usage chips already show the colour) and a restart never repeats a crossing.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

const WARN_PCT: f32 = 80.0;
const CRIT_PCT: f32 = 90.0;
/// Dropping below this re-arms the alerts (hysteresis against a percentage hovering at 80).
const REARM_PCT: f32 = 75.0;
/// Reset timestamps further apart than this belong to different windows (the API may jitter).
const NEW_WINDOW_SLACK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertLevel {
    Warn80,
    Crit90,
}

fn level_for(pct: f32) -> Option<AlertLevel> {
    if pct >= CRIT_PCT {
        Some(AlertLevel::Crit90)
    } else if pct >= WARN_PCT {
        Some(AlertLevel::Warn80)
    } else {
        None
    }
}

fn is_new_window(known: Option<SystemTime>, now: Option<SystemTime>) -> bool {
    let (Some(known), Some(now)) = (known, now) else {
        return false;
    };
    let apart = known.duration_since(now).unwrap_or_else(|error| error.duration());
    apart > NEW_WINDOW_SLACK
}

struct ProfileAlert {
    /// Highest level already announced (or present at the baseline) in this window.
    level: Option<AlertLevel>,
    window: Option<SystemTime>,
}

/// Per-profile alert state.
#[derive(Default)]
pub struct AlertTracker {
    profiles: HashMap<String, ProfileAlert>,
}

impl AlertTracker {
    /// Feeds one reading of `name`'s weekly usage. Returns the level to announce, if the usage
    /// just crossed one. An unknown percentage (also a profile whose login is not `Ok`) neither
    /// fires nor re-arms; callers pass `None` for it.
    pub fn observe(
        &mut self,
        name: &str,
        weekly_pct: Option<f32>,
        window_resets_at: Option<SystemTime>,
    ) -> Option<AlertLevel> {
        let pct = weekly_pct.filter(|pct| pct.is_finite())?;
        let current = level_for(pct);
        let Some(state) = self.profiles.get_mut(name) else {
            self.profiles.insert(
                name.to_owned(),
                ProfileAlert {
                    level: current,
                    window: window_resets_at,
                },
            );
            return None;
        };
        if is_new_window(state.window, window_resets_at) {
            state.level = None;
        }
        if window_resets_at.is_some() {
            state.window = window_resets_at;
        }
        if pct < REARM_PCT {
            state.level = None;
            return None;
        }
        if current > state.level {
            state.level = current;
            return current;
        }
        None
    }
}
