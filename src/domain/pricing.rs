use chrono::{DateTime, Utc};

use crate::domain::provider::ProviderId;
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
    pub const ALL: [Self; 6] = [
        Self::Input,
        Self::Output,
        Self::CacheRead,
        Self::CacheCreation,
        Self::CachedInput,
        Self::ReasoningOutput,
    ];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCountField {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    CachedInput,
    ReasoningOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillableTokenExpression {
    Field(TokenCountField),
    SaturatingSub(TokenCountField, TokenCountField),
    Zero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillableTokenRule {
    pub category: TokenCategory,
    pub expression: BillableTokenExpression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBillingPolicy {
    pub provider: ProviderId,
    pub rules: &'static [BillableTokenRule],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCounts {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_output_tokens: u64,
}

const DEFAULT_BILLING_RULES: [BillableTokenRule; 6] = [
    BillableTokenRule {
        category: TokenCategory::Input,
        expression: BillableTokenExpression::Field(TokenCountField::Input),
    },
    BillableTokenRule {
        category: TokenCategory::Output,
        expression: BillableTokenExpression::Field(TokenCountField::Output),
    },
    BillableTokenRule {
        category: TokenCategory::CacheRead,
        expression: BillableTokenExpression::Field(TokenCountField::CacheRead),
    },
    BillableTokenRule {
        category: TokenCategory::CacheCreation,
        expression: BillableTokenExpression::Field(TokenCountField::CacheCreation),
    },
    BillableTokenRule {
        category: TokenCategory::CachedInput,
        expression: BillableTokenExpression::Field(TokenCountField::CachedInput),
    },
    BillableTokenRule {
        category: TokenCategory::ReasoningOutput,
        expression: BillableTokenExpression::Field(TokenCountField::ReasoningOutput),
    },
];

const OPENAI_BILLING_RULES: [BillableTokenRule; 6] = [
    BillableTokenRule {
        category: TokenCategory::Input,
        expression: BillableTokenExpression::SaturatingSub(
            TokenCountField::Input,
            TokenCountField::CachedInput,
        ),
    },
    BillableTokenRule {
        category: TokenCategory::Output,
        expression: BillableTokenExpression::Field(TokenCountField::Output),
    },
    BillableTokenRule {
        category: TokenCategory::CacheRead,
        expression: BillableTokenExpression::Field(TokenCountField::CacheRead),
    },
    BillableTokenRule {
        category: TokenCategory::CacheCreation,
        expression: BillableTokenExpression::Field(TokenCountField::CacheCreation),
    },
    BillableTokenRule {
        category: TokenCategory::CachedInput,
        expression: BillableTokenExpression::Field(TokenCountField::CachedInput),
    },
    BillableTokenRule {
        category: TokenCategory::ReasoningOutput,
        expression: BillableTokenExpression::Zero,
    },
];

const PROVIDER_BILLING_POLICIES: [ProviderBillingPolicy; 1] = [ProviderBillingPolicy {
    provider: ProviderId::Codex,
    rules: &OPENAI_BILLING_RULES,
}];

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
    pub provider: ProviderId,
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
        provider: ProviderId,
        model_id: impl Into<String>,
        token_category: TokenCategory,
        rate_per_1m_tokens: f64,
        effective_from: DateTime<Utc>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            provider,
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
    billable_token_categories(record.provider, TokenCounts::from(record))
}

pub fn billable_token_categories_for_counts(
    provider: ProviderId,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cached_input_tokens: u64,
    reasoning_output_tokens: u64,
) -> Vec<(TokenCategory, u64)> {
    billable_token_categories(
        provider,
        TokenCounts {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cached_input_tokens,
            reasoning_output_tokens,
        },
    )
}

pub fn billable_token_categories(
    provider: ProviderId,
    counts: TokenCounts,
) -> Vec<(TokenCategory, u64)> {
    billable_token_rules(provider)
        .iter()
        .filter_map(|rule| {
            let tokens = rule.expression.tokens(counts);
            (tokens > 0).then_some((rule.category, tokens))
        })
        .collect()
}

pub fn default_billing_rules() -> &'static [BillableTokenRule] {
    &DEFAULT_BILLING_RULES
}

pub fn provider_billing_policies() -> &'static [ProviderBillingPolicy] {
    &PROVIDER_BILLING_POLICIES
}

pub fn billable_token_rules(provider: ProviderId) -> &'static [BillableTokenRule] {
    provider_billing_policies()
        .iter()
        .find(|policy| policy.provider == provider)
        .map_or(default_billing_rules(), |policy| policy.rules)
}

pub fn billable_token_rule(
    rules: &'static [BillableTokenRule],
    category: TokenCategory,
) -> BillableTokenRule {
    rules
        .iter()
        .copied()
        .find(|rule| rule.category == category)
        .expect("every billing policy must define all token categories")
}

impl BillableTokenExpression {
    pub fn tokens(self, counts: TokenCounts) -> u64 {
        match self {
            Self::Field(field) => field.tokens(counts),
            Self::SaturatingSub(minuend, subtrahend) => minuend
                .tokens(counts)
                .saturating_sub(subtrahend.tokens(counts)),
            Self::Zero => 0,
        }
    }
}

impl TokenCountField {
    pub fn tokens(self, counts: TokenCounts) -> u64 {
        match self {
            Self::Input => counts.input_tokens,
            Self::Output => counts.output_tokens,
            Self::CacheRead => counts.cache_read_tokens,
            Self::CacheCreation => counts.cache_creation_tokens,
            Self::CachedInput => counts.cached_input_tokens,
            Self::ReasoningOutput => counts.reasoning_output_tokens,
        }
    }
}

impl From<&TokenRecord> for TokenCounts {
    fn from(record: &TokenRecord) -> Self {
        Self {
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            cache_read_tokens: record.cache_read_tokens,
            cache_creation_tokens: record.cache_creation_tokens,
            cached_input_tokens: record.cached_input_tokens,
            reasoning_output_tokens: record.reasoning_output_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::provider::ProviderId;
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
            ProviderId::ClaudeCode,
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
            provider: crate::domain::provider::ProviderId::Codex,
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
