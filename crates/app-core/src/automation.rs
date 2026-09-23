//! Automation (定时任务) schedule math.
//!
//! Wall-clock schedules (`daily` / `weekly`) are resolved in the machine's
//! LOCAL timezone (`chrono::Local`), matching what the user means by
//! "每天 09:30". All timestamps crossing this module's API are epoch
//! milliseconds.

use workspace_model::{AutomationSchedule, AutomationScheduleKind};

/// Current wall-clock time as epoch milliseconds.
pub fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

/// Current wall-clock time as epoch seconds (the session-store's numeric
/// timestamp storage format).
pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Smallest firing instant (epoch milliseconds) strictly after `after_ms`
/// allowed by `schedule`. Returns `None` when the schedule can never fire
/// again (a one-shot whose moment passed, or an invalid schedule).
///
/// After a firing the scheduler recomputes the next occurrence with
/// `after_ms` set to the firing time, so a missed window is skipped rather
/// than replayed in a burst.
pub fn next_run_at_ms(schedule: &AutomationSchedule, after_ms: i64) -> Option<i64> {
    match schedule.kind {
        AutomationScheduleKind::Once => {
            let at = schedule.run_at_ms?;
            (at > after_ms).then_some(at)
        }
        AutomationScheduleKind::Interval => {
            let minutes = i64::from(schedule.interval_minutes.unwrap_or(0)).max(1);
            Some(after_ms.saturating_add(minutes * 60_000))
        }
        AutomationScheduleKind::Daily => {
            next_wall_clock_run_ms(after_ms, schedule.hour?, schedule.minute?, None)
        }
        AutomationScheduleKind::Weekly => {
            next_wall_clock_run_ms(
                after_ms,
                schedule.hour?,
                schedule.minute?,
                Some(schedule.weekday?),
            )
        }
    }
}

/// Next local `hour`:`minute` strictly after `after_ms`, optionally
/// restricted to an ISO weekday (1 = Monday … 7 = Sunday).
fn next_wall_clock_run_ms(
    after_ms: i64,
    hour: u32,
    minute: u32,
    weekday: Option<u32>,
) -> Option<i64> {
    use chrono::{Datelike, Duration, Local, NaiveTime, TimeZone};

    if hour > 23 || minute > 59 {
        return None;
    }
    if let Some(weekday) = weekday && !(1..=7).contains(&weekday) {
        return None;
    }
    let after = Local.timestamp_millis_opt(after_ms).single()?;
    let after_naive = after.naive_local();
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    // 370 days covers every weekly slot even across DST transitions and
    // short months; a valid slot is always found within a week.
    for day_offset in 0..370i64 {
        let Some(date) = after_naive.date().checked_add_signed(Duration::days(day_offset)) else {
            break;
        };
        if let Some(weekday) = weekday
            && date.weekday().number_from_monday() != weekday
        {
            continue;
        }
        let candidate = date.and_time(time);
        if candidate <= after_naive {
            continue;
        }
        // `earliest` resolves DST-ambiguous local times and skips the rare
        // spring-forward gap (that day simply has no such wall-clock time).
        let Some(dt) = Local.from_local_datetime(&candidate).earliest() else {
            continue;
        };
        return Some(dt.timestamp_millis());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Duration, Local, NaiveDateTime, TimeZone, Timelike};

    fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        let naive = NaiveDateTime::new(
            chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap(),
            chrono::NaiveTime::from_hms_opt(hour, minute, 0).unwrap(),
        );
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp_millis()
    }

    fn local_parts(ms: i64) -> (u32, u32, u32, u32) {
        let dt = Local.timestamp_millis_opt(ms).single().unwrap();
        (
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
        )
    }

    fn daily(hour: u32, minute: u32) -> AutomationSchedule {
        AutomationSchedule {
            kind: AutomationScheduleKind::Daily,
            hour: Some(hour),
            minute: Some(minute),
            ..Default::default()
        }
    }

    #[test]
    fn once_fires_only_in_the_future() {
        let mut schedule = AutomationSchedule {
            kind: AutomationScheduleKind::Once,
            run_at_ms: Some(1_000),
            ..Default::default()
        };
        assert_eq!(next_run_at_ms(&schedule, 500), Some(1_000));
        assert_eq!(next_run_at_ms(&schedule, 1_000), None);
        assert_eq!(next_run_at_ms(&schedule, 2_000), None);
        schedule.run_at_ms = None;
        assert_eq!(next_run_at_ms(&schedule, 500), None);
    }

    #[test]
    fn interval_steps_from_the_reference_instant() {
        let schedule = AutomationSchedule {
            kind: AutomationScheduleKind::Interval,
            interval_minutes: Some(15),
            ..Default::default()
        };
        assert_eq!(next_run_at_ms(&schedule, 10_000), Some(10_000 + 900_000));
        // A zero/missing interval degrades to one minute instead of spinning.
        let zero = AutomationSchedule {
            kind: AutomationScheduleKind::Interval,
            interval_minutes: Some(0),
            ..Default::default()
        };
        assert_eq!(next_run_at_ms(&zero, 0), Some(60_000));
    }

    #[test]
    fn daily_fires_at_the_next_local_wall_clock_time() {
        let schedule = daily(9, 30);
        // 08:00 → same day 09:30.
        let after = local_ms(2026, 3, 10, 8, 0);
        let next = next_run_at_ms(&schedule, after).unwrap();
        assert_eq!(next, local_ms(2026, 3, 10, 9, 30));
        // Exactly at the slot → the NEXT day's slot (strictly after).
        let next = next_run_at_ms(&schedule, next).unwrap();
        let (month, day, hour, minute) = local_parts(next);
        assert_eq!((month, day), (3, 11));
        assert_eq!((hour, minute), (9, 30));
        // 10:00 → tomorrow 09:30.
        let after = local_ms(2026, 3, 10, 10, 0);
        let next = next_run_at_ms(&schedule, after).unwrap();
        let (month, day, hour, minute) = local_parts(next);
        assert_eq!((month, day), (3, 11));
        assert_eq!((hour, minute), (9, 30));
    }

    #[test]
    fn weekly_fires_on_the_target_weekday() {
        // 2026-03-10 is a Tuesday (ISO weekday 2). Weekly on Monday(1) 08:00.
        let schedule = AutomationSchedule {
            kind: AutomationScheduleKind::Weekly,
            hour: Some(8),
            minute: Some(0),
            weekday: Some(1),
            ..Default::default()
        };
        let after = local_ms(2026, 3, 10, 12, 0);
        let next = next_run_at_ms(&schedule, after).unwrap();
        let dt = Local.timestamp_millis_opt(next).single().unwrap();
        assert_eq!(dt.weekday().number_from_monday(), 1);
        assert_eq!((dt.day(), dt.hour(), dt.minute()), (16, 8, 0));
        // Invalid weekday never fires.
        let invalid = AutomationSchedule {
            kind: AutomationScheduleKind::Weekly,
            hour: Some(8),
            minute: Some(0),
            weekday: Some(9),
            ..Default::default()
        };
        assert_eq!(next_run_at_ms(&invalid, after), None);
    }

    #[test]
    fn daily_roll_forward_across_a_month_boundary() {
        let schedule = daily(23, 59);
        let after = local_ms(2026, 1, 31, 23, 59);
        let next = next_run_at_ms(&schedule, after).unwrap();
        let (month, day, hour, minute) = local_parts(next);
        assert_eq!((month, day), (2, 1));
        assert_eq!((hour, minute), (23, 59));
    }

    #[test]
    fn invalid_wall_clock_schedules_never_fire() {
        assert_eq!(next_run_at_ms(&daily(24, 0), 0), None);
        assert_eq!(next_run_at_ms(&daily(10, 60), 0), None);
        let missing = AutomationSchedule {
            kind: AutomationScheduleKind::Daily,
            ..Default::default()
        };
        assert_eq!(next_run_at_ms(&missing, 0), None);
    }

    #[test]
    fn every_next_run_is_strictly_in_the_future() {
        let schedule = daily(7, 15);
        let mut cursor = local_ms(2026, 6, 1, 0, 0);
        for _ in 0..5 {
            let next = next_run_at_ms(&schedule, cursor).unwrap();
            assert!(next > cursor);
            let diff_days = Duration::milliseconds(next - cursor).num_days();
            assert!(diff_days <= 1, "daily jump too large: {diff_days} days");
            cursor = next;
        }
    }
}
