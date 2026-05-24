use serde::Serialize;

use crate::domain::usage::{AggregatedRow, format_cost};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPeriod {
    Daily,
    Monthly,
}

impl BudgetPeriod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Monthly => "monthly",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetWarning {
    pub period_kind: BudgetPeriod,
    pub period: String,
    pub actual_usd: f64,
    pub threshold_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetReportRow {
    pub label: String,
    pub begin: String,
    pub end: String,
    pub cost_usd: f64,
    pub threshold_usd: Option<f64>,
    pub percent_used: Option<f64>,
    pub remaining_usd: Option<f64>,
    pub top_provider: Option<String>,
    pub top_model_id: Option<String>,
    pub top_project: Option<String>,
}

impl BudgetReportRow {
    pub fn new(
        label: impl Into<String>,
        begin: impl Into<String>,
        end: impl Into<String>,
        cost_usd: f64,
        threshold_usd: Option<f64>,
    ) -> Self {
        let percent_used = threshold_usd.and_then(|threshold| {
            if threshold > 0.0 {
                Some((cost_usd / threshold) * 100.0)
            } else {
                None
            }
        });
        let remaining_usd = threshold_usd.map(|threshold| threshold - cost_usd);
        Self {
            label: label.into(),
            begin: begin.into(),
            end: end.into(),
            cost_usd,
            threshold_usd,
            percent_used,
            remaining_usd,
            top_provider: None,
            top_model_id: None,
            top_project: None,
        }
    }
}

impl BudgetWarning {
    pub fn message(&self, filter_desc: Option<&str>) -> String {
        let filter = filter_desc
            .map(|desc| format!(" ({desc})"))
            .unwrap_or_default();
        format!(
            "budget warning: {} {} cost {} reached threshold {}{}",
            self.period_kind.label(),
            self.period,
            format_cost(self.actual_usd),
            format_cost(self.threshold_usd),
            filter
        )
    }
}

pub fn evaluate_budget_rows(
    period_kind: BudgetPeriod,
    threshold_usd: Option<f64>,
    rows: &[AggregatedRow],
) -> Vec<BudgetWarning> {
    let Some(threshold_usd) = threshold_usd else {
        return Vec::new();
    };
    if !threshold_usd.is_finite() || threshold_usd < 0.0 {
        return Vec::new();
    }

    rows.iter()
        .filter(|row| row.cost_usd >= threshold_usd)
        .map(|row| BudgetWarning {
            period_kind,
            period: row.period.clone(),
            actual_usd: row.cost_usd,
            threshold_usd,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(period: &str, cost_usd: f64) -> AggregatedRow {
        AggregatedRow {
            period: period.into(),
            cost_usd,
            ..Default::default()
        }
    }

    #[test]
    fn test_budget_below_threshold_has_no_warning() {
        let warnings = evaluate_budget_rows(BudgetPeriod::Daily, Some(10.0), &[row("today", 9.99)]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_budget_exactly_at_threshold_warns() {
        let warnings = evaluate_budget_rows(BudgetPeriod::Daily, Some(10.0), &[row("today", 10.0)]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].period, "today");
    }

    #[test]
    fn test_budget_over_threshold_warns() {
        let warnings =
            evaluate_budget_rows(BudgetPeriod::Monthly, Some(10.0), &[row("2026-04", 12.0)]);
        assert_eq!(warnings[0].period_kind, BudgetPeriod::Monthly);
        assert_eq!(warnings[0].actual_usd, 12.0);
    }

    #[test]
    fn test_budget_no_threshold_or_no_cost_data_has_no_warning() {
        assert!(evaluate_budget_rows(BudgetPeriod::Daily, None, &[row("today", 12.0)]).is_empty());
        assert!(evaluate_budget_rows(BudgetPeriod::Daily, Some(1.0), &[]).is_empty());
    }

    #[test]
    fn test_budget_message_includes_period_cost_threshold_and_filters() {
        let warning = BudgetWarning {
            period_kind: BudgetPeriod::Daily,
            period: "2026-04-07".into(),
            actual_usd: 2.0,
            threshold_usd: 1.0,
        };
        let message = warning.message(Some("provider: codex"));
        assert!(message.contains("daily 2026-04-07"));
        assert!(message.contains("$2.00"));
        assert!(message.contains("$1.00"));
        assert!(message.contains("provider: codex"));
    }

    #[test]
    fn test_budget_report_row_calculates_percent_and_remaining() {
        let row = BudgetReportRow::new("today", "2026-04-07", "2026-04-07", 25.0, Some(100.0));
        assert_eq!(row.percent_used, Some(25.0));
        assert_eq!(row.remaining_usd, Some(75.0));
    }

    #[test]
    fn test_budget_report_row_handles_missing_and_zero_budget() {
        let missing = BudgetReportRow::new("today", "2026-04-07", "2026-04-07", 25.0, None);
        assert_eq!(missing.percent_used, None);
        assert_eq!(missing.remaining_usd, None);

        let zero = BudgetReportRow::new("today", "2026-04-07", "2026-04-07", 25.0, Some(0.0));
        assert_eq!(zero.percent_used, None);
        assert_eq!(zero.remaining_usd, Some(-25.0));
    }
}
