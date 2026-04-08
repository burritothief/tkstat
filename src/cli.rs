use chrono::NaiveDate;
use clap::{Parser, ValueEnum};

use crate::db::query::QueryFilter;
use crate::domain::period::TimePeriod;

/// Metric to use for chart/heatmap rendering.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ChartMetric {
    Tokens,
    Cost,
    Input,
    Output,
}

/// Resolved output mode from CLI flags.
pub enum OutputMode {
    Table(TimePeriod),
    TopDays,
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
    after_help = "Examples:\n  tkstat            Daily token usage (default)\n  tkstat -5         5-minute resolution\n  tkstat -h         Hourly statistics\n  tkstat -m         Monthly summary\n  tkstat -t 10      Top 10 days by usage\n  tkstat --model opus   Filter by model\n  tkstat --heatmap  GitHub-style usage calendar\n  tkstat --chart    Braille time-series chart\n  tkstat --json -d  Daily stats as JSON"
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

    // -- Chart modes --
    /// Show GitHub-style contribution heatmap
    #[arg(long = "heatmap")]
    pub heatmap: bool,

    /// Show braille-dot time series chart
    #[arg(long = "chart")]
    pub chart: bool,

    /// Metric for chart/heatmap
    #[arg(long = "chart-metric", value_name = "METRIC", value_enum, default_value = "tokens")]
    pub chart_metric: ChartMetric,

    // -- Filters --
    /// Filter by model family (opus, sonnet, haiku)
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,

    /// Filter by project name (substring match)
    #[arg(short = 'p', long = "project", value_name = "PROJECT")]
    pub project: Option<String>,

    /// Filter by session ID (prefix match)
    #[arg(long = "session", value_name = "SESSION_ID")]
    pub session: Option<String>,

    /// Begin date for filtering (YYYY-MM-DD)
    #[arg(short = 'b', long = "begin", value_name = "DATE")]
    pub begin: Option<NaiveDate>,

    /// End date for filtering (YYYY-MM-DD)
    #[arg(short = 'e', long = "end", value_name = "DATE")]
    pub end: Option<NaiveDate>,

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

    /// Columns to display (comma-separated: input,output,cache_rd,cache_cr,total,cost,reqs,sessions)
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

    /// Exclude subagent usage
    #[arg(long = "no-subagents")]
    pub no_subagents: bool,
}

impl Cli {
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
        if self.heatmap {
            OutputMode::Heatmap
        } else if self.chart {
            OutputMode::Chart
        } else if self.summary {
            OutputMode::Summary
        } else if self.oneline {
            OutputMode::Oneline
        } else if self.json {
            if self.top.is_some() { OutputMode::TopDays } else { OutputMode::Json(period) }
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
            model: self.model.clone(),
            project: self.project.clone(),
            session: self.session.clone(),
            include_subagents: !self.no_subagents,
        }
    }
}
