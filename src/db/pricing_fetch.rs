//! Provider-owned pricing document fetchers.
//!
//! Network access is explicit (`tkstat --pricing-refresh`) and feature-gated.
//! Parsing deliberately targets small, identifiable Markdown structures so a
//! provider documentation change fails without replacing last-known-good data.

use std::collections::HashSet;
use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDate, Utc};

use crate::db::pricing::{PricingFetcher, PricingSourceMetadata};
use crate::domain::pricing::{PricingDimensions, PricingInterval, TokenCategory};
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::parse_canonical_utc_rfc3339;

pub const ANTHROPIC_PRICING_URL: &str =
    "https://docs.anthropic.com/en/docs/about-claude/pricing.md";
pub const ANTHROPIC_MODELS_URL: &str =
    "https://docs.anthropic.com/en/docs/about-claude/models/overview.md";
pub const OPENAI_PRICING_URL: &str = "https://developers.openai.com/api/docs/pricing.md";

const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LivePricingFetcher {
    intervals: Vec<PricingInterval>,
    sources: Vec<PricingSourceMetadata>,
}

impl LivePricingFetcher {
    pub fn fetch(provider: ProviderId) -> Result<Self> {
        let retrieved_at = Utc::now().date_naive();
        match provider {
            ProviderId::ClaudeCode => {
                let pricing = fetch_document(ANTHROPIC_PRICING_URL)?;
                let models = fetch_document(ANTHROPIC_MODELS_URL)?;
                Self::from_anthropic_documents(&pricing, &models, retrieved_at)
            }
            ProviderId::Codex => {
                let pricing = fetch_document(OPENAI_PRICING_URL)?;
                Self::from_openai_document(&pricing, retrieved_at)
            }
        }
    }

    pub fn from_anthropic_documents(
        pricing: &str,
        models: &str,
        retrieved_at: NaiveDate,
    ) -> Result<Self> {
        let source = format!("provider:anthropic-pricing-{retrieved_at}");
        let intervals = parse_anthropic_pricing(pricing, models, retrieved_at, &source)?;
        Ok(Self {
            intervals,
            sources: vec![source_metadata(
                source,
                ANTHROPIC_PRICING_URL,
                retrieved_at,
                "Official Anthropic pricing and model-identity documents fetched by tkstat.",
            )],
        })
    }

    pub fn from_openai_document(pricing: &str, retrieved_at: NaiveDate) -> Result<Self> {
        let source = format!("provider:openai-pricing-{retrieved_at}");
        let intervals = parse_openai_pricing(pricing, retrieved_at, &source)?;
        Ok(Self {
            intervals,
            sources: vec![source_metadata(
                source,
                OPENAI_PRICING_URL,
                retrieved_at,
                "Official OpenAI standard token-pricing document fetched by tkstat.",
            )],
        })
    }

    /// Backdate only brand-new exact pricing keys to their earliest observed
    /// usage. Existing keys keep retrieval-time effective dating so a provider
    /// price change is never applied retroactively.
    pub fn cover_unpriced_observed_usage(mut self, conn: &rusqlite::Connection) -> Result<Self> {
        for interval in &mut self.intervals {
            let existing: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pricing_intervals
                 WHERE provider = ?1 AND model_id = ?2 AND token_category = ?3
                   AND service_tier IS ?4 AND speed IS ?5 AND region IS ?6
                   AND processing_mode IS ?7 AND source_detail IS ?8 AND currency = 'USD'",
                rusqlite::params![
                    interval.provider.as_str(),
                    interval.model_id,
                    interval.token_category.as_str(),
                    interval.dimensions.service_tier,
                    interval.dimensions.speed,
                    interval.dimensions.region,
                    interval.dimensions.processing_mode,
                    interval.dimensions.source_detail,
                ],
                |row| row.get(0),
            )?;
            if existing > 0 {
                continue;
            }
            let earliest: Option<String> = conn.query_row(
                "SELECT MIN(timestamp) FROM usage_billing_components
                 WHERE provider = ?1 AND model_id = ?2 AND token_category = ?3
                   AND service_tier IS ?4 AND speed IS ?5 AND region IS ?6
                   AND processing_mode IS ?7 AND source_detail IS ?8",
                rusqlite::params![
                    interval.provider.as_str(),
                    interval.model_id,
                    interval.token_category.as_str(),
                    interval.dimensions.service_tier,
                    interval.dimensions.speed,
                    interval.dimensions.region,
                    interval.dimensions.processing_mode,
                    interval.dimensions.source_detail,
                ],
                |row| row.get(0),
            )?;
            if let Some(earliest) = earliest {
                let earliest = parse_canonical_utc_rfc3339(&earliest)?;
                if earliest < interval.effective_from {
                    interval.effective_from = earliest;
                }
            }
        }
        Ok(self)
    }
}

impl PricingFetcher for LivePricingFetcher {
    fn fetch_current_prices(&self) -> Result<Vec<PricingInterval>> {
        Ok(self.intervals.clone())
    }

    fn fetch_source_metadata(&self) -> Result<Vec<PricingSourceMetadata>> {
        Ok(self.sources.clone())
    }
}

fn fetch_document(url: &str) -> Result<String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .get(url)
        .set("User-Agent", concat!("tkstat/", env!("CARGO_PKG_VERSION")))
        .call()
        .with_context(|| format!("fetching official pricing document {url}"))?;
    let mut reader = response.into_reader().take(MAX_DOCUMENT_BYTES + 1);
    let mut contents = String::new();
    reader
        .read_to_string(&mut contents)
        .with_context(|| format!("reading official pricing document {url}"))?;
    if contents.len() as u64 > MAX_DOCUMENT_BYTES {
        bail!("official pricing document {url} exceeds the 2 MiB safety limit");
    }
    Ok(contents)
}

fn source_metadata(
    source: String,
    url: &str,
    retrieved_at: NaiveDate,
    notes: &str,
) -> PricingSourceMetadata {
    PricingSourceMetadata {
        catalog_version: retrieved_at.to_string(),
        source: source.clone(),
        source_url: url.into(),
        source_retrieved_at: retrieved_at.to_string(),
        source_kind: "reviewed".into(),
        notes: notes.into(),
    }
}

fn parse_anthropic_pricing(
    pricing: &str,
    models: &str,
    retrieved_at: NaiveDate,
    source: &str,
) -> Result<Vec<PricingInterval>> {
    let model_ids = extract_model_ids(models, "claude-");
    if model_ids.is_empty() {
        bail!("Anthropic model document contains no Claude API model ids");
    }

    let effective_from = midnight_utc(retrieved_at)?;
    let mut in_model_table = false;
    let mut intervals = Vec::new();
    let mut matched_rows = 0usize;
    for line in pricing.lines() {
        if line.trim() == "## Model pricing" {
            in_model_table = true;
            continue;
        }
        if in_model_table && line.starts_with("<Note") {
            break;
        }
        if !in_model_table || !line.trim_start().starts_with("| Claude ") {
            continue;
        }
        if !anthropic_row_is_current(line, retrieved_at)? {
            continue;
        }
        let cells = markdown_cells(line);
        if cells.len() != 6 {
            bail!(
                "Anthropic model pricing row has {} columns; expected 6: {line}",
                cells.len()
            );
        }
        let label_key = anthropic_label_key(&cells[0]);
        let exact_ids: Vec<&String> = model_ids
            .iter()
            .filter(|model_id| anthropic_id_key(model_id) == label_key)
            .collect();
        if exact_ids.is_empty() {
            // Pricing may retain retired cloud-only models that are no longer
            // valid Claude API identities. Never synthesize an id from a label.
            continue;
        }
        let input = parse_mtok_rate(&cells[1])?;
        let cache_5m = parse_mtok_rate(&cells[2])?;
        let cache_1h = parse_mtok_rate(&cells[3])?;
        let cache_read = parse_mtok_rate(&cells[4])?;
        let output = parse_mtok_rate(&cells[5])?;
        for model_id in exact_ids {
            intervals.extend(anthropic_intervals(
                model_id,
                input,
                cache_5m,
                cache_1h,
                cache_read,
                output,
                effective_from,
                source,
            ));
        }
        matched_rows += 1;
    }
    if matched_rows == 0 || intervals.is_empty() {
        bail!("Anthropic pricing document did not contain the expected model pricing table");
    }
    Ok(intervals)
}

#[allow(clippy::too_many_arguments)]
fn anthropic_intervals(
    model_id: &str,
    input: f64,
    cache_5m: f64,
    cache_1h: f64,
    cache_read: f64,
    output: f64,
    effective_from: DateTime<Utc>,
    source: &str,
) -> Vec<PricingInterval> {
    let mut intervals = vec![
        interval(
            ProviderId::ClaudeCode,
            model_id,
            TokenCategory::Input,
            input,
            PricingDimensions::default(),
            effective_from,
            source,
        ),
        interval(
            ProviderId::ClaudeCode,
            model_id,
            TokenCategory::Output,
            output,
            PricingDimensions::default(),
            effective_from,
            source,
        ),
        interval(
            ProviderId::ClaudeCode,
            model_id,
            TokenCategory::CacheRead,
            cache_read,
            PricingDimensions::default(),
            effective_from,
            source,
        ),
        interval(
            ProviderId::ClaudeCode,
            model_id,
            TokenCategory::CacheCreation,
            cache_5m,
            PricingDimensions::default(),
            effective_from,
            source,
        ),
    ];
    for (source_detail, rate) in [("ephemeral_5m", cache_5m), ("ephemeral_1h", cache_1h)] {
        intervals.push(interval(
            ProviderId::ClaudeCode,
            model_id,
            TokenCategory::CacheCreation,
            rate,
            PricingDimensions {
                source_detail: Some(source_detail.into()),
                ..Default::default()
            },
            effective_from,
            source,
        ));
    }
    if supports_claude_us_inference(model_id) {
        let us_intervals = intervals
            .iter()
            .cloned()
            .map(|mut interval| {
                interval.dimensions.region = Some("us".into());
                interval.rate_per_1m_tokens *= 1.1;
                interval
            })
            .collect::<Vec<_>>();
        intervals.extend(us_intervals);
    }
    intervals
}

fn supports_claude_us_inference(model_id: &str) -> bool {
    let key = anthropic_id_key(model_id);
    if key.starts_with("claude-fable-") || key.starts_with("claude-mythos-") {
        return true;
    }
    let parts: Vec<&str> = key.split('-').collect();
    match parts.as_slice() {
        ["claude", "opus", major, minor, ..] | ["claude", "sonnet", major, minor, ..] => {
            major.parse::<u32>().ok().zip(minor.parse::<u32>().ok()) >= Some((4, 6))
        }
        ["claude", "sonnet", major] => major.parse::<u32>().ok().is_some_and(|major| major >= 5),
        _ => false,
    }
}

fn parse_openai_pricing(
    pricing: &str,
    retrieved_at: NaiveDate,
    source: &str,
) -> Result<Vec<PricingInterval>> {
    let effective_from = midnight_utc(retrieved_at)?;
    let standard = pricing
        .split_once("data-content-switcher-pane data-value=\"standard\"")
        .map(|(_, rest)| rest)
        .and_then(|rest| {
            rest.split_once("data-content-switcher-pane")
                .map(|(pane, _)| pane)
        })
        .ok_or_else(|| anyhow!("OpenAI pricing document lacks the standard pricing pane"))?;
    let rows = standard
        .split_once("rows={[")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("]}").map(|(rows, _)| rows))
        .ok_or_else(|| anyhow!("OpenAI standard pricing pane lacks the expected rows array"))?;

    let mut intervals = Vec::new();
    for line in rows.lines().map(str::trim) {
        if !line.starts_with("[\"") {
            continue;
        }
        let (model_id, values) = parse_openai_row(line)?;
        let dimensions = PricingDimensions {
            processing_mode: Some("standard".into()),
            ..Default::default()
        };
        intervals.push(interval(
            ProviderId::Codex,
            &model_id,
            TokenCategory::Input,
            values[0].ok_or_else(|| anyhow!("OpenAI model {model_id} lacks input pricing"))?,
            dimensions.clone(),
            effective_from,
            source,
        ));
        if let Some(cached) = values[1] {
            intervals.push(interval(
                ProviderId::Codex,
                &model_id,
                TokenCategory::CachedInput,
                cached,
                dimensions.clone(),
                effective_from,
                source,
            ));
        }
        let output = values
            .last()
            .copied()
            .flatten()
            .ok_or_else(|| anyhow!("OpenAI model {model_id} lacks output pricing"))?;
        intervals.push(interval(
            ProviderId::Codex,
            &model_id,
            TokenCategory::Output,
            output,
            dimensions.clone(),
            effective_from,
            source,
        ));
        intervals.push(interval(
            ProviderId::Codex,
            &model_id,
            TokenCategory::ReasoningOutput,
            output,
            dimensions,
            effective_from,
            source,
        ));
    }
    if intervals.is_empty() {
        bail!("OpenAI pricing document contained no standard text-token rows");
    }
    Ok(intervals)
}

fn parse_openai_row(line: &str) -> Result<(String, Vec<Option<f64>>)> {
    let remainder = &line[2..];
    let quote = remainder
        .find('"')
        .ok_or_else(|| anyhow!("malformed OpenAI pricing row: {line}"))?;
    let model_label = &remainder[..quote];
    let model_id = model_label
        .split(" (")
        .next()
        .unwrap_or(model_label)
        .trim()
        .to_string();
    if model_id.is_empty() {
        bail!("OpenAI pricing row has an empty model id");
    }
    let values: Vec<Option<f64>> = remainder[quote + 1..]
        .trim()
        .trim_start_matches(',')
        .trim()
        .trim_end_matches(',')
        .trim_end_matches(']')
        .split(',')
        .map(|value| {
            let value = value.trim().trim_matches('"');
            if matches!(value, "-" | "null") {
                Ok(None)
            } else {
                value
                    .parse::<f64>()
                    .map(Some)
                    .with_context(|| format!("invalid OpenAI price '{value}' for {model_id}"))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if values.len() < 3 {
        bail!(
            "OpenAI pricing row for {model_id} has {} values; expected at least 3",
            values.len()
        );
    }
    Ok((model_id, values))
}

fn interval(
    provider: ProviderId,
    model_id: &str,
    token_category: TokenCategory,
    rate: f64,
    dimensions: PricingDimensions,
    effective_from: DateTime<Utc>,
    source: &str,
) -> PricingInterval {
    PricingInterval {
        provider,
        model_id: model_id.into(),
        token_category,
        dimensions,
        currency: "USD".into(),
        rate_per_1m_tokens: rate,
        effective_from,
        effective_to: None,
        source: source.into(),
    }
}

fn markdown_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn parse_mtok_rate(cell: &str) -> Result<f64> {
    let value = cell
        .trim()
        .strip_prefix('$')
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| anyhow!("invalid MTok price cell '{cell}'"))?;
    value
        .parse::<f64>()
        .with_context(|| format!("invalid MTok price cell '{cell}'"))
}

fn extract_model_ids(document: &str, prefix: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    let mut rest = document;
    while let Some(start) = rest.find(prefix) {
        let candidate = &rest[start..];
        let len = candidate
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            .count();
        if len > prefix.len() {
            ids.insert(candidate[..len].to_ascii_lowercase());
        }
        rest = &candidate[len.max(1)..];
    }
    ids
}

fn anthropic_label_key(label: &str) -> String {
    let mut label = label;
    for marker in [" ([", " [", " through ", " starting "] {
        if let Some((before, _)) = label.split_once(marker) {
            label = before;
        }
    }
    label
        .to_ascii_lowercase()
        .replace('.', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn anthropic_id_key(model_id: &str) -> String {
    let mut parts: Vec<&str> = model_id.split('-').collect();
    if parts
        .last()
        .is_some_and(|part| part.len() == 8 && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        parts.pop();
    }
    parts.join("-")
}

fn anthropic_row_is_current(line: &str, today: NaiveDate) -> Result<bool> {
    for marker in [" starting ", " [starting "] {
        if let Some((_, date)) = line.split_once(marker) {
            return Ok(today >= parse_english_date(date)?);
        }
    }
    for marker in [" through ", " [through "] {
        if let Some((_, date)) = line.split_once(marker) {
            return Ok(today <= parse_english_date(date)?);
        }
    }
    Ok(true)
}

fn parse_english_date(value: &str) -> Result<NaiveDate> {
    let value = value.split(['[', ']', '|']).next().unwrap_or(value).trim();
    NaiveDate::parse_from_str(value, "%B %e, %Y")
        .with_context(|| format!("invalid effective date '{value}' in Anthropic pricing row"))
}

fn midnight_utc(date: NaiveDate) -> Result<DateTime<Utc>> {
    date.and_hms_opt(0, 0, 0)
        .map(|value| value.and_utc())
        .ok_or_else(|| anyhow!("invalid UTC pricing date {date}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::usage::{ModelFamily, TokenRecord};

    #[test]
    fn parses_anthropic_model_table_with_exact_api_ids() {
        let pricing = r#"
## Model pricing
| Model | Base Input Tokens | 5m Cache Writes | 1h Cache Writes | Cache Hits & Refreshes | Output Tokens |
| --- | --- | --- | --- | --- | --- |
| Claude Opus 4.8 | $5 / MTok | $6.25 / MTok | $10 / MTok | $0.50 / MTok | $25 / MTok |
<Note>done</Note>
"#;
        let models = "Claude API ID | claude-opus-4-8 | `claude-opus-4-8`";
        let fetcher = LivePricingFetcher::from_anthropic_documents(
            pricing,
            models,
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .unwrap();
        assert_eq!(fetcher.intervals.len(), 12);
        assert!(
            fetcher
                .intervals
                .iter()
                .all(|row| row.model_id == "claude-opus-4-8")
        );
        assert!(fetcher.intervals.iter().any(|row| {
            row.token_category == TokenCategory::CacheCreation
                && row.dimensions.source_detail.as_deref() == Some("ephemeral_1h")
                && row.rate_per_1m_tokens == 10.0
        }));
        assert!(fetcher.intervals.iter().any(|row| {
            row.token_category == TokenCategory::Input
                && row.dimensions.region.as_deref() == Some("us")
                && (row.rate_per_1m_tokens - 5.5).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn rejects_anthropic_price_without_official_model_identity() {
        let pricing = r#"
## Model pricing
| Claude Unknown 9 | $1 / MTok | $1 / MTok | $2 / MTok | $0.1 / MTok | $5 / MTok |
<Note>done</Note>
"#;
        let err = LivePricingFetcher::from_anthropic_documents(
            pricing,
            "`claude-opus-4-8`",
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("did not contain the expected model pricing table"));
    }

    #[test]
    fn parses_openai_standard_rows_with_optional_extra_input_price() {
        let pricing = r#"
<div data-content-switcher-pane data-value="standard">
rows={[
  ["gpt-5.6-sol", 5, 0.5, 6.25, 30],
  ["gpt-5.4 (<272K context length)", 2.5, 0.25, "-", 15],
]}
</div>
<div data-content-switcher-pane data-value="batch">
"#;
        let fetcher = LivePricingFetcher::from_openai_document(
            pricing,
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .unwrap();
        assert_eq!(fetcher.intervals.len(), 8);
        let sol_output = fetcher
            .intervals
            .iter()
            .find(|row| {
                row.model_id == "gpt-5.6-sol" && row.token_category == TokenCategory::Output
            })
            .unwrap();
        assert_eq!(sol_output.rate_per_1m_tokens, 30.0);
        assert_eq!(
            sol_output.dimensions.processing_mode.as_deref(),
            Some("standard")
        );
    }

    #[test]
    fn rejects_changed_openai_document_shape() {
        let err = LivePricingFetcher::from_openai_document(
            "# Pricing without structured rows",
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("standard pricing pane"));
    }

    #[test]
    fn brand_new_exact_model_is_backdated_to_observed_usage() {
        let db = Database::open_in_memory().unwrap();
        db.insert_records(&[TokenRecord {
            provider: ProviderId::Codex,
            request_id: "new-model-request".into(),
            session_id: "new-model-session".into(),
            uuid: "new-model-request".into(),
            timestamp: "2026-07-01T12:00:00Z".parse().unwrap(),
            model: ModelFamily::Unknown,
            model_id: "gpt-new".into(),
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cached_input_tokens: 10,
            reasoning_output_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            service_tier: None,
            speed: None,
            region: None,
            processing_mode: Some("standard".into()),
            cost_usd: 0.0,
            project: "test".into(),
            source_file: "/tmp/new-model.jsonl".into(),
            is_subagent: false,
        }])
        .unwrap();
        let pricing = r#"
<div data-content-switcher-pane data-value="standard">
rows={[
  ["gpt-new", 2, 0.2, 10],
]}
</div>
<div data-content-switcher-pane data-value="batch">
"#;
        let fetcher = LivePricingFetcher::from_openai_document(
            pricing,
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .unwrap()
        .cover_unpriced_observed_usage(db.conn())
        .unwrap();
        let expected: DateTime<Utc> = "2026-07-01T12:00:00Z".parse().unwrap();
        assert!(fetcher.intervals.iter().all(|interval| {
            interval.token_category == TokenCategory::ReasoningOutput
                || interval.effective_from == expected
        }));
    }
}
