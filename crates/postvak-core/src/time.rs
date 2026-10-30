//! Unix timestamps: the current one, and shown in local time.
use chrono::TimeZone;

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

    /// The moment this span after `from`; `None` when that lies outside the calendar.
    pub fn after(self, from: i64) -> Option<i64> {
        match self {
            Span::Seconds(seconds) => from.checked_add(i64::try_from(seconds).ok()?),
            Span::Months(months) => {
                let at = chrono::Local.timestamp_opt(from, 0).single()?;
                Some(
                    at.checked_add_months(chrono::Months::new(months))?
                        .timestamp(),
                )
            }
        }
    }

    /// The moment this span before `now`; `None` when that lies outside the calendar.
    pub fn before(self, now: i64) -> Option<i64> {
        match self {
            Span::Seconds(seconds) => now.checked_sub(i64::try_from(seconds).ok()?),
            Span::Months(months) => months_back(now, months),
        }
    }
}

/// The same clock time `months` months before `now`, in local time: the calendar moves, not a fixed day count.
pub(crate) fn months_back(now: i64, months: u32) -> Option<i64> {
    let at = chrono::Local.timestamp_opt(now, 0).single()?;
    // A day the earlier month lacks becomes its last day.
    let earlier = at.checked_sub_months(chrono::Months::new(months))?;
    Some(earlier.timestamp())
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
    }
}
