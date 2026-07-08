use std::path::Path;
use workspace_model::SessionFileChange;

pub(super) fn upsert_loaded_change(items: &mut Vec<SessionFileChange>, item: SessionFileChange) {
    let normalized = normalize_change_path(&item.path);
    if let Some(existing) = items
        .iter_mut()
        .find(|change| normalize_change_path(&change.path) == normalized)
    {
        if item.new_text.len() >= existing.new_text.len() || item.timestamp >= existing.timestamp {
            *existing = item;
        }
    } else {
        items.push(item);
    }
}

pub(super) fn normalize_change_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("//?/")
        .or_else(|| normalized.strip_prefix("//./"))
        .unwrap_or(&normalized)
        .to_string();
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let mut chars: Vec<char> = normalized.chars().collect();
        chars[0] = chars[0].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        normalized
    }
}

pub(super) fn normalize_workspace_root(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalize_change_path(&path.to_string_lossy())
}

pub(super) fn now_iso() -> String {
    // UTC timestamp as decimal epoch SECONDS (despite the `_iso` name, this is
    // the numeric storage format, not ISO-8601). `created_at`/`updated_at`
    // columns rely on fixed-width numeric strings for `CAST(created_at AS
    // INTEGER)` date filters and `ORDER BY created_at`. Display-side ISO
    // conversion happens at read boundaries via [`epoch_secs_to_iso_utc`] /
    // [`instant_to_iso_utc`].
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();
    format!("{secs}")
}

/// Parse a user-supplied instant (ISO-8601 or decimal epoch seconds) into Unix
/// epoch seconds. Used by `load_usage_events_for_summary` to compare against
/// the epoch-seconds string stored in `usage_events.created_at`.
///
/// Accepts:
/// - Decimal integer seconds, e.g. `"1751328000"`.
/// - ISO-8601 with `Z`, e.g. `"2026-06-30T00:00:00Z"`, `"2026-06-30T00:00:00.123Z"`.
/// - ISO-8601 with explicit offset, e.g. `"2026-06-30T08:00:00+08:00"`,
///   `"2026-06-30T08:00:00+0800"`, with optional subseconds.
///
/// Returns `None` for anything else. Pure-Rust, no `chrono` dependency.
pub(super) fn parse_instant_to_epoch_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Decimal integer seconds: all digits, optional leading minus.
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse::<i64>().ok();
    }
    if let Some(neg) = s.strip_prefix('-') {
        if neg.chars().all(|c| c.is_ascii_digit()) {
            return neg.parse::<i64>().ok().map(|n| -n);
        }
        return None;
    }

    // ISO-8601 date-time. Required parts: `YYYY-MM-DDTHH:MM:SS`. Optional:
    // subseconds, timezone designator `Z` or `±HH:MM` / `±HHMM`.
    let (date, rest) = s.split_once('T')?;
    if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return None;
    }
    let year: i64 = date[0..4].parse().ok()?;
    let month: i64 = date[5..7].parse().ok()?;
    let day: i64 = date[8..10].parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=31).contains(&day) {
        return None;
    }

    // Time portion: `HH:MM:SS` plus optional fractional and timezone.
    let (time, tz) = match rest.find(|c: char| c == 'Z' || c == '+' || c == '-') {
        Some(idx) if idx >= 6 => (&rest[..idx], &rest[idx..]),
        _ => return None,
    };
    if time.len() < 8 || time.as_bytes()[2] != b':' || time.as_bytes()[5] != b':' {
        return None;
    }
    let hour: i64 = time[0..2].parse().ok()?;
    let minute: i64 = time[3..5].parse().ok()?;
    let second: i64 = time[6..8].parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    // Fractional seconds: optional `.N` ... `N` (up to 9 digits; we keep up to 9).
    let frac_str = if time.len() > 8 {
        let tail = &time[8..];
        if !tail.starts_with('.') {
            return None;
        }
        let digits = &tail[1..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits
    } else {
        ""
    };

    // Timezone offset in minutes east of UTC. `Z` => 0; `±HH:MM` or `±HHMM` => parsed.
    let tz_minutes: i64 = if tz == "Z" {
        0
    } else {
        let (sign, body) = match tz.as_bytes()[0] {
            b'+' => (1i64, &tz[1..]),
            b'-' => (-1i64, &tz[1..]),
            _ => return None,
        };
        let (h, m) = if body.len() == 5 && body.as_bytes()[2] == b':' {
            (body[0..2].parse::<i64>().ok()?, body[3..5].parse::<i64>().ok()?)
        } else if body.len() == 4 {
            (body[0..2].parse::<i64>().ok()?, body[2..4].parse::<i64>().ok()?)
        } else {
            return None;
        };
        if !(0..24).contains(&h) || !(0..60).contains(&m) {
            return None;
        }
        sign * (h * 60 + m)
    };

    // Days from civil (algorithm by Howard Hinnant, public domain).
    let days_from_unix = days_from_civil(year, month as i32, day as i32)?;
    let secs = days_from_unix
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(tz_minutes * 60)?;
    // Subsecond precision is intentionally discarded: the caller uses
    // `CAST(u.created_at AS INTEGER)` for numeric comparison, which truncates
    // fractional seconds anyway. We still validate `frac_str` above so that
    // malformed inputs like `T00:00:00.5xZ` are rejected.
    let _ = frac_str;
    Some(secs)
}

/// Howard Hinnant's days_from_civil (public domain). Returns days since 1970-01-01.
fn days_from_civil(y: i64, m: i32, d: i32) -> Option<i64> {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64; // [0, 146096]
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_civil`]: days since 1970-01-01 -> `(year, month, day)`
/// (Howard Hinnant, public domain). Used to format epoch seconds as ISO-8601.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d as u32)
}

/// Format non-negative Unix epoch seconds as canonical ISO-8601 UTC
/// `YYYY-MM-DDTHH:MM:SSZ` (no subseconds, `Z` suffix). Public so the app-core
/// reducer can produce display timestamps without duplicating the calendar
/// algorithm; storage stays epoch-seconds (see [`now_epoch_secs`]).
pub fn epoch_secs_to_iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = (secs % 86_400) as u64;
    let h = tod / 3600;
    let m = (tod % 3600) / 60;
    let s = tod % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Normalize a stored instant -- decimal epoch seconds OR any ISO-8601 (with
/// optional offset/subseconds) -- into canonical ISO-8601 UTC for display.
/// Used at usage read boundaries so the dock and summary panels show readable
/// times regardless of the source row's format. Storage itself stays
/// epoch-seconds to keep the `CAST(created_at AS INTEGER)` date filter working
/// without a data migration.
pub fn instant_to_iso_utc(value: &str) -> String {
    match parse_instant_to_epoch_secs(value) {
        Some(secs) if secs >= 0 => epoch_secs_to_iso_utc(secs as u64),
        _ => value.trim().to_string(),
    }
}

/// Normalize a stored instant into a UTC calendar date `YYYY-MM-DD` for
/// daily usage bucketing. Returns `None` for inputs that cannot be parsed as
/// epoch seconds or ISO-8601, so callers can skip unbucketable rows.
pub fn instant_to_date_utc(value: &str) -> Option<String> {
    let secs = parse_instant_to_epoch_secs(value)?;
    if secs < 0 {
        return None;
    }
    let days = (secs as u64) / 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    Some(format!("{y:04}-{mo:02}-{d:02}"))
}

pub(super) fn cap_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let boundary = (0..=max_bytes)
            .rev()
            .find(|index| s.is_char_boundary(*index))
            .unwrap_or(0);
        s[..boundary].to_string()
    }
}

pub(super) fn decode_json_vec<T>(json: Option<&str>) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    json.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{epoch_secs_to_iso_utc, instant_to_iso_utc, parse_instant_to_epoch_secs};

    #[test]
    fn epoch_secs_format_iso_utc() {
        assert_eq!(epoch_secs_to_iso_utc(0), "1970-01-01T00:00:00Z");
        // Round-trips with parses_iso_utc_zulu: 2026-06-30T00:00:00Z == 1_782_777_600.
        assert_eq!(epoch_secs_to_iso_utc(1_782_777_600), "2026-06-30T00:00:00Z");
    }

    #[test]
    fn instant_to_iso_utc_normalizes_mixed_formats() {
        assert_eq!(instant_to_iso_utc("1782777600"), "2026-06-30T00:00:00Z");
        // Offset normalized to UTC.
        assert_eq!(instant_to_iso_utc("2026-06-30T08:00:00+08:00"), "2026-06-30T00:00:00Z");
        // Subseconds dropped.
        assert_eq!(instant_to_iso_utc("2026-06-30T00:00:00.123Z"), "2026-06-30T00:00:00Z");
        // Already-canonical ISO passes through unchanged.
        assert_eq!(instant_to_iso_utc("2026-06-30T00:00:00Z"), "2026-06-30T00:00:00Z");
        // Unparseable input is returned trimmed, not corrupted.
        assert_eq!(instant_to_iso_utc("  yesterday  "), "yesterday");
    }

    #[test]
    fn parses_decimal_seconds() {
        assert_eq!(parse_instant_to_epoch_secs("0"), Some(0));
        assert_eq!(parse_instant_to_epoch_secs("1751328000"), Some(1_751_328_000));
        assert_eq!(parse_instant_to_epoch_secs("  1751328000  "), Some(1_751_328_000));
    }

    #[test]
    fn parses_iso_utc_zulu() {
        // 2026-06-30T00:00:00Z == 1_782_777_600 (verified independently).
        let v = parse_instant_to_epoch_secs("2026-06-30T00:00:00Z").unwrap();
        assert_eq!(v, 1_782_777_600);
    }

    #[test]
    fn parses_iso_with_offset() {
        // 2026-06-30T08:00:00+08:00 == 2026-06-30T00:00:00Z
        let cn = parse_instant_to_epoch_secs("2026-06-30T08:00:00+08:00").unwrap();
        let utc = parse_instant_to_epoch_secs("2026-06-30T00:00:00Z").unwrap();
        assert_eq!(cn, utc);
    }

    #[test]
    fn parses_iso_compact_offset() {
        // 2026-06-30T08:00:00+0800 == 2026-06-30T00:00:00Z
        let cn = parse_instant_to_epoch_secs("2026-06-30T08:00:00+0800").unwrap();
        let utc = parse_instant_to_epoch_secs("2026-06-30T00:00:00Z").unwrap();
        assert_eq!(cn, utc);
    }

    #[test]
    fn parses_iso_negative_offset() {
        // 2026-06-29T20:00:00-04:00 == 2026-06-30T00:00:00Z
        let east = parse_instant_to_epoch_secs("2026-06-29T20:00:00-04:00").unwrap();
        let utc = parse_instant_to_epoch_secs("2026-06-30T00:00:00Z").unwrap();
        assert_eq!(east, utc);
    }

    #[test]
    fn parses_iso_with_subseconds() {
        // Fractional seconds are accepted but truncated to integer seconds.
        let a = parse_instant_to_epoch_secs("2026-06-30T00:00:00.123Z").unwrap();
        let b = parse_instant_to_epoch_secs("2026-06-30T00:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parses_leap_day() {
        // 2024-02-29T00:00:00Z exists.
        let v = parse_instant_to_epoch_secs("2024-02-29T00:00:00Z");
        assert!(v.is_some());
    }

    #[test]
    fn rejects_malformed_inputs() {
        assert_eq!(parse_instant_to_epoch_secs(""), None);
        assert_eq!(parse_instant_to_epoch_secs("   "), None);
        assert_eq!(parse_instant_to_epoch_secs("yesterday"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-06-30T25:00:00Z"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-06-30T00:60:00Z"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-06-30T00:00:60Z"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-06-30T00:00:00.5xZ"), None);
        assert_eq!(parse_instant_to_epoch_secs("2026-06-30"), None);
    }
}
