//! Minimal standard cron expression parser (5-field: min hour dom mon dow).
//!
//! Supports: `*`, `*/step`, `a,b,c`, `a-b`, `a-b/step`, and plain numbers,
//! for each field. Used by the schedule tool to compute the next run time
//! from a cron expression (Hermes cronjob parity).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    minutes: Vec<u8>,
    hours: Vec<u8>,
    doms: Vec<u8>,
    months: Vec<u8>,
    dows: Vec<u8>,
}

impl CronExpr {
    /// Parse a standard 5-field cron expression.
    /// Example: `*/5 * * * *` (every 5 minutes), `0 9 * * 1-5` (weekdays 9am).
    pub fn parse(expr: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "cron expression must have 5 fields (min hour dom mon dow), got {}: {expr:?}",
                fields.len()
            ));
        }
        Ok(Self {
            minutes: parse_field(fields[0], 0, 59)?,
            hours: parse_field(fields[1], 0, 23)?,
            doms: parse_field(fields[2], 1, 31)?,
            months: parse_field(fields[3], 1, 12)?,
            dows: parse_field(fields[4], 0, 7)?, // 0 and 7 both = Sunday
        })
    }

    /// Compute the next run time (unix epoch seconds) strictly after `after`.
    pub fn next_after(&self, after: u64) -> Option<u64> {
        // Start one minute after `after` floored to the minute.
        let start = after + 60 - (after % 60);
        let mut t = start;
        for _ in 0..(5 * 365 * 24 * 60) {
            // bounded search (~5 years) to avoid infinite loops
            let dt = datetime_from_unix(t)?;
            if !self.months.contains(&dt.month) {
                t = unix_from_datetime(dt.year, dt.month + 1, 1, 0, 0)?;
                continue;
            }
            if !self.doms.contains(&dt.day) || !self.dows.contains(&dt.dow) {
                t = unix_from_datetime(dt.year, dt.month, dt.day + 1, 0, 0)?;
                continue;
            }
            if !self.hours.contains(&dt.hour) {
                t = unix_from_datetime(dt.year, dt.month, dt.day, dt.hour + 1, 0)?;
                continue;
            }
            if !self.minutes.contains(&dt.minute) {
                t += 60;
                continue;
            }
            return Some(t);
        }
        None
    }
}

/// Parse one field into a sorted list of allowed values.
fn parse_field(field: &str, min: u8, max: u8) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Split step: `*/5`, `1-10/2`, `5/15`
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u8 = s
                    .parse()
                    .map_err(|_| format!("invalid step in cron field {part:?}"))?;
                if step == 0 {
                    return Err(format!("step must be > 0 in {part:?}"));
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let a: u8 = a.trim().parse().map_err(|_| format!("invalid cron value {a:?}"))?;
            let b: u8 = b.trim().parse().map_err(|_| format!("invalid cron value {b:?}"))?;
            (a, b)
        } else {
            let v: u8 = range.trim().parse().map_err(|_| format!("invalid cron value {range:?}"))?;
            (v, v)
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!(
                "cron value out of range [{min}-{max}]: {part:?}"
            ));
        }
        let mut v = lo;
        while v <= hi {
            out.push(v);
            v = v.saturating_add(step);
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(format!("empty cron field {field:?}"));
    }
    Ok(out)
}

/// Days-in-month helper (Gregorian, no timezone — UTC).
fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 30,
    }
}

/// Civil date/time breakdown (UTC).
struct DateTime {
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    dow: u8, // 0=Sunday
}

/// Unix epoch -> civil date (UTC). `days_from_civil` inverse (Howard Hinnant).
fn datetime_from_unix(secs: u64) -> Option<DateTime> {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let hour = (rem / 3600) as u8;
    let minute = ((rem % 3600) / 60) as u8;
    // civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = if m <= 2 { y + 1 } else { y };
    // dow: 1970-01-01 was Thursday (4). days mod 7 with Monday=0 offset.
    let dow = ((days + 4) % 7 + 7) % 7;
    Some(DateTime { year, month: m, day: d, hour, minute, dow: dow as u8 })
}

/// Civil date (UTC) -> unix epoch seconds (at the given time).
fn unix_from_datetime(year: i64, month: u8, day: u8, hour: u8, minute: u8) -> Option<u64> {
    let days = days_from_civil(year, month, day)?;
    if days < 0 {
        return None;
    }
    let secs = days as u64 * 86400 + hour as u64 * 3600 + minute as u64 * 60;
    Some(secs)
}

/// Days since 1970-01-01 (Howard Hinnant `days_from_civil`).
fn days_from_civil(y: i64, m: u8, d: u8) -> Option<i64> {
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(expr: &str, after: u64) -> Option<u64> {
        CronExpr::parse(expr).unwrap().next_after(after)
    }

    #[test]
    fn parse_rejects_wrong_field_count() {
        assert!(CronExpr::parse("* * * *").is_err());
        assert!(CronExpr::parse("* * * * * *").is_err());
    }

    #[test]
    fn parse_rejects_out_of_range() {
        assert!(CronExpr::parse("60 * * * *").is_err());
        assert!(CronExpr::parse("* 24 * * *").is_err());
        assert!(CronExpr::parse("* * 32 * *").is_err());
        assert!(CronExpr::parse("* * * 13 *").is_err());
    }

    #[test]
    fn every_minute_fires_next_minute() {
        // after 12:00:30 -> next is 12:01:00
        let after = 12 * 3600 + 30; // 1970-01-01 12:00:30 UTC
        let n = next("* * * * *", after).unwrap();
        assert_eq!(n, 12 * 3600 + 60);
    }

    #[test]
    fn every_five_minutes() {
        // after 12:02:00 -> next is 12:05:00
        let after = 12 * 3600 + 2 * 60;
        let n = next("*/5 * * * *", after).unwrap();
        assert_eq!(n, 12 * 3600 + 5 * 60);
    }

    #[test]
    fn daily_at_nine() {
        // 1970-01-01 00:00:00 UTC (Thursday). Next 09:00 same day.
        let after = 0;
        let n = next("0 9 * * *", after).unwrap();
        assert_eq!(n, 9 * 3600);
    }

    #[test]
    fn weekdays_at_nine() {
        // 1970-01-01 is Thursday (dow 4), which IS in 1-5, so the next
        // weekday 09:00 is the same day Jan 1 09:00.
        let after = 0;
        let n = next("0 9 * * 1-5", after).unwrap();
        assert_eq!(n, 9 * 3600);
    }

    #[test]
    fn saturday_schedule_skips_to_monday() {
        // Jan 1 1970 = Thursday (dow 4), Jan 2 = Fri (5), Jan 3 = Sat (6),
        // Jan 4 = Sun (0), Jan 5 = Mon (1).
        // A Mon-Fri 09:00 job run on Saturday Jan 3 (day 2) must skip
        // Sat+Sun and fire Monday Jan 5 (day 4) at 09:00.
        let saturday = 2 * 86400; // Jan 3 1970 00:00 UTC
        let n = next("0 9 * * 1-5", saturday).unwrap();
        let monday = 4 * 86400 + 9 * 3600; // Jan 5 1970 09:00
        assert_eq!(n, monday);
    }

    #[test]
    fn dom_and_month_combination() {
        // Every year on Dec 25 at midnight.
        let after = 0; // Jan 1 1970
        let n = next("0 0 25 12 *", after).unwrap();
        // Dec 25 1970: days_from_civil(1970, 12, 25)
        let days = days_from_civil(1970, 12, 25).unwrap();
        assert_eq!(n, (days as u64) * 86400);
    }

    #[test]
    fn step_in_range() {
        // 0-30/10 -> minutes 0,10,20,30
        let e = CronExpr::parse("0-30/10 * * * *").unwrap();
        assert_eq!(e.minutes, vec![0, 10, 20, 30]);
    }

    #[test]
    fn list_values() {
        let e = CronExpr::parse("1,15,30 * * * *").unwrap();
        assert_eq!(e.minutes, vec![1, 15, 30]);
    }

    #[test]
    fn sunday_zero_and_seven_equivalent() {
        // 0 0 * * 0 and 0 0 * * 7 both allow Sunday.
        let e0 = CronExpr::parse("0 0 * * 0").unwrap();
        let e7 = CronExpr::parse("0 0 * * 7").unwrap();
        assert!(e0.dows.contains(&0));
        assert!(e7.dows.contains(&7));
    }
}
