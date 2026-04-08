use std::collections::HashMap;

use anyhow::Result;
use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeDelta};
use rusqlite::Connection;

use crate::domain::period::TimePeriod;
use crate::domain::usage::AggregatedRow;

/// Filter parameters for queries.
#[derive(Debug, Default)]
pub struct QueryFilter {
    pub begin: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub model: Option<String>,
    pub project: Option<String>,
    pub session: Option<String>,
    pub include_subagents: bool,
}

/// Query aggregated usage by time period.
pub fn query_by_period(
    conn: &Connection,
    period: TimePeriod,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    let group_expr = period.sql_group_expr();
    let (where_clause, params) = build_where_clause(filter);

    let sql = format!(
        "SELECT
            {group_expr} AS period,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(total_tokens),
            SUM(cost_usd),
            COUNT(*),
            COUNT(DISTINCT session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY {group_expr}
         ORDER BY {group_expr} ASC",
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let result: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), row_to_aggregated)?
        .collect::<Result<Vec<_>, _>>()?;

    let filled = fill_gaps(period, result);

    let limit = limit as usize;
    if filled.len() > limit {
        Ok(filled[filled.len() - limit..].to_vec())
    } else {
        Ok(filled)
    }
}

/// Query top days by total tokens.
pub fn query_top(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    let (where_clause, params) = build_where_clause(filter);

    let sql = format!(
        "SELECT
            date(timestamp, 'localtime') AS period,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(total_tokens),
            SUM(cost_usd),
            COUNT(*),
            COUNT(DISTINCT session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY date(timestamp, 'localtime')
         ORDER BY SUM(total_tokens) DESC
         LIMIT ?"
    );

    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = params;
    all_params.push(Box::new(limit));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), row_to_aggregated)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Compute a single summary row across all data matching the filter.
pub fn query_summary(conn: &Connection, filter: &QueryFilter) -> Result<AggregatedRow> {
    let (where_clause, params) = build_where_clause(filter);

    let sql = format!(
        "SELECT
            'total' AS period,
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(cache_creation_tokens), 0),
            COALESCE(SUM(cache_read_tokens), 0),
            COALESCE(SUM(total_tokens), 0),
            COALESCE(SUM(cost_usd), 0.0),
            COUNT(*),
            COUNT(DISTINCT session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt.query_row(param_refs.as_slice(), row_to_aggregated)?)
}

/// Daily totals for heatmap and chart rendering.
#[derive(Debug)]
pub struct DailyTotal {
    pub date: String,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Query daily totals for heatmap and chart rendering.
pub fn query_daily_totals(conn: &Connection, filter: &QueryFilter) -> Result<Vec<DailyTotal>> {
    let (where_clause, params) = build_where_clause(filter);

    let sql = format!(
        "SELECT
            date(timestamp, 'localtime') AS day,
            SUM(total_tokens),
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cost_usd)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY date(timestamp, 'localtime')
         ORDER BY day ASC"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(DailyTotal {
                date: row.get(0)?,
                total_tokens: row.get::<_, i64>(1).map(|v| v.max(0) as u64)?,
                input_tokens: row.get::<_, i64>(2).map(|v| v.max(0) as u64)?,
                output_tokens: row.get::<_, i64>(3).map(|v| v.max(0) as u64)?,
                cost_usd: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Shared row mapper for all aggregation queries.
fn row_to_aggregated(row: &rusqlite::Row<'_>) -> rusqlite::Result<AggregatedRow> {
    let safe_u64 = |v: i64| -> u64 { v.max(0) as u64 };
    Ok(AggregatedRow {
        period: row.get(0)?,
        input_tokens: row.get::<_, i64>(1).map(safe_u64)?,
        output_tokens: row.get::<_, i64>(2).map(safe_u64)?,
        cache_creation_tokens: row.get::<_, i64>(3).map(safe_u64)?,
        cache_read_tokens: row.get::<_, i64>(4).map(safe_u64)?,
        total_tokens: row.get::<_, i64>(5).map(safe_u64)?,
        cost_usd: row.get(6)?,
        request_count: row.get::<_, i64>(7).map(safe_u64)?,
        session_count: row.get::<_, i64>(8).map(safe_u64)?,
    })
}

// -- Gap filling --

fn fill_gaps(period: TimePeriod, rows: Vec<AggregatedRow>) -> Vec<AggregatedRow> {
    if rows.len() < 2 {
        return rows;
    }

    let mut by_label: HashMap<String, AggregatedRow> = HashMap::new();
    for row in &rows {
        by_label.insert(row.period.clone(), row.clone());
    }

    let first = &rows[0].period;
    let last = &rows[rows.len() - 1].period;

    let labels = match period {
        TimePeriod::FiveMinutes => generate_time_labels(
            first,
            last,
            "%Y-%m-%d %H:%M",
            TimeDelta::minutes(5),
            "%Y-%m-%d %H:%M",
        ),
        TimePeriod::Hourly => generate_time_labels(
            first,
            last,
            "%Y-%m-%d %H:%M",
            TimeDelta::hours(1),
            "%Y-%m-%d %H:00",
        ),
        TimePeriod::Daily => {
            generate_time_labels(first, last, "%Y-%m-%d", TimeDelta::days(1), "%Y-%m-%d")
        }
        TimePeriod::Monthly => generate_monthly_labels(first, last),
        TimePeriod::Yearly => return rows,
    };

    let Some(labels) = labels else { return rows };

    labels
        .into_iter()
        .map(|label| {
            by_label.remove(&label).unwrap_or(AggregatedRow {
                period: label,
                ..Default::default()
            })
        })
        .collect()
}

/// Generate gap-filling labels for sub-daily and daily periods.
/// Parses first/last with `parse_fmt`, steps by `delta`, formats output with `out_fmt`.
fn generate_time_labels(
    first: &str,
    last: &str,
    parse_fmt: &str,
    delta: TimeDelta,
    out_fmt: &str,
) -> Option<Vec<String>> {
    let parse = |s: &str| -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(s, parse_fmt)
            .ok()
            .or_else(|| {
                NaiveDate::parse_from_str(s, parse_fmt)
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
            })
    };
    let start = parse(first)?;
    let end = parse(last)?;
    let mut labels = Vec::new();
    let mut current = start;
    while current <= end {
        labels.push(current.format(out_fmt).to_string());
        current += delta;
    }
    Some(labels)
}

fn generate_monthly_labels(first: &str, last: &str) -> Option<Vec<String>> {
    let start = NaiveDate::parse_from_str(&format!("{first}-01"), "%Y-%m-%d").ok()?;
    let end = NaiveDate::parse_from_str(&format!("{last}-01"), "%Y-%m-%d").ok()?;
    let mut labels = Vec::new();
    let mut current = start;
    while current <= end {
        labels.push(current.format("%Y-%m").to_string());
        current = if current.month() == 12 {
            NaiveDate::from_ymd_opt(current.year() + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(current.year(), current.month() + 1, 1)?
        };
    }
    Some(labels)
}

// -- WHERE clause builder --

fn build_where_clause(filter: &QueryFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref begin) = filter.begin {
        params.push(Box::new(begin.to_string()));
        clauses.push(format!(
            "AND date(timestamp, 'localtime') >= ?{}",
            params.len()
        ));
    }

    if let Some(ref end) = filter.end {
        params.push(Box::new(end.to_string()));
        clauses.push(format!(
            "AND date(timestamp, 'localtime') <= ?{}",
            params.len()
        ));
    }

    if let Some(ref model) = filter.model {
        params.push(Box::new(model.clone()));
        clauses.push(format!("AND model_family = ?{}", params.len()));
    }

    if let Some(ref project) = filter.project {
        params.push(Box::new(format!("%{project}%")));
        clauses.push(format!("AND project LIKE ?{}", params.len()));
    }

    if let Some(ref session) = filter.session {
        params.push(Box::new(format!("{session}%")));
        clauses.push(format!("AND session_id LIKE ?{}", params.len()));
    }

    if !filter.include_subagents {
        clauses.push(" AND is_subagent = 0".into());
    }

    (clauses.join(" "), params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::usage::ModelFamily;

    fn seed_db(db: &Database) {
        let records = vec![
            make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 100, 50),
            make_record("r2", "2026-04-05T14:00:00Z", "sonnet", "proj-a", 200, 80),
            make_record("r3", "2026-04-06T09:00:00Z", "opus", "proj-b", 300, 120),
            make_record("r4", "2026-04-07T11:00:00Z", "haiku", "proj-a", 50, 20),
            make_record("r5", "2026-04-07T15:00:00Z", "opus", "proj-a", 500, 200),
        ];
        db.insert_records(&records).unwrap();
    }

    fn make_record(
        id: &str,
        ts: &str,
        family: &str,
        project: &str,
        input: u64,
        output: u64,
    ) -> crate::domain::usage::TokenRecord {
        crate::domain::usage::TokenRecord {
            request_id: id.into(),
            session_id: "s1".into(),
            uuid: format!("u-{id}"),
            timestamp: ts.parse().unwrap(),
            model: family.parse().unwrap_or(ModelFamily::Unknown),
            model_raw: format!("claude-{family}-4-6"),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: (input as f64 * 15.0 + output as f64 * 75.0) / 1_000_000.0,
            project: project.into(),
            source_file: "/test.jsonl".into(),
            is_subagent: false,
        }
    }

    #[test]
    fn test_query_daily() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        assert!(rows.len() >= 2);
    }

    #[test]
    fn test_query_with_model_filter() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            model: Some("opus".into()),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        assert_eq!(total_input, 100 + 300 + 500);
    }

    #[test]
    fn test_query_with_project_filter() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            project: Some("proj-b".into()),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        assert_eq!(total_input, 300);
    }

    #[test]
    fn test_query_top() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_top(db.conn(), &filter, 10).unwrap();
        assert!(!rows.is_empty());
        // Verify descending order by total_tokens
        for w in rows.windows(2) {
            assert!(
                w[0].total_tokens >= w[1].total_tokens,
                "expected descending order: {} >= {}",
                w[0].total_tokens,
                w[1].total_tokens
            );
        }
    }

    #[test]
    fn test_query_summary() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 5);
        assert_eq!(summary.input_tokens, 100 + 200 + 300 + 50 + 500);
    }

    #[test]
    fn test_query_date_range() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            begin: NaiveDate::from_ymd_opt(2026, 4, 6),
            end: NaiveDate::from_ymd_opt(2026, 4, 6),
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.input_tokens, 300);
    }

    #[test]
    fn test_query_empty_db() {
        let db = Database::open_in_memory().unwrap();
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        assert!(rows.is_empty());
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn test_gap_fill_daily_labels() {
        let labels = generate_time_labels(
            "2026-04-01",
            "2026-04-05",
            "%Y-%m-%d",
            TimeDelta::days(1),
            "%Y-%m-%d",
        )
        .unwrap();
        assert_eq!(labels.len(), 5);
        assert_eq!(labels[0], "2026-04-01");
        assert_eq!(labels[4], "2026-04-05");
    }

    #[test]
    fn test_gap_fill_hourly_labels() {
        let labels = generate_time_labels(
            "2026-04-01 10:00",
            "2026-04-01 14:00",
            "%Y-%m-%d %H:%M",
            TimeDelta::hours(1),
            "%Y-%m-%d %H:00",
        )
        .unwrap();
        assert_eq!(labels.len(), 5);
    }

    #[test]
    fn test_gap_fill_5min_labels() {
        let labels = generate_time_labels(
            "2026-04-01 10:00",
            "2026-04-01 10:20",
            "%Y-%m-%d %H:%M",
            TimeDelta::minutes(5),
            "%Y-%m-%d %H:%M",
        )
        .unwrap();
        assert_eq!(labels.len(), 5);
        assert_eq!(labels[1], "2026-04-01 10:05");
    }

    #[test]
    fn test_gap_fill_monthly_labels() {
        let labels = generate_monthly_labels("2026-01", "2026-04").unwrap();
        assert_eq!(labels.len(), 4);
    }

    #[test]
    fn test_gap_fill_inserts_zero_rows() {
        let rows = vec![
            AggregatedRow {
                period: "2026-04-01".into(),
                request_count: 5,
                total_tokens: 100,
                ..Default::default()
            },
            AggregatedRow {
                period: "2026-04-03".into(),
                request_count: 3,
                total_tokens: 200,
                ..Default::default()
            },
        ];
        let filled = fill_gaps(TimePeriod::Daily, rows);
        assert_eq!(filled.len(), 3);
        assert_eq!(filled[1].period, "2026-04-02");
        assert_eq!(filled[1].request_count, 0);
    }

    #[test]
    fn test_limit_applied_after_gap_fill() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 2).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_query_daily_totals() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let totals = query_daily_totals(db.conn(), &filter).unwrap();
        assert!(totals.len() >= 2);
        assert!(totals.first().unwrap().date <= totals.last().unwrap().date);
    }
}
