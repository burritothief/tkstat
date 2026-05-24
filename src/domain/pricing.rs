use chrono::{DateTime, Utc};

use crate::domain::usage::TokenRecord;

/// Normalized token categories used for pricing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenCategory {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    CachedInput,
    ReasoningOutput,
}

impl TokenCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::CacheRead => "cache_read",
            Self::CacheCreation => "cache_creation",
            Self::CachedInput => "cached_input",
            Self::ReasoningOutput => "reasoning_output",
        }
    }
}

impl std::fmt::Display for TokenCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TokenCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            "cache_read" => Ok(Self::CacheRead),
            "cache_creation" => Ok(Self::CacheCreation),
            "cached_input" => Ok(Self::CachedInput),
            "reasoning_output" => Ok(Self::ReasoningOutput),
            _ => Err(format!("unknown token category '{s}'")),
        }
    }
}

/// Effective-dated price for one provider/model/category.
#[derive(Debug, Clone)]
pub struct PricingInterval {
    pub provider: String,
    pub model_id: String,
    pub token_category: TokenCategory,
    pub currency: String,
    pub rate_per_1m_tokens: f64,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub source: String,
}

impl PricingInterval {
    pub fn usd(
        provider: impl Into<String>,
        model_id: impl Into<String>,
        token_category: TokenCategory,
        rate_per_1m_tokens: f64,
        effective_from: DateTime<Utc>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
            token_category,
            currency: "USD".into(),
            rate_per_1m_tokens,
            effective_from,
            effective_to: None,
            source: source.into(),
        }
    }

    pub fn cost_for_tokens(&self, tokens: u64) -> f64 {
        tokens as f64 * self.rate_per_1m_tokens / 1_000_000.0
    }
}

pub fn nonzero_token_categories(record: &TokenRecord) -> Vec<(TokenCategory, u64)> {
    billable_token_categories_for_counts(
        &record.provider,
        record.input_tokens,
        record.output_tokens,
        record.cache_read_tokens,
        record.cache_creation_tokens,
        record.cached_input_tokens,
        record.reasoning_output_tokens,
    )
}

pub fn billable_token_categories_for_counts(
    provider: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cached_input_tokens: u64,
    reasoning_output_tokens: u64,
) -> Vec<(TokenCategory, u64)> {
    let input_billable = if is_openai_style_provider(provider) {
        input_tokens.saturating_sub(cached_input_tokens)
    } else {
        input_tokens
    };
    let reasoning_billable = if is_openai_style_provider(provider) {
        0
    } else {
        reasoning_output_tokens
    };

    [
        (TokenCategory::Input, input_billable),
        (TokenCategory::Output, output_tokens),
        (TokenCategory::CacheRead, cache_read_tokens),
        (TokenCategory::CacheCreation, cache_creation_tokens),
        (TokenCategory::CachedInput, cached_input_tokens),
        (TokenCategory::ReasoningOutput, reasoning_billable),
    ]
    .into_iter()
    .filter(|(_, tokens)| *tokens > 0)
    .collect()
}

fn is_openai_style_provider(provider: &str) -> bool {
    provider == "codex"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::usage::{ModelFamily, TokenRecord};

    #[test]
    fn test_token_category_roundtrip() {
        for category in [
            TokenCategory::Input,
            TokenCategory::Output,
            TokenCategory::CacheRead,
            TokenCategory::CacheCreation,
            TokenCategory::CachedInput,
            TokenCategory::ReasoningOutput,
        ] {
            assert_eq!(
                category.as_str().parse::<TokenCategory>().unwrap(),
                category
            );
        }
        assert!("bogus".parse::<TokenCategory>().is_err());
    }

    #[test]
    fn test_pricing_interval_cost_for_tokens() {
        let interval = PricingInterval::usd(
            "claude",
            "claude-opus-4-6",
            TokenCategory::Input,
            15.0,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "test",
        );
        assert!((interval.cost_for_tokens(1_000_000) - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_nonzero_token_categories_uses_non_overlapping_codex_billing() {
        let record = TokenRecord {
            provider: "codex".into(),
            request_id: "r1".into(),
            session_id: "s1".into(),
            uuid: "u1".into(),
            timestamp: "2026-05-24T00:00:00Z".parse().unwrap(),
            model: ModelFamily::Unknown,
            model_id: "gpt-5.5".into(),
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cached_input_tokens: 40,
            reasoning_output_tokens: 7,
            cost_usd: 0.0,
            project: "tkstat".into(),
            source_file: "/tmp/session.jsonl".into(),
            is_subagent: false,
        };
        let categories = nonzero_token_categories(&record);
        assert!(categories.contains(&(TokenCategory::Input, 60)));
        assert!(categories.contains(&(TokenCategory::CachedInput, 40)));
        assert!(categories.contains(&(TokenCategory::Output, 20)));
        assert!(
            !categories
                .iter()
                .any(|(c, _)| *c == TokenCategory::ReasoningOutput)
        );
        assert!(
            !categories
                .iter()
                .any(|(c, _)| *c == TokenCategory::CacheRead)
        );
    }
}
