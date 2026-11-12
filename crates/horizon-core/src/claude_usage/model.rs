//! Usage model plus the pure functions that parse and format usage windows.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::UsageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageState {
    Ok,
    Expired,
    Missing,
    Stale,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Used percentage (0-100); `None` when the API reported no window.
    pub percent: Option<f32>,
    /// RFC 3339 reset timestamp as returned by the API.
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileUsage {
    pub name: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub five_hour: UsageWindow,
    pub seven_day: UsageWindow,
    pub severity: Option<String>,
    pub state: UsageState,
    pub fetched_at_ms: Option<i64>,
}

#[allow(clippy::cast_possible_truncation)] // f32 precision is plenty for a percentage
fn window_from(value: Option<&Value>) -> UsageWindow {
    let Some(obj) = value.and_then(Value::as_object) else {
        return UsageWindow::default();
    };
    UsageWindow {
        percent: obj
            .get("utilization")
            .and_then(Value::as_f64)
            .filter(|p| p.is_finite())
            .map(|p| p as f32),
        resets_at: obj.get("resets_at").and_then(Value::as_str).map(str::to_owned),
    }
}

fn limit_for_group<'a>(limits: &'a [Value], group: &str) -> Option<&'a Value> {
    limits
        .iter()
        .find(|limit| limit.get("group").and_then(Value::as_str) == Some(group))
}

/// Parses the OAuth usage response into `(five_hour, seven_day, severity)`.
///
/// Reads `five_hour` / `seven_day` first; a `null` or absent window falls back
/// to the `limits[]` entry with the matching `group`. Severity comes from the
/// five-hour limit, else the seven-day one.
///
/// # Errors
/// `UsageError::Malformed` when `body` is not a JSON object.
pub fn parse_usage_response(body: &str) -> Result<(UsageWindow, UsageWindow, Option<String>), UsageError> {
    let root: Value = serde_json::from_str(body).map_err(|_| UsageError::Malformed)?;
    if !root.is_object() {
        return Err(UsageError::Malformed);
    }
    let limits: &[Value] = root.get("limits").and_then(Value::as_array).map_or(&[], Vec::as_slice);

    let mut windows = ["five_hour", "seven_day"].map(|group| {
        let direct = window_from(root.get(group));
        if direct.percent.is_some() || direct.resets_at.is_some() {
            direct
        } else {
            window_from(limit_for_group(limits, group))
        }
    });
    let severity = ["five_hour", "seven_day"].iter().find_map(|group| {
        limit_for_group(limits, group)
            .and_then(|limit| limit.get("severity"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let [five_hour, seven_day] = std::mem::take(&mut windows);
    Ok((five_hour, seven_day, severity))
}

/// Remaining percentage for a used percentage: `100 - round(used)`, never below 0.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100 first
pub fn remaining_percent(used: f32) -> u32 {
    let used = used.clamp(0.0, 100.0).round() as u32;
    100 - used
}

/// Used percentage as shown in the UI: `"37"`, or an em dash when unknown.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // percentages are small
pub fn used_percent_label(window: &UsageWindow) -> String {
    window
        .percent
        .map_or_else(|| "\u{2014}".to_owned(), |p| format!("{}", p.round() as i64))
}

/// Parses an RFC 3339 timestamp (`Z` or `+hh:mm` offset, optional fraction)
/// into Unix seconds. Hand-written to avoid a date dependency.
#[must_use]
pub fn parse_rfc3339_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = b.get(range)?;
        if digits.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(digits).ok()?.parse().ok()
        } else {
            None
        }
    };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut idx = 19;
    if b[idx] == b'.' {
        idx += 1;
        let start = idx;
        while b.get(idx).is_some_and(u8::is_ascii_digit) {
            idx += 1;
        }
        if idx == start {
            return None;
        }
    }
    let offset_secs = match b.get(idx)? {
        b'Z' | b'z' if idx + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if idx + 6 == b.len() && b[idx + 3] == b':' => {
            let (oh, om) = (num(idx + 1..idx + 3)?, num(idx + 4..idx + 6)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let secs = oh * 3600 + om * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };

    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_five_hour_seven_day() {
        let body = r#"{"five_hour":{"utilization":37.0,"resets_at":"2026-11-12T18:00:00.5+00:00"},
            "seven_day":{"utilization":12,"resets_at":"2026-11-15T00:00:00Z"}}"#;
        let (five, seven, severity) = parse_usage_response(body).unwrap();
        assert_eq!(five.percent, Some(37.0));
        assert_eq!(five.resets_at.as_deref(), Some("2026-11-12T18:00:00.5+00:00"));
        assert_eq!(seven.percent, Some(12.0));
        assert_eq!(severity, None);
        assert_eq!(used_percent_label(&five), "37");
    }

    #[test]
    fn parse_null_windows_are_dashes() {
        let (five, seven, severity) = parse_usage_response(r#"{"five_hour":null,"seven_day":null}"#).unwrap();
        assert_eq!(five, UsageWindow::default());
        assert_eq!(seven, UsageWindow::default());
        assert_eq!(severity, None);
        assert_eq!(used_percent_label(&five), "\u{2014}");
        assert!(parse_usage_response("[]").is_err());
        assert_eq!(parse_usage_response("nope"), Err(UsageError::Malformed));
    }

    #[test]
    fn parse_falls_back_to_limits_by_group() {
        let body = r#"{"five_hour":null,"limits":[
            {"group":"seven_day","utilization":80.4,"resets_at":"2026-11-15T00:00:00Z","severity":"warn"},
            {"group":"five_hour","utilization":91.0,"resets_at":"2026-11-12T18:00:00Z","severity":"critical"}]}"#;
        let (five, seven, severity) = parse_usage_response(body).unwrap();
        assert_eq!(five.percent, Some(91.0));
        assert_eq!(seven.percent.map(f32::round), Some(80.0));
        assert_eq!(severity.as_deref(), Some("critical"));
    }

    #[test]
    fn remaining_clamps_at_zero() {
        assert_eq!(remaining_percent(37.4), 63);
        assert_eq!(remaining_percent(100.0), 0);
        assert_eq!(remaining_percent(130.0), 0);
        assert_eq!(remaining_percent(-5.0), 100);
        assert_eq!(remaining_percent(f32::NAN), 100);
    }

    #[test]
    fn rfc3339_parses_offset_and_z() {
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_unix("2026-11-12T18:00:00Z"), Some(1_794_506_400));
        assert_eq!(parse_rfc3339_unix("2026-11-12T18:00:00+00:00"), Some(1_794_506_400));
        assert_eq!(
            parse_rfc3339_unix("2026-11-12T15:00:00.123456-03:00"),
            Some(1_794_506_400)
        );
        assert_eq!(parse_rfc3339_unix("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_rfc3339_unix("2026-13-12T18:00:00Z"), None);
        assert_eq!(parse_rfc3339_unix("2026-11-12T18:00:00"), None);
        assert_eq!(parse_rfc3339_unix("garbage"), None);
    }

    #[test]
    fn usage_state_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&UsageState::Expired).unwrap(), "\"expired\"");
    }
}
