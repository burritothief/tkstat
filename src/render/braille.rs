use textplots::{Chart, Plot, Shape};

use crate::domain::usage::{format_cost, format_tokens};

const CHART_WIDTH: u32 = 120;
const CHART_HEIGHT: u32 = 20;

/// Render a bar chart from daily data using textplots.
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
    let shape = Shape::Bars(&points);
    let mut chart = Chart::new(CHART_WIDTH, CHART_HEIGHT, 0.0, x_max);
    let c = chart.lineplot(&shape);
    c.axis();
    c.figures();
    let chart_str = format!("{c}");

    out.push('\n');
    out.push_str(&chart_str);

    // Date labels below the x-axis
    if let (Some(first), Some(last)) = (daily_data.first(), daily_data.last()) {
        let chart_content_width = CHART_WIDTH as usize / 2;
        let padding = chart_content_width
            .saturating_sub(first.0.len())
            .saturating_sub(last.0.len());
        out.push_str(&format!("   {}{:padding$}{}\n", first.0, "", last.0));
    }

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
}
