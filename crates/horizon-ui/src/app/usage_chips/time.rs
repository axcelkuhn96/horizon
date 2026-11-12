//! Clock and duration text for the chip tooltips. Dependency-free: the local
//! UTC offset comes from `date +%z`; without it times read as UTC.

const MINUTE: i64 = 60;
const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Parses `date +%z` output (`+0200`, `-0330`) into seconds east of UTC.
#[must_use]
pub(super) fn parse_utc_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    (hours <= 14 && minutes < 60).then_some(sign * (hours * HOUR + minutes * MINUTE))
}

/// Local UTC offset in seconds; `None` when it cannot be determined (the
/// caller then labels times as UTC). Spawns `date` on every call, so callers
/// ask once per snapshot (not per frame): a long-running session then follows
/// a daylight-saving change instead of keeping the offset from startup.
#[must_use]
pub(in crate::app) fn local_offset_secs() -> Option<i64> {
    if !cfg!(unix) {
        return None;
    }
    let output = std::process::Command::new("date").arg("+%z").output().ok()?;
    parse_utc_offset(std::str::from_utf8(&output.stdout).ok()?)
}

/// `14:30`, or `Fri 09:00` with `weekday`. Without an offset the time is UTC and says so.
#[must_use]
pub(in crate::app) fn format_clock(unix: i64, offset: Option<i64>, weekday: bool) -> String {
    let local = unix.saturating_add(offset.unwrap_or(0));
    let seconds_of_day = local.rem_euclid(DAY);
    let (hours, minutes) = (seconds_of_day / HOUR, seconds_of_day % HOUR / MINUTE);
    // 1970-01-01 was a Thursday (index 4 of `WEEKDAYS`).
    let day_index = usize::try_from((local.div_euclid(DAY) + 4).rem_euclid(7)).unwrap_or(0);
    let mut text = if weekday {
        format!("{} {hours:02}:{minutes:02}", WEEKDAYS[day_index])
    } else {
        format!("{hours:02}:{minutes:02}")
    };
    if offset.is_none() {
        text.push_str(" UTC");
    }
    text
}

/// Time until a reset: `45s`-level precision is noise, so `<1m`, `12m`, `1h 12m`, `3d 4h`.
#[must_use]
pub(super) fn format_relative(secs: i64) -> String {
    if secs <= 0 {
        "now".to_owned()
    } else if secs < MINUTE {
        "<1m".to_owned()
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h {}m", secs / HOUR, secs % HOUR / MINUTE)
    } else {
        format!("{}d {}h", secs / DAY, secs % DAY / HOUR)
    }
}

/// Age of a snapshot: `just now`, `3 min ago`, `2 h ago`, `1 d ago`.
#[must_use]
pub(super) fn format_age(secs: i64) -> String {
    if secs < MINUTE {
        "just now".to_owned()
    } else if secs < HOUR {
        format!("{} min ago", secs / MINUTE)
    } else if secs < DAY {
        format!("{} h ago", secs / HOUR)
    } else {
        format!("{} d ago", secs / DAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_offset_parses_signed_hhmm() {
        assert_eq!(parse_utc_offset("+0200\n"), Some(7_200));
        assert_eq!(parse_utc_offset("-0330"), Some(-12_600));
        assert_eq!(parse_utc_offset("+0000"), Some(0));
        assert_eq!(parse_utc_offset("UTC"), None);
        assert_eq!(parse_utc_offset("+25"), None);
        assert_eq!(parse_utc_offset("+1575"), None);
    }

    #[test]
    fn clock_applies_the_offset_and_names_the_weekday() {
        // 2026-11-13 12:30:00 UTC is a Friday.
        let unix = 1_794_573_000;
        assert_eq!(format_clock(unix, Some(0), false), "12:30");
        assert_eq!(format_clock(unix, Some(-10_800), false), "09:30");
        assert_eq!(format_clock(unix, Some(0), true), "Fri 12:30");
        assert_eq!(format_clock(unix + 12 * HOUR, Some(0), true), "Sat 00:30");
    }

    #[test]
    fn clock_without_offset_is_labelled_utc() {
        assert_eq!(format_clock(0, None, false), "00:00 UTC");
        assert_eq!(format_clock(0, None, true), "Thu 00:00 UTC");
    }

    #[test]
    fn relative_durations_pick_two_units() {
        assert_eq!(format_relative(-5), "now");
        assert_eq!(format_relative(30), "<1m");
        assert_eq!(format_relative(12 * MINUTE), "12m");
        assert_eq!(format_relative(HOUR + 12 * MINUTE), "1h 12m");
        assert_eq!(format_relative(3 * DAY + 4 * HOUR), "3d 4h");
    }

    #[test]
    fn age_reads_naturally() {
        assert_eq!(format_age(5), "just now");
        assert_eq!(format_age(3 * MINUTE), "3 min ago");
        assert_eq!(format_age(2 * HOUR), "2 h ago");
        assert_eq!(format_age(DAY), "1 d ago");
    }
}
