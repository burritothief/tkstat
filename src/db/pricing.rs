use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{Connection, types::ValueRef};
use serde::Deserialize;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::domain::pricing::{
    PricingDimensions, PricingInterval, TokenCategory, billable_token_categories_for_counts,
    billable_usage_components,
};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::{format_utc_rfc3339, parse_canonical_utc_rfc3339};
use crate::domain::usage::TokenRecord;

const BUNDLED_PRICING_CATALOG_JSON: &str = include_str!("../../pricing/catalog.json");
const SOURCE_STALE_AFTER_DAYS: i64 = 90;

pub trait PricingFetcher {
    fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>>;

    fn fetch_source_metadata(&self) -> Result<Vec<PricingSourceMetadata>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PricingAuditSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PricingAuditKind {
    MissingDatabase,
    MissingSchema,
    MalformedCatalogRow,
    MalformedUsageRow,
    UnsupportedProviderId,
    Gap,
    Overlap,
    DuplicateCurrent,
    MissingCurrent,
    UnsupportedCurrency,
    MissingCoverage,
    UsageBeforeFirstInterval,
    UsageAfterLastInterval,
    MissingSourceMetadata,
    StaleSource,
    BundledFallbackSource,
    UnknownObservedModel,
    UnsupportedModifier,
    BillingComponentIntegrity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PricingAuditFinding {
    pub severity: PricingAuditSeverity,
    pub kind: PricingAuditKind,
    pub provider: String,
    pub model_id: String,
    pub token_category: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub remediation: String,
}

#[derive(Debug, Clone, Copy)]
struct AuditKey<'a> {
    provider: &'a str,
    model_id: &'a str,
    category: TokenCategory,
    dimensions: &'a PricingDimensions,
}

#[derive(Debug, Clone, Copy)]
struct RawAuditKey<'a> {
    provider: &'a str,
    model_id: &'a str,
    category: &'a str,
}

type UsageCoverageKey = (String, String, TokenCategory, PricingDimensions);
type UsageCoverageRange = (DateTime<Utc>, DateTime<Utc>);

#[derive(Debug, Clone)]
struct RawPricingInterval {
    provider: String,
    model_id: String,
    token_category: String,
    service_tier: Option<String>,
    speed: Option<String>,
    region: Option<String>,
    processing_mode: Option<String>,
    source_detail: Option<String>,
    currency: String,
    rate_per_1m_tokens: Option<f64>,
    effective_from: String,
    effective_to: Option<String>,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingSourceMetadata {
    pub source: String,
    pub source_url: String,
    pub source_retrieved_at: String,
    pub catalog_version: String,
    pub source_kind: String,
    pub notes: String,
}

#[derive(Debug, Clone)]
struct PricingCatalogData {
    intervals: Vec<PricingInterval>,
    sources: Vec<PricingSourceMetadata>,
}

#[derive(Debug, Deserialize)]
struct PricingCatalog {
    schema_version: u32,
    notes: String,
    sources: Vec<CatalogSource>,
    entries: Vec<CatalogEntry>,
}

#[derive(Debug, Deserialize)]
struct CatalogSource {
    id: String,
    url: String,
    retrieved_at: String,
    notes: String,
}

#[derive(Debug, Deserialize)]
struct CatalogEntry {
    provider: String,
    model_ids: Vec<String>,
    #[serde(default)]
    model_aliases: Vec<String>,
    currency: String,
    effective_from: String,
    effective_to: Option<String>,
    source: String,
    source_ref: String,
    #[serde(default)]
    dimensions: CatalogDimensions,
    rates_per_1m_tokens: BTreeMap<String, f64>,
    notes: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CatalogDimensions {
    service_tier: Option<String>,
    speed: Option<String>,
    region: Option<String>,
    processing_mode: Option<String>,
    source_detail: Option<String>,
}

struct StaticPricingFetcher {
    intervals: Vec<PricingInterval>,
    sources: Vec<PricingSourceMetadata>,
}

impl PricingFetcher for StaticPricingFetcher {
    fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>> {
        Ok(self.intervals.clone())
    }

    fn fetch_source_metadata(&self) -> Result<Vec<PricingSourceMetadata>> {
        Ok(self.sources.clone())
    }
}

fn audit_key<'a>(
    provider: &'a str,
    model_id: &'a str,
    category: TokenCategory,
    dimensions: &'a PricingDimensions,
) -> AuditKey<'a> {
    AuditKey {
        provider,
        model_id,
        category,
        dimensions,
    }
}

fn raw_audit_key<'a>(provider: &'a str, model_id: &'a str, category: &'a str) -> RawAuditKey<'a> {
    RawAuditKey {
        provider,
        model_id,
        category,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BundledPricingFetcher;

impl PricingFetcher for BundledPricingFetcher {
    fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>> {
        Ok(seed_intervals())
    }

    fn fetch_source_metadata(&self) -> Result<Vec<PricingSourceMetadata>> {
        Ok(bundled_catalog_data()?.sources)
    }
}

pub fn insert_interval(conn: &Connection, interval: &PricingInterval) -> Result<()> {
    conn.execute(
        "INSERT INTO pricing_intervals
            (provider, model_id, token_category, service_tier, speed, region, processing_mode,
             source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            interval.provider.as_str(),
            interval.model_id,
            interval.token_category.as_str(),
            interval.dimensions.service_tier,
            interval.dimensions.speed,
            interval.dimensions.region,
            interval.dimensions.processing_mode,
            interval.dimensions.source_detail,
            interval.currency,
            interval.rate_per_1m_tokens,
            format_utc_rfc3339(interval.effective_from),
            interval.effective_to.map(format_utc_rfc3339),
            interval.source,
        ],
    )?;
    Ok(())
}

pub fn insert_interval_if_missing(conn: &Connection, interval: &PricingInterval) -> Result<bool> {
    validate_interval(interval)?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO pricing_intervals
            (provider, model_id, token_category, service_tier, speed, region, processing_mode,
             source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            interval.provider.as_str(),
            interval.model_id,
            interval.token_category.as_str(),
            interval.dimensions.service_tier,
            interval.dimensions.speed,
            interval.dimensions.region,
            interval.dimensions.processing_mode,
            interval.dimensions.source_detail,
            interval.currency,
            interval.rate_per_1m_tokens,
            format_utc_rfc3339(interval.effective_from),
            interval.effective_to.map(format_utc_rfc3339),
            interval.source,
        ],
    )?;
    Ok(changed > 0)
}

pub fn seed_pricing(conn: &Connection) -> Result<usize> {
    let data = bundled_catalog_data()?;
    seed_pricing_catalog_data(conn, &data)
}

#[cfg(test)]
fn seed_pricing_intervals(conn: &Connection, intervals: &[PricingInterval]) -> Result<usize> {
    for interval in intervals {
        validate_interval(interval)?;
    }

    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0;
    for interval in intervals {
        if insert_interval_if_missing(&tx, interval)? {
            inserted += 1;
        }
    }
    tx.commit()?;
    Ok(inserted)
}

fn seed_pricing_catalog_data(conn: &Connection, data: &PricingCatalogData) -> Result<usize> {
    for source in &data.sources {
        validate_source_metadata(source)?;
    }
    for interval in &data.intervals {
        validate_interval(interval)?;
    }

    let tx = conn.unchecked_transaction()?;
    for source in &data.sources {
        upsert_source_metadata(&tx, source)?;
    }
    let mut inserted = 0;
    for interval in &data.intervals {
        if insert_interval_if_missing(&tx, interval)? {
            inserted += 1;
        }
    }
    tx.commit()?;
    Ok(inserted)
}

pub fn refresh_pricing(conn: &Connection, fetcher: &dyn PricingFetcher) -> Result<usize> {
    let intervals = fetcher.fetch_current_prices()?;
    let sources = fetcher.fetch_source_metadata()?;
    let tx = conn.unchecked_transaction()?;
    for source in &sources {
        validate_source_metadata(source)?;
        upsert_source_metadata(&tx, source)?;
    }
    let mut changed = 0;
    for interval in intervals {
        validate_interval(&interval)?;
        changed += upsert_current_interval(&tx, &interval)?;
    }
    tx.commit()?;
    Ok(changed)
}

pub fn import_pricing_catalog_file(conn: &Connection, path: &Path) -> Result<usize> {
    let contents = std::fs::read_to_string(path)?;
    import_pricing_catalog_json(conn, &contents)
}

pub fn import_pricing_catalog_json(conn: &Connection, contents: &str) -> Result<usize> {
    let data = catalog_data_from_str(contents, "reviewed")?;
    refresh_pricing(
        conn,
        &StaticPricingFetcher {
            intervals: data.intervals,
            sources: data.sources,
        },
    )
}

pub fn upsert_source_metadata(conn: &Connection, source: &PricingSourceMetadata) -> Result<()> {
    validate_source_metadata(source)?;
    conn.execute(
        "INSERT OR REPLACE INTO pricing_sources
            (source, source_url, source_retrieved_at, catalog_version, source_kind, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source.source,
            source.source_url,
            source.source_retrieved_at,
            source.catalog_version,
            source.source_kind,
            source.notes,
        ],
    )?;
    Ok(())
}

fn upsert_current_interval(conn: &Connection, interval: &PricingInterval) -> Result<usize> {
    let existing = open_interval(
        conn,
        interval.provider,
        &interval.model_id,
        interval.token_category,
        &interval.dimensions,
    )?;

    match existing {
        Some(existing)
            if (existing.rate_per_1m_tokens - interval.rate_per_1m_tokens).abs() < f64::EPSILON
                && existing.currency == interval.currency =>
        {
            if existing.source == interval.source {
                return Ok(0);
            }
            conn.execute(
                "UPDATE pricing_intervals
                 SET source = ?1
                 WHERE provider = ?2
                   AND model_id = ?3
                   AND token_category = ?4
                   AND service_tier IS ?5
                   AND speed IS ?6
                   AND region IS ?7
                   AND processing_mode IS ?8
                   AND source_detail IS ?9
                   AND currency = ?10
                   AND effective_from = ?11
                   AND effective_to IS NULL",
                rusqlite::params![
                    interval.source,
                    existing.provider.as_str(),
                    existing.model_id,
                    existing.token_category.as_str(),
                    existing.dimensions.service_tier,
                    existing.dimensions.speed,
                    existing.dimensions.region,
                    existing.dimensions.processing_mode,
                    existing.dimensions.source_detail,
                    existing.currency,
                    format_utc_rfc3339(existing.effective_from),
                ],
            )?;
            Ok(1)
        }
        Some(existing) => {
            if existing.effective_from > interval.effective_from {
                bail!(
                    "stale pricing interval for provider={}, model={}, category={}: fetched effective_from {} is before current open interval {}",
                    interval.provider,
                    interval.model_id,
                    interval.token_category,
                    format_utc_rfc3339(interval.effective_from),
                    format_utc_rfc3339(existing.effective_from)
                );
            }
            if existing.effective_from == interval.effective_from {
                let changed = conn.execute(
                    "UPDATE pricing_intervals
                     SET rate_per_1m_tokens = ?1,
                         source = ?2,
                         effective_to = ?3
                     WHERE provider = ?4
                       AND model_id = ?5
                       AND token_category = ?6
                       AND service_tier IS ?7
                       AND speed IS ?8
                       AND region IS ?9
                       AND processing_mode IS ?10
                       AND source_detail IS ?11
                       AND currency = ?12
                       AND effective_from = ?13
                       AND effective_to IS NULL",
                    rusqlite::params![
                        interval.rate_per_1m_tokens,
                        interval.source,
                        interval.effective_to.map(format_utc_rfc3339),
                        existing.provider.as_str(),
                        existing.model_id,
                        existing.token_category.as_str(),
                        existing.dimensions.service_tier,
                        existing.dimensions.speed,
                        existing.dimensions.region,
                        existing.dimensions.processing_mode,
                        existing.dimensions.source_detail,
                        existing.currency,
                        format_utc_rfc3339(existing.effective_from),
                    ],
                )?;
                if changed != 1 {
                    bail!(
                        "failed to replace same-effective-date pricing interval for provider={}, model={}, category={}{}",
                        interval.provider,
                        interval.model_id,
                        interval.token_category,
                        dimension_suffix(&interval.dimensions)
                    );
                }
                return Ok(changed);
            }
            conn.execute(
                "UPDATE pricing_intervals
                 SET effective_to = ?1
                 WHERE provider = ?2
                   AND model_id = ?3
                   AND token_category = ?4
                   AND service_tier IS ?5
                   AND speed IS ?6
                   AND region IS ?7
                   AND processing_mode IS ?8
                   AND source_detail IS ?9
                   AND currency = ?10
                   AND effective_from = ?11",
                rusqlite::params![
                    format_utc_rfc3339(interval.effective_from),
                    existing.provider.as_str(),
                    existing.model_id,
                    existing.token_category.as_str(),
                    existing.dimensions.service_tier,
                    existing.dimensions.speed,
                    existing.dimensions.region,
                    existing.dimensions.processing_mode,
                    existing.dimensions.source_detail,
                    existing.currency,
                    format_utc_rfc3339(existing.effective_from),
                ],
            )?;
            if !insert_interval_if_missing(conn, interval)? {
                bail!(
                    "failed to insert refreshed pricing interval for provider={}, model={}, category={}, effective_from={}",
                    interval.provider,
                    interval.model_id,
                    interval.token_category,
                    format_utc_rfc3339(interval.effective_from)
                );
            }
            Ok(2)
        }
        None => Ok(insert_interval_if_missing(conn, interval)? as usize),
    }
}

fn open_interval(
    conn: &Connection,
    provider: ProviderId,
    model_id: &str,
    token_category: TokenCategory,
    dimensions: &PricingDimensions,
) -> Result<Option<PricingInterval>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, service_tier, speed, region, processing_mode,
                source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source
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
           AND effective_to IS NULL
         ORDER BY effective_from DESC",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                provider.as_str(),
                model_id,
                token_category.as_str(),
                dimensions.service_tier,
                dimensions.speed,
                dimensions.region,
                dimensions.processing_mode,
                dimensions.source_detail,
            ],
            row_to_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.into_iter().next()),
        _ => bail!(
            "multiple open prices for provider={provider}, model={model_id}, category={token_category}{}",
            dimension_suffix(dimensions)
        ),
    }
}

pub fn applicable_interval(
    conn: &Connection,
    provider: ProviderId,
    model_id: &str,
    token_category: TokenCategory,
    timestamp: DateTime<Utc>,
) -> Result<PricingInterval> {
    applicable_interval_for_dimensions(
        conn,
        provider,
        model_id,
        token_category,
        timestamp,
        &PricingDimensions::default(),
    )
}

pub fn applicable_interval_for_dimensions(
    conn: &Connection,
    provider: ProviderId,
    model_id: &str,
    token_category: TokenCategory,
    timestamp: DateTime<Utc>,
    dimensions: &PricingDimensions,
) -> Result<PricingInterval> {
    let dimensions = dimensions.clone().normalized_for_provider(provider);
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, service_tier, speed, region, processing_mode,
                source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source
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
           AND (effective_to IS NULL OR ?9 < effective_to)
         ORDER BY effective_from ASC",
    )?;

    let rows = stmt
        .query_map(
            rusqlite::params![
                provider.as_str(),
                model_id,
                token_category.as_str(),
                dimensions.service_tier,
                dimensions.speed,
                dimensions.region,
                dimensions.processing_mode,
                dimensions.source_detail,
                format_utc_rfc3339(timestamp),
            ],
            row_to_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    match rows.len() {
        0 => bail!(
            "missing price for provider={provider}, model={model_id}, category={token_category}{}, timestamp={}",
            dimension_suffix(&dimensions),
            format_utc_rfc3339(timestamp),
        ),
        1 => Ok(rows.into_iter().next().unwrap()),
        _ => bail!(
            "overlapping prices for provider={provider}, model={model_id}, category={token_category}{}, timestamp={}",
            dimension_suffix(&dimensions),
            format_utc_rfc3339(timestamp),
        ),
    }
}

pub fn calculate_record_cost(conn: &Connection, record: &TokenRecord) -> Result<f64> {
    let mut total = 0.0;
    for component in billable_usage_components(record) {
        let dimensions = PricingDimensions::from_component(&component);
        let interval = applicable_interval_for_dimensions(
            conn,
            component.provider,
            &component.model_id,
            component.token_category,
            component.timestamp,
            &dimensions,
        )?;
        total += interval.cost_for_tokens(component.tokens);
    }
    Ok(total)
}

pub fn audit_pricing(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if !table_exists(conn, "pricing_intervals")? {
        return Ok(vec![schema_finding(
            PricingAuditKind::MissingSchema,
            "pricing_intervals table is missing; run `tkstat --pricing-seed` or `tkstat --pricing-refresh`",
        )]);
    }

    let mut findings = audit_catalog(conn)?;
    if table_exists(conn, "token_usage")? {
        findings.extend(audit_billing_component_integrity(conn)?);
        findings.extend(audit_usage_coverage(conn)?);
        findings.extend(audit_usage_source_quality(conn)?);
    } else {
        findings.push(schema_finding(
            PricingAuditKind::MissingSchema,
            "token_usage table is missing; run `tkstat --force-update` to ingest usage before checking coverage",
        ));
    }
    Ok(findings)
}

fn audit_billing_component_integrity(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if !table_exists(conn, "usage_billing_components")?
        || table_row_count(conn, "usage_billing_components")? == 0
    {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    findings.extend(audit_orphan_billing_components(conn)?);
    findings.extend(audit_duplicate_billing_components(conn)?);
    findings.extend(audit_component_token_totals(conn)?);
    Ok(findings)
}

fn audit_orphan_billing_components(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT c.provider, c.request_id, c.model_id, c.token_category
         FROM usage_billing_components c
         LEFT JOIN token_usage u
           ON u.provider = c.provider
          AND u.request_id = c.request_id
         WHERE u.id IS NULL
         ORDER BY c.provider, c.request_id, c.component_ordinal",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(provider, request_id, model_id, category)| {
            billing_component_integrity_finding(
                &provider,
                &model_id,
                &category,
                &format!(
                    "usage_billing_components row for request_id={request_id} has no matching token_usage row; delete orphan components or reingest usage"
                ),
            )
        })
        .collect())
}

fn audit_duplicate_billing_components(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT provider, request_id, model_id, token_category, COUNT(*)
         FROM usage_billing_components
         GROUP BY provider, request_id, token_category,
                  COALESCE(service_tier, ''), COALESCE(speed, ''), COALESCE(region, ''),
                  COALESCE(processing_mode, ''), COALESCE(source_detail, '')
         HAVING COUNT(*) > 1
         ORDER BY provider, request_id, token_category",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(provider, request_id, model_id, category, count)| {
            billing_component_integrity_finding(
                &provider,
                &model_id,
                &category,
                &format!(
                    "request_id={request_id} has {count} duplicate billing components for the same pricing dimensions; reingest or repair usage_billing_components"
                ),
            )
        })
        .collect())
}

fn audit_component_token_totals(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut expected = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT provider, request_id, model_id, timestamp, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens, cached_input_tokens,
                reasoning_output_tokens
         FROM token_usage",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let request_id: String = row.get(1)?;
        let model_id: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            continue;
        };
        let categories = billable_token_categories_for_counts(
            provider_id,
            row.get::<_, i64>(4)?.max(0) as u64,
            row.get::<_, i64>(5)?.max(0) as u64,
            row.get::<_, i64>(6)?.max(0) as u64,
            row.get::<_, i64>(7)?.max(0) as u64,
            row.get::<_, i64>(8)?.max(0) as u64,
            row.get::<_, i64>(9)?.max(0) as u64,
        );
        for (category, tokens) in categories {
            expected.insert(
                (
                    provider.clone(),
                    request_id.clone(),
                    model_id.clone(),
                    category.as_str().to_string(),
                ),
                (tokens as i64, timestamp.clone()),
            );
        }
    }

    let mut actual = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT provider, request_id, model_id, token_category, SUM(tokens)
         FROM usage_billing_components
         GROUP BY provider, request_id, model_id, token_category",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        actual.insert(
            (
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ),
            row.get::<_, i64>(4)?,
        );
    }

    let mut findings = Vec::new();
    for ((provider, request_id, model_id, category), (expected_tokens, timestamp)) in &expected {
        let actual_tokens = actual
            .get(&(
                provider.clone(),
                request_id.clone(),
                model_id.clone(),
                category.clone(),
            ))
            .copied()
            .unwrap_or(0);
        if actual_tokens != *expected_tokens {
            findings.push(billing_component_integrity_finding_at(
                provider,
                model_id,
                category,
                Some(timestamp.clone()),
                &format!(
                    "request_id={request_id} expected {expected_tokens} billable tokens from token_usage but found {actual_tokens} in usage_billing_components; reingest or repair usage_billing_components"
                ),
            ));
        }
    }
    for ((provider, request_id, model_id, category), actual_tokens) in actual {
        if !expected.contains_key(&(
            provider.clone(),
            request_id.clone(),
            model_id.clone(),
            category.clone(),
        )) {
            findings.push(billing_component_integrity_finding(
                &provider,
                &model_id,
                &category,
                &format!(
                    "request_id={request_id} has {actual_tokens} unexpected billable tokens in usage_billing_components; reingest or repair usage_billing_components"
                ),
            ));
        }
    }

    Ok(findings)
}

fn audit_usage_source_quality(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if !table_exists(conn, "usage_billing_components")?
        || table_row_count(conn, "usage_billing_components")? == 0
    {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    findings.extend(audit_unknown_observed_models(conn)?);
    findings.extend(audit_unsupported_modifiers(conn)?);
    findings.extend(audit_used_pricing_sources(conn)?);
    Ok(findings)
}

fn audit_unknown_observed_models(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.provider, c.model_id
         FROM usage_billing_components c
         LEFT JOIN pricing_intervals p
           ON p.provider = c.provider
          AND p.model_id = c.model_id
         WHERE p.id IS NULL
         ORDER BY c.provider, c.model_id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(provider, model_id)| {
            finding_raw(
                PricingAuditSeverity::Warning,
                PricingAuditKind::UnknownObservedModel,
                raw_audit_key(&provider, &model_id, ""),
                None,
                None,
                "add reviewed pricing for the observed model id or import an updated pricing catalog",
            )
        })
        .collect())
}

fn audit_unsupported_modifiers(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if !pricing_intervals_have_dimensions(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.provider, c.model_id, c.token_category, c.service_tier, c.speed,
                c.region, c.processing_mode, c.source_detail
         FROM usage_billing_components c
         WHERE (c.service_tier IS NOT NULL
             OR c.speed IS NOT NULL
             OR c.region IS NOT NULL
             OR c.processing_mode IS NOT NULL
             OR c.source_detail IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1
               FROM pricing_intervals p
               WHERE p.provider = c.provider
                 AND p.model_id = c.model_id
                 AND p.token_category = c.token_category
                 AND p.service_tier IS c.service_tier
                 AND p.speed IS c.speed
                 AND p.region IS c.region
                 AND p.processing_mode IS c.processing_mode
                 AND p.source_detail IS c.source_detail
           )
         ORDER BY c.provider, c.model_id, c.token_category",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                PricingDimensions {
                    service_tier: row.get(3)?,
                    speed: row.get(4)?,
                    region: row.get(5)?,
                    processing_mode: row.get(6)?,
                    source_detail: row.get(7)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut findings = Vec::new();
    for (provider, model_id, token_category, dimensions) in rows {
        let Ok(category) = token_category.parse::<TokenCategory>() else {
            continue;
        };
        findings.push(finding(
            PricingAuditSeverity::Warning,
            PricingAuditKind::UnsupportedModifier,
            audit_key(&provider, &model_id, category, &dimensions),
            None,
            None,
            "add a specialized pricing interval for the observed modifier combination",
        ));
    }
    Ok(findings)
}

fn audit_used_pricing_sources(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if table_exists(conn, "pricing_sources")? {
        audit_used_pricing_sources_with_metadata(conn)
    } else {
        audit_used_pricing_sources_without_metadata(conn)
    }
}

fn audit_used_pricing_sources_without_metadata(
    conn: &Connection,
) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.provider, c.model_id, c.token_category, p.effective_from,
                p.effective_to, p.source
         FROM usage_billing_components c
         JOIN pricing_intervals p
           ON p.provider = c.provider
          AND p.model_id = c.model_id
          AND p.token_category = c.token_category
          AND p.effective_from <= c.timestamp
          AND (p.effective_to IS NULL OR c.timestamp < p.effective_to)
         ORDER BY c.provider, c.model_id, c.token_category, p.source",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(provider, model_id, category, start, end, source)| {
            finding_raw(
                PricingAuditSeverity::Warning,
                PricingAuditKind::MissingSourceMetadata,
                raw_audit_key(&provider, &model_id, &category),
                Some(start),
                end,
                &format!(
                    "pricing source metadata is missing for source={source}; import a reviewed catalog or reseed pricing"
                ),
            )
        })
        .collect())
}

fn audit_used_pricing_sources_with_metadata(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let cutoff = Utc::now().date_naive() - Duration::days(SOURCE_STALE_AFTER_DAYS);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.provider, c.model_id, c.token_category, p.effective_from,
                p.effective_to, p.source, s.source_url, s.source_retrieved_at,
                s.catalog_version, s.source_kind
         FROM usage_billing_components c
         JOIN pricing_intervals p
           ON p.provider = c.provider
          AND p.model_id = c.model_id
          AND p.token_category = c.token_category
          AND p.service_tier IS c.service_tier
          AND p.speed IS c.speed
          AND p.region IS c.region
          AND p.processing_mode IS c.processing_mode
          AND p.source_detail IS c.source_detail
          AND p.effective_from <= c.timestamp
          AND (p.effective_to IS NULL OR c.timestamp < p.effective_to)
         LEFT JOIN pricing_sources s
           ON s.source = p.source
         ORDER BY c.provider, c.model_id, c.token_category, p.source",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut findings = Vec::new();
    for (
        provider,
        model_id,
        category,
        start,
        end,
        source,
        source_url,
        retrieved_at,
        catalog_version,
        source_kind,
    ) in rows
    {
        let key = raw_audit_key(&provider, &model_id, &category);
        if source_url.as_deref().is_none_or(str::is_empty)
            || retrieved_at.as_deref().is_none_or(str::is_empty)
            || catalog_version.as_deref().is_none_or(str::is_empty)
            || source_kind.as_deref().is_none_or(str::is_empty)
        {
            findings.push(finding_raw(
                PricingAuditSeverity::Warning,
                PricingAuditKind::MissingSourceMetadata,
                key,
                Some(start.clone()),
                end.clone(),
                &format!(
                    "pricing source metadata is missing for source={source}; import a reviewed catalog or reseed pricing"
                ),
            ));
            continue;
        }

        let retrieved_at = retrieved_at.unwrap();
        if let Ok(retrieved_date) = NaiveDate::parse_from_str(&retrieved_at, "%Y-%m-%d")
            && retrieved_date < cutoff
        {
            findings.push(finding_raw(
                PricingAuditSeverity::Warning,
                PricingAuditKind::StaleSource,
                key,
                Some(start.clone()),
                end.clone(),
                &format!(
                    "pricing source {source} was retrieved at {retrieved_at}; import a reviewed catalog if provider pricing changed"
                ),
            ));
        }

        if source_kind.as_deref() == Some("bundled") {
            findings.push(finding_raw(
                PricingAuditSeverity::Info,
                PricingAuditKind::BundledFallbackSource,
                key,
                Some(start),
                end,
                &format!(
                    "usage was priced with bundled fallback source={source}; import reviewed pricing for current local estimates"
                ),
            ));
        }
    }
    Ok(findings)
}

fn audit_catalog(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let dimension_select = pricing_dimension_select_list(conn)?;
    let order_by = if pricing_intervals_have_dimensions(conn)? {
        "provider, model_id, token_category, currency, service_tier, speed, region,
         processing_mode, source_detail, effective_from"
    } else {
        "provider, model_id, token_category, currency, effective_from"
    };
    let sql = format!(
        "SELECT provider, model_id, token_category, {dimension_select},
                currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals
         ORDER BY {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let intervals = stmt
        .query_map([], row_to_raw_interval)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut findings = Vec::new();
    let mut by_key: HashMap<
        (String, String, TokenCategory, PricingDimensions, String),
        Vec<PricingInterval>,
    > = HashMap::new();

    for raw in intervals {
        findings.extend(raw_catalog_findings(&raw));
        let Some(interval) = raw_to_valid_interval(&raw) else {
            continue;
        };

        if interval.currency != "USD" {
            findings.push(finding(
                PricingAuditSeverity::Error,
                PricingAuditKind::UnsupportedCurrency,
                audit_key(
                    interval.provider.as_str(),
                    &interval.model_id,
                    interval.token_category,
                    &interval.dimensions,
                ),
                Some(interval.effective_from),
                interval.effective_to,
                "use USD pricing intervals",
            ));
        }
        by_key
            .entry((
                interval.provider.as_str().to_string(),
                interval.model_id.clone(),
                interval.token_category,
                interval.dimensions.clone(),
                interval.currency.clone(),
            ))
            .or_default()
            .push(interval);
    }

    for ((provider, model_id, category, dimensions, _currency), mut intervals) in by_key {
        intervals.sort_by_key(|interval| interval.effective_from);
        let open_count = intervals
            .iter()
            .filter(|interval| interval.effective_to.is_none())
            .count();
        if open_count == 0 {
            findings.push(finding(
                PricingAuditSeverity::Warning,
                PricingAuditKind::MissingCurrent,
                audit_key(&provider, &model_id, category, &dimensions),
                intervals.last().map(|interval| interval.effective_from),
                None,
                "insert a current open-ended pricing interval",
            ));
        }
        if open_count > 1 {
            findings.push(finding(
                PricingAuditSeverity::Error,
                PricingAuditKind::DuplicateCurrent,
                audit_key(&provider, &model_id, category, &dimensions),
                None,
                None,
                "close superseded open pricing intervals",
            ));
        }

        for pair in intervals.windows(2) {
            let current = &pair[0];
            let next = &pair[1];
            match current.effective_to {
                Some(to) if to < next.effective_from => findings.push(finding(
                    PricingAuditSeverity::Error,
                    PricingAuditKind::Gap,
                    audit_key(&provider, &model_id, category, &dimensions),
                    Some(to),
                    Some(next.effective_from),
                    "insert a pricing interval that covers the gap",
                )),
                Some(to) if to > next.effective_from => findings.push(finding(
                    PricingAuditSeverity::Error,
                    PricingAuditKind::Overlap,
                    audit_key(&provider, &model_id, category, &dimensions),
                    Some(next.effective_from),
                    Some(to),
                    "adjust effective_from/effective_to so intervals do not overlap",
                )),
                None => findings.push(finding(
                    PricingAuditSeverity::Error,
                    PricingAuditKind::Overlap,
                    audit_key(&provider, &model_id, category, &dimensions),
                    Some(next.effective_from),
                    None,
                    "close the earlier open-ended interval before the next interval starts",
                )),
                _ => {}
            }
        }
    }

    Ok(findings)
}

fn audit_usage_coverage(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    if table_exists(conn, "usage_billing_components")?
        && table_row_count(conn, "usage_billing_components")? > 0
    {
        return audit_component_usage_coverage(conn);
    }

    let mut stmt = conn.prepare(
        "SELECT provider, model_id, timestamp, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cached_input_tokens, reasoning_output_tokens
         FROM token_usage",
    )?;
    let mut rows = stmt.query([])?;
    let mut usage: HashMap<UsageCoverageKey, UsageCoverageRange> = HashMap::new();
    let mut findings = Vec::new();

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let Ok(timestamp) = parse_canonical_utc_rfc3339(&timestamp) else {
            findings.push(malformed_usage_timestamp_finding(
                &provider, &model_id, &timestamp,
            ));
            continue;
        };
        let Some(provider_id) = ProviderId::from_canonical(&provider) else {
            findings.push(unsupported_usage_provider_finding(
                &provider, &model_id, timestamp,
            ));
            continue;
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
        for (category, _tokens) in categories {
            usage
                .entry((
                    provider.clone(),
                    model_id.clone(),
                    category,
                    PricingDimensions::default(),
                ))
                .and_modify(|(min, max)| {
                    *min = (*min).min(timestamp);
                    *max = (*max).max(timestamp);
                })
                .or_insert((timestamp, timestamp));
        }
    }

    for ((provider, model_id, category, dimensions), (start, end)) in usage {
        findings.extend(audit_usage_key(
            conn,
            &provider,
            &model_id,
            category,
            &dimensions,
            start,
            end,
        )?);
    }
    Ok(findings)
}

fn audit_component_usage_coverage(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, timestamp, token_category, service_tier, speed, region,
                processing_mode, source_detail
         FROM usage_billing_components",
    )?;
    let mut rows = stmt.query([])?;
    let mut usage: HashMap<UsageCoverageKey, UsageCoverageRange> = HashMap::new();
    let mut findings = Vec::new();

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let token_category: String = row.get(3)?;
        let Ok(timestamp) = parse_canonical_utc_rfc3339(&timestamp) else {
            findings.push(malformed_usage_timestamp_finding(
                &provider, &model_id, &timestamp,
            ));
            continue;
        };
        if ProviderId::from_canonical(&provider).is_none() {
            findings.push(unsupported_usage_provider_finding(
                &provider, &model_id, timestamp,
            ));
            continue;
        };
        let Ok(category) = token_category.parse::<TokenCategory>() else {
            findings.push(finding_raw(
                PricingAuditSeverity::Error,
                PricingAuditKind::MalformedUsageRow,
                raw_audit_key(&provider, &model_id, &token_category),
                Some(format_utc_rfc3339(timestamp)),
                Some(format_utc_rfc3339(timestamp)),
                "store usage_billing_components.token_category as a supported token category",
            ));
            continue;
        };
        let dimensions = PricingDimensions {
            service_tier: row.get(4)?,
            speed: row.get(5)?,
            region: row.get(6)?,
            processing_mode: row.get(7)?,
            source_detail: row.get(8)?,
        };
        usage
            .entry((provider, model_id, category, dimensions))
            .and_modify(|(min, max)| {
                *min = (*min).min(timestamp);
                *max = (*max).max(timestamp);
            })
            .or_insert((timestamp, timestamp));
    }

    for ((provider, model_id, category, dimensions), (start, end)) in usage {
        findings.extend(audit_usage_key(
            conn,
            &provider,
            &model_id,
            category,
            &dimensions,
            start,
            end,
        )?);
    }
    Ok(findings)
}

fn unsupported_usage_provider_finding(
    provider: &str,
    model_id: &str,
    timestamp: DateTime<Utc>,
) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        PricingAuditKind::UnsupportedProviderId,
        raw_audit_key(provider, model_id, ""),
        Some(format_utc_rfc3339(timestamp)),
        Some(format_utc_rfc3339(timestamp)),
        "use a supported canonical provider id such as claude-code or codex",
    )
}

fn malformed_usage_timestamp_finding(
    provider: &str,
    model_id: &str,
    timestamp: &str,
) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        PricingAuditKind::MalformedUsageRow,
        raw_audit_key(provider, model_id, ""),
        Some(timestamp.to_string()),
        Some(timestamp.to_string()),
        "store token_usage.timestamp as canonical UTC RFC3339 such as 2026-04-07T10:00:00+00:00",
    )
}

fn audit_usage_key(
    conn: &Connection,
    provider: &str,
    model_id: &str,
    category: TokenCategory,
    dimensions: &PricingDimensions,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<PricingAuditFinding>> {
    let raw_intervals = if pricing_intervals_have_dimensions(conn)? {
        let mut stmt = conn.prepare(
            "SELECT provider, model_id, token_category, service_tier, speed, region, processing_mode,
                    source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source
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
             ORDER BY effective_from",
        )?;
        stmt.query_map(
            rusqlite::params![
                provider,
                model_id,
                category.as_str(),
                dimensions.service_tier,
                dimensions.speed,
                dimensions.region,
                dimensions.processing_mode,
                dimensions.source_detail,
            ],
            row_to_raw_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else if dimensions.is_default() {
        let mut stmt = conn.prepare(
            "SELECT provider, model_id, token_category, NULL AS service_tier, NULL AS speed,
                    NULL AS region, NULL AS processing_mode, NULL AS source_detail,
                    currency, rate_per_1m_tokens, effective_from, effective_to, source
             FROM pricing_intervals
             WHERE provider = ?1
               AND model_id = ?2
               AND token_category = ?3
               AND currency = 'USD'
             ORDER BY effective_from",
        )?;
        stmt.query_map(
            rusqlite::params![provider, model_id, category.as_str()],
            row_to_raw_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let intervals: Vec<PricingInterval> = raw_intervals
        .iter()
        .filter_map(raw_to_valid_interval)
        .collect();

    if intervals.is_empty() {
        return Ok(vec![finding(
            PricingAuditSeverity::Error,
            PricingAuditKind::MissingCoverage,
            audit_key(provider, model_id, category, dimensions),
            Some(start),
            Some(end),
            "run `tkstat --pricing-seed` or `tkstat --pricing-refresh`",
        )]);
    }

    let mut findings = Vec::new();
    if intervals[0].effective_from > start {
        findings.push(finding(
            PricingAuditSeverity::Error,
            PricingAuditKind::UsageBeforeFirstInterval,
            audit_key(provider, model_id, category, dimensions),
            Some(start),
            Some(intervals[0].effective_from),
            "add pricing effective before the first usage timestamp",
        ));
    }

    let mut cursor = start;
    for interval in intervals {
        if interval.effective_from > cursor && cursor <= end {
            findings.push(finding(
                PricingAuditSeverity::Error,
                PricingAuditKind::MissingCoverage,
                audit_key(provider, model_id, category, dimensions),
                Some(cursor),
                Some(interval.effective_from),
                "insert a pricing interval that covers the usage gap",
            ));
        }
        match interval.effective_to {
            Some(to) if to > cursor => cursor = to,
            None => return Ok(findings),
            _ => {}
        }
        if cursor > end {
            return Ok(findings);
        }
    }

    if cursor <= end {
        findings.push(finding(
            PricingAuditSeverity::Error,
            PricingAuditKind::UsageAfterLastInterval,
            audit_key(provider, model_id, category, dimensions),
            Some(cursor),
            Some(end),
            "add a current pricing interval that covers the latest usage",
        ));
    }
    Ok(findings)
}

fn finding(
    severity: PricingAuditSeverity,
    kind: PricingAuditKind,
    key: AuditKey<'_>,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    remediation: &str,
) -> PricingAuditFinding {
    let remediation = if key.dimensions.is_default() {
        remediation.to_string()
    } else {
        format!(
            "{remediation}; pricing dimensions {}",
            dimension_summary(key.dimensions)
        )
    };
    finding_raw(
        severity,
        kind,
        raw_audit_key(key.provider, key.model_id, key.category.as_str()),
        start.map(format_utc_rfc3339),
        end.map(format_utc_rfc3339),
        &remediation,
    )
}

fn billing_component_integrity_finding(
    provider: &str,
    model_id: &str,
    category: &str,
    remediation: &str,
) -> PricingAuditFinding {
    billing_component_integrity_finding_at(provider, model_id, category, None, remediation)
}

fn billing_component_integrity_finding_at(
    provider: &str,
    model_id: &str,
    category: &str,
    timestamp: Option<String>,
    remediation: &str,
) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        PricingAuditKind::BillingComponentIntegrity,
        raw_audit_key(provider, model_id, category),
        timestamp.clone(),
        timestamp,
        remediation,
    )
}

fn finding_raw(
    severity: PricingAuditSeverity,
    kind: PricingAuditKind,
    key: RawAuditKey<'_>,
    start: Option<String>,
    end: Option<String>,
    remediation: &str,
) -> PricingAuditFinding {
    PricingAuditFinding {
        severity,
        kind,
        provider: key.provider.into(),
        model_id: key.model_id.into(),
        token_category: key.category.into(),
        start,
        end,
        remediation: remediation.into(),
    }
}

pub fn missing_database_finding(db_path: &std::path::Path) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        PricingAuditKind::MissingDatabase,
        raw_audit_key("", "", ""),
        None,
        None,
        &format!(
            "database does not exist at {}; run `tkstat --pricing-seed` or choose an existing --db path",
            db_path.display()
        ),
    )
}

fn schema_finding(kind: PricingAuditKind, remediation: &str) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        kind,
        raw_audit_key("", "", ""),
        None,
        None,
        remediation,
    )
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

fn pricing_intervals_have_dimensions(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "pricing_intervals")? {
        return Ok(false);
    }
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(pricing_intervals)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok([
        "service_tier",
        "speed",
        "region",
        "processing_mode",
        "source_detail",
    ]
    .iter()
    .all(|column| columns.iter().any(|existing| existing == column)))
}

fn pricing_dimension_select_list(conn: &Connection) -> Result<&'static str> {
    if pricing_intervals_have_dimensions(conn)? {
        Ok("service_tier, speed, region, processing_mode, source_detail")
    } else {
        Ok(
            "NULL AS service_tier, NULL AS speed, NULL AS region, NULL AS processing_mode, NULL AS source_detail",
        )
    }
}

fn raw_catalog_findings(raw: &RawPricingInterval) -> Vec<PricingAuditFinding> {
    let mut findings = Vec::new();
    for (field, value) in [
        ("provider", raw.provider.as_str()),
        ("model id", raw.model_id.as_str()),
        ("token category", raw.token_category.as_str()),
        ("currency", raw.currency.as_str()),
        ("effective_from", raw.effective_from.as_str()),
        ("source", raw.source.as_str()),
    ] {
        if value.trim().is_empty() {
            findings.push(malformed_finding(
                raw,
                &format!("populate non-empty {field}"),
            ));
        }
    }
    if raw.rate_per_1m_tokens.is_none() {
        findings.push(malformed_finding(raw, "store a numeric non-negative rate"));
    }
    for (field, value) in [
        ("service_tier", raw.service_tier.as_deref()),
        ("speed", raw.speed.as_deref()),
        ("region", raw.region.as_deref()),
        ("processing_mode", raw.processing_mode.as_deref()),
        ("source_detail", raw.source_detail.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            findings.push(malformed_finding(
                raw,
                &format!("store {field} as NULL or a non-empty pricing dimension"),
            ));
        }
    }
    if ProviderId::from_canonical(raw.provider.as_str()).is_none() {
        findings.push(malformed_finding(
            raw,
            "use a supported canonical provider id such as claude-code or codex",
        ));
    }
    if let Some(rate) = raw.rate_per_1m_tokens
        && rate < 0.0
    {
        findings.push(malformed_finding(raw, "store a non-negative rate"));
    }
    if raw.token_category.parse::<TokenCategory>().is_err() {
        findings.push(malformed_finding(
            raw,
            "use a supported token category such as input, output, cache_read, cache_creation, or cached_input",
        ));
    }

    let from = parse_canonical_utc_rfc3339(&raw.effective_from);
    if from.is_err() {
        findings.push(malformed_finding(
            raw,
            "store effective_from as canonical UTC RFC3339 such as 2026-04-07T10:00:00+00:00",
        ));
    }
    let to = raw
        .effective_to
        .as_ref()
        .map(|dt| parse_canonical_utc_rfc3339(dt))
        .transpose();
    if to.is_err() {
        findings.push(malformed_finding(
            raw,
            "store effective_to as canonical UTC RFC3339 or NULL",
        ));
    }
    if let (Ok(from), Ok(Some(to))) = (from.as_ref(), to.as_ref())
        && to <= from
    {
        findings.push(malformed_finding(
            raw,
            "make effective_to later than effective_from",
        ));
    }
    findings
}

fn malformed_finding(raw: &RawPricingInterval, remediation: &str) -> PricingAuditFinding {
    finding_raw(
        PricingAuditSeverity::Error,
        PricingAuditKind::MalformedCatalogRow,
        raw_audit_key(&raw.provider, &raw.model_id, &raw.token_category),
        Some(raw.effective_from.clone()),
        raw.effective_to.clone(),
        remediation,
    )
}

fn raw_to_valid_interval(raw: &RawPricingInterval) -> Option<PricingInterval> {
    if raw.provider.trim().is_empty()
        || raw.model_id.trim().is_empty()
        || raw.token_category.trim().is_empty()
        || raw.currency.trim().is_empty()
        || raw.effective_from.trim().is_empty()
        || raw.source.trim().is_empty()
    {
        return None;
    }
    let rate = raw.rate_per_1m_tokens?;
    if rate < 0.0 {
        return None;
    }
    let provider = ProviderId::from_canonical(raw.provider.as_str())?;
    let token_category = raw.token_category.parse().ok()?;
    let effective_from = parse_canonical_utc_rfc3339(&raw.effective_from).ok()?;
    let effective_to = raw
        .effective_to
        .as_ref()
        .map(|dt| parse_canonical_utc_rfc3339(dt))
        .transpose()
        .ok()?;
    if let Some(effective_to) = effective_to
        && effective_to <= effective_from
    {
        return None;
    }
    Some(PricingInterval {
        provider,
        model_id: raw.model_id.clone(),
        token_category,
        dimensions: PricingDimensions {
            service_tier: raw.service_tier.clone(),
            speed: raw.speed.clone(),
            region: raw.region.clone(),
            processing_mode: raw.processing_mode.clone(),
            source_detail: raw.source_detail.clone(),
        },
        currency: raw.currency.clone(),
        rate_per_1m_tokens: rate,
        effective_from,
        effective_to,
        source: raw.source.clone(),
    })
}

fn row_to_raw_interval(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawPricingInterval> {
    Ok(RawPricingInterval {
        provider: text_cell(row, 0)?,
        model_id: text_cell(row, 1)?,
        token_category: text_cell(row, 2)?,
        service_tier: optional_text_cell(row, 3)?,
        speed: optional_text_cell(row, 4)?,
        region: optional_text_cell(row, 5)?,
        processing_mode: optional_text_cell(row, 6)?,
        source_detail: optional_text_cell(row, 7)?,
        currency: text_cell(row, 8)?,
        rate_per_1m_tokens: numeric_cell(row, 9)?,
        effective_from: text_cell(row, 10)?,
        effective_to: optional_text_cell(row, 11)?,
        source: text_cell(row, 12)?,
    })
}

fn text_cell(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<String> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(String::new()),
        ValueRef::Integer(value) => Ok(value.to_string()),
        ValueRef::Real(value) => Ok(value.to_string()),
        ValueRef::Text(value) => Ok(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(_) => Ok(String::from("<blob>")),
    }
}

fn optional_text_cell(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<String>> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(None),
        _ => text_cell(row, idx).map(Some),
    }
}

fn numeric_cell(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<f64>> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(value) => Ok(Some(value as f64)),
        ValueRef::Real(value) => Ok(Some(value)),
        ValueRef::Text(value) => Ok(std::str::from_utf8(value)
            .ok()
            .and_then(|text| text.parse().ok())),
        ValueRef::Blob(_) => Ok(None),
    }
}

fn row_to_interval(row: &rusqlite::Row<'_>) -> rusqlite::Result<PricingInterval> {
    let provider: String = row.get(0)?;
    let token_category: String = row.get(2)?;
    let effective_from: String = row.get(10)?;
    let effective_to: Option<String> = row.get(11)?;
    Ok(PricingInterval {
        provider: ProviderId::from_canonical(&provider).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown provider id '{provider}'").into(),
            )
        })?,
        model_id: row.get(1)?,
        token_category: token_category.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, e.into())
        })?,
        dimensions: PricingDimensions {
            service_tier: row.get(3)?,
            speed: row.get(4)?,
            region: row.get(5)?,
            processing_mode: row.get(6)?,
            source_detail: row.get(7)?,
        },
        currency: row.get(8)?,
        rate_per_1m_tokens: row.get(9)?,
        effective_from: parse_canonical_utc_rfc3339(&effective_from).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?,
        effective_to: effective_to
            .map(|dt| {
                parse_canonical_utc_rfc3339(&dt).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        11,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        source: row.get(12)?,
    })
}

fn dimension_suffix(dimensions: &PricingDimensions) -> String {
    if dimensions.is_default() {
        String::new()
    } else {
        format!(", dimensions={}", dimension_summary(dimensions))
    }
}

fn dimension_summary(dimensions: &PricingDimensions) -> String {
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
        "default".into()
    } else {
        parts.join(",")
    }
}

pub fn validate_interval(interval: &PricingInterval) -> Result<()> {
    if interval.currency != "USD" {
        return Err(anyhow!(
            "unsupported pricing currency '{}'",
            interval.currency
        ));
    }
    if interval.rate_per_1m_tokens < 0.0 {
        return Err(anyhow!(
            "negative price for provider={}, model={}, category={}",
            interval.provider,
            interval.model_id,
            interval.token_category
        ));
    }
    for (field, value) in [
        ("service_tier", interval.dimensions.service_tier.as_deref()),
        ("speed", interval.dimensions.speed.as_deref()),
        ("region", interval.dimensions.region.as_deref()),
        (
            "processing_mode",
            interval.dimensions.processing_mode.as_deref(),
        ),
        (
            "source_detail",
            interval.dimensions.source_detail.as_deref(),
        ),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!(
                "invalid pricing dimension {field} for provider={}, model={}, category={}: use NULL or a non-empty value",
                interval.provider,
                interval.model_id,
                interval.token_category
            ));
        }
    }
    if let Some(effective_to) = interval.effective_to
        && effective_to <= interval.effective_from
    {
        return Err(anyhow!(
            "invalid pricing interval for provider={}, model={}, category={}: effective_to must be after effective_from",
            interval.provider,
            interval.model_id,
            interval.token_category
        ));
    }
    Ok(())
}

pub fn seed_intervals() -> Vec<PricingInterval> {
    bundled_catalog_intervals().expect("bundled pricing catalog should be valid")
}

fn bundled_catalog_intervals() -> Result<Vec<PricingInterval>> {
    catalog_intervals_from_str(BUNDLED_PRICING_CATALOG_JSON)
}

fn bundled_catalog_data() -> Result<PricingCatalogData> {
    catalog_data_from_str(BUNDLED_PRICING_CATALOG_JSON, "bundled")
}

fn catalog_intervals_from_str(contents: &str) -> Result<Vec<PricingInterval>> {
    Ok(catalog_data_from_str(contents, "reviewed")?.intervals)
}

fn catalog_data_from_str(contents: &str, source_kind: &str) -> Result<PricingCatalogData> {
    let catalog: PricingCatalog = serde_json::from_str(contents)?;
    validate_catalog(&catalog)?;
    Ok(PricingCatalogData {
        intervals: catalog_to_intervals(&catalog)?,
        sources: catalog_to_source_metadata(&catalog, source_kind)?,
    })
}

fn validate_catalog(catalog: &PricingCatalog) -> Result<()> {
    if catalog.schema_version != 1 {
        bail!(
            "unsupported pricing catalog schema_version {}; expected 1",
            catalog.schema_version
        );
    }
    if catalog.notes.trim().is_empty() {
        bail!("pricing catalog notes must not be empty");
    }
    if catalog.sources.is_empty() {
        bail!("pricing catalog must contain at least one source");
    }
    if catalog.entries.is_empty() {
        bail!("pricing catalog must contain at least one entry");
    }

    let mut source_ids = HashSet::new();
    for source in &catalog.sources {
        if source.id.trim().is_empty() {
            bail!("pricing catalog source id must not be empty");
        }
        if !source_ids.insert(source.id.as_str()) {
            bail!("duplicate pricing catalog source id {}", source.id);
        }
        if !source.url.starts_with("https://") {
            bail!("pricing catalog source {} must use an https URL", source.id);
        }
        NaiveDate::parse_from_str(&source.retrieved_at, "%Y-%m-%d").map_err(|err| {
            anyhow!(
                "pricing catalog source {} has invalid retrieved_at date: {err}",
                source.id
            )
        })?;
        if source.notes.trim().is_empty() {
            bail!(
                "pricing catalog source {} notes must not be empty",
                source.id
            );
        }
    }

    let mut interval_keys = HashSet::new();
    for entry in &catalog.entries {
        validate_catalog_entry(entry, &source_ids)?;
        let provider = ProviderId::from_canonical(&entry.provider)
            .ok_or_else(|| anyhow!("unsupported provider id {}", entry.provider))?;
        let dimensions = entry.dimensions.pricing_dimensions();
        let effective_from = parse_canonical_utc_rfc3339(&entry.effective_from)?;
        let effective_to = entry
            .effective_to
            .as_ref()
            .map(|dt| parse_canonical_utc_rfc3339(dt))
            .transpose()?;
        for model_id in &entry.model_ids {
            for (category, rate) in &entry.rates_per_1m_tokens {
                let token_category = category.parse::<TokenCategory>().map_err(|err| {
                    anyhow!("pricing catalog entry uses invalid token category {category}: {err}")
                })?;
                let interval = PricingInterval {
                    provider,
                    model_id: model_id.clone(),
                    token_category,
                    dimensions: dimensions.clone(),
                    currency: entry.currency.clone(),
                    rate_per_1m_tokens: *rate,
                    effective_from,
                    effective_to,
                    source: entry.source.clone(),
                };
                validate_interval(&interval)?;
                let key = (
                    provider.as_str().to_string(),
                    model_id.clone(),
                    token_category,
                    dimensions.clone(),
                    entry.currency.clone(),
                    format_utc_rfc3339(effective_from),
                );
                if !interval_keys.insert(key) {
                    bail!(
                        "duplicate pricing catalog interval for provider={}, model={}, category={}, effective_from={}",
                        provider,
                        model_id,
                        token_category,
                        format_utc_rfc3339(effective_from)
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_source_metadata(source: &PricingSourceMetadata) -> Result<()> {
    if source.source.trim().is_empty() {
        bail!("pricing source metadata source must not be empty");
    }
    if source.source_url.trim().is_empty() {
        bail!(
            "pricing source metadata {} source_url must not be empty",
            source.source
        );
    }
    if source.catalog_version.trim().is_empty() {
        bail!(
            "pricing source metadata {} catalog_version must not be empty",
            source.source
        );
    }
    if source.notes.trim().is_empty() {
        bail!(
            "pricing source metadata {} notes must not be empty",
            source.source
        );
    }
    if !matches!(
        source.source_kind.as_str(),
        "bundled" | "reviewed" | "manual"
    ) {
        bail!(
            "pricing source metadata {} has unsupported source_kind {}; expected bundled, reviewed, or manual",
            source.source,
            source.source_kind
        );
    }
    NaiveDate::parse_from_str(&source.source_retrieved_at, "%Y-%m-%d").map_err(|err| {
        anyhow!(
            "pricing source metadata {} has invalid source_retrieved_at date: {err}",
            source.source
        )
    })?;
    Ok(())
}

fn validate_catalog_entry(entry: &CatalogEntry, source_ids: &HashSet<&str>) -> Result<()> {
    if entry.model_ids.is_empty() {
        bail!(
            "pricing catalog entry for provider {} has no model_ids",
            entry.provider
        );
    }
    if entry.rates_per_1m_tokens.is_empty() {
        bail!(
            "pricing catalog entry for provider {} has no rates_per_1m_tokens",
            entry.provider
        );
    }
    if entry.source.trim().is_empty() {
        bail!("pricing catalog entry source must not be empty");
    }
    if !source_ids.contains(entry.source_ref.as_str()) {
        bail!(
            "pricing catalog entry source_ref {} does not match a source",
            entry.source_ref
        );
    }
    if entry.notes.trim().is_empty() {
        bail!("pricing catalog entry notes must not be empty");
    }
    for model_id in &entry.model_ids {
        if model_id.trim().is_empty() {
            bail!("pricing catalog entry contains an empty model_id");
        }
    }
    for alias in &entry.model_aliases {
        if alias.trim().is_empty() {
            bail!("pricing catalog entry contains an empty model alias");
        }
    }
    Ok(())
}

fn catalog_to_intervals(catalog: &PricingCatalog) -> Result<Vec<PricingInterval>> {
    let mut intervals = Vec::new();
    for entry in &catalog.entries {
        let provider = ProviderId::from_canonical(&entry.provider)
            .ok_or_else(|| anyhow!("unsupported provider id {}", entry.provider))?;
        let dimensions = entry.dimensions.pricing_dimensions();
        let effective_from = parse_canonical_utc_rfc3339(&entry.effective_from)?;
        let effective_to = entry
            .effective_to
            .as_ref()
            .map(|dt| parse_canonical_utc_rfc3339(dt))
            .transpose()?;
        for model_id in &entry.model_ids {
            for (category, rate) in &entry.rates_per_1m_tokens {
                intervals.push(PricingInterval {
                    provider,
                    model_id: model_id.clone(),
                    token_category: category.parse().map_err(|err| {
                        anyhow!(
                            "pricing catalog entry uses invalid token category {category}: {err}"
                        )
                    })?,
                    dimensions: dimensions.clone(),
                    currency: entry.currency.clone(),
                    rate_per_1m_tokens: *rate,
                    effective_from,
                    effective_to,
                    source: entry.source.clone(),
                });
            }
        }
    }
    Ok(intervals)
}

fn catalog_to_source_metadata(
    catalog: &PricingCatalog,
    source_kind: &str,
) -> Result<Vec<PricingSourceMetadata>> {
    let sources_by_id: HashMap<&str, &CatalogSource> = catalog
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut metadata_by_source = BTreeMap::new();
    for entry in &catalog.entries {
        let source = sources_by_id
            .get(entry.source_ref.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "pricing catalog entry source_ref {} does not match a source",
                    entry.source_ref
                )
            })?;
        metadata_by_source
            .entry(entry.source.clone())
            .or_insert_with(|| PricingSourceMetadata {
                source: entry.source.clone(),
                source_url: source.url.clone(),
                source_retrieved_at: source.retrieved_at.clone(),
                catalog_version: catalog.schema_version.to_string(),
                source_kind: source_kind.into(),
                notes: format!("{}; {}", source.notes, entry.notes),
            });
    }
    let metadata: Vec<_> = metadata_by_source.into_values().collect();
    for source in &metadata {
        validate_source_metadata(source)?;
    }
    Ok(metadata)
}

impl CatalogDimensions {
    fn pricing_dimensions(&self) -> PricingDimensions {
        PricingDimensions {
            service_tier: self.service_tier.clone(),
            speed: self.speed.clone(),
            region: self.region.clone(),
            processing_mode: self.processing_mode.clone(),
            source_detail: self.source_detail.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::usage::ModelFamily;

    struct MockFetcher {
        intervals: Vec<PricingInterval>,
    }

    impl PricingFetcher for MockFetcher {
        fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>> {
            Ok(self.intervals.clone())
        }
    }

    fn interval(
        category: TokenCategory,
        rate: f64,
        from: &str,
        to: Option<&str>,
    ) -> PricingInterval {
        let mut interval = PricingInterval::usd(
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
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

    fn with_processing_mode(
        mut interval: PricingInterval,
        processing_mode: &str,
    ) -> PricingInterval {
        interval.dimensions.processing_mode = Some(processing_mode.into());
        interval
    }

    fn with_source_detail(mut interval: PricingInterval, source_detail: &str) -> PricingInterval {
        interval.dimensions.source_detail = Some(source_detail.into());
        interval
    }

    fn record(model_id: &str) -> TokenRecord {
        TokenRecord {
            provider: crate::domain::provider::ProviderId::ClaudeCode,
            request_id: "r1".into(),
            session_id: "s1".into(),
            uuid: "u1".into(),
            timestamp: "2026-04-07T10:00:00Z".parse().unwrap(),
            model: ModelFamily::Opus,
            model_id: model_id.into(),
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
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
            cost_usd: 0.0,
            project: "test".into(),
            source_file: "/test.jsonl".into(),
            is_subagent: false,
        }
    }

    fn insert_raw_pricing_row(
        conn: &Connection,
        provider: &str,
        model_id: &str,
        token_category: &str,
        rate: f64,
        effective_from: &str,
        source: &str,
    ) {
        conn.execute(
            "INSERT INTO pricing_intervals
             (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES (?1, ?2, ?3, 'USD', ?4, ?5, NULL, ?6)",
            rusqlite::params![
                provider,
                model_id,
                token_category,
                rate,
                effective_from,
                source
            ],
        )
        .unwrap();
    }

    fn insert_raw_pricing_row_with_to(
        conn: &Connection,
        model_id: &str,
        effective_from: &str,
        effective_to: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO pricing_intervals
             (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES ('claude-code', ?1, 'input', 'USD', 1.0, ?2, ?3, 'test')",
            rusqlite::params![model_id, effective_from, effective_to],
        )
        .unwrap();
    }

    fn single_input_catalog(rate: f64, effective_from: &str) -> String {
        format!(
            r#"{{
  "schema_version": 1,
  "notes": "offline pricing snapshot test catalog",
  "sources": [
    {{
      "id": "test-source",
      "url": "https://example.com/pricing",
      "retrieved_at": "2026-05-23",
      "notes": "test source"
    }}
  ],
  "entries": [
    {{
      "provider": "claude-code",
      "model_ids": ["claude-opus-4-6"],
      "model_aliases": ["opus"],
      "currency": "USD",
      "effective_from": "{effective_from}",
      "effective_to": null,
      "source": "seed:test-source",
      "source_ref": "test-source",
      "dimensions": {{}},
      "rates_per_1m_tokens": {{
        "input": {rate}
      }},
      "notes": "test entry"
    }}
  ]
}}"#
        )
    }

    fn source_metadata(
        source: &str,
        retrieved_at: &str,
        source_kind: &str,
    ) -> PricingSourceMetadata {
        PricingSourceMetadata {
            source: source.into(),
            source_url: "https://example.com/pricing".into(),
            source_retrieved_at: retrieved_at.into(),
            catalog_version: "1".into(),
            source_kind: source_kind.into(),
            notes: "test pricing source metadata".into(),
        }
    }

    #[test]
    fn test_insert_and_select_applicable_price() {
        let db = Database::open_in_memory().unwrap();
        let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        validate_interval(&interval).unwrap();
        insert_interval(db.conn(), &interval).unwrap();

        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
    }

    #[test]
    fn test_insert_interval_canonicalizes_utc_timestamp_storage() {
        let db = Database::open_in_memory().unwrap();
        let mut interval = PricingInterval::usd(
            ProviderId::ClaudeCode,
            "claude-offset",
            TokenCategory::Input,
            15.0,
            "2026-04-07T03:00:00-07:00"
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc),
            "test",
        );
        interval.effective_to = Some(
            "2026-04-08T03:30:00-07:00"
                .parse::<DateTime<chrono::FixedOffset>>()
                .unwrap()
                .with_timezone(&Utc),
        );

        insert_interval(db.conn(), &interval).unwrap();

        let (stored_from, stored_to): (String, String) = db
            .conn()
            .query_row(
                "SELECT effective_from, effective_to FROM pricing_intervals WHERE model_id = 'claude-offset'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_from, "2026-04-07T10:00:00+00:00");
        assert_eq!(stored_to, "2026-04-08T10:30:00+00:00");
    }

    #[test]
    fn test_selects_effective_interval_by_timestamp() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(
                TokenCategory::Input,
                10.0,
                "2026-01-01T00:00:00Z",
                Some("2026-04-01T00:00:00Z"),
            ),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None),
        )
        .unwrap();

        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);

        let offset_timestamp = "2026-03-31T17:30:00-07:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc);
        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            offset_timestamp,
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
    }

    #[test]
    fn test_lookup_selects_matching_pricing_dimensions_and_default_only_matches_default() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &with_speed(
                interval(TokenCategory::Input, 20.0, "2026-01-01T00:00:00Z", None),
                "fast",
            ),
        )
        .unwrap();

        let default = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(default.rate_per_1m_tokens, 10.0);

        let standard_dimensions = PricingDimensions {
            service_tier: Some("standard".into()),
            speed: Some("standard".into()),
            ..Default::default()
        };
        let standard = applicable_interval_for_dimensions(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
            &standard_dimensions,
        )
        .unwrap();
        assert_eq!(standard.rate_per_1m_tokens, 10.0);

        let fast_dimensions = PricingDimensions {
            speed: Some("fast".into()),
            ..Default::default()
        };
        let fast = applicable_interval_for_dimensions(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
            &fast_dimensions,
        )
        .unwrap();
        assert_eq!(fast.rate_per_1m_tokens, 20.0);

        let priority_dimensions = PricingDimensions {
            speed: Some("priority".into()),
            ..Default::default()
        };
        let err = applicable_interval_for_dimensions(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
            &priority_dimensions,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing price"));
        assert!(err.contains("speed=priority"));
    }

    #[test]
    fn test_claude_standard_modifiers_use_default_pricing_dimensions() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &PricingInterval::usd(
                ProviderId::ClaudeCode,
                "claude-haiku-4-5-20251001",
                TokenCategory::CacheRead,
                0.1,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
        )
        .unwrap();

        let mut usage = record("claude-haiku-4-5-20251001");
        usage.model = ModelFamily::Haiku;
        usage.input_tokens = 0;
        usage.output_tokens = 0;
        usage.cache_creation_tokens = 0;
        usage.cache_read_tokens = 1_000_000;
        usage.service_tier = Some("standard".into());
        usage.speed = Some("standard".into());

        let cost = calculate_record_cost(db.conn(), &usage).unwrap();
        assert!((cost - 0.1).abs() < 0.000001);
    }

    #[test]
    fn test_claude_non_default_speed_still_requires_specialized_pricing() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &PricingInterval::usd(
                ProviderId::ClaudeCode,
                "claude-haiku-4-5-20251001",
                TokenCategory::CacheRead,
                0.1,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
        )
        .unwrap();

        let mut usage = record("claude-haiku-4-5-20251001");
        usage.model = ModelFamily::Haiku;
        usage.input_tokens = 0;
        usage.output_tokens = 0;
        usage.cache_creation_tokens = 0;
        usage.cache_read_tokens = 1_000_000;
        usage.service_tier = Some("standard".into());
        usage.speed = Some("fast".into());

        let err = calculate_record_cost(db.conn(), &usage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing price"));
        assert!(err.contains("speed=fast"));

        insert_interval(
            db.conn(),
            &with_speed(
                PricingInterval::usd(
                    ProviderId::ClaudeCode,
                    "claude-haiku-4-5-20251001",
                    TokenCategory::CacheRead,
                    0.2,
                    "2026-01-01T00:00:00Z".parse().unwrap(),
                    "test",
                ),
                "fast",
            ),
        )
        .unwrap();

        let cost = calculate_record_cost(db.conn(), &usage).unwrap();
        assert!((cost - 0.2).abs() < 0.000001);
    }

    #[test]
    fn test_unknown_model_returns_explicit_error() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();

        let err = calculate_record_cost(db.conn(), &record("claude-unknown-1"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing price"));
        assert!(err.contains("claude-unknown-1"));
    }

    #[test]
    fn test_uncovered_interval_returns_explicit_error() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 15.0, "2026-05-01T00:00:00Z", None),
        )
        .unwrap();

        let err = calculate_record_cost(db.conn(), &record("claude-opus-4-6"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing price"));
        assert!(err.contains("2026-04-07T10:00:00+00:00"));
    }

    #[test]
    fn test_overlapping_intervals_return_explicit_error() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 15.0, "2026-02-01T00:00:00Z", None),
        )
        .unwrap();

        let err = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("overlapping prices"));
    }

    #[test]
    fn test_calculate_record_cost_uses_all_nonzero_categories() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Output, 75.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();

        let cost = calculate_record_cost(db.conn(), &record("claude-opus-4-6")).unwrap();
        assert!((cost - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_codex_cost_uses_non_overlapping_billable_categories() {
        let db = Database::open_in_memory().unwrap();
        for (category, rate) in [
            (TokenCategory::Input, 10.0),
            (TokenCategory::CachedInput, 1.0),
            (TokenCategory::Output, 100.0),
            (TokenCategory::ReasoningOutput, 999.0),
        ] {
            insert_interval(
                db.conn(),
                &PricingInterval::usd(
                    ProviderId::Codex,
                    "gpt-audit",
                    category,
                    rate,
                    "2026-01-01T00:00:00Z".parse().unwrap(),
                    "test",
                ),
            )
            .unwrap();
        }

        let mut usage = record("gpt-audit");
        usage.provider = crate::domain::provider::ProviderId::Codex;
        usage.model = ModelFamily::Unknown;
        usage.input_tokens = 100;
        usage.cached_input_tokens = 40;
        usage.output_tokens = 20;
        usage.reasoning_output_tokens = 7;

        let cost = calculate_record_cost(db.conn(), &usage).unwrap();
        let expected = (60.0 * 10.0 + 40.0 * 1.0 + 20.0 * 100.0) / 1_000_000.0;
        assert!((cost - expected).abs() < 0.000001);
    }

    #[test]
    fn test_calculate_codex_cost_uses_processing_mode_specific_price() {
        let db = Database::open_in_memory().unwrap();
        for (mode, rate) in [("standard", 10.0), ("batch", 5.0)] {
            insert_interval(
                db.conn(),
                &with_processing_mode(
                    PricingInterval::usd(
                        ProviderId::Codex,
                        "gpt-audit",
                        TokenCategory::Input,
                        rate,
                        "2026-01-01T00:00:00Z".parse().unwrap(),
                        "test",
                    ),
                    mode,
                ),
            )
            .unwrap();
        }

        let mut usage = record("gpt-audit");
        usage.provider = crate::domain::provider::ProviderId::Codex;
        usage.model = ModelFamily::Unknown;
        usage.output_tokens = 0;
        usage.processing_mode = Some("batch".into());

        let cost = calculate_record_cost(db.conn(), &usage).unwrap();
        assert!((cost - 5.0).abs() < 0.000001);

        usage.processing_mode = Some("priority".into());
        let err = calculate_record_cost(db.conn(), &usage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing price"));
        assert!(err.contains("processing_mode=priority"));
    }

    #[test]
    fn test_calculate_claude_cost_requires_cache_creation_source_detail_prices() {
        let db = Database::open_in_memory().unwrap();
        for (category, rate) in [
            (TokenCategory::Input, 10.0),
            (TokenCategory::Output, 20.0),
            (TokenCategory::CacheRead, 1.0),
            (TokenCategory::CacheCreation, 12.0),
        ] {
            insert_interval(
                db.conn(),
                &interval(category, rate, "2026-01-01T00:00:00Z", None),
            )
            .unwrap();
        }

        let mut usage = record("claude-opus-4-6");
        usage.input_tokens = 0;
        usage.output_tokens = 0;
        usage.cache_creation_tokens = 150;
        usage.cache_creation_5m_tokens = 100;
        usage.cache_creation_1h_tokens = 50;

        let err = calculate_record_cost(db.conn(), &usage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("source_detail=ephemeral_5m"));

        insert_interval(
            db.conn(),
            &with_source_detail(
                interval(
                    TokenCategory::CacheCreation,
                    12.0,
                    "2026-01-01T00:00:00Z",
                    None,
                ),
                "ephemeral_5m",
            ),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &with_source_detail(
                interval(
                    TokenCategory::CacheCreation,
                    60.0,
                    "2026-01-01T00:00:00Z",
                    None,
                ),
                "ephemeral_1h",
            ),
        )
        .unwrap();

        let cost = calculate_record_cost(db.conn(), &usage).unwrap();
        let expected = (100.0 * 12.0 + 50.0 * 60.0) / 1_000_000.0;
        assert!((cost - expected).abs() < 0.000001);
    }

    #[test]
    fn test_calculate_claude_cost_requires_speed_specific_price() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();

        let mut usage = record("claude-opus-4-6");
        usage.output_tokens = 0;
        usage.speed = Some("fast".into());

        let err = calculate_record_cost(db.conn(), &usage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("speed=fast"));
    }

    #[test]
    fn test_seed_pricing_inserts_bundled_fallback_once() {
        let db = Database::open_in_memory().unwrap();
        let inserted = seed_pricing(db.conn()).unwrap();
        assert!(inserted > 0);
        assert_eq!(seed_pricing(db.conn()).unwrap(), 0);
        let legacy_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude-code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_count, 0);
        assert!(canonical_count > 0);

        let selected = applicable_interval_for_dimensions(
            db.conn(),
            ProviderId::Codex,
            "gpt-5.4",
            TokenCategory::CachedInput,
            "2026-05-24T00:00:00Z".parse().unwrap(),
            &PricingDimensions {
                processing_mode: Some("standard".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 0.25);
        assert!(selected.source.starts_with("seed:"));
        let source_kind: String = db
            .conn()
            .query_row(
                "SELECT source_kind FROM pricing_sources WHERE source = ?1",
                [selected.source],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_kind, "bundled");
    }

    #[test]
    fn test_bundled_pricing_catalog_schema_is_valid() {
        let catalog: PricingCatalog = serde_json::from_str(BUNDLED_PRICING_CATALOG_JSON).unwrap();
        validate_catalog(&catalog).unwrap();

        assert_eq!(catalog.schema_version, 1);
        assert!(catalog.notes.contains("offline pricing snapshot"));
        assert!(catalog.sources.iter().all(|source| {
            source.url.starts_with("https://")
                && !source.retrieved_at.is_empty()
                && !source.notes.is_empty()
        }));
        assert!(catalog.entries.iter().any(|entry| {
            entry.provider == "codex"
                && entry.dimensions.processing_mode.as_deref() == Some("standard")
                && entry.rates_per_1m_tokens.contains_key("cached_input")
        }));
    }

    #[test]
    fn test_bundled_pricing_catalog_intervals_insert_and_audit_cleanly() {
        let intervals = bundled_catalog_intervals().unwrap();
        assert_eq!(intervals.len(), 56);

        let db = Database::open_in_memory().unwrap();
        assert_eq!(seed_pricing_intervals(db.conn(), &intervals).unwrap(), 56);
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn test_pricing_catalog_docs_explain_snapshot_sources() {
        let docs = include_str!("../../docs/pricing-catalog.md");
        assert!(docs.contains("pricing/catalog.json"));
        assert!(docs.contains("snapshot of official provider"));
        assert!(docs.contains("effective-dated SQLite"));
    }

    #[test]
    fn test_seed_pricing_intervals_prevalidates_and_leaves_catalog_unchanged_on_error() {
        let db = Database::open_in_memory().unwrap();
        let valid = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        let invalid = interval(TokenCategory::Output, -1.0, "2026-01-01T00:00:00Z", None);

        let err = seed_pricing_intervals(db.conn(), &[valid, invalid])
            .unwrap_err()
            .to_string();

        assert!(err.contains("negative price"));
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_import_pricing_catalog_closes_changed_open_interval() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(
            import_pricing_catalog_json(
                db.conn(),
                &single_input_catalog(10.0, "2026-01-01T00:00:00+00:00"),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            import_pricing_catalog_json(
                db.conn(),
                &single_input_catalog(12.0, "2026-03-01T00:00:00+00:00"),
            )
            .unwrap(),
            2
        );

        let old = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-02-01T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let new = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-01T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(old.rate_per_1m_tokens, 10.0);
        assert_eq!(
            old.effective_to,
            Some("2026-03-01T00:00:00Z".parse().unwrap())
        );
        assert_eq!(new.rate_per_1m_tokens, 12.0);
        assert_eq!(new.effective_to, None);
        let source_kind: String = db
            .conn()
            .query_row(
                "SELECT source_kind FROM pricing_sources WHERE source = 'seed:test-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_kind, "reviewed");
    }

    #[test]
    fn test_seed_pricing_covers_observed_claude_opus_20251101_categories() {
        let db = Database::open_in_memory().unwrap();
        seed_pricing(db.conn()).unwrap();

        for category in [
            TokenCategory::Input,
            TokenCategory::Output,
            TokenCategory::CacheRead,
            TokenCategory::CacheCreation,
        ] {
            let selected = applicable_interval(
                db.conn(),
                ProviderId::ClaudeCode,
                "claude-opus-4-5-20251101",
                category,
                "2026-01-31T21:20:19.858Z".parse().unwrap(),
            )
            .unwrap();
            assert_eq!(selected.currency, "USD");
            assert!(selected.rate_per_1m_tokens > 0.0);
        }
    }

    #[test]
    fn test_pricing_audit_clean_seed_has_no_findings() {
        let db = Database::open_in_memory().unwrap();
        seed_pricing(db.conn()).unwrap();
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn test_pricing_audit_detects_gap_between_intervals() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(
                TokenCategory::Input,
                10.0,
                "2026-01-01T00:00:00Z",
                Some("2026-02-01T00:00:00Z"),
            ),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 20.0, "2026-03-01T00:00:00Z", None),
        )
        .unwrap();
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == PricingAuditKind::Gap)
        );
    }

    #[test]
    fn test_pricing_audit_detects_overlapping_intervals() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(
                TokenCategory::Input,
                10.0,
                "2026-01-01T00:00:00Z",
                Some("2026-03-01T00:00:00Z"),
            ),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 20.0, "2026-02-01T00:00:00Z", None),
        )
        .unwrap();
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == PricingAuditKind::Overlap)
        );
    }

    #[test]
    fn test_pricing_audit_detects_duplicate_open_intervals() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 20.0, "2026-02-01T00:00:00Z", None),
        )
        .unwrap();
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == PricingAuditKind::DuplicateCurrent)
        );
    }

    #[test]
    fn test_pricing_audit_treats_modifier_timelines_independently() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &with_speed(
                interval(
                    TokenCategory::Input,
                    20.0,
                    "2026-01-01T00:00:00Z",
                    Some("2026-02-01T00:00:00Z"),
                ),
                "fast",
            ),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &with_speed(
                interval(TokenCategory::Input, 30.0, "2026-03-01T00:00:00Z", None),
                "fast",
            ),
        )
        .unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::Gap && finding.remediation.contains("speed=fast")
        }));
        assert!(!findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::DuplicateCurrent
                || finding.kind == PricingAuditKind::Overlap
        }));
    }

    #[test]
    fn test_pricing_audit_detects_unsupported_currency() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO pricing_intervals
                 (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
                 VALUES ('claude-code', 'claude-opus-4-6', 'input', 'EUR', 1.0, '2026-01-01T00:00:00+00:00', NULL, 'test')",
                [],
            )
            .unwrap();
        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == PricingAuditKind::UnsupportedCurrency)
        );
    }

    #[test]
    fn test_pricing_audit_reports_billing_component_integrity_findings() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Output, 20.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        db.insert_records(&[record("claude-opus-4-6")]).unwrap();
        db.conn()
            .execute(
                "DELETE FROM usage_billing_components
                 WHERE provider = 'claude-code'
                   AND request_id = 'r1'
                   AND token_category = 'output'",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO usage_billing_components
                 (usage_id, provider, request_id, model_id, timestamp, token_category, tokens,
                  component_ordinal)
                 VALUES (9999, 'claude-code', 'missing-request', 'claude-opus-4-6',
                         '2026-04-07T10:00:00+00:00', 'input', 10, 999)",
                [],
            )
            .unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::BillingComponentIntegrity
                && finding
                    .remediation
                    .contains("expected 1000000 billable tokens")
                && finding.remediation.contains("found 0")
        }));
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::BillingComponentIntegrity
                && finding.remediation.contains("missing-request")
                && finding.remediation.contains("no matching token_usage row")
        }));
    }

    #[test]
    fn test_pricing_audit_reports_malformed_catalog_rows() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute("PRAGMA ignore_check_constraints = ON", [])
            .unwrap();
        insert_raw_pricing_row(
            db.conn(),
            ProviderId::ClaudeCode.as_str(),
            "claude-opus-4-6",
            "mystery",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            ProviderId::ClaudeCode.as_str(),
            "claude-opus-4-7",
            "input",
            1.0,
            "not-a-date",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            ProviderId::ClaudeCode.as_str(),
            "claude-opus-4-8",
            "input",
            -1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            "",
            "claude-opus-4-9",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            "claude",
            "claude-opus-4-alias",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            ProviderId::ClaudeCode.as_str(),
            "",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            ProviderId::ClaudeCode.as_str(),
            "claude-opus-4-10",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "",
        );

        let findings = audit_pricing(db.conn()).unwrap();
        for (provider, model_id, category, remediation) in [
            (
                "claude-code",
                "claude-opus-4-6",
                "mystery",
                "supported token category",
            ),
            ("claude-code", "claude-opus-4-7", "input", "RFC3339"),
            (
                "claude-code",
                "claude-opus-4-8",
                "input",
                "non-negative rate",
            ),
            ("", "claude-opus-4-9", "input", "non-empty provider"),
            (
                "claude",
                "claude-opus-4-alias",
                "input",
                "canonical provider id",
            ),
            ("claude-code", "", "input", "non-empty model id"),
            (
                "claude-code",
                "claude-opus-4-10",
                "input",
                "non-empty source",
            ),
        ] {
            assert!(
                findings.iter().any(|finding| {
                    finding.kind == PricingAuditKind::MalformedCatalogRow
                        && finding.provider == provider
                        && finding.model_id == model_id
                        && finding.token_category == category
                        && finding.remediation.contains(remediation)
                }),
                "missing malformed finding for {provider}/{model_id}/{category}: {remediation}; findings: {findings:#?}"
            );
        }
    }

    #[test]
    fn test_pricing_audit_reports_noncanonical_catalog_timestamps() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute("PRAGMA ignore_check_constraints = ON", [])
            .unwrap();
        for (model_id, from, to) in [
            ("zulu-utc", "2026-04-07T10:00:00Z", None),
            ("naive", "2026-04-07 10:00:00", None),
            ("local-offset", "2026-04-07T03:00:00-07:00", None),
            (
                "noncanonical-to",
                "2026-04-07T10:00:00+00:00",
                Some("2026-04-08T03:30:00-07:00"),
            ),
        ] {
            insert_raw_pricing_row_with_to(db.conn(), model_id, from, to);
        }

        let findings = audit_pricing(db.conn()).unwrap();
        for model_id in ["zulu-utc", "naive", "local-offset", "noncanonical-to"] {
            assert!(
                findings.iter().any(|finding| {
                    finding.kind == PricingAuditKind::MalformedCatalogRow
                        && finding.model_id == model_id
                        && finding.remediation.contains("canonical UTC RFC3339")
                }),
                "missing noncanonical catalog timestamp finding for {model_id}: {findings:#?}"
            );
        }
    }

    #[test]
    fn test_pricing_audit_detects_missing_cached_token_pricing_for_usage() {
        let db = Database::open_in_memory().unwrap();
        let mut usage = record("gpt-audit");
        usage.provider = crate::domain::provider::ProviderId::Codex;
        usage.model = ModelFamily::Unknown;
        usage.input_tokens = 0;
        usage.output_tokens = 0;
        usage.cached_input_tokens = 10;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::MissingCoverage
                && finding.provider == "codex"
                && finding.model_id == "gpt-audit"
                && finding.token_category == "cached_input"
        }));
    }

    #[test]
    fn test_pricing_audit_reports_stale_source_metadata_for_used_prices() {
        let db = Database::open_in_memory().unwrap();
        let mut price = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
        price.source = "reviewed:old-source".into();
        insert_interval(db.conn(), &price).unwrap();
        upsert_source_metadata(
            db.conn(),
            &source_metadata("reviewed:old-source", "2025-01-01", "reviewed"),
        )
        .unwrap();
        let mut usage = record("claude-opus-4-6");
        usage.output_tokens = 0;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::StaleSource
                && finding.provider == "claude-code"
                && finding.model_id == "claude-opus-4-6"
                && finding.token_category == "input"
                && finding.remediation.contains("reviewed:old-source")
        }));
    }

    #[test]
    fn test_pricing_audit_reports_missing_source_metadata_for_used_prices() {
        let db = Database::open_in_memory().unwrap();
        let mut price = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
        price.source = "manual:without-metadata".into();
        insert_interval(db.conn(), &price).unwrap();
        let mut usage = record("claude-opus-4-6");
        usage.output_tokens = 0;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::MissingSourceMetadata
                && finding.provider == "claude-code"
                && finding.model_id == "claude-opus-4-6"
                && finding.token_category == "input"
                && finding.remediation.contains("manual:without-metadata")
        }));
    }

    #[test]
    fn test_pricing_audit_reports_unknown_observed_model_ids() {
        let db = Database::open_in_memory().unwrap();
        let mut usage = record("claude-unknown-4-9");
        usage.output_tokens = 0;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::UnknownObservedModel
                && finding.provider == "claude-code"
                && finding.model_id == "claude-unknown-4-9"
                && finding.token_category.is_empty()
        }));
    }

    #[test]
    fn test_pricing_audit_reports_observed_modifiers_without_specialized_prices() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        let mut usage = record("claude-opus-4-6");
        usage.output_tokens = 0;
        usage.speed = Some("fast".into());
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::UnsupportedModifier
                && finding.provider == "claude-code"
                && finding.model_id == "claude-opus-4-6"
                && finding.token_category == "input"
                && finding.remediation.contains("speed=fast")
        }));
    }

    #[test]
    fn test_pricing_audit_reports_bundled_fallback_usage() {
        let db = Database::open_in_memory().unwrap();
        seed_pricing(db.conn()).unwrap();
        let mut usage = record("claude-opus-4-6");
        usage.output_tokens = 0;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::BundledFallbackSource
                && finding.severity == PricingAuditSeverity::Info
                && finding.provider == "claude-code"
                && finding.model_id == "claude-opus-4-6"
                && finding.token_category == "input"
        }));
    }

    #[test]
    fn test_pricing_audit_reports_unsupported_usage_provider_without_aborting() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pricing_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                token_category TEXT NOT NULL,
                currency TEXT NOT NULL DEFAULT 'USD',
                rate_per_1m_tokens REAL NOT NULL,
                effective_from TEXT NOT NULL,
                effective_to TEXT,
                source TEXT NOT NULL
             );
             CREATE TABLE token_usage (
                provider TEXT NOT NULL,
                request_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                uuid TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                model_family TEXT NOT NULL,
                model_id TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL,
                cached_input_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                project TEXT NOT NULL,
                source_file TEXT NOT NULL,
                is_subagent INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO token_usage
                (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cached_input_tokens, reasoning_output_tokens, total_tokens, cost_usd, project, source_file, is_subagent)
             VALUES
                ('unsupported-provider', 'r1', 's1', 'u1', '2026-04-07T10:00:00+00:00', 'unknown', 'legacy-model',
                 10, 0, 0, 0, 0, 0, 10, 0.0, 'legacy', '/legacy.jsonl', 0);",
        )
        .unwrap();

        let findings = audit_pricing(&conn).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::UnsupportedProviderId
                && finding.provider == "unsupported-provider"
                && finding.model_id == "legacy-model"
                && finding.token_category.is_empty()
                && finding
                    .remediation
                    .contains("supported canonical provider id")
        }));
    }

    #[test]
    fn test_pricing_audit_reports_malformed_usage_timestamps_without_aborting() {
        let db = Database::open_in_memory().unwrap();
        for (request_id, timestamp, model_id) in [
            ("bad", "not-a-date", "bad-model"),
            ("naive", "2026-04-07 10:00:00", "naive-model"),
            ("offset", "2026-04-07T03:00:00-07:00", "offset-model"),
        ] {
            db.conn()
                .execute(
                    "INSERT INTO token_usage
                     (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                      input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                      cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent)
                     VALUES ('claude-code', ?1, 's1', 'u1', ?2, 'unknown', ?3,
                      10, 0, 0, 0, 0, 0, 0.0, 'manual', '/manual.jsonl', 0)",
                    rusqlite::params![request_id, timestamp, model_id],
                )
                .unwrap();
        }

        let findings = audit_pricing(db.conn()).unwrap();
        for model_id in ["bad-model", "naive-model", "offset-model"] {
            assert!(
                findings.iter().any(|finding| {
                    finding.kind == PricingAuditKind::MalformedUsageRow
                        && finding.provider == "claude-code"
                        && finding.model_id == model_id
                        && finding.remediation.contains("canonical UTC RFC3339")
                }),
                "missing malformed usage timestamp finding for {model_id}: {findings:#?}"
            );
        }
    }

    #[test]
    fn test_pricing_audit_does_not_require_separate_codex_reasoning_price() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &PricingInterval::usd(
                ProviderId::Codex,
                "gpt-audit",
                TokenCategory::Output,
                100.0,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
        )
        .unwrap();
        let mut usage = record("gpt-audit");
        usage.provider = crate::domain::provider::ProviderId::Codex;
        usage.model = ModelFamily::Unknown;
        usage.input_tokens = 0;
        usage.output_tokens = 20;
        usage.reasoning_output_tokens = 7;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(!findings.iter().any(|finding| {
            finding.kind == PricingAuditKind::MissingCoverage
                && finding.token_category == "reasoning_output"
        }));
    }

    #[test]
    fn test_pricing_audit_detects_usage_before_first_price_interval() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        let mut usage = record("claude-opus-4-6");
        usage.timestamp = "2025-12-31T23:00:00Z".parse().unwrap();
        usage.output_tokens = 0;
        db.insert_records(&[usage]).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == PricingAuditKind::UsageBeforeFirstInterval)
        );
    }

    #[test]
    fn test_refresh_pricing_noops_when_prices_unchanged() {
        let db = Database::open_in_memory().unwrap();
        let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        insert_interval(db.conn(), &interval).unwrap();

        let fetcher = MockFetcher {
            intervals: vec![interval],
        };
        assert_eq!(refresh_pricing(db.conn(), &fetcher).unwrap(), 0);
    }

    #[test]
    fn test_refresh_pricing_closes_previous_open_interval_when_price_changes() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();

        let changed = interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None);
        let fetcher = MockFetcher {
            intervals: vec![changed],
        };
        assert_eq!(refresh_pricing(db.conn(), &fetcher).unwrap(), 2);

        let old_to: String = db
            .conn()
            .query_row(
                "SELECT effective_to FROM pricing_intervals WHERE rate_per_1m_tokens = 10.0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_to, "2026-04-01T00:00:00+00:00");

        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);

        let (count, open_count): (i64, i64) = db
            .conn()
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END)
                 FROM pricing_intervals
                 WHERE provider = 'claude-code'
                   AND model_id = 'claude-opus-4-6'
                   AND token_category = 'input'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(open_count, 1);
    }

    #[test]
    fn test_refresh_pricing_replaces_same_effective_date_interval_in_place() {
        let db = Database::open_in_memory().unwrap();
        let mut original = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
        original.source = "seed:old-source".into();
        insert_interval(db.conn(), &original).unwrap();

        let mut corrected = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        corrected.source = "reviewed:corrected-source".into();
        let fetcher = MockFetcher {
            intervals: vec![corrected],
        };
        assert_eq!(refresh_pricing(db.conn(), &fetcher).unwrap(), 1);

        let (count, open_count, rate, source): (i64, i64, f64, String) = db
            .conn()
            .query_row(
                "SELECT COUNT(*),
                        SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END),
                        MAX(rate_per_1m_tokens),
                        MAX(source)
                 FROM pricing_intervals
                 WHERE provider = 'claude-code'
                   AND model_id = 'claude-opus-4-6'
                   AND token_category = 'input'
                   AND effective_from = '2026-01-01T00:00:00+00:00'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(open_count, 1);
        assert_eq!(rate, 15.0);
        assert_eq!(source, "reviewed:corrected-source");

        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
    }

    #[test]
    fn test_pricing_audit_accepts_same_effective_date_replacement() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();

        let corrected = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        let fetcher = MockFetcher {
            intervals: vec![corrected],
        };
        refresh_pricing(db.conn(), &fetcher).unwrap();

        let findings = audit_pricing(db.conn()).unwrap();
        assert!(
            findings.iter().all(|finding| {
                !matches!(
                    finding.kind,
                    PricingAuditKind::MissingCurrent
                        | PricingAuditKind::Gap
                        | PricingAuditKind::Overlap
                )
            }),
            "unexpected lifecycle finding after same-date replacement: {findings:?}"
        );
    }

    #[test]
    fn test_refresh_pricing_updates_only_matching_pricing_dimensions() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
        insert_interval(
            db.conn(),
            &with_speed(
                interval(TokenCategory::Input, 20.0, "2026-01-01T00:00:00Z", None),
                "fast",
            ),
        )
        .unwrap();

        let changed = with_speed(
            interval(TokenCategory::Input, 30.0, "2026-04-01T00:00:00Z", None),
            "fast",
        );
        let fetcher = MockFetcher {
            intervals: vec![changed],
        };
        assert_eq!(refresh_pricing(db.conn(), &fetcher).unwrap(), 2);

        let default_open_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals
                 WHERE speed IS NULL AND effective_to IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(default_open_count, 1);

        let old_fast_to: String = db
            .conn()
            .query_row(
                "SELECT effective_to FROM pricing_intervals
                 WHERE speed = 'fast' AND rate_per_1m_tokens = 20.0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_fast_to, "2026-04-01T00:00:00+00:00");
    }

    #[test]
    fn test_refresh_pricing_rolls_back_when_later_interval_is_invalid() {
        let db = Database::open_in_memory().unwrap();
        let valid = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        let mut invalid = interval(TokenCategory::Output, 75.0, "2026-01-01T00:00:00Z", None);
        invalid.currency = "EUR".into();
        let fetcher = MockFetcher {
            intervals: vec![valid, invalid],
        };

        let err = refresh_pricing(db.conn(), &fetcher)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported pricing currency"));
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_refresh_pricing_rejects_stale_effective_interval_and_preserves_existing() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &interval(TokenCategory::Input, 10.0, "2026-05-01T00:00:00Z", None),
        )
        .unwrap();
        let stale = interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None);
        let fetcher = MockFetcher {
            intervals: vec![stale],
        };

        let err = refresh_pricing(db.conn(), &fetcher)
            .unwrap_err()
            .to_string();
        assert!(err.contains("stale pricing interval"));
        assert!(err.contains("2026-04-01T00:00:00+00:00"));
        assert!(err.contains("2026-05-01T00:00:00+00:00"));

        let (count, open_count, rate): (i64, i64, f64) = db
            .conn()
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END), MAX(rate_per_1m_tokens)
                 FROM pricing_intervals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(open_count, 1);
        assert_eq!(rate, 10.0);
    }

    #[test]
    fn test_offline_fallback_uses_cached_prices_when_refresh_fails() {
        struct FailingFetcher;
        impl PricingFetcher for FailingFetcher {
            fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>> {
                Err(anyhow!("offline"))
            }
        }

        let db = Database::open_in_memory().unwrap();
        let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        insert_interval(db.conn(), &interval).unwrap();

        assert!(refresh_pricing(db.conn(), &FailingFetcher).is_err());
        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
    }
}
