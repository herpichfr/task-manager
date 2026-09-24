//! Pure date helpers for a task's `start_date` and `deadline` fields.
//!
//! Every function here takes `now` (unix seconds) as an explicit parameter
//! instead of reading the clock itself, so callers control what "now" means
//! and tests are deterministic. Local-time boundaries use `chrono::Local`,
//! i.e. the machine's configured timezone.

use chrono::{Duration, Local, NaiveDate, TimeZone};

/// Parses a user-typed date into unix seconds at 00:00 local time.
/// Accepts: "YYYY-MM-DD"; "today"; "tomorrow"; "+Nd" / "Nd" (N days out);
/// "+Nw" (weeks). Empty or whitespace-only input => Ok(None) (clears it).
/// Anything else => Err with a short user-facing message.
pub fn parse_date(input: &str, now: i64) -> Result<Option<i64>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed.eq_ignore_ascii_case("today") {
        return Ok(Some(midnight_epoch(local_date_from_epoch(now))));
    }
    if trimmed.eq_ignore_ascii_case("tomorrow") {
        let tomorrow = local_date_from_epoch(now) + Duration::days(1);
        return Ok(Some(midnight_epoch(tomorrow)));
    }

    if let Some(rest) = trimmed.strip_suffix('d') {
        let rest = rest.strip_prefix('+').unwrap_or(rest);
        if !rest.is_empty() {
            if let Ok(n) = rest.parse::<i64>() {
                let date = local_date_from_epoch(now) + Duration::days(n);
                return Ok(Some(midnight_epoch(date)));
            }
        }
    }

    if let Some(rest) = trimmed.strip_suffix('w') {
        let rest = rest.strip_prefix('+').unwrap_or(rest);
        if !rest.is_empty() {
            if let Ok(n) = rest.parse::<i64>() {
                let date = local_date_from_epoch(now) + Duration::weeks(n);
                return Ok(Some(midnight_epoch(date)));
            }
        }
    }

    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() == 3 && parts[0].len() == 4 {
        if let (Ok(y), Ok(m), Ok(d)) =
            (parts[0].parse::<i32>(), parts[1].parse::<u32>(), parts[2].parse::<u32>())
        {
            if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                return Ok(Some(midnight_epoch(date)));
            }
        }
    }

    Err(format!("not a recognized date: \"{trimmed}\" (try YYYY-MM-DD, today, tomorrow, +Nd, +Nw)"))
}

/// Formats unix seconds as "YYYY-MM-DD" for display in the form.
pub fn format_date(ts: i64) -> String {
    local_date_from_epoch(ts).format("%Y-%m-%d").to_string()
}

/// Adds calendar days while preserving local-date semantics.
pub fn add_days(ts: i64, days: i64) -> i64 {
    midnight_epoch(local_date_from_epoch(ts) + Duration::days(days))
}

/// Whole days from `now` until `deadline`, negative when overdue. Both are
/// compared at local-midnight granularity by comparing calendar dates, so a
/// deadline later *today* is 0, not a fraction of a day.
pub fn days_until(deadline: i64, now: i64) -> i64 {
    (local_date_from_epoch(deadline) - local_date_from_epoch(now)).num_days()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    None,
    Distant,
    Soon,
    Near,
    Imminent,
    Overdue,
}

/// Buckets a task by its deadline. `deadline: None` => `Urgency::None`.
///
/// ```text
/// >= 15 days  => Distant
/// 5..=14      => Soon
/// 2..=4       => Near
/// 0..=1       => Imminent
/// < 0         => Overdue
/// ```
pub fn urgency(deadline: Option<i64>, now: i64) -> Urgency {
    let Some(deadline) = deadline else {
        return Urgency::None;
    };
    match days_until(deadline, now) {
        d if d < 0 => Urgency::Overdue,
        0 | 1 => Urgency::Imminent,
        2..=4 => Urgency::Near,
        5..=14 => Urgency::Soon,
        _ => Urgency::Distant,
    }
}

/// The calendar date (in local time) that `unix_secs` falls on.
fn local_date_from_epoch(unix_secs: i64) -> NaiveDate {
    match Local.timestamp_opt(unix_secs, 0) {
        chrono::LocalResult::Single(dt) => dt.date_naive(),
        chrono::LocalResult::Ambiguous(dt, _) => dt.date_naive(),
        chrono::LocalResult::None => {
            // Should not happen for a real unix timestamp; fall back to UTC
            // rather than panic on a clock read.
            chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs, 0)
                .map(|dt| dt.naive_utc().date())
                .unwrap_or_default()
        }
    }
}

/// Unix seconds for 00:00 local time on `date`.
fn midnight_epoch(date: NaiveDate) -> i64 {
    let naive = date.and_hms_opt(0, 0, 0).expect("00:00:00 is always a valid time");
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.timestamp(),
        chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
        chrono::LocalResult::None => {
            // Midnight falls in a DST spring-forward gap; a later hour on
            // the same calendar date always resolves.
            let bumped = date.and_hms_opt(3, 0, 0).expect("03:00:00 is always a valid time");
            Local
                .from_local_datetime(&bumped)
                .single()
                .expect("post-gap local time must resolve")
                .timestamp()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed "now": 2024-06-15 12:00:00 local time (a Saturday, no DST
    /// transition nearby on any real timezone).
    fn fixed_now() -> i64 {
        Local.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).single().unwrap().timestamp()
    }

    #[test]
    fn parse_empty_clears() {
        assert_eq!(parse_date("", fixed_now()).unwrap(), None);
        assert_eq!(parse_date("   ", fixed_now()).unwrap(), None);
    }

    #[test]
    fn parse_today_and_tomorrow() {
        let now = fixed_now();
        let today = parse_date("today", now).unwrap().unwrap();
        assert_eq!(format_date(today), "2024-06-15");
        let tomorrow = parse_date("tomorrow", now).unwrap().unwrap();
        assert_eq!(format_date(tomorrow), "2024-06-16");
        // case-insensitive
        assert_eq!(parse_date("TODAY", now).unwrap().unwrap(), today);
        assert_eq!(parse_date("Tomorrow", now).unwrap().unwrap(), tomorrow);
    }

    #[test]
    fn parse_relative_days_and_weeks() {
        let now = fixed_now();
        assert_eq!(format_date(parse_date("+3d", now).unwrap().unwrap()), "2024-06-18");
        assert_eq!(format_date(parse_date("3d", now).unwrap().unwrap()), "2024-06-18");
        assert_eq!(format_date(parse_date("+2w", now).unwrap().unwrap()), "2024-06-29");
        assert_eq!(format_date(parse_date("0d", now).unwrap().unwrap()), "2024-06-15");
    }

    #[test]
    fn parse_iso_date() {
        let now = fixed_now();
        assert_eq!(format_date(parse_date("2025-01-31", now).unwrap().unwrap()), "2025-01-31");
        assert_eq!(format_date(parse_date("2025-1-5", now).unwrap().unwrap()), "2025-01-05");
    }

    #[test]
    fn parse_rejects_garbage() {
        let now = fixed_now();
        assert!(parse_date("31-12-2025", now).is_err());
        assert!(parse_date("not a date", now).is_err());
        assert!(parse_date("2025-13-01", now).is_err());
        assert!(parse_date("2025-02-30", now).is_err());
        assert!(parse_date("+d", now).is_err());
        assert!(parse_date("dd", now).is_err());
    }

    #[test]
    fn format_round_trips_through_parse() {
        let now = fixed_now();
        let ts = parse_date("2030-07-04", now).unwrap().unwrap();
        assert_eq!(format_date(ts), "2030-07-04");
    }

    #[test]
    fn days_until_boundaries() {
        let now = fixed_now();
        let today = local_date_from_epoch(now);
        let at = |offset: i64| midnight_epoch(today + Duration::days(offset));

        assert_eq!(days_until(at(-1), now), -1);
        assert_eq!(days_until(at(0), now), 0);
        assert_eq!(days_until(at(1), now), 1);
        assert_eq!(days_until(at(2), now), 2);
        assert_eq!(days_until(at(4), now), 4);
        assert_eq!(days_until(at(5), now), 5);
        assert_eq!(days_until(at(14), now), 14);
        assert_eq!(days_until(at(15), now), 15);
        assert_eq!(days_until(at(16), now), 16);
    }

    #[test]
    fn urgency_at_every_boundary() {
        let now = fixed_now();
        let today = local_date_from_epoch(now);
        let at = |offset: i64| midnight_epoch(today + Duration::days(offset));

        assert_eq!(urgency(Some(at(-1)), now), Urgency::Overdue);
        assert_eq!(urgency(Some(at(0)), now), Urgency::Imminent);
        assert_eq!(urgency(Some(at(1)), now), Urgency::Imminent);
        assert_eq!(urgency(Some(at(2)), now), Urgency::Near);
        assert_eq!(urgency(Some(at(4)), now), Urgency::Near);
        assert_eq!(urgency(Some(at(5)), now), Urgency::Soon);
        assert_eq!(urgency(Some(at(14)), now), Urgency::Soon);
        assert_eq!(urgency(Some(at(15)), now), Urgency::Distant);
        assert_eq!(urgency(Some(at(16)), now), Urgency::Distant);
        assert_eq!(urgency(None, now), Urgency::None);
    }

    #[test]
    fn deadline_later_today_is_imminent_not_overdue() {
        // `now` is 12:00 today; a deadline of 23:00 the same day is a later
        // timestamp but the same calendar date.
        let now = Local.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).single().unwrap().timestamp();
        let later_today = Local.with_ymd_and_hms(2024, 6, 15, 23, 0, 0).single().unwrap().timestamp();
        assert!(later_today > now);
        assert_eq!(days_until(later_today, now), 0);
        assert_eq!(urgency(Some(later_today), now), Urgency::Imminent);
    }
}
