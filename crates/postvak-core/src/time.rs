//! Unix timestamps: the current one, and shown in local time.
use chrono::{Datelike, TimeZone, Timelike};

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The timestamp in local time, in a chrono `format`.
pub fn local_time(ts: i64, format: &str) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|at| at.format(format).to_string())
        .unwrap_or_default()
}

/// `YYYY-MM-DD HH:MM` in local time, as the CLI and the window's Activity and Trash views show it.
pub fn date_time(ts: i64) -> String {
    local_time(ts, "%Y-%m-%d %H:%M")
}

/// `HH:MM` in local time.
pub fn clock(ts: i64) -> String {
    local_time(ts, "%H:%M")
}

/// A span of time counted back from a moment: `30min`, `1h`, `2d`, `3w`, `6mo` or `1y`, or with the unit written out
/// (`2days`). Months and years move the calendar. A bare `m` is refused: it means minutes to some, months to others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Seconds(u64),
    Months(u32),
}

impl Span {
    pub fn parse(text: &str) -> Result<Span, String> {
        let text = text.trim();
        let digits = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (count, unit) = text.split_at(digits);
        let expected =
            || format!("expected a span such as 30min, 1h, 2d, 3w, 6mo or 1y, got '{text}'");
        let count: u64 = count.parse().map_err(|_| expected())?;
        let too_long = || format!("'{text}' is too long");
        let seconds = |per: u64| {
            count
                .checked_mul(per)
                .map(Span::Seconds)
                .ok_or_else(too_long)
        };
        let months = |per: u64| {
            count
                .checked_mul(per)
                .and_then(|months| u32::try_from(months).ok())
                .map(Span::Months)
                .ok_or_else(too_long)
        };
        match unit.trim().to_ascii_lowercase().as_str() {
            "min" | "mins" | "minute" | "minutes" => seconds(60),
            "h" | "hour" | "hours" => seconds(3_600),
            "d" | "day" | "days" => seconds(86_400),
            "w" | "week" | "weeks" => seconds(7 * 86_400),
            "mo" | "month" | "months" => months(1),
            "y" | "year" | "years" => months(12),
            "m" => Err(format!(
                "'{text}' is ambiguous: write {count}min for minutes or {count}mo for months"
            )),
            _ => Err(expected()),
        }
    }

    /// The moment this span before `now`; `None` when that lies outside the calendar.
    pub fn before(self, now: i64) -> Option<i64> {
        match self {
            Span::Seconds(seconds) => now.checked_sub(i64::try_from(seconds).ok()?),
            Span::Months(months) => months_back(now, months),
        }
    }

    /// The moment this span after `now`; `None` when that lies outside the calendar.
    pub fn after(self, now: i64) -> Option<i64> {
        match self {
            Span::Seconds(seconds) => now.checked_add(i64::try_from(seconds).ok()?),
            Span::Months(months) => months_ahead(now, months),
        }
    }
}

/// Days of the week: `mon-fri`, `sat,sun`, each day by its first three letters or in full; a range such as `fri-mon`
/// wraps past Sunday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weekdays(u8);

impl Weekdays {
    pub fn parse(text: &str) -> Result<Weekdays, String> {
        let expected = || format!("expected days such as mon-fri or sat,sun, got '{text}'");
        let day = |name: &str| {
            name.trim()
                .parse::<chrono::Weekday>()
                .map_err(|_| expected())
        };
        let mut days = 0u8;
        for part in text.split(',') {
            let (first, last) = match part.split_once('-') {
                Some((first, last)) => (day(first)?, day(last)?),
                None => (day(part)?, day(part)?),
            };
            let mut at = first;
            days |= 1 << at.num_days_from_monday();
            while at != last {
                at = at.succ();
                days |= 1 << at.num_days_from_monday();
            }
        }
        Ok(Weekdays(days))
    }

    pub fn contains(self, day: chrono::Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }
}

/// A daily window `HH:MM-HH:MM`, its start included and its end not; an end before the start wraps past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hours {
    start: u16,
    end: u16,
}

impl Hours {
    pub fn parse(text: &str) -> Result<Hours, String> {
        let expected = || format!("expected hours such as 09:00-18:00, got '{text}'");
        let minute = |clock: &str| {
            let (hours, minutes) = clock.trim().split_once(':').ok_or_else(expected)?;
            match (hours.parse::<u16>(), minutes.parse::<u16>()) {
                (Ok(h), Ok(m)) if h < 24 && m < 60 && minutes.len() == 2 => Ok(h * 60 + m),
                _ => Err(expected()),
            }
        };
        let (start, end) = text.split_once('-').ok_or_else(expected)?;
        let hours = Hours {
            start: minute(start)?,
            end: minute(end)?,
        };
        if hours.start == hours.end {
            return Err(format!("'{text}' starts where it ends"));
        }
        Ok(hours)
    }

    /// Whether `minute`, counted from midnight, falls in the window.
    pub fn contains(self, minute: u16) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

/// The local weekday of `ts` and its minute counted from midnight.
pub fn local_day_minute(ts: i64) -> Option<(chrono::Weekday, u16)> {
    let at = chrono::Local.timestamp_opt(ts, 0).single()?;
    let minute = u16::try_from(at.hour() * 60 + at.minute()).ok()?;
    Some((at.weekday(), minute))
}

/// The moment a rules preview evaluates as of, `as_of` or now, and with `as_arrival` when every message arrived: now,
/// the same now `as_of` is read against, so `15min` is exactly 15 minutes after the arrival.
pub fn preview_clock(as_of: Option<&str>, as_arrival: bool) -> Result<(i64, Option<i64>), String> {
    let now = self::now();
    let at = as_of.map_or(Ok(now), |text| self::as_of(text, now))?;
    Ok((at, as_arrival.then_some(now)))
}

/// The moment a rules preview evaluates as of: a span after `now` (`15min`, `1d`), a local date (its midnight), a
/// local `YYYY-MM-DDTHH:MM[:SS]`, or an RFC 3339 timestamp.
pub fn as_of(text: &str, now: i64) -> Result<i64, String> {
    let text = text.trim();
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(at.timestamp());
    }
    let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| midnight(text));
    if let Some(naive) = naive {
        return local(naive).ok_or_else(|| {
            format!("'{text}' does not exist in local time: a clock change skips it")
        });
    }
    let span = Span::parse(text)
        .map_err(|e| format!("{e} (or give a date such as 2026-11-01 or 2026-11-01T14:00)"))?;
    span.after(now)
        .ok_or_else(|| format!("'{text}' is too far ahead"))
}

/// The local midnight starting a `YYYY-MM-DD` date.
pub(crate) fn local_date(text: &str) -> Option<i64> {
    local(midnight(text)?)
}

fn midnight(text: &str) -> Option<chrono::NaiveDateTime> {
    let date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    Some(date.and_time(chrono::NaiveTime::MIN))
}

/// A local wall-clock time as a timestamp, the earlier one where clocks go back; `None` for one a clock change skips.
pub(crate) fn local(naive: chrono::NaiveDateTime) -> Option<i64> {
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.timestamp())
}

/// The same clock time `months` months before `now`, in local time: the calendar moves, not a fixed day count.
pub(crate) fn months_back(now: i64, months: u32) -> Option<i64> {
    let at = chrono::Local.timestamp_opt(now, 0).single()?;
    // A day the earlier month lacks becomes its last day.
    let earlier = at.checked_sub_months(chrono::Months::new(months))?;
    Some(earlier.timestamp())
}

/// The same clock time `months` months after `now`, in local time, as `months_back` counts.
fn months_ahead(now: i64, months: u32) -> Option<i64> {
    let at = chrono::Local.timestamp_opt(now, 0).single()?;
    let later = at.checked_add_months(chrono::Months::new(months))?;
    Some(later.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_take_short_and_written_out_units_and_refuse_a_bare_m() {
        for (text, span) in [
            ("30min", Span::Seconds(1_800)),
            ("1h", Span::Seconds(3_600)),
            ("2 days", Span::Seconds(2 * 86_400)),
            ("3w", Span::Seconds(21 * 86_400)),
            ("6mo", Span::Months(6)),
            ("1Y", Span::Months(12)),
        ] {
            assert_eq!(Span::parse(text), Ok(span), "{text}");
        }
        assert_eq!(
            Span::parse("6m"),
            Err("'6m' is ambiguous: write 6min for minutes or 6mo for months".into())
        );
        for text in ["", "h", "1x", "-1d", "2026-01-15", "99999999999999999999d"] {
            assert!(Span::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_span_beyond_the_calendar_has_no_moment() {
        assert_eq!(Span::Seconds(60).before(1_000), Some(940));
        assert_eq!(Span::parse("1000000y").unwrap().before(0), None);
        assert_eq!(Span::Seconds(u64::MAX).before(0), None);
        assert_eq!(Span::Seconds(u64::MAX).after(0), None);
    }

    #[test]
    fn as_of_takes_a_span_ahead_a_local_date_or_time_or_rfc_3339() {
        let now = 1_792_843_200;
        assert_eq!(as_of("15min", now), Ok(now + 900));
        assert_eq!(as_of(" 1d ", now), Ok(now + 86_400));
        let month = as_of("1mo", now).unwrap();
        // A DST change in between moves it by an hour.
        assert!((28 * 86_400 - 3_600..=31 * 86_400 + 3_600).contains(&(month - now)));
        let midnight = chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(as_of("2026-11-01", now), Ok(local(midnight).unwrap()));
        assert_eq!(
            as_of("2026-11-01T14:30", now),
            Ok(local(midnight).unwrap() + 14 * 3_600 + 30 * 60)
        );
        assert_eq!(
            as_of("2026-11-01T14:30:15", now),
            Ok(local(midnight).unwrap() + 14 * 3_600 + 30 * 60 + 15)
        );
        assert_eq!(as_of("2026-10-24T12:00:00Z", 0), Ok(now));
        assert_eq!(as_of("2026-10-24T14:00:00+02:00", 0), Ok(now));
        assert_eq!(
            as_of("6m", now),
            Err("'6m' is ambiguous: write 6min for minutes or 6mo for months (or give a date such as 2026-11-01 or 2026-11-01T14:00)".into())
        );
        assert_eq!(
            as_of("1000000y", now),
            Err("'1000000y' is too far ahead".into())
        );
        for text in ["", "soon", "-1d", "2026-13-01", "2026-11-01 14:00"] {
            assert!(as_of(text, now).is_err(), "{text}");
        }
    }

    #[test]
    fn weekdays_take_days_ranges_and_lists_and_wrap_past_sunday() {
        use chrono::Weekday::*;
        let days = |text: &str| {
            Weekdays::parse(text).map(|days| {
                [Mon, Tue, Wed, Thu, Fri, Sat, Sun]
                    .into_iter()
                    .filter(|day| days.contains(*day))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(days("mon-fri"), Ok(vec![Mon, Tue, Wed, Thu, Fri]));
        assert_eq!(days("Sat, sunday"), Ok(vec![Sat, Sun]));
        assert_eq!(days("fri-mon"), Ok(vec![Mon, Fri, Sat, Sun]));
        assert_eq!(days("wed"), Ok(vec![Wed]));
        assert_eq!(days("mon-tue,thu"), Ok(vec![Mon, Tue, Thu]));
        for text in ["", "weekend", "mon-", "mon,,tue", "mon-tue-wed"] {
            assert!(Weekdays::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn hours_include_the_start_not_the_end_and_wrap_past_midnight() {
        let at = |h: u16, m: u16| h * 60 + m;
        let day = Hours::parse("09:00-18:00").unwrap();
        assert!(day.contains(at(9, 0)) && day.contains(at(17, 59)));
        assert!(!day.contains(at(8, 59)) && !day.contains(at(18, 0)));
        let night = Hours::parse(" 22:00 - 7:30 ").unwrap();
        assert!(night.contains(at(23, 0)) && night.contains(at(0, 0)) && night.contains(at(7, 29)));
        assert!(!night.contains(at(7, 30)) && !night.contains(at(12, 0)));
        let evening = Hours::parse("18:00-00:00").unwrap();
        assert!(evening.contains(at(23, 59)) && !evening.contains(at(0, 0)));
        for text in [
            "",
            "9-17",
            "09:00",
            "09:00-09:00",
            "24:00-01:00",
            "09:60-10:00",
            "x-y",
        ] {
            assert!(Hours::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn preview_clock_reads_as_of_against_the_arrival() {
        let (at, arrived_at) = preview_clock(Some("15min"), true).unwrap();
        assert_eq!(arrived_at.map(|arrived_at| at - arrived_at), Some(900));
        let (at, arrived_at) = preview_clock(None, true).unwrap();
        assert_eq!(arrived_at, Some(at));
        assert_eq!(preview_clock(Some("15min"), false).unwrap().1, None);
        assert!(preview_clock(Some("soon"), true).is_err());
    }
}
