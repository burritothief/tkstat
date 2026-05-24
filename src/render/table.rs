use crate::domain::usage::AggregatedRow;
use crate::render;
use crate::render::columns::Column;

const PERIOD_WIDTH: usize = 10;
const COL_WIDTH: usize = 8;

/// Render a vnstat-style ASCII table from aggregated rows.
pub fn render_table(
    provider_label: &str,
    period_name: &str,
    rows: &[AggregatedRow],
    columns: &[Column],
    filter_desc: Option<&str>,
) -> String {
    if rows.is_empty() {
        return format!(
            "{}{}",
            render::header(provider_label, period_name, filter_desc),
            " No data available.\n"
        );
    }

    let total = AggregatedRow::sum(rows);
    let col_widths = compute_col_widths(columns, rows, &total);
    let period_width = compute_period_width(rows);

    let mut out = render::header(provider_label, period_name, filter_desc);

    // Header row
    out.push_str(&format!(" {:period_width$}", ""));
    for (i, (col, &w)) in columns.iter().zip(&col_widths).enumerate() {
        if i > 0 {
            out.push_str(" |");
        }
        out.push_str(&format!("  {:>w$}", col.header()));
    }
    out.push('\n');

    let sep = build_separator(period_width, &col_widths);
    out.push_str(&sep);

    // Data rows — group by date prefix for sub-daily periods
    let mut last_date_group: Option<&str> = None;

    for row in rows {
        if let Some((date, time)) = split_period_label(&row.period) {
            if last_date_group != Some(date) {
                last_date_group = Some(date);
                out.push_str(&format!(" {date}\n"));
            }
            out.push_str(&format_data_row(
                time,
                row,
                columns,
                &col_widths,
                period_width,
            ));
        } else {
            out.push_str(&format_data_row(
                &row.period,
                row,
                columns,
                &col_widths,
                period_width,
            ));
        }
    }

    out.push_str(&sep);
    out.push_str(&format_data_row(
        "total",
        &total,
        columns,
        &col_widths,
        period_width,
    ));
    out
}

fn compute_period_width(rows: &[AggregatedRow]) -> usize {
    rows.iter()
        .map(|r| {
            split_period_label(&r.period)
                .map(|(_, time)| time.len())
                .unwrap_or(r.period.len())
        })
        .max()
        .unwrap_or(PERIOD_WIDTH)
        .max(PERIOD_WIDTH)
        .max("total".len())
}

fn compute_col_widths(
    columns: &[Column],
    rows: &[AggregatedRow],
    total: &AggregatedRow,
) -> Vec<usize> {
    columns
        .iter()
        .map(|col| {
            let header_w = col.header().len();
            let data_w = rows
                .iter()
                .map(|r| {
                    if r.request_count == 0 {
                        1
                    } else {
                        col.format_value(r).len()
                    }
                })
                .max()
                .unwrap_or(0);
            let total_w = col.format_value(total).len();
            COL_WIDTH.max(header_w).max(data_w).max(total_w)
        })
        .collect()
}

fn build_separator(period_width: usize, col_widths: &[usize]) -> String {
    let mut s = format!(" {:-<period_width$}", "");
    for (i, &w) in col_widths.iter().enumerate() {
        if i > 0 {
            s.push_str("-+");
        }
        s.push_str("--");
        for _ in 0..w {
            s.push('-');
        }
    }
    s.push('\n');
    s
}

fn format_data_row(
    label: &str,
    row: &AggregatedRow,
    columns: &[Column],
    col_widths: &[usize],
    period_width: usize,
) -> String {
    let is_empty = row.request_count == 0;
    let mut s = format!(" {:>period_width$}", label);
    for (i, (col, &w)) in columns.iter().zip(col_widths).enumerate() {
        if i > 0 {
            s.push_str(" |");
        }
        let val = if is_empty {
            "-".to_string()
        } else {
            col.format_value(row)
        };
        s.push_str(&format!("  {:>w$}", val));
    }
    s.push('\n');
    s
}

/// Split "2026-04-07 14:00" → Some(("2026-04-07", "14:00"))
fn split_period_label(label: &str) -> Option<(&str, &str)> {
    let idx = label.find(' ')?;
    let date = &label[..idx];
    let time = &label[idx + 1..];
    if date.len() == 10 && date.as_bytes()[4] == b'-' {
        Some((date, time))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::columns::default_columns;

    fn sample_rows() -> Vec<AggregatedRow> {
        vec![
            AggregatedRow {
                period: "2026-04-05".into(),
                input_tokens: 1200,
                output_tokens: 856,
                cache_creation_tokens: 4100,
                cache_read_tokens: 52300,
                total_tokens: 58456,
                cost_usd: 0.84,
                request_count: 5,
                session_count: 2,
                ..Default::default()
            },
            AggregatedRow {
                period: "2026-04-06".into(),
                input_tokens: 3400,
                output_tokens: 1200,
                cache_creation_tokens: 12300,
                cache_read_tokens: 128700,
                total_tokens: 145600,
                cost_usd: 2.11,
                request_count: 12,
                session_count: 3,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn test_render_table_has_header() {
        let output = render_table(
            "claude-code",
            "daily",
            &sample_rows(),
            &default_columns(),
            None,
        );
        assert!(output.contains("claude-code / daily"));
    }

    #[test]
    fn test_render_table_has_columns() {
        let output = render_table(
            "claude-code",
            "daily",
            &sample_rows(),
            &default_columns(),
            None,
        );
        for name in ["input", "output", "cache rd", "cache cr", "total", "cost"] {
            assert!(output.contains(name), "missing column: {name}");
        }
    }

    #[test]
    fn test_render_table_empty() {
        let output = render_table("claude-code", "daily", &[], &default_columns(), None);
        assert!(output.contains("No data"));
    }

    #[test]
    fn test_custom_columns() {
        let cols = vec![Column::Cost, Column::Requests, Column::Sessions];
        let output = render_table("claude-code", "daily", &sample_rows(), &cols, None);
        assert!(output.contains("cost"));
        assert!(output.contains("reqs"));
        assert!(!output.contains("  input"));
    }

    #[test]
    fn test_provider_specific_token_columns_render() {
        let rows = vec![AggregatedRow {
            period: "codex/gpt-5.5".into(),
            cached_input_tokens: 4480,
            reasoning_output_tokens: 70,
            request_count: 1,
            ..Default::default()
        }];
        let cols = vec![Column::CachedInput, Column::ReasoningOutput];
        let output = render_table("codex", "by model", &rows, &cols, None);
        assert!(output.contains("cached in"));
        assert!(output.contains("reason"));
        assert!(output.contains("4.5 K"));
        assert!(output.contains("70"));
    }

    #[test]
    fn test_hourly_groups_by_date() {
        let rows = vec![
            AggregatedRow {
                period: "2026-04-05 10:00".into(),
                request_count: 2,
                ..Default::default()
            },
            AggregatedRow {
                period: "2026-04-06 09:00".into(),
                request_count: 1,
                ..Default::default()
            },
        ];
        let output = render_table("claude-code", "hourly", &rows, &default_columns(), None);
        assert!(output.contains(" 2026-04-05\n"));
        assert!(output.contains(" 2026-04-06\n"));
    }

    #[test]
    fn test_empty_rows_show_dash() {
        let rows = vec![AggregatedRow {
            period: "2026-04-05 11:00".into(),
            request_count: 0,
            ..Default::default()
        }];
        let output = render_table("claude-code", "hourly", &rows, &default_columns(), None);
        let line = output.lines().find(|l| l.contains("11:00")).unwrap();
        assert!(line.contains("-"));
    }

    #[test]
    fn test_split_period_label() {
        assert_eq!(
            split_period_label("2026-04-07 14:00"),
            Some(("2026-04-07", "14:00"))
        );
        assert_eq!(split_period_label("2026-04-07"), None);
        assert_eq!(split_period_label("2026-04"), None);
    }

    #[test]
    fn test_all_lines_same_width() {
        let output = render_table(
            "claude-code",
            "daily",
            &sample_rows(),
            &default_columns(),
            None,
        );
        let data_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains('|') || l.contains('+'))
            .collect();
        assert!(!data_lines.is_empty());
        let expected = data_lines[0].len();
        for line in &data_lines {
            assert_eq!(
                line.len(),
                expected,
                "misaligned: {:?} (got {}, expected {})",
                line,
                line.len(),
                expected
            );
        }
    }

    #[test]
    fn test_hourly_total_aligns_with_time_labels() {
        let rows = vec![
            AggregatedRow {
                period: "2026-04-05 10:00".into(),
                input_tokens: 100,
                request_count: 1,
                ..Default::default()
            },
            AggregatedRow {
                period: "2026-04-05 11:00".into(),
                input_tokens: 200,
                request_count: 2,
                ..Default::default()
            },
        ];
        let cols = vec![Column::Input, Column::Cost];
        let output = render_table("claude-code", "hourly", &rows, &cols, None);
        let time_line = output.lines().find(|l| l.contains("10:00")).unwrap();
        // Last line with "total" is the totals row (not the column header)
        let total_line = output.lines().rev().find(|l| l.contains("total")).unwrap();
        // Both should have the same length (aligned columns)
        assert_eq!(time_line.len(), total_line.len());
        // "total" right edge should align with "10:00" right edge
        let time_pos = time_line.find("10:00").unwrap() + "10:00".len();
        let total_pos = total_line.find("total").unwrap() + "total".len();
        assert_eq!(
            time_pos, total_pos,
            "total label not aligned with time labels"
        );
    }

    #[test]
    fn test_columns_no_wider_than_needed() {
        let rows = vec![AggregatedRow {
            period: "2026-04".into(),
            input_tokens: 50,
            cost_usd: 0.01,
            request_count: 1,
            ..Default::default()
        }];
        let cols = vec![Column::Input, Column::Cost];
        let output = render_table("claude-code", "monthly", &rows, &cols, None);
        // With dynamic widths, the separator should be compact.
        // Period col = 7 ("2026-04"), input col = 5 ("input"), cost col = 5 ("$0.01").
        // Separator: " -------" + "--" + "-----" + "-+--" + "-----" = 26 chars
        let sep_line = output.lines().find(|l| l.contains("--")).unwrap();
        assert!(
            sep_line.len() < 40,
            "table is wider than needed: {} chars",
            sep_line.len()
        );
    }

    #[test]
    fn test_long_model_labels_fit_first_column() {
        let rows = vec![AggregatedRow {
            period: "claude/claude-sonnet-4-5-20250929".into(),
            input_tokens: 50,
            request_count: 1,
            ..Default::default()
        }];
        let output = render_table("all providers", "by model", &rows, &[Column::Input], None);
        assert!(output.contains("claude/claude-sonnet-4-5-20250929"));

        let data_line = output
            .lines()
            .find(|l| l.contains("claude-sonnet-4-5-20250929"))
            .unwrap();
        assert!(data_line.contains("  50"));
    }

    #[test]
    fn test_provider_label_is_rendered() {
        let output = render_table("codex", "daily", &sample_rows(), &default_columns(), None);
        assert!(output.contains("codex / daily"));
    }
}
