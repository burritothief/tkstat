use anyhow::{Result, anyhow};
use rusqlite::Connection;

use crate::domain::provider::ProviderId;

/// Recompute every materialized usage cost from normalized billing components.
///
/// This is used by schema migrations and explicit repair paths. Normal ingestion
/// prices only newly inserted usage rows, while catalog updates reprice only the
/// providers whose pricing state was marked dirty by SQLite triggers.
pub(crate) fn reprice_all_usage(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE pricing_state
         SET generation = generation + CASE WHEN dirty = 1 THEN 1 ELSE 0 END,
             dirty = 0",
        [],
    )?;
    conn.execute("DELETE FROM usage_costs", [])?;
    materialize_usage_costs(conn, None, None)?;
    conn.execute(
        "UPDATE integrity_state SET billing_components_dirty = 0 WHERE id = 1",
        [],
    )?;
    Ok(())
}

pub(crate) fn reprice_usage_range(conn: &Connection, first: i64, last: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM usage_costs WHERE usage_id BETWEEN ?1 AND ?2",
        [first, last],
    )?;
    materialize_usage_costs(conn, None, Some((first, last)))
}

pub(crate) fn reprice_provider_usage(conn: &Connection, provider: ProviderId) -> Result<()> {
    conn.execute(
        "UPDATE pricing_state
         SET generation = generation + 1, dirty = 0,
             last_refreshed_at = datetime('now')
         WHERE provider = ?1",
        [provider.as_str()],
    )?;
    conn.execute(
        "DELETE FROM usage_costs
         WHERE usage_id IN (SELECT id FROM token_usage WHERE provider = ?1)",
        [provider.as_str()],
    )?;
    materialize_usage_costs(conn, Some(provider), None)?;
    Ok(())
}

pub(crate) fn reprice_dirty_usage(conn: &Connection) -> Result<()> {
    let providers = {
        let mut stmt = conn.prepare("SELECT provider FROM pricing_state WHERE dirty = 1")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for provider in providers {
        let provider = ProviderId::from_canonical(&provider)
            .ok_or_else(|| anyhow!("unsupported provider id '{provider}' in pricing_state"))?;
        reprice_provider_usage(conn, provider)?;
    }
    Ok(())
}

fn materialize_usage_costs(
    conn: &Connection,
    provider: Option<ProviderId>,
    usage_range: Option<(i64, i64)>,
) -> Result<()> {
    let mut filters = Vec::new();
    let mut component_filters = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(provider) = provider {
        params.push(Box::new(provider.as_str().to_string()));
        filters.push(format!("u.provider = ?{}", params.len()));
        component_filters.push(format!("c.provider = ?{}", params.len()));
    }
    if let Some((first, last)) = usage_range {
        params.push(Box::new(first));
        let first_param = params.len();
        params.push(Box::new(last));
        let last_param = params.len();
        filters.push(format!("u.id BETWEEN ?{first_param} AND ?{last_param}"));
        component_filters.push(format!(
            "c.usage_id BETWEEN ?{first_param} AND ?{last_param}"
        ));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", filters.join(" AND "))
    };
    let component_where_clause = if component_filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", component_filters.join(" AND "))
    };
    let sql = format!(
        "WITH component_matches AS (
             SELECT c.usage_id, c.id,
                    COUNT(p.id) AS price_matches,
                    CASE WHEN COUNT(p.id) = 1
                         THEN SUM(c.tokens * p.rate_per_1m_tokens) / 1000000.0
                         ELSE NULL
                    END AS component_cost
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
             {component_where_clause}
             GROUP BY c.id
         ),
         usage_price AS (
             SELECT usage_id,
                    COUNT(*) AS component_count,
                    SUM(CASE WHEN price_matches = 1 THEN 1 ELSE 0 END) AS priced_components,
                    SUM(CASE WHEN price_matches > 1 THEN 1 ELSE 0 END) AS ambiguous_components,
                    SUM(component_cost) AS cost_usd
             FROM component_matches
             GROUP BY usage_id
         )
         INSERT OR REPLACE INTO usage_costs
             (usage_id, cost_usd, status, pricing_generation, detail)
         SELECT u.id,
                CASE
                    WHEN COALESCE(up.ambiguous_components, 0) > 0 THEN NULL
                    WHEN up.component_count IS NULL
                     AND (u.input_tokens + u.output_tokens + u.cache_creation_tokens
                          + u.cache_read_tokens + u.cached_input_tokens
                          + u.reasoning_output_tokens) > 0 THEN NULL
                    WHEN COALESCE(up.priced_components, 0) < COALESCE(up.component_count, 0) THEN NULL
                    ELSE COALESCE(up.cost_usd, 0.0)
                END,
                CASE
                    WHEN COALESCE(up.ambiguous_components, 0) > 0 THEN 'ambiguous'
                    WHEN up.component_count IS NULL
                     AND (u.input_tokens + u.output_tokens + u.cache_creation_tokens
                          + u.cache_read_tokens + u.cached_input_tokens
                          + u.reasoning_output_tokens) > 0 THEN 'integrity'
                    WHEN COALESCE(up.priced_components, 0) < COALESCE(up.component_count, 0) THEN 'missing'
                    ELSE 'priced'
                END,
                ps.generation,
                CASE
                    WHEN COALESCE(up.ambiguous_components, 0) > 0 THEN 'multiple prices match a billing component'
                    WHEN up.component_count IS NULL
                     AND (u.input_tokens + u.output_tokens + u.cache_creation_tokens
                          + u.cache_read_tokens + u.cached_input_tokens
                          + u.reasoning_output_tokens) > 0 THEN 'billable usage has no billing components'
                    WHEN COALESCE(up.priced_components, 0) < COALESCE(up.component_count, 0) THEN 'one or more billing components lack pricing'
                    ELSE ''
                END
         FROM token_usage u
         JOIN pricing_state ps ON ps.provider = u.provider
         LEFT JOIN usage_price up ON up.usage_id = u.id
         {where_clause}"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|param| param.as_ref()).collect();
    conn.execute(&sql, param_refs.as_slice())?;
    Ok(())
}
