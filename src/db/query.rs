use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeDelta, Timelike, Utc};
use rusqlite::Connection;
use serde::Serialize;

use crate::db::table_exists;
use crate::domain::period::{ReportTimeZone, TimePeriod, day_sql_expr};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::{format_utc_rfc3339, parse_canonical_utc_rfc3339};
use crate::domain::usage::{AggregatedRow, ModelFamily};

/// Filter parameters for queries.
#[derive(Debug, Default, Clone)]
pub struct QueryFilter {
    pub begin: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub report_timezone: ReportTimeZone,
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
    let group_expr = period_group_expr(period, filter.report_timezone);
    let (where_clause, params) = build_where_clause(filter);
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);

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
            {cost_expr},
            COUNT(*),
            COUNT(DISTINCT token_usage.provider || ':' || token_usage.session_id),
            MIN(timestamp),
            MAX(timestamp)
         FROM token_usage
         {cost_join}
         WHERE 1=1 {where_clause}
         GROUP BY {group_expr}
         ORDER BY {group_expr} ASC",
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let result: Vec<PeriodAggregate> = stmt
        .query_map(param_refs.as_slice(), row_to_period_aggregate)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let filled = fill_gaps(conn, period, filter.report_timezone, result)?;

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
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);
    let daily_expr = report_day_expr(filter.report_timezone);

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
            {cost_expr},
            COUNT(*),
            COUNT(DISTINCT token_usage.provider || ':' || token_usage.session_id)
         FROM token_usage
         {cost_join}
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
    query_grouped(conn, GroupBy::Model, filter, limit, true)
}

pub fn query_by_model_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    query_grouped(conn, GroupBy::Model, filter, limit, cost_required)
}

/// Query aggregated usage grouped by provider.
pub fn query_by_provider(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    query_grouped(conn, GroupBy::Provider, filter, limit, true)
}

pub fn query_by_provider_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    query_grouped(conn, GroupBy::Provider, filter, limit, cost_required)
}

/// Query aggregated usage grouped by normalized project name.
pub fn query_by_project(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
) -> Result<Vec<AggregatedRow>> {
    query_grouped(conn, GroupBy::Project, filter, limit, true)
}

pub fn query_by_project_with_cost_requirement(
    conn: &Connection,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    query_grouped(conn, GroupBy::Project, filter, limit, cost_required)
}

#[derive(Debug, Clone, Copy)]
enum GroupBy {
    Model,
    Provider,
    Project,
}

impl GroupBy {
    fn sql(self) -> GroupSql {
        match self {
            Self::Model => GroupSql {
                period: "token_usage.provider || '/' || token_usage.model_id",
                provider: "token_usage.provider",
                model_id: "token_usage.model_id",
                project: "NULL",
                group_by: "token_usage.provider, token_usage.model_id",
                stable_order: "token_usage.provider ASC, token_usage.model_id ASC",
            },
            Self::Provider => GroupSql {
                period: "token_usage.provider",
                provider: "token_usage.provider",
                model_id: "NULL",
                project: "NULL",
                group_by: "token_usage.provider",
                stable_order: "token_usage.provider ASC",
            },
            Self::Project => GroupSql {
                period: "COALESCE(NULLIF(token_usage.project, ''), 'unknown')",
                provider: "NULL",
                model_id: "NULL",
                project: "COALESCE(NULLIF(token_usage.project, ''), 'unknown')",
                group_by: "COALESCE(NULLIF(token_usage.project, ''), 'unknown')",
                stable_order: "period ASC",
            },
        }
    }
}

struct GroupSql {
    period: &'static str,
    provider: &'static str,
    model_id: &'static str,
    project: &'static str,
    group_by: &'static str,
    stable_order: &'static str,
}

fn query_grouped(
    conn: &Connection,
    group: GroupBy,
    filter: &QueryFilter,
    limit: u32,
    cost_required: bool,
) -> Result<Vec<AggregatedRow>> {
    validate_pricing_if_required(conn, filter, cost_required)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);
    let group = group.sql();
    let period = group.period;
    let provider = group.provider;
    let model_id = group.model_id;
    let project = group.project;
    let group_by = group.group_by;
    let stable_order = group.stable_order;

    let sql = format!(
        "SELECT
            {period} AS period,
            {provider} AS provider,
            {model_id} AS model_id,
            {project} AS project,
            SUM(input_tokens),
            SUM(output_tokens),
            SUM(cache_creation_tokens),
            SUM(cache_read_tokens),
            SUM(cached_input_tokens),
            SUM(reasoning_output_tokens),
            SUM(total_tokens),
            {cost_expr},
            COUNT(*),
            COUNT(DISTINCT token_usage.provider || ':' || token_usage.session_id)
         FROM token_usage
         {cost_join}
         WHERE 1=1 {where_clause}
         GROUP BY {group_by}
         ORDER BY SUM(total_tokens) DESC, {stable_order}
         LIMIT ?"
    );

    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = params;
    all_params.push(Box::new(limit));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<AggregatedRow> = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(AggregatedRow {
                period: row.get(0)?,
                provider: row.get(1)?,
                model_id: row.get(2)?,
                project: row.get(3)?,
                input_tokens: nonnegative_u64(row, 4)?,
                output_tokens: nonnegative_u64(row, 5)?,
                cache_creation_tokens: nonnegative_u64(row, 6)?,
                cache_read_tokens: nonnegative_u64(row, 7)?,
                cached_input_tokens: nonnegative_u64(row, 8)?,
                reasoning_output_tokens: nonnegative_u64(row, 9)?,
                total_tokens: nonnegative_u64(row, 10)?,
                cost_usd: row.get(11)?,
                request_count: nonnegative_u64(row, 12)?,
                session_count: nonnegative_u64(row, 13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

fn nonnegative_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    row.get::<_, i64>(index).map(|value| value.max(0) as u64)
}

/// Compute a single summary row across all data matching the filter.
pub fn query_summary(conn: &Connection, filter: &QueryFilter) -> Result<AggregatedRow> {
    validate_materialized_pricing(conn, filter)?;
    let (where_clause, params) = build_where_clause(filter);
    let cost_join = cost_join_sql(true);
    let cost_expr = cost_aggregate_sql(true);

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
            {cost_expr},
            COUNT(*),
            COUNT(DISTINCT token_usage.provider || ':' || token_usage.session_id)
         FROM token_usage
         {cost_join}
         WHERE 1=1 {where_clause}"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt.query_row(param_refs.as_slice(), row_to_aggregated)?)
}

pub fn explain_cost(conn: &Connection, filter: &QueryFilter) -> Result<CostExplanation> {
    explain_cost_at(conn, filter, Utc::now().date_naive())
}

fn explain_cost_at(
    conn: &Connection,
    filter: &QueryFilter,
    reference_date: NaiveDate,
) -> Result<CostExplanation> {
    let summary = query_summary(conn, filter)?;
    let assumptions = cost_assumptions_at(conn, filter, reference_date)?;
    let component_count = filtered_component_count(conn, filter)?;
    let confidence = if assumptions.is_empty() {
        CostConfidence::High
    } else {
        CostConfidence::Estimated
    };
    Ok(CostExplanation {
        confidence,
        cost_usd: summary.cost_usd,
        component_count,
        assumptions,
    })
}

fn filtered_component_count(conn: &Connection, filter: &QueryFilter) -> Result<u64> {
    if !table_exists(conn, "usage_billing_components")? {
        return Ok(0);
    }
    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT COUNT(*)
         FROM (SELECT provider, request_id FROM token_usage WHERE 1=1 {where_clause}) token_usage
         JOIN usage_billing_components c
           ON c.provider = token_usage.provider
          AND c.request_id = token_usage.request_id"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let count: i64 = conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn cost_assumptions_at(
    conn: &Connection,
    filter: &QueryFilter,
    reference_date: NaiveDate,
) -> Result<Vec<CostAssumption>> {
    if !table_exists(conn, "usage_billing_components")? {
        return Ok(Vec::new());
    }
    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT DISTINCT c.provider, c.model_id, c.token_category, c.service_tier, c.speed,
                c.region, c.processing_mode, p.source, s.source_kind, s.source_retrieved_at
         FROM (SELECT provider, request_id FROM token_usage WHERE 1=1 {where_clause}) token_usage
         JOIN usage_billing_components c
           ON c.provider = token_usage.provider
          AND c.request_id = token_usage.request_id
         LEFT JOIN pricing_intervals p
           ON p.provider = c.provider
          AND p.model_id = c.model_id
          AND p.token_category = c.token_category
          AND p.service_tier IS c.service_tier
          AND p.speed IS c.speed
          AND p.region IS c.region
          AND p.processing_mode IS c.processing_mode
          AND p.source_detail IS c.source_detail
          AND p.currency = 'USD'
          AND p.effective_from <= c.timestamp
          AND (p.effective_to IS NULL OR c.timestamp < p.effective_to)
         LEFT JOIN pricing_sources s
           ON s.source = p.source
         ORDER BY c.provider, c.model_id, c.token_category"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(CostAssumptionRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                token_category: row.get(2)?,
                service_tier: row.get(3)?,
                speed: row.get(4)?,
                region: row.get(5)?,
                processing_mode: row.get(6)?,
                source: row.get(7)?,
                source_kind: row.get(8)?,
                source_retrieved_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let cutoff = crate::db::pricing::stale_source_cutoff(reference_date);
    let mut assumptions = Vec::new();
    for row in rows {
        assumptions.extend(default_modifier_assumptions(&row));
        match row.source_kind.as_deref() {
            Some("bundled") => assumptions.push(source_assumption(
                CostAssumptionKind::BundledPricingSource,
                &row,
                "bundled pricing snapshot was used; import reviewed pricing for current local estimates",
            )),
            Some(_) => {}
            None if row.source.is_some() => assumptions.push(source_assumption(
                CostAssumptionKind::MissingPricingSourceMetadata,
                &row,
                "pricing source metadata is missing; import a reviewed catalog or reseed pricing",
            )),
            None => {}
        }
        if let Some(retrieved_at) = &row.source_retrieved_at
            && let Ok(retrieved_date) = NaiveDate::parse_from_str(retrieved_at, "%Y-%m-%d")
            && retrieved_date < cutoff
        {
            assumptions.push(source_assumption(
                CostAssumptionKind::StalePricingSource,
                &row,
                "pricing source retrieval date is stale; import reviewed pricing if provider pricing changed",
            ));
        }
    }
    assumptions.sort_by(|a, b| {
        (
            &a.kind,
            &a.provider,
            &a.model_id,
            &a.token_category,
            &a.dimension,
            &a.source,
        )
            .cmp(&(
                &b.kind,
                &b.provider,
                &b.model_id,
                &b.token_category,
                &b.dimension,
                &b.source,
            ))
    });
    assumptions.dedup();
    Ok(assumptions)
}

#[derive(Debug)]
struct CostAssumptionRow {
    provider: String,
    model_id: String,
    token_category: String,
    service_tier: Option<String>,
    speed: Option<String>,
    region: Option<String>,
    processing_mode: Option<String>,
    source: Option<String>,
    source_kind: Option<String>,
    source_retrieved_at: Option<String>,
}

fn default_modifier_assumptions(row: &CostAssumptionRow) -> Vec<CostAssumption> {
    let mut assumptions = Vec::new();
    if row.provider == ProviderId::ClaudeCode.as_str() {
        for (dimension, detail) in [
            (
                "service_tier",
                "Claude service tier was not present in the usage log, so pricing used the default tier key",
            ),
            (
                "speed",
                "Claude speed modifier was not present in the usage log, so pricing used the default speed key",
            ),
            (
                "region",
                "Claude inference region was not present in the usage log, so pricing used the default region key",
            ),
        ] {
            if modifier_value(row, dimension).is_none() {
                assumptions.push(default_modifier_assumption(row, dimension, None, detail));
            }
        }
    }
    if row.provider == ProviderId::Codex.as_str()
        && row.processing_mode.as_deref() == Some("standard")
    {
        assumptions.push(default_modifier_assumption(
            row,
            "processing_mode",
            Some("standard"),
            "Codex/OpenAI processing mode is standard; local logs do not prove account-level pricing beyond this mode",
        ));
    }
    assumptions
}

fn modifier_value<'a>(row: &'a CostAssumptionRow, dimension: &str) -> Option<&'a str> {
    match dimension {
        "service_tier" => row.service_tier.as_deref(),
        "speed" => row.speed.as_deref(),
        "region" => row.region.as_deref(),
        "processing_mode" => row.processing_mode.as_deref(),
        _ => None,
    }
}

fn default_modifier_assumption(
    row: &CostAssumptionRow,
    dimension: &str,
    value: Option<&str>,
    detail: &str,
) -> CostAssumption {
    CostAssumption {
        kind: CostAssumptionKind::AssumedDefaultModifier,
        provider: row.provider.clone(),
        model_id: row.model_id.clone(),
        token_category: row.token_category.clone(),
        dimension: Some(dimension.into()),
        value: value.map(str::to_string),
        source: None,
        detail: detail.into(),
    }
}

fn source_assumption(
    kind: CostAssumptionKind,
    row: &CostAssumptionRow,
    detail: &str,
) -> CostAssumption {
    CostAssumption {
        kind,
        provider: row.provider.clone(),
        model_id: row.model_id.clone(),
        token_category: row.token_category.clone(),
        dimension: None,
        value: row.source_kind.clone(),
        source: row.source.clone(),
        detail: detail.into(),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CostConfidence {
    High,
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum CostAssumptionKind {
    AssumedDefaultModifier,
    BundledPricingSource,
    StalePricingSource,
    MissingPricingSourceMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CostAssumption {
    pub kind: CostAssumptionKind,
    pub provider: String,
    pub model_id: String,
    pub token_category: String,
    pub dimension: Option<String>,
    pub value: Option<String>,
    pub source: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostExplanation {
    pub confidence: CostConfidence,
    pub cost_usd: f64,
    pub component_count: u64,
    pub assumptions: Vec<CostAssumption>,
}

struct PeriodAggregate {
    row: AggregatedRow,
    first_timestamp: DateTime<Utc>,
    last_timestamp: DateTime<Utc>,
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
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);
    let daily_expr = report_day_expr(filter.report_timezone);

    let sql = format!(
        "SELECT
            {daily_expr} AS day,
            SUM(total_tokens),
            SUM(input_tokens),
            SUM(output_tokens),
            {cost_expr}
         FROM token_usage
         {cost_join}
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

fn row_to_period_aggregate(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeriodAggregate> {
    let first_timestamp: String = row.get(11)?;
    let last_timestamp: String = row.get(12)?;
    Ok(PeriodAggregate {
        row: row_to_aggregated(row)?,
        first_timestamp: parse_canonical_utc_rfc3339(&first_timestamp).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(e))
        })?,
        last_timestamp: parse_canonical_utc_rfc3339(&last_timestamp).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

fn cost_join_sql(cost_required: bool) -> &'static str {
    if !cost_required {
        return "";
    }

    "LEFT JOIN usage_costs component_cost
       ON component_cost.usage_id = token_usage.id"
}

fn cost_aggregate_sql(cost_required: bool) -> String {
    if !cost_required {
        return "0.0".into();
    }
    "COALESCE(SUM(component_cost.cost_usd), 0.0)".into()
}

fn validate_pricing_if_required(
    conn: &Connection,
    filter: &QueryFilter,
    cost_required: bool,
) -> Result<()> {
    if cost_required {
        validate_materialized_pricing(conn, filter)
    } else {
        Ok(())
    }
}

fn validate_materialized_pricing(conn: &Connection, filter: &QueryFilter) -> Result<()> {
    let components_dirty: bool = conn.query_row(
        "SELECT billing_components_dirty != 0 FROM integrity_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if components_dirty {
        validate_pricing_coverage(conn, filter)?;
        bail!(
            "billing component integrity state is stale; run `tkstat --force-update` to rebuild usage"
        );
    }

    let pricing_dirty = match filter.provider {
        Some(provider) => conn.query_row(
            "SELECT dirty != 0 FROM pricing_state WHERE provider = ?1",
            [provider.as_str()],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pricing_state WHERE dirty != 0)",
            [],
            |row| row.get(0),
        )?,
    };
    if pricing_dirty {
        validate_pricing_coverage(conn, filter)?;
        bail!("materialized costs are stale; run `tkstat --pricing-refresh`");
    }

    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT token_usage.provider, token_usage.request_id, token_usage.model_id,
                component_cost.status, component_cost.detail
         FROM token_usage
         LEFT JOIN usage_costs component_cost ON component_cost.usage_id = token_usage.id
         JOIN pricing_state materialized_state ON materialized_state.provider = token_usage.provider
         WHERE 1=1 {where_clause}
           AND (component_cost.usage_id IS NULL
                OR component_cost.status != 'priced'
                OR component_cost.pricing_generation != materialized_state.generation)
         LIMIT 1"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|param| param.as_ref()).collect();
    let issue = conn.query_row(&sql, param_refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    });
    match issue {
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(err) => Err(err.into()),
        Ok((provider, request_id, model_id, status, detail)) => {
            validate_pricing_coverage(conn, filter)?;
            bail!(
                "materialized cost is unavailable for provider={provider}, request_id={request_id}, model={model_id}, status={}, detail={}; run `tkstat --pricing-refresh`",
                status.as_deref().unwrap_or("missing"),
                detail.as_deref().unwrap_or("cost cache row is missing")
            )
        }
    }
}

fn period_group_expr(period: TimePeriod, timezone: ReportTimeZone) -> String {
    period.sql_group_expr(timezone)
}

fn report_day_expr(timezone: ReportTimeZone) -> String {
    day_sql_expr(timezone)
}

fn validate_pricing_coverage(conn: &Connection, filter: &QueryFilter) -> Result<()> {
    let (where_clause, params) = build_where_clause(filter);
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    if let Some(issue) = crate::db::pricing_validation::scan_billing_integrity(
        conn,
        &where_clause,
        &param_refs,
        Some(1),
    )?
    .into_iter()
    .next()
    {
        bail!(issue.report_error());
    }

    let observations = crate::db::pricing_validation::collect_usage_observations(
        conn,
        &where_clause,
        &param_refs,
    )?;
    if let Some(issue) = observations.issues.first() {
        return Err(usage_row_issue_error(issue));
    }
    for (key, timestamps) in observations.usage {
        validate_category_coverage(conn, &key, timestamps)?;
    }

    Ok(())
}

fn usage_row_issue_error(issue: &crate::db::pricing_validation::UsageRowIssue) -> anyhow::Error {
    use crate::db::pricing_validation::{UsageRowIssue, UsageSource};

    match issue {
        UsageRowIssue::MalformedTimestamp {
            provider,
            model_id,
            timestamp,
            source,
        } => {
            let source = match source {
                UsageSource::TokenUsage => "token_usage",
                UsageSource::BillingComponent => "usage_billing_components",
            };
            anyhow::anyhow!(
                "missing pricing coverage for provider={provider}, model={model_id}, category=timestamp, usage timestamp {timestamp}; timestamp is not canonical UTC RFC3339, reingest or repair {source}.timestamp"
            )
        }
        UsageRowIssue::UnsupportedProvider {
            provider,
            model_id,
            timestamp,
        } => anyhow::anyhow!(
            "missing pricing coverage for provider={provider}, model={model_id}, category=provider, usage range {} to {}; unsupported provider id in usage row, reingest or repair the database with a supported provider id such as claude-code or codex",
            format_utc_rfc3339(*timestamp),
            format_utc_rfc3339(*timestamp)
        ),
        UsageRowIssue::UnsupportedCategory {
            provider,
            model_id,
            category,
            timestamp,
        } => anyhow::anyhow!(
            "missing pricing coverage for provider={provider}, model={model_id}, category={category}, usage range {} to {}; unknown token category '{category}', reingest or repair usage_billing_components.token_category",
            format_utc_rfc3339(*timestamp),
            format_utc_rfc3339(*timestamp)
        ),
    }
}

fn validate_category_coverage(
    conn: &Connection,
    key: &crate::domain::pricing::PricingKey,
    mut timestamps: Vec<DateTime<Utc>>,
) -> Result<()> {
    timestamps.sort_unstable();
    timestamps.dedup();
    let Some(start) = timestamps.first().copied() else {
        return Ok(());
    };
    let end = timestamps.last().copied().unwrap_or(start);

    let mut stmt = conn.prepare(
        "SELECT COUNT(*)
         FROM pricing_intervals
         WHERE provider = ?1
           AND model_id = ?2
           AND token_category = ?3
           AND service_tier IS ?4
           AND speed IS ?5
           AND region IS ?6
           AND processing_mode IS ?7
           AND source_detail IS ?8
           AND currency = 'USD'
           AND effective_from <= ?9
           AND (effective_to IS NULL OR ?9 < effective_to)",
    )?;

    for timestamp in timestamps {
        let matching_count: i64 = stmt.query_row(
            rusqlite::params![
                key.provider.as_str(),
                key.model_id,
                key.token_category.as_str(),
                key.dimensions.service_tier,
                key.dimensions.speed,
                key.dimensions.region,
                key.dimensions.processing_mode,
                key.dimensions.source_detail,
                format_utc_rfc3339(timestamp),
            ],
            |row| row.get(0),
        )?;
        match matching_count {
            0 => bail!(
                "missing pricing coverage for provider={}, model={}, category={}{}, usage range {} to {}; no interval covers usage timestamp {}; run `tkstat --pricing-refresh` or `tkstat --pricing-seed`",
                key.provider,
                key.model_id,
                key.token_category,
                key.dimensions.display_suffix(),
                format_utc_rfc3339(start),
                format_utc_rfc3339(end),
                format_utc_rfc3339(timestamp)
            ),
            1 => {}
            _ => bail!(
                "overlapping pricing intervals for provider={}, model={}, category={}{} near {}, usage range {} to {}",
                key.provider,
                key.model_id,
                key.token_category,
                key.dimensions.display_suffix(),
                format_utc_rfc3339(timestamp),
                format_utc_rfc3339(start),
                format_utc_rfc3339(end)
            ),
        }
    }

    Ok(())
}

// -- Gap filling --

fn fill_gaps(
    conn: &Connection,
    period: TimePeriod,
    timezone: ReportTimeZone,
    rows: Vec<PeriodAggregate>,
) -> Result<Vec<AggregatedRow>> {
    if timezone == ReportTimeZone::Local
        && matches!(period, TimePeriod::FiveMinutes | TimePeriod::Hourly)
    {
        return fill_subdaily_local_gaps(conn, period, rows);
    }

    Ok(fill_naive_gaps(
        period,
        rows.into_iter().map(|row| row.row).collect(),
    ))
}

fn fill_naive_gaps(period: TimePeriod, rows: Vec<AggregatedRow>) -> Vec<AggregatedRow> {
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

fn fill_subdaily_local_gaps(
    conn: &Connection,
    period: TimePeriod,
    rows: Vec<PeriodAggregate>,
) -> Result<Vec<AggregatedRow>> {
    if rows.len() < 2 {
        return Ok(rows.into_iter().map(|row| row.row).collect());
    }

    let first_timestamp = rows
        .iter()
        .map(|row| row.first_timestamp)
        .min()
        .expect("non-empty rows have a first timestamp");
    let last_timestamp = rows
        .iter()
        .map(|row| row.last_timestamp)
        .max()
        .expect("non-empty rows have a last timestamp");

    let mut by_label: HashMap<String, AggregatedRow> = rows
        .into_iter()
        .map(|aggregate| (aggregate.row.period.clone(), aggregate.row))
        .collect();
    let step = match period {
        TimePeriod::FiveMinutes => TimeDelta::minutes(5),
        TimePeriod::Hourly => TimeDelta::hours(1),
        _ => return Ok(by_label.into_values().collect()),
    };

    let mut labels = Vec::new();
    let mut seen = HashSet::new();
    let mut current = floor_utc_for_period(first_timestamp, period);
    let end = floor_utc_for_period(last_timestamp, period);
    while current <= end {
        let label = period_label_for_utc(conn, period, ReportTimeZone::Local, current)?;
        if seen.insert(label.clone()) {
            labels.push(label);
        }
        current += step;
    }

    Ok(labels
        .into_iter()
        .map(|label| {
            by_label.remove(&label).unwrap_or(AggregatedRow {
                period: label,
                ..Default::default()
            })
        })
        .collect())
}

fn floor_utc_for_period(timestamp: DateTime<Utc>, period: TimePeriod) -> DateTime<Utc> {
    let timestamp = timestamp
        .with_second(0)
        .and_then(|dt| dt.with_nanosecond(0))
        .expect("valid timestamp second/nanosecond floor");
    match period {
        TimePeriod::FiveMinutes => timestamp
            .with_minute((timestamp.minute() / 5) * 5)
            .expect("valid timestamp 5-minute floor"),
        TimePeriod::Hourly => timestamp
            .with_minute(0)
            .expect("valid timestamp hourly floor"),
        TimePeriod::Daily | TimePeriod::Monthly | TimePeriod::Yearly => timestamp,
    }
}

fn period_label_for_utc(
    conn: &Connection,
    period: TimePeriod,
    timezone: ReportTimeZone,
    timestamp: DateTime<Utc>,
) -> Result<String> {
    let expr = period.sql_group_expr(timezone).replace("timestamp", "?1");
    Ok(conn.query_row(
        &format!("SELECT {expr}"),
        [format_utc_rfc3339(timestamp)],
        |row| row.get(0),
    )?)
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
            "AND {} >= ?{}",
            report_day_expr(filter.report_timezone),
            params.len()
        ));
    }

    if let Some(ref end) = filter.end {
        params.push(Box::new(end.to_string()));
        clauses.push(format!(
            "AND {} <= ?{}",
            report_day_expr(filter.report_timezone),
            params.len()
        ));
    }

    if let Some(ref provider) = filter.provider {
        params.push(Box::new(provider.as_str().to_string()));
        clauses.push(format!("AND token_usage.provider = ?{}", params.len()));
    }

    if let Some(ref model) = filter.model {
        params.push(Box::new(model.clone()));
        let exact_param = params.len();
        if let Ok(family) = model.parse::<ModelFamily>() {
            params.push(Box::new(family.as_str().to_string()));
            clauses.push(format!(
                "AND (token_usage.model_id = ?{exact_param} OR token_usage.model_family = ?{})",
                params.len()
            ));
        } else {
            clauses.push(format!("AND token_usage.model_id = ?{exact_param}"));
        }
    }

    if let Some(ref family) = filter.model_family {
        let parsed = family
            .parse::<ModelFamily>()
            .map(|f| f.as_str().to_string())
            .unwrap_or_else(|_| family.to_ascii_lowercase());
        params.push(Box::new(parsed));
        clauses.push(format!("AND token_usage.model_family = ?{}", params.len()));
    }

    if let Some(ref project) = filter.project {
        params.push(Box::new(format!("%{project}%")));
        clauses.push(format!("AND token_usage.project LIKE ?{}", params.len()));
    }

    if let Some(ref session) = filter.session {
        params.push(Box::new(format!("{session}%")));
        clauses.push(format!("AND token_usage.session_id LIKE ?{}", params.len()));
    }

    if !filter.include_subagents {
        clauses.push(" AND token_usage.is_subagent = 0".into());
    }

    (clauses.join(" "), params)
}

#[cfg(test)]
mod tests;
