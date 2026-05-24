use crate::budget::BudgetReportRow;
use crate::domain::usage::format_cost;

pub fn render_budget_report(provider_label: &str, rows: &[BudgetReportRow]) -> String {
    let mut out = format!(" {provider_label} / budget\n");
    out.push_str(
        " period           range                     cost      budget     used    remaining  top\n",
    );
    out.push_str(" ---------------- ------------------------- --------- ---------- ------- ---------- ----------------\n");
    if rows.is_empty() {
        return out;
    }
    for row in rows {
        out.push_str(&format!(
            " {:<16} {:<25} {:>9} {:>10} {:>7} {:>10} {}\n",
            row.label,
            format!("{}..{}", row.begin, row.end),
            format_cost(row.cost_usd),
            row.threshold_usd
                .map(format_cost)
                .unwrap_or_else(|| "-".into()),
            row.percent_used
                .map(|value| format!("{value:.0}%"))
                .unwrap_or_else(|| "-".into()),
            row.remaining_usd
                .map(format_cost)
                .unwrap_or_else(|| "-".into()),
            top_label(row)
        ));
    }
    out
}

fn top_label(row: &BudgetReportRow) -> String {
    [
        row.top_provider.as_deref(),
        row.top_model_id.as_deref(),
        row.top_project.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_budget_report_includes_cost_budget_and_top() {
        let mut row =
            BudgetReportRow::new("selected", "2026-04-01", "2026-04-30", 25.0, Some(100.0));
        row.top_provider = Some("codex".into());
        row.top_model_id = Some("gpt-5.5".into());
        row.top_project = Some("tkstat".into());
        let output = render_budget_report("all providers", &[row]);
        assert!(output.contains("all providers / budget"));
        assert!(output.contains("$25.0"));
        assert!(output.contains("$100"));
        assert!(output.contains("25%"));
        assert!(output.contains("codex / gpt-5.5 / tkstat"));
    }
}
