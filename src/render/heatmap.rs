use std::collections::HashMap;

use chrono::{Datelike, Local, NaiveDate, TimeDelta, Weekday};
use owo_colors::OwoColorize;

const BLOCK: &str = "█";
const EMPTY: &str = "·";

/// Blue range from the Vega/D3 blues spectrum.
const BLUES: [(u8, u8, u8); 6] = [
    (198, 219, 239), // #c6dbef
    (158, 202, 225), // #9ecae1
    (107, 174, 214), // #6baed6
    (66, 146, 198),  // #4292c6
    (33, 113, 181),  // #2171b5
    (8, 81, 156),    // #08519c
];

fn interpolate_color(t: f64) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let n = BLUES.len() - 1;
    let scaled = t * n as f64;
    let idx = (scaled as usize).min(n - 1);
    let frac = scaled - idx as f64;

    let lerp = |a: u8, b: u8, f: f64| -> u8 { (a as f64 + (b as f64 - a as f64) * f) as u8 };
    let (r0, g0, b0) = BLUES[idx];
    let (r1, g1, b1) = BLUES[idx + 1];
    (lerp(r0, r1, frac), lerp(g0, g1, frac), lerp(b0, b1, frac))
}

/// Format a colored block or fall back to a plain block if NO_COLOR is set.
fn colored_block(r: u8, g: u8, b: u8) -> String {
    if std::env::var_os("NO_COLOR").is_some() {
        BLOCK.to_string()
    } else {
        format!("{}", BLOCK.truecolor(r, g, b))
    }
}

/// Render a contribution heatmap from daily data.
pub fn render_heatmap(daily_data: &[(String, f64)], metric_label: &str) -> String {
    if daily_data.is_empty() {
        return format!(" claude / heatmap ({metric_label})\n No data available.\n");
    }

    let mut by_date: HashMap<NaiveDate, f64> = HashMap::new();
    for (date_str, value) in daily_data {
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            by_date.insert(date, *value);
        }
    }

    if by_date.is_empty() {
        return format!(" claude / heatmap ({metric_label})\n No data available.\n");
    }

    let today = Local::now().date_naive();
    let Some(start_month) = NaiveDate::from_ymd_opt(today.year() - 1, today.month(), 1) else {
        return format!(" claude / heatmap ({metric_label})\n No data available.\n");
    };
    let end_month = if today.month() == 12 {
        NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
    };
    let Some(end_month) = end_month.map(|d| d - TimeDelta::days(1)) else {
        return format!(" claude / heatmap ({metric_label})\n No data available.\n");
    };

    let start = start_month - TimeDelta::days(start_month.weekday().num_days_from_sunday() as i64);
    let end = end_month + TimeDelta::days(6 - end_month.weekday().num_days_from_sunday() as i64);

    let max_val = by_date.values().cloned().fold(0.0f64, f64::max);
    let log_max = if max_val > 1.0 { max_val.ln() } else { 1.0 };

    let label_pad = "     ";
    let mut out = format!(" claude / heatmap ({metric_label})\n\n");

    // Month labels
    out.push_str(label_pad);
    let mut current = start;
    let mut last_month_year: Option<(u32, i32)> = None;
    let mut skip = 0usize;
    while current <= end {
        let m = current.month();
        let y = current.year();
        if skip > 0 {
            skip -= 1;
        } else if last_month_year != Some((m, y)) && current >= start_month {
            let abbrev = month_abbrev(m);
            out.push_str(abbrev);
            skip = abbrev.len() - 1;
            last_month_year = Some((m, y));
        } else {
            out.push(' ');
        }
        current += TimeDelta::days(7);
    }
    out.push('\n');

    let row_order = [
        (Weekday::Sun, "    "),
        (Weekday::Mon, " Mon"),
        (Weekday::Tue, "    "),
        (Weekday::Wed, " Wed"),
        (Weekday::Thu, "    "),
        (Weekday::Fri, " Fri"),
        (Weekday::Sat, "    "),
    ];

    for (weekday, label) in &row_order {
        out.push_str(label);
        out.push(' ');

        let dow_offset = weekday.num_days_from_sunday() as i64;
        let mut day = start + TimeDelta::days(dow_offset);
        while day <= end {
            let val = by_date.get(&day).copied().unwrap_or(0.0);
            if val <= 0.0 {
                out.push_str(EMPTY);
            } else {
                let t = if val >= max_val { 1.0 } else { val.ln() / log_max };
                let (r, g, b) = interpolate_color(t.max(0.0));
                out.push_str(&colored_block(r, g, b));
            }
            day += TimeDelta::days(7);
        }
        out.push('\n');
    }

    out.push_str(&format!("\n{label_pad}Less {EMPTY}"));
    for i in 0..8 {
        let t = i as f64 / 7.0;
        let (r, g, b) = interpolate_color(t);
        out.push_str(&colored_block(r, g, b));
    }
    out.push_str(" More\n");

    out
}

fn month_abbrev(month: u32) -> &'static str {
    match month {
        1 => "Jan", 2 => "Feb", 3 => "Mar", 4 => "Apr",
        5 => "May", 6 => "Jun", 7 => "Jul", 8 => "Aug",
        9 => "Sep", 10 => "Oct", 11 => "Nov", 12 => "Dec",
        _ => "???",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heatmap_empty() {
        let output = render_heatmap(&[], "tokens");
        assert!(output.contains("No data"));
    }

    #[test]
    fn test_heatmap_single_day() {
        let data = vec![("2026-04-07".into(), 1000.0)];
        let output = render_heatmap(&data, "tokens");
        assert!(output.contains("claude / heatmap"));
        assert!(output.contains("Apr"));
    }

    #[test]
    fn test_heatmap_multiple_days() {
        let data = vec![
            ("2026-04-01".into(), 100.0),
            ("2026-04-02".into(), 500.0),
            ("2026-04-03".into(), 1000.0),
            ("2026-04-04".into(), 2000.0),
            ("2026-04-05".into(), 50.0),
        ];
        let output = render_heatmap(&data, "tokens");
        assert!(output.contains("Mon"));
        assert!(output.contains("Fri"));
        assert!(output.contains("Less"));
        assert!(output.contains("More"));
    }

    #[test]
    fn test_heatmap_shows_mon_wed_fri_labels() {
        let data = vec![("2026-04-07".into(), 100.0)];
        let output = render_heatmap(&data, "cost");
        assert!(output.contains("Mon"));
        assert!(output.contains("Wed"));
        assert!(output.contains("Fri"));
        assert!(!output.contains("Tue"));
        assert!(!output.contains("Thu"));
    }

    #[test]
    fn test_heatmap_has_7_data_rows() {
        let data = vec![("2026-04-01".into(), 100.0), ("2026-04-07".into(), 200.0)];
        let output = render_heatmap(&data, "tokens");
        let lines: Vec<&str> = output.lines().collect();
        let first_data = lines.iter().position(|l| l.contains("Mon") || l.contains(EMPTY)).unwrap_or(0);
        let legend = lines.iter().position(|l| l.contains("Less")).unwrap_or(lines.len());
        let data_rows = lines[first_data..legend].iter().filter(|l| !l.is_empty()).count();
        assert_eq!(data_rows, 7, "should have 7 rows (Sun-Sat), got {data_rows}");
    }

    #[test]
    fn test_heatmap_sunday_first_saturday_last() {
        let data = vec![("2026-04-05".into(), 100.0)];
        let output = render_heatmap(&data, "tokens");
        let lines: Vec<&str> = output.lines().collect();
        let mon_idx = lines.iter().position(|l| l.contains(" Mon ")).unwrap();
        assert!(mon_idx > 0);
        assert!(!lines[mon_idx - 1].contains("Mon"));
    }

    #[test]
    fn test_interpolate_color_endpoints() {
        assert_eq!(interpolate_color(0.0), BLUES[0]);
        assert_eq!(interpolate_color(1.0), *BLUES.last().unwrap());
    }

    #[test]
    fn test_interpolate_color_clamps() {
        assert_eq!(interpolate_color(-1.0), interpolate_color(0.0));
        assert_eq!(interpolate_color(2.0), interpolate_color(1.0));
    }

    #[test]
    fn test_interpolate_color_midpoint() {
        let (r, g, b) = interpolate_color(0.5);
        // Should be in the middle of the blues spectrum
        assert!(r < 150 && g > 100 && b > 180);
    }
}
