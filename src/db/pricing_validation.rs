use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, types::ToSql};

use crate::db::{table_exists, table_row_count};
use crate::domain::pricing::{
    BillableTokenExpression, PricingDimensions, PricingKey, TokenCategory,
    billable_token_categories_for_counts, billable_token_expression,
};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::parse_canonical_utc_rfc3339;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageSource {
    TokenUsage,
    BillingComponent,
}

#[derive(Debug, Clone)]
pub(crate) enum UsageRowIssue {
    MalformedTimestamp {
        provider: String,
        model_id: String,
        timestamp: String,
        source: UsageSource,
    },
    UnsupportedProvider {
        provider: String,
        model_id: String,
        timestamp: DateTime<Utc>,
    },
    UnsupportedCategory {
        provider: String,
        model_id: String,
        category: String,
        timestamp: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct UsageObservations {
    pub usage: HashMap<PricingKey, Vec<DateTime<Utc>>>,
    pub issues: Vec<UsageRowIssue>,
}

pub(crate) fn collect_usage_observations(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> Result<UsageObservations> {
    if table_exists(conn, "usage_billing_components")?
        && table_row_count(conn, "usage_billing_components")? > 0
    {
        collect_component_observations(conn, where_clause, params)
    } else {
        collect_wide_observations(conn, where_clause, params)
    }
}

fn collect_component_observations(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> Result<UsageObservations> {
    let sql = format!(
        "SELECT c.provider, c.model_id, c.timestamp, c.token_category, c.service_tier, c.speed,
                c.region, c.processing_mode, c.source_detail
         FROM (SELECT id FROM token_usage WHERE 1=1 {where_clause}) token_usage
         JOIN usage_billing_components c ON c.usage_id = token_usage.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut observations = UsageObservations {
        usage: HashMap::new(),
        issues: Vec::new(),
    };
    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let raw_timestamp: String = row.get(2)?;
        let category: String = row.get(3)?;
        let Ok(timestamp) = parse_canonical_utc_rfc3339(&raw_timestamp) else {
            observations.issues.push(UsageRowIssue::MalformedTimestamp {
                provider,
                model_id,
                timestamp: raw_timestamp,
                source: UsageSource::BillingComponent,
            });
            continue;
        };
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            observations
                .issues
                .push(UsageRowIssue::UnsupportedProvider {
                    provider,
                    model_id,
                    timestamp,
                });
            continue;
        };
        let Ok(token_category) = category.parse::<TokenCategory>() else {
            observations
                .issues
                .push(UsageRowIssue::UnsupportedCategory {
                    provider,
                    model_id,
                    category,
                    timestamp,
                });
            continue;
        };
        observations
            .usage
            .entry(PricingKey {
                provider: provider_id,
                model_id,
                token_category,
                dimensions: PricingDimensions {
                    service_tier: row.get(4)?,
                    speed: row.get(5)?,
                    region: row.get(6)?,
                    processing_mode: row.get(7)?,
                    source_detail: row.get(8)?,
                },
            })
            .or_default()
            .push(timestamp);
    }
    Ok(observations)
}

fn collect_wide_observations(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> Result<UsageObservations> {
    let sql = format!(
        "SELECT provider, model_id, timestamp, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cached_input_tokens, reasoning_output_tokens
         FROM token_usage
         WHERE 1=1 {where_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut observations = UsageObservations {
        usage: HashMap::new(),
        issues: Vec::new(),
    };
    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let raw_timestamp: String = row.get(2)?;
        let Ok(timestamp) = parse_canonical_utc_rfc3339(&raw_timestamp) else {
            observations.issues.push(UsageRowIssue::MalformedTimestamp {
                provider,
                model_id,
                timestamp: raw_timestamp,
                source: UsageSource::TokenUsage,
            });
            continue;
        };
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            observations
                .issues
                .push(UsageRowIssue::UnsupportedProvider {
                    provider,
                    model_id,
                    timestamp,
                });
            continue;
        };
        for (token_category, _) in billable_token_categories_for_counts(
            provider_id,
            row.get::<_, i64>(3)?.max(0) as u64,
            row.get::<_, i64>(4)?.max(0) as u64,
            row.get::<_, i64>(5)?.max(0) as u64,
            row.get::<_, i64>(6)?.max(0) as u64,
            row.get::<_, i64>(7)?.max(0) as u64,
            row.get::<_, i64>(8)?.max(0) as u64,
        ) {
            observations
                .usage
                .entry(PricingKey {
                    provider: provider_id,
                    model_id: model_id.clone(),
                    token_category,
                    dimensions: PricingDimensions::default(),
                })
                .or_default()
                .push(timestamp);
        }
    }
    Ok(observations)
}

#[derive(Debug, Clone)]
pub(crate) struct BillingIntegrityIssue {
    pub provider: String,
    pub request_id: String,
    pub model_id: String,
    pub category: String,
    pub timestamp: Option<String>,
    pub detail: String,
}

impl BillingIntegrityIssue {
    pub fn report_error(&self) -> String {
        format!(
            "billing component integrity error for provider={}, request_id={}, model={}, category={}: {}",
            self.provider, self.request_id, self.model_id, self.category, self.detail
        )
    }
}

pub(crate) fn scan_billing_integrity(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
    limit: Option<usize>,
) -> Result<Vec<BillingIntegrityIssue>> {
    if !table_exists(conn, "usage_billing_components")?
        || table_row_count(conn, "usage_billing_components")? == 0
    {
        return Ok(Vec::new());
    }

    let mut issues = Vec::new();
    collect_orphans(conn, &mut issues, limit)?;
    if limit_reached(&issues, limit) {
        return Ok(issues);
    }
    collect_duplicates(conn, &mut issues, limit)?;
    if limit_reached(&issues, limit) {
        return Ok(issues);
    }
    collect_mismatches(conn, where_clause, params, &mut issues, limit)?;
    if limit_reached(&issues, limit) {
        return Ok(issues);
    }
    collect_unexpected(conn, where_clause, params, &mut issues, limit)?;
    Ok(issues)
}

fn remaining_limit(issues: &[BillingIntegrityIssue], limit: Option<usize>) -> String {
    limit
        .map(|limit| format!(" LIMIT {}", limit.saturating_sub(issues.len())))
        .unwrap_or_default()
}

fn limit_reached(issues: &[BillingIntegrityIssue], limit: Option<usize>) -> bool {
    limit.is_some_and(|limit| issues.len() >= limit)
}

fn collect_orphans(
    conn: &Connection,
    issues: &mut Vec<BillingIntegrityIssue>,
    limit: Option<usize>,
) -> Result<()> {
    let sql = format!(
        "SELECT c.provider, c.request_id, c.model_id, c.token_category
         FROM usage_billing_components c
         LEFT JOIN token_usage u
           ON u.id = c.usage_id
          AND u.provider = c.provider
          AND u.request_id = c.request_id
         WHERE u.id IS NULL
         ORDER BY c.provider, c.request_id, c.component_ordinal{}",
        remaining_limit(issues, limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let request_id: String = row.get(1)?;
        Ok(BillingIntegrityIssue {
            provider: row.get(0)?,
            request_id: request_id.clone(),
            model_id: row.get(2)?,
            category: row.get(3)?,
            timestamp: None,
            detail: format!(
                "usage_billing_components row for request_id={request_id} has no matching token_usage row; reingest or repair usage_billing_components"
            ),
        })
    })?;
    issues.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(())
}

fn collect_duplicates(
    conn: &Connection,
    issues: &mut Vec<BillingIntegrityIssue>,
    limit: Option<usize>,
) -> Result<()> {
    let sql = format!(
        "SELECT provider, request_id, model_id, token_category, COUNT(*)
         FROM usage_billing_components
         GROUP BY provider, request_id, token_category,
                  COALESCE(service_tier, ''), COALESCE(speed, ''), COALESCE(region, ''),
                  COALESCE(processing_mode, ''), COALESCE(source_detail, '')
         HAVING COUNT(*) > 1
         ORDER BY provider, request_id, token_category{}",
        remaining_limit(issues, limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let request_id: String = row.get(1)?;
        let count: i64 = row.get(4)?;
        Ok(BillingIntegrityIssue {
            provider: row.get(0)?,
            request_id: request_id.clone(),
            model_id: row.get(2)?,
            category: row.get(3)?,
            timestamp: None,
            detail: format!(
                "request_id={request_id} has {count} duplicate billing components for the same pricing dimensions; reingest or repair usage_billing_components"
            ),
        })
    })?;
    issues.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(())
}

fn collect_mismatches(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
    issues: &mut Vec<BillingIntegrityIssue>,
    limit: Option<usize>,
) -> Result<()> {
    let expected_sql = expected_component_union_sql();
    let sql = format!(
        "WITH filtered_usage AS (
             SELECT * FROM token_usage WHERE 1=1 {where_clause}
         ), expected AS ({expected_sql}), actual AS (
             SELECT c.provider, c.request_id, c.token_category, SUM(c.tokens) AS actual_tokens
             FROM filtered_usage token_usage
             JOIN usage_billing_components c ON c.usage_id = token_usage.id
             GROUP BY c.provider, c.request_id, c.token_category
         )
         SELECT e.provider, e.request_id, e.model_id, e.timestamp, e.token_category,
                e.expected_tokens, COALESCE(a.actual_tokens, 0)
         FROM expected e
         LEFT JOIN actual a ON a.provider = e.provider
                           AND a.request_id = e.request_id
                           AND a.token_category = e.token_category
         WHERE e.expected_tokens > 0 AND COALESCE(a.actual_tokens, 0) != e.expected_tokens
         ORDER BY e.provider, e.request_id, e.token_category{}",
        remaining_limit(issues, limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, |row| {
        let request_id: String = row.get(1)?;
        let expected: i64 = row.get(5)?;
        let actual: i64 = row.get(6)?;
        Ok(BillingIntegrityIssue {
            provider: row.get(0)?,
            request_id: request_id.clone(),
            model_id: row.get(2)?,
            timestamp: row.get(3)?,
            category: row.get(4)?,
            detail: format!(
                "request_id={request_id} expected {expected} billable tokens from token_usage but found {actual} in usage_billing_components; reingest or repair usage_billing_components"
            ),
        })
    })?;
    issues.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(())
}

fn collect_unexpected(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn ToSql],
    issues: &mut Vec<BillingIntegrityIssue>,
    limit: Option<usize>,
) -> Result<()> {
    let expected_sql = expected_component_union_sql();
    let sql = format!(
        "WITH filtered_usage AS (
             SELECT * FROM token_usage WHERE 1=1 {where_clause}
         ), expected AS ({expected_sql}), actual AS (
             SELECT c.provider, c.request_id, c.model_id, c.timestamp, c.token_category,
                    SUM(c.tokens) AS actual_tokens
             FROM filtered_usage token_usage
             JOIN usage_billing_components c ON c.usage_id = token_usage.id
             GROUP BY c.provider, c.request_id, c.model_id, c.timestamp, c.token_category
         )
         SELECT a.provider, a.request_id, a.model_id, a.timestamp, a.token_category, a.actual_tokens
         FROM actual a
         LEFT JOIN expected e ON e.provider = a.provider
                             AND e.request_id = a.request_id
                             AND e.token_category = a.token_category
                             AND e.expected_tokens > 0
         WHERE e.request_id IS NULL
         ORDER BY a.provider, a.request_id, a.token_category{}",
        remaining_limit(issues, limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, |row| {
        let request_id: String = row.get(1)?;
        let actual: i64 = row.get(5)?;
        Ok(BillingIntegrityIssue {
            provider: row.get(0)?,
            request_id: request_id.clone(),
            model_id: row.get(2)?,
            timestamp: row.get(3)?,
            category: row.get(4)?,
            detail: format!(
                "request_id={request_id} has {actual} unexpected billable tokens in usage_billing_components; reingest or repair usage_billing_components"
            ),
        })
    })?;
    issues.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(())
}

fn expected_component_union_sql() -> String {
    TokenCategory::ALL
        .into_iter()
        .map(|category| {
            format!(
                "SELECT provider, request_id, model_id, timestamp, '{}' AS token_category,
                        {} AS expected_tokens
                 FROM filtered_usage token_usage",
                category.as_str(),
                billable_tokens_sql(category)
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

fn billable_tokens_sql(category: TokenCategory) -> String {
    let default = billable_token_expression(ProviderId::ClaudeCode, category);
    let mut sql = token_expression_sql(default);
    for provider in ProviderId::ALL.into_iter().rev() {
        let expression = billable_token_expression(provider, category);
        if expression != default {
            sql = format!(
                "CASE WHEN token_usage.provider = '{}' THEN {} ELSE {} END",
                provider.as_str(),
                token_expression_sql(expression),
                sql
            );
        }
    }
    sql
}

fn token_expression_sql(expression: BillableTokenExpression) -> String {
    match expression {
        BillableTokenExpression::Field(category) => token_count_column(category).into(),
        BillableTokenExpression::SaturatingSub(minuend, subtrahend) => format!(
            "MAX({} - {}, 0)",
            token_count_column(minuend),
            token_count_column(subtrahend)
        ),
        BillableTokenExpression::Zero => "0".into(),
    }
}

fn token_count_column(category: TokenCategory) -> &'static str {
    match category {
        TokenCategory::Input => "input_tokens",
        TokenCategory::Output => "output_tokens",
        TokenCategory::CacheRead => "cache_read_tokens",
        TokenCategory::CacheCreation => "cache_creation_tokens",
        TokenCategory::CachedInput => "cached_input_tokens",
        TokenCategory::ReasoningOutput => "reasoning_output_tokens",
    }
}
