use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fmt;

/// Normalized model family for grouping and pricing lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    Opus,
    Sonnet,
    Haiku,
    Unknown,
}

impl ModelFamily {
    pub fn from_model_str(s: &str) -> Self {
        let s = s.to_ascii_lowercase();
        if s.contains("opus") {
            Self::Opus
        } else if s.contains("sonnet") {
            Self::Sonnet
        } else if s.contains("haiku") {
            Self::Haiku
        } else {
            Self::Unknown
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ModelFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ModelFamily {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "opus" => Ok(Self::Opus),
            "sonnet" => Ok(Self::Sonnet),
            "haiku" => Ok(Self::Haiku),
            _ => Err(format!("unknown model family '{s}'")),
        }
    }
}

/// A single deduplicated API request's token usage.
#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub request_id: String,
    pub session_id: String,
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub model: ModelFamily,
    pub model_raw: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub project: String,
    pub source_file: String,
    pub is_subagent: bool,
}

/// A single row in an aggregated report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AggregatedRow {
    pub period: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub request_count: u64,
    pub session_count: u64,
}

impl AggregatedRow {
    /// Compute a totals row by summing a slice of rows.
    pub fn sum(rows: &[Self]) -> Self {
        let mut total = Self {
            period: "total".into(),
            ..Default::default()
        };
        for r in rows {
            total.input_tokens += r.input_tokens;
            total.output_tokens += r.output_tokens;
            total.cache_write_tokens += r.cache_write_tokens;
            total.cache_read_tokens += r.cache_read_tokens;
            total.total_tokens += r.total_tokens;
            total.cost_usd += r.cost_usd;
            total.request_count += r.request_count;
            total.session_count += r.session_count;
        }
        total
    }
}

/// Format a token count for display: 0, 856, 52.3 K, 1.2 M, etc.
pub fn format_tokens(n: u64) -> String {
    match n {
        0 => "0".into(),
        1..=999 => format!("{n}"),
        1_000..=99_999 => format!("{:.1} K", n as f64 / 1_000.0),
        100_000..=999_999 => format!("{} K", n / 1_000),
        1_000_000..=99_999_999 => format!("{:.1} M", n as f64 / 1_000_000.0),
        100_000_000..=999_999_999 => format!("{} M", n / 1_000_000),
        _ => format!("{:.1} B", n as f64 / 1_000_000_000.0),
    }
}

/// Format a USD cost for display.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        "$0.00".into()
    } else if usd < 10.0 {
        format!("${usd:.2}")
    } else if usd < 100.0 {
        format!("${usd:.1}")
    } else {
        format!("${usd:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_family_from_model_str() {
        assert_eq!(ModelFamily::from_model_str("claude-opus-4-6"), ModelFamily::Opus);
        assert_eq!(ModelFamily::from_model_str("claude-sonnet-4-5-20250929"), ModelFamily::Sonnet);
        assert_eq!(ModelFamily::from_model_str("claude-haiku-4-5-20251001"), ModelFamily::Haiku);
        assert_eq!(ModelFamily::from_model_str("sonnet"), ModelFamily::Sonnet);
        assert_eq!(ModelFamily::from_model_str("gpt-4"), ModelFamily::Unknown);
    }

    #[test]
    fn test_model_family_from_str_trait() {
        assert_eq!("opus".parse::<ModelFamily>().unwrap(), ModelFamily::Opus);
        assert_eq!("Sonnet".parse::<ModelFamily>().unwrap(), ModelFamily::Sonnet);
        assert!("gpt-4".parse::<ModelFamily>().is_err());
    }

    #[test]
    fn test_aggregated_row_sum() {
        let rows = vec![
            AggregatedRow { input_tokens: 100, output_tokens: 50, cost_usd: 1.0, request_count: 2, ..Default::default() },
            AggregatedRow { input_tokens: 200, output_tokens: 80, cost_usd: 2.0, request_count: 3, ..Default::default() },
        ];
        let total = AggregatedRow::sum(&rows);
        assert_eq!(total.input_tokens, 300);
        assert_eq!(total.output_tokens, 130);
        assert_eq!(total.request_count, 5);
        assert!((total.cost_usd - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5 K");
        assert_eq!(format_tokens(150_000), "150 K");
        assert_eq!(format_tokens(1_500_000), "1.5 M");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.005), "$0.00");
        assert_eq!(format_cost(1.23), "$1.23");
        assert_eq!(format_cost(45.6), "$45.6");
        assert_eq!(format_cost(123.0), "$123");
    }
}
