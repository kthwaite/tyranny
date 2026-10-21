//! Local-time calendar maths for stats periods. Pure: the zone is a parameter.
#![allow(dead_code)] // Tasks 6-8 use this

use crate::stats_types::PeriodKind;

/// UTC offset of a local zone at a UTC instant.
pub(crate) trait Zone {
    fn offset_secs(&self, utc: i64) -> i32;
}

/// The server machine's local zone.
pub(crate) struct SystemZone;

impl Zone for SystemZone {
    fn offset_secs(&self, utc: i64) -> i32 {
        let time = utc as libc::time_t;
        // SAFETY: `tm` is plain data and `localtime_r` fills it for valid pointers.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let filled = unsafe { libc::localtime_r(&time, &mut tm) };
        if filled.is_null() {
            0
        } else {
            tm.tm_gmtoff as i32
        }
    }
}

#[cfg(test)]
pub(crate) struct FixedZone(pub i32);

#[cfg(test)]
impl Zone for FixedZone {
    fn offset_secs(&self, _utc: i64) -> i32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Parses `YYYY-MM-DD`, rejecting impossible dates.
    pub(crate) fn parse(text: &str) -> Option<Date> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| -> Option<u32> {
            let part = &text[range];
            if part.bytes().all(|b| b.is_ascii_digit()) {
                part.parse().ok()
            } else {
                None
            }
        };
        let year = digits(0..4)? as i32;
        let month = digits(5..7)? as u8;
        let day = digits(8..10)? as u8;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    fn to_days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    fn from_days(days: i64) -> Date {
        let (year, month, day) = civil_from_days(days);
        Date { year, month, day }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Local {
    pub date: Date,
    /// 0 = Monday.
    pub weekday: u8,
    pub hour: u8,
}

pub(crate) fn local(zone: &dyn Zone, utc: i64) -> Local {
    let shifted = utc + i64::from(zone.offset_secs(utc));
    let days = shifted.div_euclid(86_400);
    Local {
        date: Date::from_days(days),
        // 1970-01-01 was a Thursday.
        weekday: (days + 3).rem_euclid(7) as u8,
        hour: (shifted.rem_euclid(86_400) / 3600) as u8,
    }
}

/// UTC seconds of local midnight.
pub(crate) fn start_of_day(zone: &dyn Zone, date: Date) -> i64 {
    let l = date.to_days() * 86_400;
    l - i64::from(zone.offset_secs(l - i64::from(zone.offset_secs(l))))
}

/// UTC seconds, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub start: i64,
    pub end: i64,
}

/// Normalises `anchor` to the start of its period (weeks start on Monday).
pub(crate) fn bounds(
    zone: &dyn Zone,
    kind: PeriodKind,
    anchor: Date,
    first_play: Option<i64>,
    now: i64,
) -> Bounds {
    let (first, next) = match kind {
        PeriodKind::All => {
            return Bounds {
                start: first_play.unwrap_or(now),
                end: now + 1,
            };
        }
        PeriodKind::Day => (anchor, Date::from_days(anchor.to_days() + 1)),
        PeriodKind::Week => {
            let weekday = (anchor.to_days() + 3).rem_euclid(7);
            let monday = anchor.to_days() - weekday;
            (Date::from_days(monday), Date::from_days(monday + 7))
        }
        PeriodKind::Month => {
            let first = Date { day: 1, ..anchor };
            (first, add_months(first, 1))
        }
        PeriodKind::Year => {
            let first = Date {
                month: 1,
                day: 1,
                ..anchor
            };
            (first, add_months(first, 12))
        }
    };
    Bounds {
        start: start_of_day(zone, first),
        end: start_of_day(zone, next),
    }
}

/// Sub-bucket starts: All -> years, Year -> months, Month/Week -> days, Day -> hours.
pub(crate) fn buckets(zone: &dyn Zone, kind: PeriodKind, bounds: Bounds) -> Vec<i64> {
    let first = local(zone, bounds.start).date;
    let mut starts = Vec::new();
    match kind {
        PeriodKind::Day => {
            let mut t = bounds.start;
            while t < bounds.end {
                starts.push(t);
                t += 3600;
            }
        }
        PeriodKind::Week | PeriodKind::Month => {
            let mut days = first.to_days();
            loop {
                let t = start_of_day(zone, Date::from_days(days));
                if t >= bounds.end {
                    break;
                }
                starts.push(t);
                days += 1;
            }
        }
        PeriodKind::Year | PeriodKind::All => {
            let step = if kind == PeriodKind::Year { 1 } else { 12 };
            let mut cursor = if kind == PeriodKind::Year {
                Date { day: 1, ..first }
            } else {
                Date {
                    month: 1,
                    day: 1,
                    ..first
                }
            };
            loop {
                let t = start_of_day(zone, cursor);
                if t >= bounds.end {
                    break;
                }
                starts.push(t);
                cursor = add_months(cursor, step);
            }
        }
    }
    starts
}

fn add_months(date: Date, months: u32) -> Date {
    let index = date.year * 12 + i32::from(date.month) - 1 + months as i32;
    Date {
        year: index.div_euclid(12),
        month: index.rem_euclid(12) as u8 + 1,
        day: date.day,
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(year: i32, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    fn utc_secs(year: i32, month: u8, day: u8, hour: i64) -> i64 {
        // Midnight UTC via the zero-offset zone.
        start_of_day(&FixedZone(0), d(year, month, day)) + hour * 3600
    }

    /// Europe/London 2026: +0 before 2026-03-29T01:00Z, +3600 until 2026-10-25T01:00Z, +0 after.
    struct London;

    impl Zone for London {
        fn offset_secs(&self, utc: i64) -> i32 {
            if utc >= utc_secs(2026, 3, 29, 1) && utc < utc_secs(2026, 10, 25, 1) {
                3600
            } else {
                0
            }
        }
    }

    #[test]
    fn week_starts_on_monday() {
        let zone = FixedZone(3600);
        let b = bounds(&zone, PeriodKind::Week, d(2026, 10, 21), None, 0);
        assert_eq!(b.start, start_of_day(&zone, d(2026, 10, 19)));
        assert_eq!(b.end, start_of_day(&zone, d(2026, 10, 26)));
    }

    #[test]
    fn week_across_dst_is_169_hours() {
        let b = bounds(&London, PeriodKind::Week, d(2026, 10, 21), None, 0);
        assert_eq!(b.end - b.start, 169 * 3600);
        assert_eq!(buckets(&London, PeriodKind::Week, b).len(), 7);
    }

    #[test]
    fn month_and_year_bounds() {
        let z = FixedZone(0);
        let feb = bounds(&z, PeriodKind::Month, d(2026, 2, 10), None, 0);
        assert_eq!(feb.start, start_of_day(&z, d(2026, 2, 1)));
        assert_eq!(feb.end, start_of_day(&z, d(2026, 3, 1)));
        let leap = bounds(&z, PeriodKind::Month, d(2024, 2, 10), None, 0);
        assert_eq!((leap.end - leap.start) / 86400, 29);
        let year = bounds(&z, PeriodKind::Year, d(2026, 6, 15), None, 0);
        assert_eq!(year.start, start_of_day(&z, d(2026, 1, 1)));
        assert_eq!(year.end, start_of_day(&z, d(2027, 1, 1)));
        assert_eq!(buckets(&z, PeriodKind::Year, year).len(), 12);
        assert_eq!(buckets(&z, PeriodKind::Month, feb).len(), 28);
    }

    #[test]
    fn day_buckets_are_hours_and_dst_day_has_25() {
        let normal = bounds(&London, PeriodKind::Day, d(2026, 7, 1), None, 0);
        assert_eq!(buckets(&London, PeriodKind::Day, normal).len(), 24);
        let autumn = bounds(&London, PeriodKind::Day, d(2026, 10, 25), None, 0);
        let hours = buckets(&London, PeriodKind::Day, autumn);
        assert_eq!(hours.len(), 25);
        assert_eq!(hours[0], autumn.start);
        let spring = bounds(&London, PeriodKind::Day, d(2026, 3, 29), None, 0);
        assert_eq!(buckets(&London, PeriodKind::Day, spring).len(), 23);
    }

    #[test]
    fn all_runs_from_first_play_to_now_inclusive() {
        let z = FixedZone(0);
        let first = utc_secs(2024, 5, 10, 12);
        let now = utc_secs(2026, 10, 21, 9);
        let b = bounds(&z, PeriodKind::All, d(2026, 1, 1), Some(first), now);
        assert_eq!((b.start, b.end), (first, now + 1));
        assert_eq!(
            bounds(&z, PeriodKind::All, d(2026, 1, 1), None, now).start,
            now
        );
        let years = buckets(&z, PeriodKind::All, b);
        assert_eq!(years.len(), 3);
        assert_eq!(years[0], start_of_day(&z, d(2024, 1, 1)));
    }

    #[test]
    fn local_weekday_and_hour() {
        let l = local(
            &FixedZone(3600),
            utc_secs(2026, 10, 19, 0) + 23 * 3600 + 1800,
        );
        assert_eq!(l.date, d(2026, 10, 20));
        assert_eq!(l.weekday, 1);
        assert_eq!(l.hour, 0);
    }

    #[test]
    fn rejects_bad_anchor() {
        assert_eq!(Date::parse("2026-13-01"), None);
        assert_eq!(Date::parse("2026-02-30"), None);
        assert_eq!(Date::parse("x"), None);
        assert_eq!(Date::parse("2026-10-21"), Some(d(2026, 10, 21)));
    }

    #[test]
    fn system_zone_offset_is_plausible() {
        let off = SystemZone.offset_secs(1_800_000_000);
        assert!(off.abs() <= 14 * 3600);
    }
}
