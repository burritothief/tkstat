use crate::domain::usage::AggregatedRow;
use crate::render;
use crate::render::columns::Column;

const NUMERIC_COL_WIDTH: usize = 11;
const PERIOD_COL_WIDTH: usize = 16;

/// Render a vnstat-style ASCII table from aggregated rows.
pub fn render_table(
    period_name: &str,
    rows: &[AggregatedRow],
    columns: &[Column],
    filter_desc: Option<&str>,
) -> String {
    if rows.is_empty() {
        return format!(
            "{}{}",
            render::header(period_name, filter_desc),
            " No data available.\n"
        );
    }

    let total = AggregatedRow::sum(rows);
    let mut out = render::header(period_name, filter_desc);

    // Header row
    out.push_str(&format!(" {:>PERIOD_COL_WIDTH$}", ""));
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push_str(" |");
        }
        out.push_str(&format!("  {:>NUMERIC_COL_WIDTH$}", col.header()));
    }
    out.push('\n');

    let sep = build_separator(columns);
    out.push_str(&sep);

    // Data rows — group by date prefix for sub-daily periods
    let mut last_date_group: Option<&str> = None;

    for row in rows {
        if let Some((date, time)) = split_period_label(&row.period) {
            if last_date_group != Some(date) {
                last_date_group = Some(date);
                out.push_str(&format!(" {date}\n"));
            }
            // Indent time labels to half the period column width
            let indent = PERIOD_COL_WIDTH / 2;
            out.push_str(&format_row_width(time, row, columns, indent));
        } else {
            out.push_str(&format_row(row.period.as_str(), row, columns));
        }
    }

    out.push_str(&sep);
    out.push_str(&format_row("total", &total, columns));
    out
}

fn build_separator(columns: &[Column]) -> String {
    let mut s = format!(" {:-<PERIOD_COL_WIDTH$}", "");
    for i in 0..columns.len() {
        if i > 0 {
            s.push_str("-+--");
        } else {
            s.push_str("--");
        }
        s.push_str(&format!("{:-<NUMERIC_COL_WIDTH$}", ""));
    }
    s.push('\n');
    s
}

fn format_row(label: &str, row: &AggregatedRow, columns: &[Column]) -> String {
    format_row_width(label, row, columns, PERIOD_COL_WIDTH)
}

fn format_row_width(
    label: &str,
    row: &AggregatedRow,
    columns: &[Column],
    label_width: usize,
) -> String {
    let is_empty = row.request_count == 0;
    // Pad to align with the full PERIOD_COL_WIDTH
    let padding = PERIOD_COL_WIDTH - label_width;
    let mut s = format!(" {:>label_width$}{:padding$}", label, "");
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            s.push_str(" |");
        }
        let val = if is_empty {
            "-".to_string()
        } else {
            col.format_value(row)
        };
        s.push_str(&format!("  {:>NUMERIC_COL_WIDTH$}", val));
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
            },
        ]
    }

    #[test]
    fn test_render_table_has_header() {
        let output = render_table("daily", &sample_rows(), &default_columns(), None);
        assert!(output.contains("claude / daily"));
    }

    #[test]
    fn test_render_table_has_columns() {
        let output = render_table("daily", &sample_rows(), &default_columns(), None);
        for name in ["input", "output", "cache rd", "cache cr", "total", "cost"] {
            assert!(output.contains(name), "missing column: {name}");
        }
    }

    #[test]
    fn test_render_table_empty() {
        let output = render_table("daily", &[], &default_columns(), None);
        assert!(output.contains("No data"));
    }

    #[test]
    fn test_custom_columns() {
        let cols = vec![Column::Cost, Column::Requests, Column::Sessions];
        let output = render_table("daily", &sample_rows(), &cols, None);
        assert!(output.contains("cost"));
        assert!(output.contains("reqs"));
        assert!(!output.contains("  input"));
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
        let output = render_table("hourly", &rows, &default_columns(), None);
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
        let output = render_table("hourly", &rows, &default_columns(), None);
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
}
