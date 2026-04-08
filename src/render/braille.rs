use drawille::Canvas;

use crate::domain::usage::{format_cost, format_tokens};

/// Chart dimensions in braille cells.
const CHART_WIDTH: u32 = 60;
const CHART_HEIGHT: u32 = 15;

/// Render a braille-dot time series chart from daily data.
/// `daily_data` is (date_string, value) pairs in chronological order.
pub fn render_braille(daily_data: &[(String, f64)], metric_label: &str) -> String {
    if daily_data.is_empty() {
        return format!(" claude / chart ({metric_label})\n No data available.\n");
    }

    let mut out = format!(" claude / chart ({metric_label})\n\n");

    if daily_data.len() < 2 {
        out.push_str(&format!(
            "   {} : {}\n",
            daily_data[0].0,
            format_value(daily_data[0].1, metric_label),
        ));
        return out;
    }

    let values: Vec<f64> = daily_data.iter().map(|(_, v)| *v).collect();
    let y_max = values.iter().cloned().fold(0.0f64, f64::max);
    let y_max = if y_max == 0.0 { 1.0 } else { y_max };

    // Braille canvas: each cell is 2 wide x 4 tall in dots
    let dot_w = CHART_WIDTH * 2;
    let dot_h = CHART_HEIGHT * 4;

    let mut canvas = Canvas::new(dot_w, dot_h);

    // Draw vertical bars for each data point
    let n = values.len();
    let slot_w = dot_w as f64 / n as f64;
    let bar_w = (slot_w * 0.7).max(1.0) as u32;

    for (i, &val) in values.iter().enumerate() {
        let bar_h = ((val / y_max) * (dot_h - 1) as f64).round() as u32;
        let x_start = (i as f64 * slot_w + (slot_w - bar_w as f64) / 2.0).round() as u32;
        for x in x_start..x_start + bar_w {
            for y in 0..=bar_h {
                canvas.set(x, y);
            }
        }
    }

    // Render canvas to string — drawille uses bottom-left origin,
    // which matches our y-axis (0 at bottom)
    let chart_str = canvas.frame();

    // Add y-axis labels
    let lines: Vec<&str> = chart_str.lines().collect();
    let y_label_top = format_value(y_max, metric_label);
    let y_label_mid = format_value(y_max / 2.0, metric_label);
    let y_label_bot = format_value(0.0, metric_label);

    for (i, line) in lines.iter().enumerate() {
        let label = if i == 0 {
            y_label_top.clone()
        } else if i == lines.len() / 2 {
            y_label_mid.clone()
        } else if i == lines.len() - 1 {
            y_label_bot.clone()
        } else {
            String::new()
        };
        out.push_str(&format!("   {:>8} {}\n", label, line));
    }

    // X-axis labels
    if let (Some(first), Some(last)) = (daily_data.first(), daily_data.last()) {
        out.push_str(&format!("            {}", first.0,));
        let padding = (CHART_WIDTH as usize * 2)
            .saturating_sub(first.0.len())
            .saturating_sub(last.0.len());
        for _ in 0..padding {
            out.push(' ');
        }
        out.push_str(&format!("{}\n", last.0));
    }

    // Summary line
    let total: f64 = values.iter().sum();
    let avg = total / values.len() as f64;
    let max_val = y_max;

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
    fn test_braille_empty() {
        let output = render_braille(&[], "tokens");
        assert!(output.contains("No data"));
    }

    #[test]
    fn test_braille_single_point() {
        let data = vec![("2026-04-07".into(), 1000.0)];
        let output = render_braille(&data, "tokens");
        assert!(output.contains("2026-04-07"));
    }

    #[test]
    fn test_braille_renders_visible_dots() {
        let data: Vec<(String, f64)> = (1..=10)
            .map(|i| (format!("2026-04-{i:02}"), i as f64 * 100.0))
            .collect();
        let output = render_braille(&data, "tokens");
        // Check for non-blank braille characters
        let has_dots = output.chars().any(|c| {
            let u = c as u32;
            u >= 0x2801 && u <= 0x28FF
        });
        assert!(has_dots, "Expected visible braille characters in chart");
    }

    #[test]
    fn test_braille_has_labels() {
        let data: Vec<(String, f64)> = (1..=10)
            .map(|i| (format!("2026-04-{i:02}"), i as f64 * 100.0))
            .collect();
        let output = render_braille(&data, "tokens");
        assert!(output.contains("claude / chart"));
        assert!(output.contains("avg:"));
        assert!(output.contains("max:"));
        assert!(output.contains("total:"));
        assert!(output.contains("2026-04-01"));
        assert!(output.contains("2026-04-10"));
    }

    #[test]
    fn test_braille_cost_metric() {
        let data = vec![
            ("2026-04-01".into(), 1.5),
            ("2026-04-02".into(), 2.3),
            ("2026-04-03".into(), 0.8),
        ];
        let output = render_braille(&data, "cost");
        assert!(output.contains("$"));
    }

    #[test]
    fn test_braille_all_zeros() {
        let data = vec![
            ("2026-04-01".into(), 0.0),
            ("2026-04-02".into(), 0.0),
            ("2026-04-03".into(), 0.0),
        ];
        let output = render_braille(&data, "tokens");
        // Should not panic or produce garbage
        assert!(output.contains("claude / chart"));
    }
}
