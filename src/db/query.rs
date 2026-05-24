use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use rusqlite::Connection;

use crate::domain::period::TimePeriod;
use crate::domain::pricing::{
    BillableTokenExpression, TokenCategory, TokenCountField, billable_token_categories_for_counts,
    billable_token_rule, default_billing_rules, provider_billing_policies,
};
use crate::domain::provider::ProviderId;
use crate::domain::usage::{AggregatedRow, ModelFamily};

/// Filter parameters for queries.
#[derive(Debug, Default, Clone)]
pub struct QueryFilter {
    pub begin: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub provider: Option<ProviderId>,
    pub model: Option<String>,
    pub model_family: Option<String>,
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
    query_by_period_with_cost_requirement(conn, period, filter, limit, true)
}

pub fn query_by_period_with_cost_requirement(
    conn: &Connection,
    period: TimePeriod,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let group_expr = period_group_expr(period);
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);

    let sql = format!(
        "SELECT
            {group_expr} AS period,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            SUM({cost_expr}),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY {group_expr}
         ORDER BY {group_expr} ASC",
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let result: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), row_to_aggregated)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

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
    query_top_with_cost_requirement(conn, filter, limit, true)
}

pub fn query_top_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);
    let daily_expr = utc_day_expr();

    let sql = format!(
        "SELECT
            {daily_expr} AS period,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            SUM({cost_expr}),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY {daily_expr}
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

/// Query aggregated usage grouped by provider plus exact model id.
pub fn query_by_model(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    query_by_model_with_cost_requirement(conn, filter, limit, true)
}

pub fn query_by_model_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);

    let sql = format!(
        "SELECT
            provider,
            model_id,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            SUM({cost_expr}),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY provider, model_id
         ORDER BY SUM(total_tokens) DESC, provider ASC, model_id ASC
         LIMIT ?"
    );

    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = params;
    all_params.push(Box::new(limit));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let provider: String = row.get(0)?;
            let model_id: String = row.get(1)?;
            Ok(AggregatedRow {
                period: format!("{provider}/{model_id}"),
                provider: Some(provider),
                model_id: Some(model_id),
                project: None,
                input_tokens: row.get::<_, i64>(2).map(|v| v.max(0) as u64)?,
                output_tokens: row.get::<_, i64>(3).map(|v| v.max(0) as u64)?,
                cache_creation_tokens: row.get::<_, i64>(4).map(|v| v.max(0) as u64)?,
                cache_read_tokens: row.get::<_, i64>(5).map(|v| v.max(0) as u64)?,
                cached_input_tokens: row.get::<_, i64>(6).map(|v| v.max(0) as u64)?,
                reasoning_output_tokens: row.get::<_, i64>(7).map(|v| v.max(0) as u64)?,
                total_tokens: row.get::<_, i64>(8).map(|v| v.max(0) as u64)?,
                cost_usd: row.get(9)?,
                request_count: row.get::<_, i64>(10).map(|v| v.max(0) as u64)?,
                session_count: row.get::<_, i64>(11).map(|v| v.max(0) as u64)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Query aggregated usage grouped by provider.
pub fn query_by_provider(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    query_by_provider_with_cost_requirement(conn, filter, limit, true)
}

pub fn query_by_provider_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);

    let sql = format!(
        "SELECT
            provider,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            SUM({cost_expr}),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY provider
         ORDER BY SUM(total_tokens) DESC, provider ASC
         LIMIT ?"
    );

    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = params;
    all_params.push(Box::new(limit));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let provider: String = row.get(0)?;
            Ok(AggregatedRow {
                period: provider.clone(),
                provider: Some(provider),
                input_tokens: row.get::<_, i64>(1).map(|v| v.max(0) as u64)?,
                output_tokens: row.get::<_, i64>(2).map(|v| v.max(0) as u64)?,
                cache_creation_tokens: row.get::<_, i64>(3).map(|v| v.max(0) as u64)?,
                cache_read_tokens: row.get::<_, i64>(4).map(|v| v.max(0) as u64)?,
                cached_input_tokens: row.get::<_, i64>(5).map(|v| v.max(0) as u64)?,
                reasoning_output_tokens: row.get::<_, i64>(6).map(|v| v.max(0) as u64)?,
                total_tokens: row.get::<_, i64>(7).map(|v| v.max(0) as u64)?,
                cost_usd: row.get(8)?,
                request_count: row.get::<_, i64>(9).map(|v| v.max(0) as u64)?,
                session_count: row.get::<_, i64>(10).map(|v| v.max(0) as u64)?,
                ..Default::default()
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Query aggregated usage grouped by normalized project name.
pub fn query_by_project(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    query_by_project_with_cost_requirement(conn, filter, limit, true)
}

pub fn query_by_project_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);
    let project_expr = "COALESCE(NULLIF(project, ''), 'unknown')";

    let sql = format!(
        "SELECT
            {project_expr} AS project_name,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            SUM({cost_expr}),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY {project_expr}
         ORDER BY SUM(total_tokens) DESC, project_name ASC
         LIMIT ?"
    );

    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = params;
    all_params.push(Box::new(limit));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            let project: String = row.get(0)?;
            Ok(AggregatedRow {
                period: project.clone(),
                project: Some(project),
                input_tokens: row.get::<_, i64>(1).map(|v| v.max(0) as u64)?,
                output_tokens: row.get::<_, i64>(2).map(|v| v.max(0) as u64)?,
                cache_creation_tokens: row.get::<_, i64>(3).map(|v| v.max(0) as u64)?,
                cache_read_tokens: row.get::<_, i64>(4).map(|v| v.max(0) as u64)?,
                cached_input_tokens: row.get::<_, i64>(5).map(|v| v.max(0) as u64)?,
                reasoning_output_tokens: row.get::<_, i64>(6).map(|v| v.max(0) as u64)?,
                total_tokens: row.get::<_, i64>(7).map(|v| v.max(0) as u64)?,
                cost_usd: row.get(8)?,
                request_count: row.get::<_, i64>(9).map(|v| v.max(0) as u64)?,
                session_count: row.get::<_, i64>(10).map(|v| v.max(0) as u64)?,
                ..Default::default()
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// Compute a single summary row across all data matching the filter.
pub fn query_summary(conn: &Connection, filter: &QueryFilter) -> Result<AggregatedRow> {
    validate_pricing_coverage(conn, filter)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(true);

    let sql = format!(
        "SELECT
            'total' AS period,
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(cache_creation_tokens), 0),
            COALESCE(SUM(cache_read_tokens), 0),
            COALESCE(SUM(cached_input_tokens), 0),
            COALESCE(SUM(reasoning_output_tokens), 0),
            COALESCE(SUM(total_tokens), 0),
            COALESCE(SUM({cost_expr}), 0.0),
            COUNT(*),
            COUNT(DISTINCT provider || ':' || session_id)
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
    query_daily_totals_with_cost_requirement(conn, filter, true)
}

pub fn query_daily_totals_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    cost_required: bool,
) -> Result<Vec<DailyTotal>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_expr = cost_sql_expr(cost_required);
    let daily_expr = utc_day_expr();

    let sql = format!(
        "SELECT
            {daily_expr} AS day,
            SUM(total_tokens),
            SUM(input_tokens),
            SUM(output_tokens),
            SUM({cost_expr})
         FROM token_usage
         WHERE 1=1 {where_clause}
         GROUP BY {daily_expr}
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
        provider: None,
        model_id: None,
        project: None,
        input_tokens: row.get::<_, i64>(1).map(safe_u64)?,
        output_tokens: row.get::<_, i64>(2).map(safe_u64)?,
        cache_creation_tokens: row.get::<_, i64>(3).map(safe_u64)?,
        cache_read_tokens: row.get::<_, i64>(4).map(safe_u64)?,
        cached_input_tokens: row.get::<_, i64>(5).map(safe_u64)?,
        reasoning_output_tokens: row.get::<_, i64>(6).map(safe_u64)?,
        total_tokens: row.get::<_, i64>(7).map(safe_u64)?,
        cost_usd: row.get(8)?,
        request_count: row.get::<_, i64>(9).map(safe_u64)?,
        session_count: row.get::<_, i64>(10).map(safe_u64)?,
    })
}

fn cost_sql_expr(cost_required: bool) -> String {
    if !cost_required {
        return "0.0".into();
    }
    let terms = TokenCategory::ALL
        .into_iter()
        .map(|category| {
            format!(
                "{} * {}",
                billable_tokens_sql(category),
                price_lookup_sql(category)
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    format!("({terms}) / 1000000.0")
}

fn billable_tokens_sql(category: TokenCategory) -> String {
    let default_expr = billable_token_rule(default_billing_rules(), category).expression;
    let mut sql = token_expression_sql(default_expr);
    for policy in provider_billing_policies().iter().rev() {
        let policy_expr = billable_token_rule(policy.rules, category).expression;
        if policy_expr != default_expr {
            sql = format!(
                "(CASE WHEN token_usage.provider = '{}' THEN {} ELSE {} END)",
                policy.provider.as_str(),
                token_expression_sql(policy_expr),
                sql
            );
        }
    }
    sql
}

fn token_expression_sql(expression: BillableTokenExpression) -> String {
    match expression {
        BillableTokenExpression::Field(field) => token_count_field_sql(field).into(),
        BillableTokenExpression::SaturatingSub(minuend, subtrahend) => format!(
            "MAX({} - {}, 0)",
            token_count_field_sql(minuend),
            token_count_field_sql(subtrahend)
        ),
        BillableTokenExpression::Zero => "0".into(),
    }
}

fn token_count_field_sql(field: TokenCountField) -> &'static str {
    match field {
        TokenCountField::Input => "input_tokens",
        TokenCountField::Output => "output_tokens",
        TokenCountField::CacheRead => "cache_read_tokens",
        TokenCountField::CacheCreation => "cache_creation_tokens",
        TokenCountField::CachedInput => "cached_input_tokens",
        TokenCountField::ReasoningOutput => "reasoning_output_tokens",
    }
}

fn price_lookup_sql(category: TokenCategory) -> String {
    format!(
        "COALESCE((SELECT p.rate_per_1m_tokens FROM pricing_intervals p WHERE p.provider = token_usage.provider AND p.model_id = token_usage.model_id AND p.token_category = '{}' AND p.currency = 'USD' AND p.effective_from <= timestamp AND (p.effective_to IS NULL OR timestamp < p.effective_to)), 0)",
        category.as_str()
    )
}

fn validate_pricing_if_required(
    conn: &Connection,
    filter: &QueryFilter,
    cost_required: bool,
) -> Result<()> {
    if cost_required {
        validate_pricing_coverage(conn, filter)
    } else {
        Ok(())
    }
}

fn period_group_expr(period: TimePeriod) -> &'static str {
    period.sql_utc_group_expr()
}

fn utc_day_expr() -> &'static str {
    "date(timestamp)"
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CoverageKey {
    provider: String,
    model_id: String,
    category: TokenCategory,
}

fn validate_pricing_coverage(conn: &Connection, filter: &QueryFilter) -> Result<()> {
    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT provider, model_id, timestamp, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cached_input_tokens, reasoning_output_tokens
         FROM token_usage
         WHERE 1=1 {where_clause}"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut coverage: HashMap<CoverageKey, (DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
    let mut rows = stmt.query(param_refs.as_slice())?;

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let timestamp: DateTime<Utc> = timestamp.parse()?;
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            bail!(
                "missing pricing coverage for provider={provider}, model={model_id}, category=provider, usage range {} to {}; unsupported provider id in usage row, reingest or repair the database with a supported provider id such as claude-code or codex",
                timestamp.to_rfc3339(),
                timestamp.to_rfc3339()
            );
        };
        let categories = billable_token_categories_for_counts(
            provider_id,
            row.get::<_, i64>(3)?.max(0) as u64,
            row.get::<_, i64>(4)?.max(0) as u64,
            row.get::<_, i64>(5)?.max(0) as u64,
            row.get::<_, i64>(6)?.max(0) as u64,
            row.get::<_, i64>(7)?.max(0) as u64,
            row.get::<_, i64>(8)?.max(0) as u64,
        );
        for (category, tokens) in categories {
            if tokens == 0 {
                continue;
            }
            let key = CoverageKey {
                provider: provider.clone(),
                model_id: model_id.clone(),
                category,
            };
            coverage
                .entry(key)
                .and_modify(|(min, max)| {
                    *min = (*min).min(timestamp);
                    *max = (*max).max(timestamp);
                })
                .or_insert((timestamp, timestamp));
        }
    }

    for (key, (start, end)) in coverage {
        validate_category_coverage(conn, &key, start, end)?;
    }

    Ok(())
}

fn validate_category_coverage(
    conn: &Connection,
    key: &CoverageKey,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT effective_from, effective_to
         FROM pricing_intervals
         WHERE provider = ?1
           AND model_id = ?2
           AND token_category = ?3
           AND currency = 'USD'
           AND effective_from <= ?4
           AND (effective_to IS NULL OR effective_to > ?5)
         ORDER BY effective_from ASC",
    )?;
    let intervals = stmt
        .query_map(
            rusqlite::params![
                key.provider,
                key.model_id,
                key.category.as_str(),
                end.to_rfc3339(),
                start.to_rfc3339(),
            ],
            |row| {
                let from: String = row.get(0)?;
                let to: Option<String> = row.get(1)?;
                Ok((from, to))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if intervals.is_empty() {
        bail!(
            "missing pricing coverage for provider={}, model={}, category={}, usage range {} to {}; run `tkstat --pricing-refresh` or `tkstat --pricing-seed`",
            key.provider,
            key.model_id,
            key.category,
            start.to_rfc3339(),
            end.to_rfc3339()
        );
    }

    let mut cursor = start;
    let mut saw_covering_interval = false;
    for (idx, (from, to)) in intervals.iter().enumerate() {
        let from: DateTime<Utc> = from.parse()?;
        let to: Option<DateTime<Utc>> = to.as_ref().map(|dt| dt.parse()).transpose()?;

        if from > cursor {
            bail!(
                "missing pricing coverage for provider={}, model={}, category={}, gap {} to {}; run `tkstat --pricing-refresh` or `tkstat --pricing-seed`",
                key.provider,
                key.model_id,
                key.category,
                cursor.to_rfc3339(),
                from.to_rfc3339()
            );
        }
        if saw_covering_interval && from < cursor {
            bail!(
                "overlapping pricing intervals for provider={}, model={}, category={} near {}",
                key.provider,
                key.model_id,
                key.category,
                from.to_rfc3339()
            );
        }
        saw_covering_interval = true;

        match to {
            Some(to) => {
                if to > cursor {
                    cursor = to;
                }
                if cursor > end {
                    return Ok(());
                }
            }
            None => {
                if let Some((next_from, _)) = intervals.get(idx + 1) {
                    bail!(
                        "overlapping pricing intervals for provider={}, model={}, category={} near {}",
                        key.provider,
                        key.model_id,
                        key.category,
                        next_from
                    );
                }
                return Ok(());
            }
        }
    }

    bail!(
        "missing pricing coverage for provider={}, model={}, category={}, gap {} to {}; run `tkstat --pricing-refresh` or `tkstat --pricing-seed`",
        key.provider,
        key.model_id,
        key.category,
        cursor.to_rfc3339(),
        end.to_rfc3339()
    )
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
        clauses.push(format!("AND {} >= ?{}", utc_day_expr(), params.len()));
    }

    if let Some(ref end) = filter.end {
        params.push(Box::new(end.to_string()));
        clauses.push(format!("AND {} <= ?{}", utc_day_expr(), params.len()));
    }

    if let Some(ref provider) = filter.provider {
        params.push(Box::new(provider.as_str().to_string()));
        clauses.push(format!("AND provider = ?{}", params.len()));
    }

    if let Some(ref model) = filter.model {
        params.push(Box::new(model.clone()));
        let exact_param = params.len();
        if let Ok(family) = model.parse::<ModelFamily>() {
            params.push(Box::new(family.as_str().to_string()));
            clauses.push(format!(
                "AND (model_id = ?{exact_param} OR model_family = ?{})",
                params.len()
            ));
        } else {
            clauses.push(format!("AND model_id = ?{exact_param}"));
        }
    }

    if let Some(ref family) = filter.model_family {
        let parsed = family
            .parse::<ModelFamily>()
            .map(|f| f.as_str().to_string())
            .unwrap_or_else(|_| family.to_ascii_lowercase());
        params.push(Box::new(parsed));
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
    use crate::db::pricing::calculate_record_cost;
    use crate::domain::pricing::{PricingInterval, TokenCategory};
    use crate::domain::usage::ModelFamily;

    fn seed_db(db: &Database) {
        db.seed_pricing().unwrap();
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
            provider: crate::domain::provider::ProviderId::ClaudeCode,
            request_id: id.into(),
            session_id: format!("s-{id}"),
            uuid: format!("u-{id}"),
            timestamp: ts.parse().unwrap(),
            model: family.parse().unwrap_or(ModelFamily::Unknown),
            model_id: format!("claude-{family}-4-6"),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
            cost_usd: (input as f64 * 15.0 + output as f64 * 75.0) / 1_000_000.0,
            project: project.into(),
            source_file: "/test.jsonl".into(),
            is_subagent: false,
        }
    }

    fn price(
        model_id: &str,
        category: TokenCategory,
        rate: f64,
        from: &str,
        to: Option<&str>,
    ) -> PricingInterval {
        provider_price("claude-code", model_id, category, rate, from, to)
    }

    fn provider_price(
        provider: &str,
        model_id: &str,
        category: TokenCategory,
        rate: f64,
        from: &str,
        to: Option<&str>,
    ) -> PricingInterval {
        let mut interval = PricingInterval::usd(
            crate::domain::provider::ProviderId::from_canonical(provider).unwrap(),
            model_id,
            category,
            rate,
            from.parse().unwrap(),
            "test",
        );
        interval.effective_to = to.map(|dt| dt.parse().unwrap());
        interval
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
    fn test_query_with_exact_model_id_filter() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let mut opus_45 = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 100, 50);
        opus_45.model_id = "claude-opus-4-5-20250929".into();
        let mut opus_46 = make_record("r2", "2026-04-05T11:00:00Z", "opus", "proj-a", 200, 50);
        opus_46.model_id = "claude-opus-4-6".into();
        db.insert_records(&[opus_45, opus_46]).unwrap();

        let filter = QueryFilter {
            model: Some("claude-opus-4-5-20250929".into()),
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.input_tokens, 100);
    }

    #[test]
    fn test_query_with_model_family_filter_keeps_alias_behavior() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let filter = QueryFilter {
            model_family: Some("opus".into()),
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.input_tokens, 100 + 300 + 500);
    }

    #[test]
    fn test_query_by_model_groups_provider_and_exact_model_id() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let mut codex = make_record(
            "r-codex",
            "2026-04-05T10:00:00Z",
            "unknown",
            "proj-a",
            700,
            80,
        );
        codex.provider = crate::domain::provider::ProviderId::Codex;
        codex.model_id = "gpt-5.1-codex".into();
        db.insert_records(&[codex]).unwrap();

        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_model(db.conn(), &filter, 10).unwrap();

        assert!(rows.iter().any(|r| {
            r.provider.as_deref() == Some("claude-code")
                && r.model_id.as_deref() == Some("claude-opus-4-6")
        }));
        assert!(rows.iter().any(|r| {
            r.provider.as_deref() == Some("codex") && r.model_id.as_deref() == Some("gpt-5.1-codex")
        }));
    }

    #[test]
    fn test_query_provider_filter_and_combined_provider_aggregation() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let mut claude = make_record(
            "r-claude",
            "2026-04-05T10:00:00Z",
            "sonnet",
            "proj-a",
            100,
            50,
        );
        claude.session_id = "shared-session".into();
        let mut codex = make_record(
            "r-codex",
            "2026-04-05T10:00:00Z",
            "unknown",
            "proj-a",
            200,
            60,
        );
        codex.provider = crate::domain::provider::ProviderId::Codex;
        codex.model_id = "gpt-5.5".into();
        codex.session_id = "shared-session".into();
        db.insert_records(&[claude, codex]).unwrap();

        let codex_summary = query_summary(
            db.conn(),
            &QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::Codex),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(codex_summary.request_count, 1);
        assert_eq!(codex_summary.input_tokens, 200);

        let combined = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(combined.request_count, 2);
        assert_eq!(combined.input_tokens, 300);
        assert_eq!(combined.session_count, 2);
    }

    #[test]
    fn test_query_by_provider_groups_totals_and_respects_provider_filter() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let mut claude = make_record(
            "r-claude",
            "2026-04-05T10:00:00Z",
            "sonnet",
            "proj-a",
            100,
            50,
        );
        claude.session_id = "shared-session".into();
        let mut codex = make_record(
            "r-codex",
            "2026-04-05T10:00:00Z",
            "unknown",
            "proj-a",
            200,
            60,
        );
        codex.provider = crate::domain::provider::ProviderId::Codex;
        codex.model_id = "gpt-5.5".into();
        codex.session_id = "shared-session".into();
        db.insert_records(&[claude, codex]).unwrap();

        let rows = query_by_provider(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let codex = rows
            .iter()
            .find(|row| row.provider.as_deref() == Some("codex"))
            .unwrap();
        assert_eq!(codex.period, "codex");
        assert_eq!(codex.request_count, 1);
        assert_eq!(codex.input_tokens, 200);
        assert_eq!(codex.session_count, 1);

        let filtered = query_by_provider(
            db.conn(),
            &QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::Codex),
                include_subagents: true,
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn test_query_by_provider_respects_project_and_model_filters() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let rows = query_by_provider(
            db.conn(),
            &QueryFilter {
                project: Some("proj-b".into()),
                model: Some("opus".into()),
                include_subagents: true,
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider.as_deref(), Some("claude-code"));
        assert_eq!(rows[0].input_tokens, 300);
    }

    #[test]
    fn test_query_by_project_groups_totals_and_unknown_project() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let mut unknown = make_record(
            "r-unknown-project",
            "2026-04-05T10:00:00Z",
            "unknown",
            "",
            25,
            5,
        );
        unknown.provider = crate::domain::provider::ProviderId::Codex;
        unknown.model_id = "gpt-5.5".into();
        db.insert_records(&[unknown]).unwrap();

        let rows = query_by_project(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
            10,
        )
        .unwrap();
        let proj_a = rows
            .iter()
            .find(|row| row.project.as_deref() == Some("proj-a"))
            .unwrap();
        assert_eq!(proj_a.input_tokens, 100 + 200 + 50 + 500);
        let unknown = rows
            .iter()
            .find(|row| row.project.as_deref() == Some("unknown"))
            .unwrap();
        assert_eq!(unknown.period, "unknown");
        assert_eq!(unknown.input_tokens, 25);
    }

    #[test]
    fn test_query_by_project_respects_project_provider_and_model_filters() {
        let db = Database::open_in_memory().unwrap();
        seed_db(&db);
        let rows = query_by_project(
            db.conn(),
            &QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::ClaudeCode),
                project: Some("proj-b".into()),
                model: Some("opus".into()),
                include_subagents: true,
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project.as_deref(), Some("proj-b"));
        assert_eq!(rows[0].input_tokens, 300);
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
    fn test_query_daily_and_date_filters_use_utc_boundaries() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[
            make_record("late", "2026-04-07T23:30:00Z", "opus", "proj-a", 10, 1),
            make_record("early", "2026-04-08T00:30:00Z", "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-04-07", "2026-04-08"]
        );

        let filter = QueryFilter {
            begin: NaiveDate::from_ymd_opt(2026, 4, 8),
            end: NaiveDate::from_ymd_opt(2026, 4, 8),
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.input_tokens, 20);
    }

    #[test]
    fn test_hourly_grouping_is_utc_across_dst_spring_boundary() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[
            make_record("spring-1", "2026-03-08T09:30:00Z", "opus", "proj-a", 10, 1),
            make_record("spring-2", "2026-03-08T10:30:00Z", "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let filter = QueryFilter {
            begin: NaiveDate::from_ymd_opt(2026, 3, 8),
            end: NaiveDate::from_ymd_opt(2026, 3, 8),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-03-08 09:00", "2026-03-08 10:00"]
        );
    }

    #[test]
    fn test_hourly_grouping_is_utc_across_dst_fall_boundary() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[
            make_record("fall-1", "2026-11-01T08:30:00Z", "opus", "proj-a", 10, 1),
            make_record("fall-2", "2026-11-01T09:30:00Z", "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let filter = QueryFilter {
            begin: NaiveDate::from_ymd_opt(2026, 11, 1),
            end: NaiveDate::from_ymd_opt(2026, 11, 1),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-11-01 08:00", "2026-11-01 09:00"]
        );
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

    #[test]
    fn test_query_cost_uses_usage_timestamp_across_price_change() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-04-06T00:00:00Z"),
        ))
        .unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            20.0,
            "2026-04-06T00:00:00Z",
            None,
        ))
        .unwrap();

        let mut before = make_record(
            "before",
            "2026-04-05T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        before.output_tokens = 0;
        let mut after = make_record(
            "after",
            "2026-04-07T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        after.output_tokens = 0;
        db.insert_records(&[before, after]).unwrap();

        let summary = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!((summary.cost_usd - 30.0).abs() < 0.001);
    }

    #[test]
    fn test_query_fails_when_pricing_interval_missing() {
        let db = Database::open_in_memory().unwrap();
        let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
        record.output_tokens = 0;
        db.insert_records(&[record]).unwrap();

        let err = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing pricing coverage"));
        assert!(err.contains("claude-opus-4-6"));
        assert!(err.contains("tkstat --pricing-refresh"));
    }

    #[test]
    fn test_query_fails_when_pricing_intervals_overlap() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            20.0,
            "2026-02-01T00:00:00Z",
            None,
        ))
        .unwrap();
        let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
        record.output_tokens = 0;
        db.insert_records(&[record]).unwrap();

        let err = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("overlapping pricing intervals"));
    }

    #[test]
    fn test_query_allows_zero_cost_only_when_price_is_truly_zero() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            0.0,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
        let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
        record.output_tokens = 0;
        db.insert_records(&[record]).unwrap();

        let summary = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(summary.input_tokens, 1_000_000);
        assert_eq!(summary.cost_usd, 0.0);
    }

    #[test]
    fn test_query_codex_cost_uses_non_overlapping_billing_and_display_total() {
        let db = Database::open_in_memory().unwrap();
        for (category, rate) in [
            (TokenCategory::Input, 10.0),
            (TokenCategory::CachedInput, 1.0),
            (TokenCategory::Output, 100.0),
        ] {
            db.insert_pricing_interval(&provider_price(
                "codex",
                "gpt-audit",
                category,
                rate,
                "2026-01-01T00:00:00Z",
                None,
            ))
            .unwrap();
        }
        let mut record = make_record(
            "codex",
            "2026-04-07T10:00:00Z",
            "unknown",
            "proj-a",
            100,
            20,
        );
        record.provider = crate::domain::provider::ProviderId::Codex;
        record.model = ModelFamily::Unknown;
        record.model_id = "gpt-audit".into();
        record.cached_input_tokens = 40;
        record.reasoning_output_tokens = 7;
        record.cost_usd = 0.0;
        db.insert_records(&[record]).unwrap();

        let summary = query_summary(
            db.conn(),
            &QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::Codex),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();

        let expected = (60.0 * 10.0 + 40.0 * 1.0 + 20.0 * 100.0) / 1_000_000.0;
        assert!((summary.cost_usd - expected).abs() < 0.000001);
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(summary.cached_input_tokens, 40);
        assert_eq!(summary.output_tokens, 20);
        assert_eq!(summary.reasoning_output_tokens, 7);
        assert_eq!(summary.total_tokens, 120);
    }

    #[test]
    fn test_sql_cost_matches_record_cost_billing_policy_for_mixed_providers() {
        let db = Database::open_in_memory().unwrap();
        for (category, rate) in [
            (TokenCategory::Input, 10.0),
            (TokenCategory::Output, 100.0),
            (TokenCategory::CacheCreation, 12.5),
            (TokenCategory::CacheRead, 1.0),
            (TokenCategory::CachedInput, 0.5),
            (TokenCategory::ReasoningOutput, 50.0),
        ] {
            db.insert_pricing_interval(&provider_price(
                "claude-code",
                "claude-policy",
                category,
                rate,
                "2026-01-01T00:00:00Z",
                None,
            ))
            .unwrap();
        }
        for (category, rate) in [
            (TokenCategory::Input, 10.0),
            (TokenCategory::CachedInput, 1.0),
            (TokenCategory::Output, 100.0),
        ] {
            db.insert_pricing_interval(&provider_price(
                "codex",
                "gpt-policy",
                category,
                rate,
                "2026-01-01T00:00:00Z",
                None,
            ))
            .unwrap();
        }

        let mut claude = make_record(
            "policy-claude",
            "2026-04-07T10:00:00Z",
            "unknown",
            "proj-a",
            100,
            20,
        );
        claude.model_id = "claude-policy".into();
        claude.cache_creation_tokens = 10;
        claude.cache_read_tokens = 5;
        claude.reasoning_output_tokens = 3;

        let mut codex_cached = make_record(
            "policy-codex-cached",
            "2026-04-07T11:00:00Z",
            "unknown",
            "proj-a",
            100,
            20,
        );
        codex_cached.provider = crate::domain::provider::ProviderId::Codex;
        codex_cached.model = ModelFamily::Unknown;
        codex_cached.model_id = "gpt-policy".into();
        codex_cached.cached_input_tokens = 40;
        codex_cached.reasoning_output_tokens = 7;

        let mut codex_overcached = make_record(
            "policy-codex-overcached",
            "2026-04-07T12:00:00Z",
            "unknown",
            "proj-a",
            30,
            10,
        );
        codex_overcached.provider = crate::domain::provider::ProviderId::Codex;
        codex_overcached.model = ModelFamily::Unknown;
        codex_overcached.model_id = "gpt-policy".into();
        codex_overcached.cached_input_tokens = 40;
        codex_overcached.reasoning_output_tokens = 9;

        let mut codex_uncached = make_record(
            "policy-codex-uncached",
            "2026-04-07T13:00:00Z",
            "unknown",
            "proj-a",
            25,
            5,
        );
        codex_uncached.provider = crate::domain::provider::ProviderId::Codex;
        codex_uncached.model = ModelFamily::Unknown;
        codex_uncached.model_id = "gpt-policy".into();

        let records = vec![claude, codex_cached, codex_overcached, codex_uncached];
        db.insert_records(&records).unwrap();

        let expected = records
            .iter()
            .map(|record| calculate_record_cost(db.conn(), record).unwrap())
            .sum::<f64>();
        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        let daily = query_by_period(db.conn(), TimePeriod::Daily, &filter, 10).unwrap();

        assert!((summary.cost_usd - expected).abs() < 0.000001);
        assert_eq!(daily.len(), 1);
        assert!((daily[0].cost_usd - expected).abs() < 0.000001);
        assert_eq!(summary.input_tokens, 255);
        assert_eq!(summary.cached_input_tokens, 80);
        assert_eq!(summary.reasoning_output_tokens, 19);
    }

    #[test]
    fn test_seed_pricing_covers_observed_claude_opus_cache_creation_usage() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let mut record = make_record(
            "observed-opus",
            "2026-01-31T21:20:19.858Z",
            "opus",
            "proj-a",
            0,
            0,
        );
        record.model_id = "claude-opus-4-5-20251101".into();
        record.cache_creation_tokens = 100;
        db.insert_records(&[record]).unwrap();

        let summary = query_summary(
            db.conn(),
            &QueryFilter {
                model: Some("claude-opus-4-5-20251101".into()),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.cache_creation_tokens, 100);
        assert!(summary.cost_usd > 0.0);
    }
}
