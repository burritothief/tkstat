use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

#[derive(Clone)]
enum ReportTimeZone {
    Iana(Tz),
    System,
}

pub fn register_local_bucket_function(conn: &Connection) -> Result<()> {
    register_bucket_function(conn, resolve_timezone())
}

#[cfg(test)]
pub(crate) fn register_local_bucket_function_for_timezone(
    conn: &Connection,
    timezone: Tz,
) -> Result<()> {
    register_bucket_function(conn, ReportTimeZone::Iana(timezone))
}

fn register_bucket_function(conn: &Connection, timezone: ReportTimeZone) -> Result<()> {
    let timezone = Arc::new(timezone);
    conn.create_scalar_function(
        "tkstat_local_bucket",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |context| {
            let timestamp = context.get_raw(0).as_str()?;
            let period = context.get_raw(1).as_str()?;
            let timestamp = DateTime::parse_from_rfc3339(timestamp)
                .map_err(|err| rusqlite::Error::UserFunctionError(Box::new(err)))?
                .with_timezone(&Utc);
            match timezone.as_ref() {
                ReportTimeZone::Iana(timezone) => {
                    Ok(format_bucket(timestamp.with_timezone(timezone), period))
                }
                ReportTimeZone::System => {
                    Ok(format_bucket(timestamp.with_timezone(&Local), period))
                }
            }
        },
    )
    .context("registering local report-timezone SQLite function")?;
    Ok(())
}

fn resolve_timezone() -> ReportTimeZone {
    let configured = std::env::var("TZ")
        .ok()
        .map(|value| value.trim_start_matches(':').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| iana_time_zone::get_timezone().ok());
    configured
        .and_then(|name| name.parse::<Tz>().ok())
        .map(ReportTimeZone::Iana)
        .unwrap_or(ReportTimeZone::System)
}

fn format_bucket<T: chrono::TimeZone>(timestamp: DateTime<T>, period: &str) -> String
where
    T::Offset: std::fmt::Display,
{
    match period {
        "five_minutes" => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            timestamp.year(),
            timestamp.month(),
            timestamp.day(),
            timestamp.hour(),
            (timestamp.minute() / 5) * 5
        ),
        "hour" => format!(
            "{:04}-{:02}-{:02} {:02}:00",
            timestamp.year(),
            timestamp.month(),
            timestamp.day(),
            timestamp.hour()
        ),
        "day" => format!(
            "{:04}-{:02}-{:02}",
            timestamp.year(),
            timestamp.month(),
            timestamp.day()
        ),
        "month" => format!("{:04}-{:02}", timestamp.year(), timestamp.month()),
        "year" => format!("{:04}", timestamp.year()),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bucket_uses_iana_dst_rules() {
        let timezone: Tz = "America/Los_Angeles".parse().unwrap();
        let before: DateTime<Utc> = "2026-03-08T09:30:00Z".parse().unwrap();
        let after: DateTime<Utc> = "2026-03-08T10:30:00Z".parse().unwrap();
        assert_eq!(
            format_bucket(before.with_timezone(&timezone), "hour"),
            "2026-03-08 01:00"
        );
        assert_eq!(
            format_bucket(after.with_timezone(&timezone), "hour"),
            "2026-03-08 03:00"
        );
    }

    #[test]
    fn registered_function_buckets_canonical_timestamp() {
        let conn = Connection::open_in_memory().unwrap();
        register_local_bucket_function(&conn).unwrap();
        let day: String = conn
            .query_row(
                "SELECT tkstat_local_bucket('2026-05-24T00:40:02+00:00', 'day')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(day.len(), 10);
    }
}
