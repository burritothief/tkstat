use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeDelta, Timelike, Utc};
use rusqlite::Connection;
use serde::Serialize;

use crate::domain::period::{ReportTimeZone, TimePeriod, day_sql_expr};
use crate::domain::pricing::{
    BillableTokenExpression, PricingDimensions, TokenCategory, TokenCountField,
    billable_token_categories_for_counts, billable_token_rule, default_billing_rules,
    provider_billing_policies,
};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::{format_utc_rfc3339, parse_canonical_utc_rfc3339};
use crate::domain::usage::{AggregatedRow, ModelFamily};

const SOURCE_STALE_AFTER_DAYS: i64 = 90;

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
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);

    let sql = format!(
        "SELECT
            token_usage.provider,
            token_usage.model_id,
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
         GROUP BY token_usage.provider, token_usage.model_id
         ORDER BY SUM(total_tokens) DESC, token_usage.provider ASC, token_usage.model_id ASC
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
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);

    let sql = format!(
        "SELECT
            token_usage.provider,
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
         GROUP BY token_usage.provider
         ORDER BY SUM(total_tokens) DESC, token_usage.provider ASC
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
    let cost_join = cost_join_sql(cost_required);
    let cost_expr = cost_aggregate_sql(cost_required);
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
            {cost_expr},
            COUNT(*),
            COUNT(DISTINCT token_usage.provider || ':' || token_usage.session_id)
         FROM token_usage
         {cost_join}
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
    let summary = query_summary(conn, filter)?;
    let assumptions = cost_assumptions(conn, filter)?;
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

fn cost_assumptions(conn: &Connection, filter: &QueryFilter) -> Result<Vec<CostAssumption>> {
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

    let cutoff = Utc::now().date_naive() - TimeDelta::days(SOURCE_STALE_AFTER_DAYS);
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

    "LEFT JOIN (
        SELECT
            c.provider,
            c.request_id,
            COUNT(DISTINCT c.id) AS component_count,
            CASE
                WHEN COUNT(p.id) = COUNT(DISTINCT c.id)
                THEN SUM(c.tokens * p.rate_per_1m_tokens) / 1000000.0
                ELSE NULL
            END AS cost_usd
        FROM usage_billing_components c
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
        GROUP BY c.provider, c.request_id
    ) component_cost
      ON component_cost.provider = token_usage.provider
     AND component_cost.request_id = token_usage.request_id"
}

fn cost_aggregate_sql(cost_required: bool) -> String {
    if !cost_required {
        return "0.0".into();
    }
    let has_billable_tokens = has_billable_tokens_sql();
    format!(
        "CASE
            WHEN COALESCE(SUM(
                CASE
                    WHEN component_cost.cost_usd IS NULL
                     AND (
                        component_cost.component_count IS NOT NULL
                        OR {has_billable_tokens}
                     )
                    THEN 1
                    ELSE 0
                END
            ), 0) > 0
            THEN NULL
            ELSE COALESCE(SUM(component_cost.cost_usd), 0.0)
        END"
    )
}

fn has_billable_tokens_sql() -> String {
    TokenCategory::ALL
        .into_iter()
        .map(|category| format!("({}) > 0", billable_tokens_sql(category)))
        .collect::<Vec<_>>()
        .join(" OR ")
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

fn period_group_expr(period: TimePeriod, timezone: ReportTimeZone) -> String {
    period.sql_group_expr(timezone)
}

fn report_day_expr(timezone: ReportTimeZone) -> String {
    day_sql_expr(timezone)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CoverageKey {
    provider: String,
    model_id: String,
    category: TokenCategory,
    dimensions: PricingDimensions,
}

fn validate_pricing_coverage(conn: &Connection, filter: &QueryFilter) -> Result<()> {
    if table_exists(conn, "usage_billing_components")?
        && table_row_count(conn, "usage_billing_components")? > 0
    {
        return validate_component_pricing_coverage(conn, filter);
    }

    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT provider, model_id, timestamp, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cached_input_tokens, reasoning_output_tokens
         FROM token_usage
         WHERE 1=1 {where_clause}"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut coverage: HashMap<CoverageKey, Vec<DateTime<Utc>>> = HashMap::new();
    let mut rows = stmt.query(param_refs.as_slice())?;

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let timestamp = parse_canonical_utc_rfc3339(&timestamp).map_err(|err| {
            anyhow::anyhow!(
                "missing pricing coverage for provider={provider}, model={model_id}, category=timestamp, usage timestamp {timestamp}; {err}, reingest or repair token_usage.timestamp"
            )
        })?;
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            bail!(
                "missing pricing coverage for provider={provider}, model={model_id}, category=provider, usage range {} to {}; unsupported provider id in usage row, reingest or repair the database with a supported provider id such as claude-code or codex",
                format_utc_rfc3339(timestamp),
                format_utc_rfc3339(timestamp)
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
                dimensions: PricingDimensions::default(),
            };
            coverage.entry(key).or_default().push(timestamp);
        }
    }

    for (key, timestamps) in coverage {
        validate_category_coverage(conn, &key, timestamps)?;
    }

    Ok(())
}

fn validate_component_pricing_coverage(conn: &Connection, filter: &QueryFilter) -> Result<()> {
    let (where_clause, params) = build_where_clause(filter);
    let sql = format!(
        "SELECT c.provider, c.model_id, c.timestamp, c.token_category, c.service_tier, c.speed,
                c.region, c.processing_mode, c.source_detail
         FROM (SELECT provider, request_id FROM token_usage WHERE 1=1 {where_clause}) token_usage
         JOIN usage_billing_components c
           ON c.provider = token_usage.provider
          AND c.request_id = token_usage.request_id"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut coverage: HashMap<CoverageKey, Vec<DateTime<Utc>>> = HashMap::new();
    let mut rows = stmt.query(param_refs.as_slice())?;

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let token_category: String = row.get(3)?;
        let timestamp = parse_canonical_utc_rfc3339(&timestamp).map_err(|err| {
            anyhow::anyhow!(
                "missing pricing coverage for provider={provider}, model={model_id}, category=timestamp, usage timestamp {timestamp}; {err}, reingest or repair usage_billing_components.timestamp"
            )
        })?;
        let category = token_category.parse::<TokenCategory>().map_err(|err| {
            anyhow::anyhow!(
                "missing pricing coverage for provider={provider}, model={model_id}, category={token_category}, usage range {} to {}; {err}, reingest or repair usage_billing_components.token_category",
                format_utc_rfc3339(timestamp),
                format_utc_rfc3339(timestamp)
            )
        })?;
        let key = CoverageKey {
            provider,
            model_id,
            category,
            dimensions: PricingDimensions {
                service_tier: row.get(4)?,
                speed: row.get(5)?,
                region: row.get(6)?,
                processing_mode: row.get(7)?,
                source_detail: row.get(8)?,
            },
        };
        coverage.entry(key).or_default().push(timestamp);
    }

    for (key, timestamps) in coverage {
        validate_category_coverage(conn, &key, timestamps)?;
    }

    Ok(())
}

fn validate_category_coverage(
    conn: &Connection,
    key: &CoverageKey,
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
                key.provider,
                key.model_id,
                key.category.as_str(),
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
                key.category,
                dimension_suffix(&key.dimensions),
                format_utc_rfc3339(start),
                format_utc_rfc3339(end),
                format_utc_rfc3339(timestamp)
            ),
            1 => {}
            _ => bail!(
                "overlapping pricing intervals for provider={}, model={}, category={}{} near {}, usage range {} to {}",
                key.provider,
                key.model_id,
                key.category,
                dimension_suffix(&key.dimensions),
                format_utc_rfc3339(timestamp),
                format_utc_rfc3339(start),
                format_utc_rfc3339(end)
            ),
        }
    }

    Ok(())
}

fn dimension_suffix(dimensions: &PricingDimensions) -> String {
    let mut parts = Vec::new();
    if let Some(value) = &dimensions.service_tier {
        parts.push(format!("service_tier={value}"));
    }
    if let Some(value) = &dimensions.speed {
        parts.push(format!("speed={value}"));
    }
    if let Some(value) = &dimensions.region {
        parts.push(format!("region={value}"));
    }
    if let Some(value) = &dimensions.processing_mode {
        parts.push(format!("processing_mode={value}"));
    }
    if let Some(value) = &dimensions.source_detail {
        parts.push(format!("source_detail={value}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(", {}", parts.join(","))
    }
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn table_row_count(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
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
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::db::pricing::{
        PricingSourceMetadata, calculate_record_cost, upsert_source_metadata,
    };
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
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            service_tier: None,
            speed: None,
            region: None,
            processing_mode: None,
            cost_usd: (input as f64 * 15.0 + output as f64 * 75.0) / 1_000_000.0,
            project: project.into(),
            source_file: "/test.jsonl".into(),
            is_subagent: false,
        }
    }

    fn sqlite_local_day(conn: &Connection, timestamp: &str) -> String {
        conn.query_row("SELECT date(?1, 'localtime')", [timestamp], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn sqlite_local_hour(conn: &Connection, timestamp: &str) -> String {
        conn.query_row(
            "SELECT strftime('%Y-%m-%d %H:00', ?1, 'localtime')",
            [timestamp],
            |row| row.get(0),
        )
        .unwrap()
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

    fn with_speed(mut interval: PricingInterval, speed: &str) -> PricingInterval {
        interval.dimensions.speed = Some(speed.into());
        interval
    }

    fn reviewed_source(source: &str) -> PricingSourceMetadata {
        PricingSourceMetadata {
            source: source.into(),
            source_url: "https://example.com/pricing".into(),
            source_retrieved_at: "2026-05-23".into(),
            catalog_version: "1".into(),
            source_kind: "reviewed".into(),
            notes: "reviewed test pricing source".into(),
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
        codex.processing_mode = Some("standard".into());
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
        codex.processing_mode = Some("standard".into());
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
        codex.processing_mode = Some("standard".into());
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
        unknown.processing_mode = Some("standard".into());
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
    fn test_query_daily_and_date_filters_default_to_local_boundaries() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let before_local_midnight = "2026-04-08T06:30:00+00:00";
        let after_local_midnight = "2026-04-08T07:30:00+00:00";
        db.insert_records(&[
            make_record("local-late", before_local_midnight, "opus", "proj-a", 10, 1),
            make_record("local-early", after_local_midnight, "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let filter = QueryFilter {
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
        let expected_days = [
            sqlite_local_day(db.conn(), before_local_midnight),
            sqlite_local_day(db.conn(), after_local_midnight),
        ];
        let mut expected_periods = vec![expected_days[0].as_str()];
        if expected_days[1] != expected_days[0] {
            expected_periods.push(expected_days[1].as_str());
        }
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            expected_periods
        );

        let filter = QueryFilter {
            begin: NaiveDate::parse_from_str(&expected_days[0], "%Y-%m-%d").ok(),
            end: NaiveDate::parse_from_str(&expected_days[0], "%Y-%m-%d").ok(),
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        if expected_days[0] == expected_days[1] {
            assert_eq!(summary.request_count, 2);
            assert_eq!(summary.input_tokens, 30);
        } else {
            assert_eq!(summary.request_count, 1);
            assert_eq!(summary.input_tokens, 10);
        }
    }

    #[test]
    fn test_query_daily_and_date_filters_can_use_utc_boundaries() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[
            make_record("late", "2026-04-07T23:30:00Z", "opus", "proj-a", 10, 1),
            make_record("early", "2026-04-08T00:30:00Z", "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let filter = QueryFilter {
            report_timezone: ReportTimeZone::Utc,
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
            report_timezone: ReportTimeZone::Utc,
            include_subagents: true,
            ..Default::default()
        };
        let summary = query_summary(db.conn(), &filter).unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.input_tokens, 20);
    }

    #[test]
    fn test_hourly_grouping_defaults_to_local_time() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let first = "2026-04-08T07:30:00+00:00";
        let second = "2026-04-08T08:30:00+00:00";
        db.insert_records(&[
            make_record("local-hour-1", first, "opus", "proj-a", 10, 1),
            make_record("local-hour-2", second, "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let local_day = sqlite_local_day(db.conn(), first);
        let filter = QueryFilter {
            begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            vec![
                sqlite_local_hour(db.conn(), first),
                sqlite_local_hour(db.conn(), second)
            ]
        );
    }

    #[test]
    fn test_hourly_grouping_can_use_utc_across_dst_spring_boundary() {
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
            report_timezone: ReportTimeZone::Utc,
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
    fn test_hourly_grouping_can_use_utc_across_dst_fall_boundary() {
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
            report_timezone: ReportTimeZone::Utc,
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
    fn test_local_hourly_gap_fill_skips_spring_forward_nonexistent_hour() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let first = "2026-03-08T09:30:00Z";
        let second = "2026-03-08T10:30:00Z";
        db.insert_records(&[
            make_record("spring-local-1", first, "opus", "proj-a", 10, 1),
            make_record("spring-local-2", second, "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let local_day = sqlite_local_day(db.conn(), first);
        let filter = QueryFilter {
            begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
        let expected = vec![
            sqlite_local_hour(db.conn(), first),
            sqlite_local_hour(db.conn(), second),
        ];
        assert_eq!(
            rows.iter()
                .map(|row| row.period.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            rows.iter().all(|row| !row.period.ends_with("02:00")),
            "local gap filling should not synthesize the nonexistent DST spring-forward hour"
        );
    }

    #[test]
    fn test_local_hourly_gap_fill_combines_fall_back_repeated_hour() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let first = "2026-11-01T08:30:00Z";
        let second = "2026-11-01T09:30:00Z";
        db.insert_records(&[
            make_record("fall-local-1", first, "opus", "proj-a", 10, 1),
            make_record("fall-local-2", second, "opus", "proj-a", 20, 1),
        ])
        .unwrap();

        let local_day = sqlite_local_day(db.conn(), first);
        let filter = QueryFilter {
            begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
            include_subagents: true,
            ..Default::default()
        };
        let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
        let first_label = sqlite_local_hour(db.conn(), first);
        let second_label = sqlite_local_hour(db.conn(), second);
        assert_eq!(first_label, second_label);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].period, first_label);
        assert_eq!(rows[0].request_count, 2);
        assert_eq!(rows[0].input_tokens, 30);
    }

    #[test]
    fn test_report_timezone_does_not_change_cost_totals() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[
            make_record(
                "cost-local-1",
                "2026-04-08T06:30:00Z",
                "opus",
                "proj-a",
                10,
                1,
            ),
            make_record(
                "cost-local-2",
                "2026-04-08T07:30:00Z",
                "opus",
                "proj-a",
                20,
                2,
            ),
        ])
        .unwrap();

        let local = QueryFilter {
            report_timezone: ReportTimeZone::Local,
            include_subagents: true,
            ..Default::default()
        };
        let utc = QueryFilter {
            report_timezone: ReportTimeZone::Utc,
            include_subagents: true,
            ..Default::default()
        };
        let local_total = query_summary(db.conn(), &local).unwrap().cost_usd;
        let utc_total = query_summary(db.conn(), &utc).unwrap().cost_usd;
        assert!((local_total - utc_total).abs() < f64::EPSILON);
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
        let filled = fill_naive_gaps(TimePeriod::Daily, rows);
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
    fn test_query_cost_joins_component_pricing_dimensions() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
        db.insert_pricing_interval(&with_speed(
            price(
                "claude-opus-4-6",
                TokenCategory::Input,
                20.0,
                "2026-01-01T00:00:00Z",
                None,
            ),
            "fast",
        ))
        .unwrap();
        let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
        record.output_tokens = 0;
        record.speed = Some("fast".into());
        db.insert_records(&[record]).unwrap();

        let summary = query_summary(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!((summary.cost_usd - 20.0).abs() < 0.001);
    }

    #[test]
    fn test_explain_cost_high_confidence_when_modifiers_and_reviewed_source_are_explicit() {
        let db = Database::open_in_memory().unwrap();
        let mut interval = price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        );
        interval.dimensions.service_tier = Some("standard".into());
        interval.dimensions.speed = Some("fast".into());
        interval.dimensions.region = Some("us".into());
        interval.source = "reviewed:explicit-source".into();
        db.insert_pricing_interval(&interval).unwrap();
        upsert_source_metadata(db.conn(), &reviewed_source("reviewed:explicit-source")).unwrap();
        let mut record = make_record(
            "explicit",
            "2026-04-05T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        record.output_tokens = 0;
        record.service_tier = Some("standard".into());
        record.speed = Some("fast".into());
        record.region = Some("us".into());
        db.insert_records(&[record]).unwrap();

        let explanation = explain_cost(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(explanation.confidence, CostConfidence::High);
        assert_eq!(explanation.component_count, 1);
        assert_eq!(explanation.cost_usd, 10.0);
        assert!(explanation.assumptions.is_empty());
    }

    #[test]
    fn test_explain_cost_reports_assumed_standard_processing_mode() {
        let db = Database::open_in_memory().unwrap();
        let mut interval = provider_price(
            "codex",
            "gpt-explain",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        );
        interval.dimensions.processing_mode = Some("standard".into());
        interval.source = "reviewed:codex-source".into();
        db.insert_pricing_interval(&interval).unwrap();
        upsert_source_metadata(db.conn(), &reviewed_source("reviewed:codex-source")).unwrap();
        let mut record = make_record(
            "codex-standard",
            "2026-04-05T10:00:00Z",
            "unknown",
            "proj-a",
            1_000_000,
            0,
        );
        record.provider = crate::domain::provider::ProviderId::Codex;
        record.model = ModelFamily::Unknown;
        record.model_id = "gpt-explain".into();
        record.output_tokens = 0;
        record.processing_mode = Some("standard".into());
        db.insert_records(&[record]).unwrap();

        let explanation = explain_cost(
            db.conn(),
            &QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::Codex),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(explanation.confidence, CostConfidence::Estimated);
        assert!(explanation.assumptions.iter().any(|assumption| {
            assumption.kind == CostAssumptionKind::AssumedDefaultModifier
                && assumption.dimension.as_deref() == Some("processing_mode")
                && assumption.value.as_deref() == Some("standard")
        }));
    }

    #[test]
    fn test_explain_cost_reports_bundled_pricing_source() {
        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let mut record = make_record(
            "bundled",
            "2026-04-05T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        record.output_tokens = 0;
        db.insert_records(&[record]).unwrap();

        let explanation = explain_cost(
            db.conn(),
            &QueryFilter {
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(explanation.confidence, CostConfidence::Estimated);
        assert!(explanation.assumptions.iter().any(|assumption| {
            assumption.kind == CostAssumptionKind::BundledPricingSource
                && assumption
                    .source
                    .as_deref()
                    .is_some_and(|source| source.starts_with("seed:"))
        }));
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
    fn test_query_fails_when_component_pricing_modifier_is_missing() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
        let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
        record.output_tokens = 0;
        record.speed = Some("fast".into());
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
        assert!(err.contains("speed=fast"));
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
    fn test_query_coverage_ignores_unobserved_price_gaps() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        ))
        .unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-03-01T00:00:00Z",
            None,
        ))
        .unwrap();
        let mut before = make_record(
            "before-gap",
            "2026-01-15T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        before.output_tokens = 0;
        let mut after = make_record(
            "after-gap",
            "2026-03-15T10:00:00Z",
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
        assert_eq!(summary.cost_usd, 20.0);
    }

    #[test]
    fn test_query_coverage_fails_for_observed_price_gap() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        ))
        .unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-03-01T00:00:00Z",
            None,
        ))
        .unwrap();
        let mut record = make_record(
            "in-gap",
            "2026-02-15T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
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
        assert!(err.contains("usage timestamp 2026-02-15T10:00:00+00:00"));
    }

    #[test]
    fn test_query_coverage_detects_dimension_specific_overlap() {
        let db = Database::open_in_memory().unwrap();
        db.insert_pricing_interval(&price(
            "claude-opus-4-6",
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
        db.insert_pricing_interval(&with_speed(
            price(
                "claude-opus-4-6",
                TokenCategory::Input,
                20.0,
                "2026-01-01T00:00:00Z",
                None,
            ),
            "fast",
        ))
        .unwrap();
        db.insert_pricing_interval(&with_speed(
            price(
                "claude-opus-4-6",
                TokenCategory::Input,
                30.0,
                "2026-02-01T00:00:00Z",
                None,
            ),
            "fast",
        ))
        .unwrap();
        let mut record = make_record(
            "fast",
            "2026-04-05T10:00:00Z",
            "opus",
            "proj-a",
            1_000_000,
            0,
        );
        record.output_tokens = 0;
        record.speed = Some("fast".into());
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
        assert!(err.contains("speed=fast"));
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
