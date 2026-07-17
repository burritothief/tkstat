use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{Connection, types::ValueRef};
use serde::Deserialize;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::db::{table_exists, table_row_count};
use crate::domain::pricing::{
    PricingDimensions, PricingInterval, TokenCategory, billable_usage_components,
};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::{format_utc_rfc3339, parse_canonical_utc_rfc3339};
use crate::domain::usage::TokenRecord;

const BUNDLED_PRICING_CATALOG_JSON: &str = include_str!("../../pricing/catalog.json");
const SOURCE_STALE_AFTER_DAYS: i64 = 90;

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
pub struct PricingSnapshot {
    pub(crate) intervals: Vec<PricingInterval>,
    pub(crate) sources: Vec<PricingSourceMetadata>,
}

impl PricingSnapshot {
    pub fn new(intervals: Vec<PricingInterval>, sources: Vec<PricingSourceMetadata>) -> Self {
        Self { intervals, sources }
    }

    pub fn intervals(&self) -> &[PricingInterval] {
        &self.intervals
    }

    pub fn sources(&self) -> &[PricingSourceMetadata] {
        &self.sources
    }
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
    dimensions: PricingDimensions,
    rates_per_1m_tokens: BTreeMap<String, f64>,
    notes: String,
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

pub fn bundled_pricing_snapshot() -> Result<PricingSnapshot> {
    bundled_catalog_data()
}

pub fn insert_interval(conn: &Connection, interval: &PricingInterval) -> Result<()> {
    insert_interval_raw(conn, interval)?;
    crate::db::cost::reprice_provider_usage(conn, interval.provider)?;
    Ok(())
}

fn insert_interval_raw(conn: &Connection, interval: &PricingInterval) -> Result<()> {
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
    let snapshot = bundled_pricing_snapshot()?;
    apply_pricing_snapshot(conn, &snapshot, ApplyMode::Seed)
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

#[derive(Debug, Clone, Copy)]
enum ApplyMode {
    Seed,
    Refresh,
}

fn apply_pricing_snapshot(
    conn: &Connection,
    snapshot: &PricingSnapshot,
    mode: ApplyMode,
) -> Result<usize> {
    for source in &snapshot.sources {
        validate_source_metadata(source)?;
    }
    for interval in &snapshot.intervals {
        validate_interval(interval)?;
    }

    let tx = conn.unchecked_transaction()?;
    for source in &snapshot.sources {
        upsert_source_metadata(&tx, source)?;
    }
    let mut changed = 0;
    for interval in &snapshot.intervals {
        changed += match mode {
            ApplyMode::Seed => usize::from(insert_interval_if_missing(&tx, interval)?),
            ApplyMode::Refresh => upsert_current_interval(&tx, interval)?,
        };
    }
    crate::db::cost::reprice_dirty_usage(&tx)?;
    tx.commit()?;
    Ok(changed)
}

pub fn refresh_pricing(conn: &Connection, snapshot: &PricingSnapshot) -> Result<usize> {
    apply_pricing_snapshot(conn, snapshot, ApplyMode::Refresh)
}

pub fn import_pricing_catalog_file(conn: &Connection, path: &Path) -> Result<usize> {
    let contents = std::fs::read_to_string(path)?;
    import_pricing_catalog_json(conn, &contents)
}

pub fn import_pricing_catalog_json(conn: &Connection, contents: &str) -> Result<usize> {
    let snapshot = catalog_data_from_str(contents, "reviewed")?;
    refresh_pricing(conn, &snapshot)
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
                        interval.dimensions.display_suffix()
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
            dimensions.display_suffix()
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
            dimensions.display_suffix(),
            format_utc_rfc3339(timestamp),
        ),
        1 => Ok(rows.into_iter().next().unwrap()),
        _ => bail!(
            "overlapping prices for provider={provider}, model={model_id}, category={token_category}{}, timestamp={}",
            dimensions.display_suffix(),
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
    audit_pricing_at(conn, Utc::now().date_naive())
}

fn audit_pricing_at(
    conn: &Connection,
    reference_date: NaiveDate,
) -> Result<Vec<PricingAuditFinding>> {
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
        findings.extend(audit_usage_source_quality(conn, reference_date)?);
    } else {
        findings.push(schema_finding(
            PricingAuditKind::MissingSchema,
            "token_usage table is missing; run `tkstat --force-update` to ingest usage before checking coverage",
        ));
    }
    Ok(findings)
}

fn audit_billing_component_integrity(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    Ok(
        crate::db::pricing_validation::scan_billing_integrity(conn, "", &[], None)?
            .into_iter()
            .map(|issue| {
                billing_component_integrity_finding_at(
                    &issue.provider,
                    &issue.model_id,
                    &issue.category,
                    issue.timestamp,
                    &issue.detail,
                )
            })
            .collect(),
    )
}

fn audit_usage_source_quality(
    conn: &Connection,
    reference_date: NaiveDate,
) -> Result<Vec<PricingAuditFinding>> {
    if !table_exists(conn, "usage_billing_components")?
        || table_row_count(conn, "usage_billing_components")? == 0
    {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    findings.extend(audit_unknown_observed_models(conn)?);
    findings.extend(audit_unsupported_modifiers(conn)?);
    findings.extend(audit_used_pricing_sources(conn, reference_date)?);
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

fn audit_used_pricing_sources(
    conn: &Connection,
    reference_date: NaiveDate,
) -> Result<Vec<PricingAuditFinding>> {
    if table_exists(conn, "pricing_sources")? {
        audit_used_pricing_sources_with_metadata(conn, reference_date)
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

fn audit_used_pricing_sources_with_metadata(
    conn: &Connection,
    reference_date: NaiveDate,
) -> Result<Vec<PricingAuditFinding>> {
    let cutoff = stale_source_cutoff(reference_date);
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

pub(crate) fn stale_source_cutoff(reference_date: NaiveDate) -> NaiveDate {
    reference_date - Duration::days(SOURCE_STALE_AFTER_DAYS)
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
    use crate::db::pricing_validation::UsageRowIssue;

    let observations = crate::db::pricing_validation::collect_usage_observations(conn, "", &[])?;
    let mut findings = Vec::new();
    for issue in observations.issues {
        findings.push(match issue {
            UsageRowIssue::MalformedTimestamp {
                provider,
                model_id,
                timestamp,
                ..
            } => malformed_usage_timestamp_finding(&provider, &model_id, &timestamp),
            UsageRowIssue::UnsupportedProvider {
                provider,
                model_id,
                timestamp,
            } => unsupported_usage_provider_finding(&provider, &model_id, timestamp),
            UsageRowIssue::UnsupportedCategory {
                provider,
                model_id,
                category,
                timestamp,
            } => finding_raw(
                PricingAuditSeverity::Error,
                PricingAuditKind::MalformedUsageRow,
                raw_audit_key(&provider, &model_id, &category),
                Some(format_utc_rfc3339(timestamp)),
                Some(format_utc_rfc3339(timestamp)),
                "store usage_billing_components.token_category as a supported token category",
            ),
        });
    }
    for (key, timestamps) in observations.usage {
        let Some(start) = timestamps.iter().min().copied() else {
            continue;
        };
        let end = timestamps.iter().max().copied().unwrap_or(start);
        findings.extend(audit_usage_key(
            conn,
            key.provider.as_str(),
            &key.model_id,
            key.token_category,
            &key.dimensions,
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

fn bundled_catalog_data() -> Result<PricingSnapshot> {
    catalog_data_from_str(BUNDLED_PRICING_CATALOG_JSON, "bundled")
}

fn catalog_intervals_from_str(contents: &str) -> Result<Vec<PricingInterval>> {
    Ok(catalog_data_from_str(contents, "reviewed")?.intervals)
}

fn catalog_data_from_str(contents: &str, source_kind: &str) -> Result<PricingSnapshot> {
    let catalog: PricingCatalog = serde_json::from_str(contents)?;
    validate_catalog(&catalog)?;
    Ok(PricingSnapshot {
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
        let dimensions = entry.dimensions.clone();
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
        let dimensions = entry.dimensions.clone();
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

#[cfg(test)]
mod tests;
