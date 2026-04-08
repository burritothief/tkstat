use textplots::{Chart, LabelBuilder, LabelFormat, Plot, Shape, TickDisplay, TickDisplayBuilder};

use crate::domain::usage::{format_cost, format_tokens};

const CHART_WIDTH: u32 = 120;
const CHART_HEIGHT: u32 = 20;

/// Render a line chart from daily data using textplots.
/// `daily_data` is (date_string, value) pairs in chronological order.
pub fn render_chart(daily_data: &[(String, f64)], metric_label: &str) -> String {
    if daily_data.is_empty() {
        return format!(" claude / chart ({metric_label})\n No data available.\n");
    }

    let mut out = format!(" claude / chart ({metric_label})\n");

    if daily_data.len() == 1 {
        out.push_str(&format!(
            "   {} : {}\n",
            daily_data[0].0,
            format_value(daily_data[0].1, metric_label),
        ));
        return out;
    }

    let points: Vec<(f32, f32)> = daily_data
        .iter()
        .enumerate()
        .map(|(i, (_, v))| (i as f32, *v as f32))
        .collect();

    let x_max = (points.len() - 1) as f32;
    let is_cost = metric_label == "cost";

    let y_formatter: Box<dyn Fn(f32) -> String> = if is_cost {
        Box::new(|v| format_cost(v as f64))
    } else {
        Box::new(|v| format_tokens(v as u64))
    };

    let shape = Shape::Lines(&points);
    let mut chart = Chart::new(CHART_WIDTH, CHART_HEIGHT, 0.0, x_max);
    let c = chart
        .x_label_format(LabelFormat::None)
        .y_label_format(LabelFormat::Custom(y_formatter))
        .y_tick_display(TickDisplay::Dense)
        .lineplot(&shape);
    c.axis();
    c.figures();
    let chart_str = format!("{c}");

    out.push('\n');
    out.push_str(&chart_str);

    // Date labels below the chart
    out.push_str(&format_date_axis(daily_data, CHART_WIDTH));

    // Summary line
    let values: Vec<f64> = daily_data.iter().map(|(_, v)| *v).collect();
    let total: f64 = values.iter().sum();
    let avg = total / values.len() as f64;
    let max_val = values.iter().cloned().fold(0.0f64, f64::max);

    out.push_str(&format!(
        "\n   avg: {}   max: {}   total: {}\n",
        format_value(avg, metric_label),
        format_value(max_val, metric_label),
        format_value(total, metric_label),
    ));

    out
}

/// Build a date axis label line with evenly spaced dates.
fn format_date_axis(daily_data: &[(String, f64)], chart_width: u32) -> String {
    let n = daily_data.len();
    // Each braille cell is 2 dots wide, so the chart area is chart_width/2 characters
    let axis_width = chart_width as usize / 2;

    // Pick ~5 tick positions evenly spaced across the data range
    let max_ticks = 5.min(n);
    let mut tick_indices: Vec<usize> = (0..max_ticks)
        .map(|i| i * (n - 1) / (max_ticks - 1))
        .collect();
    tick_indices.dedup();

    let labels: Vec<(&str, usize)> = tick_indices
        .iter()
        .map(|&idx| {
            let date = daily_data[idx].0.as_str();
            // Map data index to character position on the axis
            let pos = if n <= 1 {
                0
            } else {
                idx * axis_width / (n - 1)
            };
            (date, pos)
        })
        .collect();

    // Place labels left-to-right, skipping any that would overlap.
    // Reserve space for the last label so it always appears.
    let mut line = vec![b' '; axis_width];
    let last = labels.last().copied();
    let last_start = last.map(|(l, p)| p.min(axis_width.saturating_sub(l.len())));

    let mut cursor = 0usize;
    for (i, (label, pos)) in labels.iter().enumerate() {
        let start = (*pos).min(axis_width.saturating_sub(label.len()));
        if start < cursor {
            continue;
        }
        // Skip non-last labels that would collide with the reserved last label
        if i < labels.len() - 1
            && let Some(ls) = last_start
            && start + label.len() >= ls
        {
            continue;
        }
        let end = (start + label.len()).min(axis_width);
        for (j, &b) in label.as_bytes()[..end - start].iter().enumerate() {
            line[start + j] = b;
        }
        cursor = end + 1;
    }
    // Always place the last label
    if let Some((label, _)) = last {
        let start = last_start.unwrap();
        let end = (start + label.len()).min(axis_width);
        for (j, &b) in label.as_bytes()[..end - start].iter().enumerate() {
            line[start + j] = b;
        }
    }

    let trimmed = String::from_utf8_lossy(&line).trim_end().to_string();
    format!("{trimmed}\n")
}

fn format_value(val: f64, metric: &str) -> String {
    if metric == "cost" {
        format_cost(val)
    } else {
        format_tokens(val as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chart_empty() {
        let output = render_chart(&[], "tokens");
        assert!(output.contains("No data"));
    }

    #[test]
    fn test_chart_single_point() {
        let data = vec![("2026-04-07".into(), 1000.0)];
        let output = render_chart(&data, "tokens");
        assert!(output.contains("2026-04-07"));
    }

    #[test]
    fn test_chart_renders_visible_dots() {
        let data: Vec<(String, f64)> = (1..=10)
            .map(|i| (format!("2026-04-{i:02}"), i as f64 * 100.0))
            .collect();
        let output = render_chart(&data, "tokens");
        let has_dots = output.chars().any(|c| {
            let u = c as u32;
            u >= 0x2801 && u <= 0x28FF
        });
        assert!(has_dots, "Expected visible braille characters in chart");
    }

    #[test]
    fn test_chart_has_labels() {
        let data: Vec<(String, f64)> = (1..=10)
            .map(|i| (format!("2026-04-{i:02}"), i as f64 * 100.0))
            .collect();
        let output = render_chart(&data, "tokens");
        assert!(output.contains("claude / chart"));
        assert!(output.contains("avg:"));
        assert!(output.contains("max:"));
        assert!(output.contains("total:"));
        assert!(output.contains("2026-04-01"));
        assert!(output.contains("2026-04-10"));
    }

    #[test]
    fn test_chart_cost_metric() {
        let data = vec![
            ("2026-04-01".into(), 1.5),
            ("2026-04-02".into(), 2.3),
            ("2026-04-03".into(), 0.8),
        ];
        let output = render_chart(&data, "cost");
        assert!(output.contains("$"));
    }

    #[test]
    fn test_chart_all_zeros() {
        let data = vec![
            ("2026-04-01".into(), 0.0),
            ("2026-04-02".into(), 0.0),
            ("2026-04-03".into(), 0.0),
        ];
        let output = render_chart(&data, "tokens");
        assert!(output.contains("claude / chart"));
    }

    #[test]
    fn test_date_axis_intermediate_dates() {
        let data: Vec<(String, f64)> = (1..=30)
            .map(|i| (format!("2026-04-{i:02}"), i as f64))
            .collect();
        let axis = format_date_axis(&data, 120);
        // Should have first, last, and some intermediate dates
        assert!(axis.contains("2026-04-01"));
        assert!(axis.contains("2026-04-30"));
        // At least one intermediate date
        let date_count = (1..=30)
            .filter(|&i| axis.contains(&format!("2026-04-{i:02}")))
            .count();
        assert!(
            date_count >= 3,
            "expected at least 3 date labels, got {date_count}"
        );
    }

    #[test]
    fn test_chart_y_axis_cost_formatting() {
        let data = vec![
            ("2026-04-01".into(), 50.0),
            ("2026-04-02".into(), 100.0),
            ("2026-04-03".into(), 150.0),
        ];
        let output = render_chart(&data, "cost");
        // Y-axis labels should use $ formatting
        assert!(output.contains("$"), "y-axis should show $ for cost metric");
    }

    #[test]
    fn test_chart_no_numeric_x_axis() {
        let data: Vec<(String, f64)> = (1..=10)
            .map(|i| (format!("2026-04-{i:02}"), i as f64 * 100.0))
            .collect();
        let output = render_chart(&data, "tokens");
        // Should not contain the numeric x-axis range like "0.0" or "9.0"
        assert!(
            !output.contains("9.0"),
            "should not show numeric x-axis labels"
        );
    }
}
