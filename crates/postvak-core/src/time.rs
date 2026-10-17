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
