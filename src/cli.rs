use chrono::NaiveDate;
use clap::{Parser, ValueEnum};

use crate::db::query::QueryFilter;
use crate::domain::period::{ReportTimeZone, TimePeriod};
use crate::domain::provider::{
    ALL_PROVIDERS_LABEL, CLAUDE_CODE_PROVIDER, CODEX_PROVIDER, ProviderId,
};

/// Metric to use for chart/heatmap rendering.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ChartMetric {
    Tokens,
    Cost,
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderArg {
    All,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Codex,
}

impl ProviderArg {
    pub const fn provider(self) -> Option<ProviderId> {
        match self {
            Self::All => None,
            Self::ClaudeCode => Some(ProviderId::ClaudeCode),
            Self::Codex => Some(ProviderId::Codex),
        }
    }

    pub fn providers(self) -> &'static [ProviderId] {
        match self {
            Self::All => &ProviderId::ALL,
            Self::ClaudeCode => &[ProviderId::ClaudeCode],
            Self::Codex => &[ProviderId::Codex],
        }
    }
}

/// Resolved output mode from CLI flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Table(TimePeriod),
    TopDays,
    ByModel,
    ByProvider,
    ByProject,
    Budget,
    CostExplain,
    Summary,
    Heatmap,
    Chart,
    Json(TimePeriod),
    Oneline,
}

#[derive(Parser, Debug)]
#[command(
    name = "tkstat",
    about = "vnstat-style monitor for Claude Code token usage",
    version,
    disable_help_flag = true,
    after_help = "Examples:\n  tkstat            Daily token usage (default, system-local dates)\n  tkstat --utc -d   Daily stats with UTC calendar buckets\n  tkstat -5         5-minute resolution\n  tkstat -h         Hourly statistics\n  tkstat -m         Monthly summary\n  tkstat -t 10      Top 10 days by usage\n  tkstat --model opus   Filter by model family alias\n  tkstat --model claude-sonnet-4-5-20250929   Filter by exact model id\n  tkstat --by-model     Group by exact model id\n  tkstat --by-provider  Group by provider\n  tkstat --by-project   Group by project\n  tkstat --budget       Budget consumption\n  tkstat --heatmap  GitHub-style usage calendar\n  tkstat --chart    Braille time-series chart\n  tkstat --json -d  Daily stats as JSON"
)]
pub struct Cli {
    /// Print help
    #[arg(long = "help", action = clap::ArgAction::Help)]
    pub help: Option<bool>,

    // -- Time granularity --
    /// Show 5-minute resolution statistics
    #[arg(short = '5', long = "fiveminutes")]
    pub fiveminutes: bool,

    /// Show hourly statistics
    #[arg(short = 'h', long = "hourly")]
    pub hourly: bool,

    /// Show daily statistics (default)
    #[arg(short = 'd', long = "daily")]
    pub daily: bool,

    /// Show monthly statistics
    #[arg(short = 'm', long = "monthly")]
    pub monthly: bool,

    /// Show yearly statistics
    #[arg(short = 'y', long = "yearly")]
    pub yearly: bool,

    /// Show top N days by usage
    #[arg(short = 't', long = "top", value_name = "N")]
    pub top: Option<Option<u32>>,

    /// Show short summary
    #[arg(short = 's', long = "summary")]
    pub summary: bool,

    /// Show budget consumption report
    #[arg(long = "budget")]
    pub budget: bool,

    // -- Chart modes --
    /// Show GitHub-style contribution heatmap
    #[arg(long = "heatmap")]
    pub heatmap: bool,

    /// Show braille-dot time series chart
    #[arg(long = "chart")]
    pub chart: bool,

    /// Metric for chart/heatmap
    #[arg(
        long = "chart-metric",
        value_name = "METRIC",
        value_enum,
        default_value = "cost"
    )]
    pub chart_metric: ChartMetric,

    // -- Filters --
    /// Filter by exact model id, or family alias (opus, sonnet, haiku)
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,

    /// Ingest/query provider (all, claude-code, codex; alias: claude)
    #[arg(
        long = "provider",
        value_name = "PROVIDER",
        value_enum,
        default_value = "all"
    )]
    pub provider: ProviderArg,

    /// Filter by model family (opus, sonnet, haiku)
    #[arg(long = "model-family", value_name = "FAMILY")]
    pub model_family: Option<String>,

    /// Group usage by exact model id
    #[arg(long = "by-model")]
    pub by_model: bool,

    /// Group usage by provider
    #[arg(long = "by-provider")]
    pub by_provider: bool,

    /// Group usage by project
    #[arg(long = "by-project")]
    pub by_project: bool,

    /// Filter by project name (substring match)
    #[arg(short = 'p', long = "project", value_name = "PROJECT")]
    pub project: Option<String>,

    /// Filter by session ID (prefix match)
    #[arg(long = "session", value_name = "SESSION_ID")]
    pub session: Option<String>,

    /// Begin system-local report date for filtering (YYYY-MM-DD); use --utc for UTC
    #[arg(short = 'b', long = "begin", value_name = "DATE")]
    pub begin: Option<NaiveDate>,

    /// End system-local report date for filtering (YYYY-MM-DD); use --utc for UTC
    #[arg(short = 'e', long = "end", value_name = "DATE")]
    pub end: Option<NaiveDate>,

    /// Use UTC calendar boundaries instead of the system local timezone
    #[arg(long = "utc")]
    pub utc: bool,

    /// Maximum number of rows to display
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<u32>,

    // -- Output format --
    /// Output as JSON
    #[arg(long = "json")]
    pub json: bool,

    /// Single-line output
    #[arg(long = "oneline")]
    pub oneline: bool,

    /// Output table-shaped reports as CSV
    #[arg(
        long = "csv",
        conflicts_with_all = [
            "json",
            "oneline",
            "chart",
            "heatmap",
            "summary",
            "doctor",
            "budget",
            "cost_explain",
            "pricing_seed",
            "pricing_refresh",
            "pricing_import",
            "pricing_audit"
        ]
    )]
    pub csv: bool,

    /// Inspect tkstat runtime state without ingesting or refreshing
    #[arg(long = "doctor")]
    pub doctor: bool,

    /// Audit local pricing coverage without ingesting or refreshing
    #[arg(long = "pricing-audit")]
    pub pricing_audit: bool,

    /// Explain cost confidence and pricing assumptions for the selected usage
    #[arg(
        long = "cost-explain",
        conflicts_with_all = [
            "csv",
            "oneline",
            "chart",
            "heatmap",
            "summary",
            "budget",
            "doctor",
            "pricing_seed",
            "pricing_refresh",
            "pricing_import",
            "pricing_audit",
            "by_model",
            "by_provider",
            "by_project"
        ]
    )]
    pub cost_explain: bool,

    /// Columns to display (comma-separated: input,output,cache_rd,cache_cr,cached_input,reasoning_output,total,cost,reqs,sessions)
    #[arg(long = "columns", value_name = "COLS")]
    pub columns: Option<String>,

    /// Disable colors (set NO_COLOR=1 env var instead)
    #[arg(long = "no-color")]
    pub no_color: bool,

    // -- Database --
    /// Force full re-ingestion
    #[arg(long = "force-update")]
    pub force_update: bool,

    /// Path to the SQLite database
    #[arg(long = "db", value_name = "PATH")]
    pub db_path: Option<String>,

    /// Path to Claude data directory
    #[arg(long = "data-dir", value_name = "PATH")]
    pub data_dir: Option<String>,

    /// Seed bundled pricing intervals into the local database and exit
    #[arg(long = "pricing-seed")]
    pub pricing_seed: bool,

    /// Fetch official provider pricing, update intervals, reprice usage, and exit
    #[arg(long = "pricing-refresh")]
    pub pricing_refresh: bool,

    /// Import a reviewed pricing catalog JSON file into the local database and exit
    #[arg(long = "pricing-import", value_name = "PATH")]
    pub pricing_import: Option<String>,

    /// Exclude subagent usage
    #[arg(long = "no-subagents")]
    pub no_subagents: bool,

    /// Warn when any daily filtered cost reaches this USD threshold
    #[arg(long = "daily-budget-usd", value_name = "AMOUNT")]
    pub daily_budget_usd: Option<f64>,

    /// Warn when any monthly filtered cost reaches this USD threshold
    #[arg(long = "monthly-budget-usd", value_name = "AMOUNT")]
    pub monthly_budget_usd: Option<f64>,
}

impl Cli {
    pub fn provider_label(&self) -> &'static str {
        match self.provider {
            ProviderArg::All => ALL_PROVIDERS_LABEL,
            ProviderArg::ClaudeCode => CLAUDE_CODE_PROVIDER,
            ProviderArg::Codex => CODEX_PROVIDER,
        }
    }

    /// Resolve the time period from CLI flags.
    pub fn period(&self) -> TimePeriod {
        if self.fiveminutes {
            TimePeriod::FiveMinutes
        } else if self.hourly {
            TimePeriod::Hourly
        } else if self.monthly {
            TimePeriod::Monthly
        } else if self.yearly {
            TimePeriod::Yearly
        } else {
            TimePeriod::Daily
        }
    }

    /// Resolve the output mode from CLI flags.
    pub fn output_mode(&self) -> OutputMode {
        let period = self.period();
        if self.cost_explain {
            OutputMode::CostExplain
        } else if self.budget {
            OutputMode::Budget
        } else if self.by_project {
            OutputMode::ByProject
        } else if self.by_provider {
            OutputMode::ByProvider
        } else if self.by_model {
            OutputMode::ByModel
        } else if self.heatmap {
            OutputMode::Heatmap
        } else if self.chart {
            OutputMode::Chart
        } else if self.summary {
            OutputMode::Summary
        } else if self.oneline {
            OutputMode::Oneline
        } else if self.json {
            if self.top.is_some() {
                OutputMode::TopDays
            } else {
                OutputMode::Json(period)
            }
        } else if self.top.is_some() {
            OutputMode::TopDays
        } else {
            OutputMode::Table(period)
        }
    }

    /// Resolve the effective row limit.
    pub fn effective_limit(&self) -> u32 {
        if let Some(limit) = self.limit {
            return limit;
        }
        if let Some(Some(n)) = self.top {
            return n;
        }
        if self.top.is_some() {
            return 10;
        }
        self.period().default_limit()
    }

    /// Build a query filter from CLI flags.
    pub fn query_filter(&self) -> QueryFilter {
        QueryFilter {
            begin: self.begin,
            end: self.end,
            report_timezone: if self.utc {
                ReportTimeZone::Utc
            } else {
                ReportTimeZone::Local
            },
            provider: self.provider.provider(),
            model: self.model.clone(),
            model_family: self.model_family.clone(),
            project: self.project.clone(),
            session: self.session.clone(),
            include_subagents: !self.no_subagents,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn test_by_model_flag_selects_model_report() {
        let cli = Cli::parse_from(["tkstat", "--by-model"]);
        assert!(matches!(cli.output_mode(), OutputMode::ByModel));
    }

    #[test]
    fn test_by_provider_flag_selects_provider_report() {
        let cli = Cli::parse_from(["tkstat", "--by-provider"]);
        assert!(matches!(cli.output_mode(), OutputMode::ByProvider));
    }

    #[test]
    fn test_by_project_flag_selects_project_report() {
        let cli = Cli::parse_from(["tkstat", "--by-project"]);
        assert!(matches!(cli.output_mode(), OutputMode::ByProject));
    }

    #[test]
    fn test_budget_flag_selects_budget_report() {
        let cli = Cli::parse_from(["tkstat", "--budget"]);
        assert!(matches!(cli.output_mode(), OutputMode::Budget));
    }

    #[test]
    fn test_cost_explain_flag_selects_cost_explain_report() {
        let cli = Cli::parse_from(["tkstat", "--cost-explain"]);
        assert!(matches!(cli.output_mode(), OutputMode::CostExplain));
    }

    #[test]
    fn test_model_and_model_family_flags_populate_filter() {
        let cli = Cli::parse_from([
            "tkstat",
            "--model",
            "claude-sonnet-4-5-20250929",
            "--model-family",
            "sonnet",
        ]);
        let filter = cli.query_filter();
        assert_eq!(filter.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(filter.model_family.as_deref(), Some("sonnet"));
    }

    #[test]
    fn test_provider_flag_parses_codex() {
        let cli = Cli::parse_from(["tkstat", "--provider", "codex"]);
        assert_eq!(cli.provider.provider(), Some(ProviderId::Codex));
        assert_eq!(cli.query_filter().provider, Some(ProviderId::Codex));
    }

    #[test]
    fn test_provider_flag_canonicalizes_claude_code_and_alias() {
        let canonical = Cli::parse_from(["tkstat", "--provider", "claude-code"]);
        assert_eq!(canonical.provider.provider(), Some(ProviderId::ClaudeCode));
        assert_eq!(
            canonical.query_filter().provider,
            Some(ProviderId::ClaudeCode)
        );
        assert_eq!(canonical.provider_label(), "claude-code");

        let alias = Cli::parse_from(["tkstat", "--provider", "claude"]);
        assert_eq!(alias.provider.provider(), Some(ProviderId::ClaudeCode));
        assert_eq!(alias.query_filter().provider, Some(ProviderId::ClaudeCode));
        assert_eq!(alias.provider_label(), "claude-code");
    }

    #[test]
    fn test_report_timezone_defaults_local_and_utc_flag_overrides() {
        let default = Cli::parse_from(["tkstat"]);
        assert_eq!(
            default.query_filter().report_timezone,
            ReportTimeZone::Local
        );

        let utc = Cli::parse_from(["tkstat", "--utc"]);
        assert_eq!(utc.query_filter().report_timezone, ReportTimeZone::Utc);
    }

    #[test]
    fn test_help_mentions_model_report_flags() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--by-model"));
        assert!(help.contains("--by-provider"));
        assert!(help.contains("--by-project"));
        assert!(help.contains("--model-family"));
        assert!(help.contains("--provider"));
        assert!(help.contains("--pricing-seed"));
        assert!(help.contains("--pricing-refresh"));
        assert!(help.contains("--pricing-import"));
        assert!(help.contains("--pricing-audit"));
        assert!(help.contains("--cost-explain"));
        assert!(help.contains("--doctor"));
        assert!(help.contains("--csv"));
        assert!(help.contains("--daily-budget-usd"));
        assert!(help.contains("--monthly-budget-usd"));
        assert!(help.contains("--budget"));
        assert!(help.contains("--utc"));
        assert!(help.contains("system-local report date"));
        assert!(help.contains("UTC calendar boundaries"));
        assert!(help.contains("tkstat --utc -d"));
        assert!(help.contains("exact model id"));
    }

    #[test]
    fn test_csv_conflicts_with_json_and_oneline() {
        assert!(Cli::try_parse_from(["tkstat", "--csv", "--json"]).is_err());
        assert!(Cli::try_parse_from(["tkstat", "--csv", "--oneline"]).is_err());
    }

    #[test]
    fn test_csv_conflicts_with_non_table_modes() {
        for flag in [
            "--chart",
            "--heatmap",
            "--summary",
            "--doctor",
            "--budget",
            "--pricing-seed",
            "--pricing-refresh",
            "--pricing-import",
            "--pricing-audit",
            "--cost-explain",
        ] {
            let args = if flag == "--pricing-import" {
                vec!["tkstat", "--csv", flag, "catalog.json"]
            } else {
                vec!["tkstat", "--csv", flag]
            };
            assert!(
                Cli::try_parse_from(args).is_err(),
                "--csv should conflict with {flag}"
            );
        }
    }

    #[test]
    fn test_csv_remains_available_for_table_modes() {
        assert!(Cli::try_parse_from(["tkstat", "--csv", "-d"]).is_ok());
        assert!(Cli::try_parse_from(["tkstat", "--csv", "-t", "5"]).is_ok());
        assert!(Cli::try_parse_from(["tkstat", "--csv", "--by-model"]).is_ok());
        assert!(Cli::try_parse_from(["tkstat", "--csv", "--by-provider"]).is_ok());
        assert!(Cli::try_parse_from(["tkstat", "--csv", "--by-project"]).is_ok());
    }
}
