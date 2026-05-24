use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, types::ValueRef};
use serde::Serialize;
use std::collections::HashMap;

use crate::domain::pricing::{
    PricingInterval, TokenCategory, billable_token_categories_for_counts, nonzero_token_categories,
};
use crate::domain::usage::TokenRecord;

pub trait PricingFetcher {
    fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>>;
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
    Gap,
    Overlap,
    DuplicateCurrent,
    MissingCurrent,
    UnsupportedCurrency,
    MissingCoverage,
    UsageBeforeFirstInterval,
    UsageAfterLastInterval,
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
}

#[derive(Debug, Clone, Copy)]
struct RawAuditKey<'a> {
    provider: &'a str,
    model_id: &'a str,
    category: &'a str,
}

type UsageCoverageKey = (String, String, TokenCategory);
type UsageCoverageRange = (DateTime<Utc>, DateTime<Utc>);

#[derive(Debug, Clone)]
struct RawPricingInterval {
    provider: String,
    model_id: String,
    token_category: String,
    currency: String,
    rate_per_1m_tokens: Option<f64>,
    effective_from: String,
    effective_to: Option<String>,
    source: String,
}

fn audit_key<'a>(provider: &'a str, model_id: &'a str, category: TokenCategory) -> AuditKey<'a> {
    AuditKey {
        provider,
        model_id,
        category,
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
}

pub fn insert_interval(conn: &Connection, interval: &PricingInterval) -> Result<()> {
    conn.execute(
        "INSERT INTO pricing_intervals
            (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            interval.provider,
            interval.model_id,
            interval.token_category.as_str(),
            interval.currency,
            interval.rate_per_1m_tokens,
            interval.effective_from.to_rfc3339(),
            interval.effective_to.map(|dt| dt.to_rfc3339()),
            interval.source,
        ],
    )?;
    Ok(())
}

pub fn insert_interval_if_missing(conn: &Connection, interval: &PricingInterval) -> Result<bool> {
    validate_interval(interval)?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO pricing_intervals
            (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            interval.provider,
            interval.model_id,
            interval.token_category.as_str(),
            interval.currency,
            interval.rate_per_1m_tokens,
            interval.effective_from.to_rfc3339(),
            interval.effective_to.map(|dt| dt.to_rfc3339()),
            interval.source,
        ],
    )?;
    Ok(changed > 0)
}

pub fn seed_pricing(conn: &Connection) -> Result<usize> {
    let mut inserted = 0;
    for interval in seed_intervals() {
        if insert_interval_if_missing(conn, &interval)? {
            inserted += 1;
        }
    }
    Ok(inserted)
}

pub fn refresh_pricing(conn: &Connection, fetcher: &dyn PricingFetcher) -> Result<usize> {
    let intervals = fetcher.fetch_current_prices()?;
    let tx = conn.unchecked_transaction()?;
    let mut changed = 0;
    for interval in intervals {
        validate_interval(&interval)?;
        changed += upsert_current_interval(&tx, &interval)?;
    }
    tx.commit()?;
    Ok(changed)
}

fn upsert_current_interval(conn: &Connection, interval: &PricingInterval) -> Result<usize> {
    let existing = open_interval(
        conn,
        &interval.provider,
        &interval.model_id,
        interval.token_category,
    )?;

    match existing {
        Some(existing)
            if (existing.rate_per_1m_tokens - interval.rate_per_1m_tokens).abs() < f64::EPSILON
                && existing.currency == interval.currency =>
        {
            Ok(0)
        }
        Some(existing) => {
            if existing.effective_from > interval.effective_from {
                bail!(
                    "stale pricing interval for provider={}, model={}, category={}: fetched effective_from {} is before current open interval {}",
                    interval.provider,
                    interval.model_id,
                    interval.token_category,
                    interval.effective_from.to_rfc3339(),
                    existing.effective_from.to_rfc3339()
                );
            }
            conn.execute(
                "UPDATE pricing_intervals
                 SET effective_to = ?1
                 WHERE provider = ?2
                   AND model_id = ?3
                   AND token_category = ?4
                   AND currency = ?5
                   AND effective_from = ?6",
                rusqlite::params![
                    interval.effective_from.to_rfc3339(),
                    existing.provider,
                    existing.model_id,
                    existing.token_category.as_str(),
                    existing.currency,
                    existing.effective_from.to_rfc3339(),
                ],
            )?;
            insert_interval_if_missing(conn, interval)?;
            Ok(2)
        }
        None => Ok(insert_interval_if_missing(conn, interval)? as usize),
    }
}

fn open_interval(
    conn: &Connection,
    provider: &str,
    model_id: &str,
    token_category: TokenCategory,
) -> Result<Option<PricingInterval>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals
         WHERE provider = ?1
           AND model_id = ?2
           AND token_category = ?3
           AND currency = 'USD'
           AND effective_to IS NULL
         ORDER BY effective_from DESC",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![provider, model_id, token_category.as_str()],
            row_to_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.into_iter().next()),
        _ => bail!(
            "multiple open prices for provider={provider}, model={model_id}, category={token_category}"
        ),
    }
}

pub fn applicable_interval(
    conn: &Connection,
    provider: &str,
    model_id: &str,
    token_category: TokenCategory,
    timestamp: DateTime<Utc>,
) -> Result<PricingInterval> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals
         WHERE provider = ?1
           AND model_id = ?2
           AND token_category = ?3
           AND currency = 'USD'
           AND effective_from <= ?4
           AND (effective_to IS NULL OR ?4 < effective_to)
         ORDER BY effective_from ASC",
    )?;

    let rows = stmt
        .query_map(
            rusqlite::params![
                provider,
                model_id,
                token_category.as_str(),
                timestamp.to_rfc3339(),
            ],
            row_to_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    match rows.len() {
        0 => bail!(
            "missing price for provider={provider}, model={model_id}, category={token_category}, timestamp={}",
            timestamp.to_rfc3339()
        ),
        1 => Ok(rows.into_iter().next().unwrap()),
        _ => bail!(
            "overlapping prices for provider={provider}, model={model_id}, category={token_category}, timestamp={}",
            timestamp.to_rfc3339()
        ),
    }
}

pub fn calculate_record_cost(conn: &Connection, record: &TokenRecord) -> Result<f64> {
    let mut total = 0.0;
    for (category, tokens) in nonzero_token_categories(record) {
        let interval = applicable_interval(
            conn,
            &record.provider,
            &record.model_id,
            category,
            record.timestamp,
        )?;
        total += interval.cost_for_tokens(tokens);
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
        findings.extend(audit_usage_coverage(conn)?);
    } else {
        findings.push(schema_finding(
            PricingAuditKind::MissingSchema,
            "token_usage table is missing; run `tkstat --force-update` to ingest usage before checking coverage",
        ));
    }
    Ok(findings)
}

fn audit_catalog(conn: &Connection) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals
         ORDER BY provider, model_id, token_category, currency, effective_from",
    )?;
    let intervals = stmt
        .query_map([], row_to_raw_interval)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut findings = Vec::new();
    let mut by_key: HashMap<(String, String, TokenCategory, String), Vec<PricingInterval>> =
        HashMap::new();

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
                    &interval.provider,
                    &interval.model_id,
                    interval.token_category,
                ),
                Some(interval.effective_from),
                interval.effective_to,
                "use USD pricing intervals",
            ));
        }
        by_key
            .entry((
                interval.provider.clone(),
                interval.model_id.clone(),
                interval.token_category,
                interval.currency.clone(),
            ))
            .or_default()
            .push(interval);
    }

    for ((provider, model_id, category, _currency), mut intervals) in by_key {
        intervals.sort_by_key(|interval| interval.effective_from);
        let open_count = intervals
            .iter()
            .filter(|interval| interval.effective_to.is_none())
            .count();
        if open_count == 0 {
            findings.push(finding(
                PricingAuditSeverity::Warning,
                PricingAuditKind::MissingCurrent,
                audit_key(&provider, &model_id, category),
                intervals.last().map(|interval| interval.effective_from),
                None,
                "insert a current open-ended pricing interval",
            ));
        }
        if open_count > 1 {
            findings.push(finding(
                PricingAuditSeverity::Error,
                PricingAuditKind::DuplicateCurrent,
                audit_key(&provider, &model_id, category),
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
                    audit_key(&provider, &model_id, category),
                    Some(to),
                    Some(next.effective_from),
                    "insert a pricing interval that covers the gap",
                )),
                Some(to) if to > next.effective_from => findings.push(finding(
                    PricingAuditSeverity::Error,
                    PricingAuditKind::Overlap,
                    audit_key(&provider, &model_id, category),
                    Some(next.effective_from),
                    Some(to),
                    "adjust effective_from/effective_to so intervals do not overlap",
                )),
                None => findings.push(finding(
                    PricingAuditSeverity::Error,
                    PricingAuditKind::Overlap,
                    audit_key(&provider, &model_id, category),
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
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, timestamp, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cached_input_tokens, reasoning_output_tokens
         FROM token_usage",
    )?;
    let mut rows = stmt.query([])?;
    let mut usage: HashMap<UsageCoverageKey, UsageCoverageRange> = HashMap::new();

    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        let timestamp: DateTime<Utc> = timestamp.parse()?;
        let categories = billable_token_categories_for_counts(
            &provider,
            row.get::<_, i64>(3)?.max(0) as u64,
            row.get::<_, i64>(4)?.max(0) as u64,
            row.get::<_, i64>(5)?.max(0) as u64,
            row.get::<_, i64>(6)?.max(0) as u64,
            row.get::<_, i64>(7)?.max(0) as u64,
            row.get::<_, i64>(8)?.max(0) as u64,
        );
        for (category, _tokens) in categories {
            usage
                .entry((provider.clone(), model_id.clone(), category))
                .and_modify(|(min, max)| {
                    *min = (*min).min(timestamp);
                    *max = (*max).max(timestamp);
                })
                .or_insert((timestamp, timestamp));
        }
    }

    let mut findings = Vec::new();
    for ((provider, model_id, category), (start, end)) in usage {
        findings.extend(audit_usage_key(
            conn, &provider, &model_id, category, start, end,
        )?);
    }
    Ok(findings)
}

fn audit_usage_key(
    conn: &Connection,
    provider: &str,
    model_id: &str,
    category: TokenCategory,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<PricingAuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals
         WHERE provider = ?1
           AND model_id = ?2
           AND token_category = ?3
           AND currency = 'USD'
         ORDER BY effective_from",
    )?;
    let raw_intervals = stmt
        .query_map(
            rusqlite::params![provider, model_id, category.as_str()],
            row_to_raw_interval,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let intervals: Vec<PricingInterval> = raw_intervals
        .iter()
        .filter_map(raw_to_valid_interval)
        .collect();

    if intervals.is_empty() {
        return Ok(vec![finding(
            PricingAuditSeverity::Error,
            PricingAuditKind::MissingCoverage,
            audit_key(provider, model_id, category),
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
            audit_key(provider, model_id, category),
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
                audit_key(provider, model_id, category),
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
            audit_key(provider, model_id, category),
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
    finding_raw(
        severity,
        kind,
        raw_audit_key(key.provider, key.model_id, key.category.as_str()),
        start.map(|dt| dt.to_rfc3339()),
        end.map(|dt| dt.to_rfc3339()),
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

    let from = raw.effective_from.parse::<DateTime<Utc>>();
    if from.is_err() {
        findings.push(malformed_finding(
            raw,
            "store effective_from as an RFC3339 timestamp",
        ));
    }
    let to = raw
        .effective_to
        .as_ref()
        .map(|dt| dt.parse::<DateTime<Utc>>())
        .transpose();
    if to.is_err() {
        findings.push(malformed_finding(
            raw,
            "store effective_to as an RFC3339 timestamp or NULL",
        ));
    }
    if let (Ok(from), Ok(Some(to))) = (from, to)
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
    let token_category = raw.token_category.parse().ok()?;
    let effective_from = raw.effective_from.parse().ok()?;
    let effective_to = raw
        .effective_to
        .as_ref()
        .map(|dt| dt.parse())
        .transpose()
        .ok()?;
    if let Some(effective_to) = effective_to
        && effective_to <= effective_from
    {
        return None;
    }
    Some(PricingInterval {
        provider: raw.provider.clone(),
        model_id: raw.model_id.clone(),
        token_category,
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
        currency: text_cell(row, 3)?,
        rate_per_1m_tokens: numeric_cell(row, 4)?,
        effective_from: text_cell(row, 5)?,
        effective_to: optional_text_cell(row, 6)?,
        source: text_cell(row, 7)?,
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
    let token_category: String = row.get(2)?;
    let effective_from: String = row.get(5)?;
    let effective_to: Option<String> = row.get(6)?;
    Ok(PricingInterval {
        provider: row.get(0)?,
        model_id: row.get(1)?,
        token_category: token_category.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, e.into())
        })?,
        currency: row.get(3)?,
        rate_per_1m_tokens: row.get(4)?,
        effective_from: effective_from.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        effective_to: effective_to
            .map(|dt| {
                dt.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        source: row.get(7)?,
    })
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
    let effective_from = "2026-01-01T00:00:00Z".parse().unwrap();
    let mut intervals = Vec::new();

    for model_id in [
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-opus-4-5-20251101",
        "claude-opus-4-5-20250929",
    ] {
        add_anthropic_model(
            &mut intervals,
            model_id,
            5.0,
            25.0,
            effective_from,
            "seed:anthropic-claude-pricing-2026-05-23",
        );
    }
    for model_id in [
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4-5-20250929",
    ] {
        add_anthropic_model(
            &mut intervals,
            model_id,
            3.0,
            15.0,
            effective_from,
            "seed:anthropic-claude-pricing-2026-05-23",
        );
    }
    for model_id in [
        "claude-haiku-4-6",
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251001",
    ] {
        add_anthropic_model(
            &mut intervals,
            model_id,
            1.0,
            5.0,
            effective_from,
            "seed:anthropic-claude-pricing-2026-05-23",
        );
    }

    for model_id in ["gpt-5.1-codex", "gpt-5.4", "gpt-5.5"] {
        add_openai_model(
            &mut intervals,
            model_id,
            2.50,
            0.25,
            15.0,
            effective_from,
            "seed:openai-gpt-5-4-pricing-2026-05-23",
        );
    }

    intervals
}

fn add_anthropic_model(
    intervals: &mut Vec<PricingInterval>,
    model_id: &str,
    input: f64,
    output: f64,
    effective_from: DateTime<Utc>,
    source: &str,
) {
    for (category, rate) in [
        (TokenCategory::Input, input),
        (TokenCategory::Output, output),
        (TokenCategory::CacheCreation, input * 1.25),
        (TokenCategory::CacheRead, input * 0.10),
    ] {
        intervals.push(PricingInterval::usd(
            "claude",
            model_id,
            category,
            rate,
            effective_from,
            source,
        ));
    }
}

fn add_openai_model(
    intervals: &mut Vec<PricingInterval>,
    model_id: &str,
    input: f64,
    cached_input: f64,
    output: f64,
    effective_from: DateTime<Utc>,
    source: &str,
) {
    for (category, rate) in [
        (TokenCategory::Input, input),
        (TokenCategory::CachedInput, cached_input),
        (TokenCategory::Output, output),
        (TokenCategory::ReasoningOutput, output),
    ] {
        intervals.push(PricingInterval::usd(
            "codex",
            model_id,
            category,
            rate,
            effective_from,
            source,
        ));
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
            "claude",
            "claude-opus-4-6",
            category,
            rate,
            from.parse().unwrap(),
            "test",
        );
        interval.effective_to = to.map(|dt| dt.parse().unwrap());
        interval
    }

    fn record(model_id: &str) -> TokenRecord {
        TokenRecord {
            provider: "claude".into(),
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

    #[test]
    fn test_insert_and_select_applicable_price() {
        let db = Database::open_in_memory().unwrap();
        let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
        validate_interval(&interval).unwrap();
        insert_interval(db.conn(), &interval).unwrap();

        let selected = applicable_interval(
            db.conn(),
            "claude",
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
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
            "claude",
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
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
            "claude",
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
                    "codex",
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
        usage.provider = "codex".into();
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
    fn test_seed_pricing_inserts_bundled_fallback_once() {
        let db = Database::open_in_memory().unwrap();
        let inserted = seed_pricing(db.conn()).unwrap();
        assert!(inserted > 0);
        assert_eq!(seed_pricing(db.conn()).unwrap(), 0);

        let selected = applicable_interval(
            db.conn(),
            "codex",
            "gpt-5.4",
            TokenCategory::CachedInput,
            "2026-05-24T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 0.25);
        assert!(selected.source.starts_with("seed:"));
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
                "claude",
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
    fn test_pricing_audit_detects_unsupported_currency() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO pricing_intervals
                 (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
                 VALUES ('claude', 'claude-opus-4-6', 'input', 'EUR', 1.0, '2026-01-01T00:00:00Z', NULL, 'test')",
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
    fn test_pricing_audit_reports_malformed_catalog_rows() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute("PRAGMA ignore_check_constraints = ON", [])
            .unwrap();
        insert_raw_pricing_row(
            db.conn(),
            "claude",
            "claude-opus-4-6",
            "mystery",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            "claude",
            "claude-opus-4-7",
            "input",
            1.0,
            "not-a-date",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            "claude",
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
            "",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "test",
        );
        insert_raw_pricing_row(
            db.conn(),
            "claude",
            "claude-opus-4-10",
            "input",
            1.0,
            "2026-01-01T00:00:00Z",
            "",
        );

        let findings = audit_pricing(db.conn()).unwrap();
        for (provider, model_id, category, remediation) in [
            (
                "claude",
                "claude-opus-4-6",
                "mystery",
                "supported token category",
            ),
            ("claude", "claude-opus-4-7", "input", "RFC3339"),
            ("claude", "claude-opus-4-8", "input", "non-negative rate"),
            ("", "claude-opus-4-9", "input", "non-empty provider"),
            ("claude", "", "input", "non-empty model id"),
            ("claude", "claude-opus-4-10", "input", "non-empty source"),
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
    fn test_pricing_audit_detects_missing_cached_token_pricing_for_usage() {
        let db = Database::open_in_memory().unwrap();
        let mut usage = record("gpt-audit");
        usage.provider = "codex".into();
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
    fn test_pricing_audit_does_not_require_separate_codex_reasoning_price() {
        let db = Database::open_in_memory().unwrap();
        insert_interval(
            db.conn(),
            &PricingInterval::usd(
                "codex",
                "gpt-audit",
                TokenCategory::Output,
                100.0,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
        )
        .unwrap();
        let mut usage = record("gpt-audit");
        usage.provider = "codex".into();
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
            "claude",
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
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
            "claude",
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 15.0);
    }
}
