use crate::domain::usage::{format_cost, format_tokens, AggregatedRow};

/// Render a single-line summary, semicolon-delimited (like vnstat --oneline).
/// Format: total_tokens;input;output;cache_rd;cache_wr;cost;requests;sessions
pub fn render_oneline(summary: &AggregatedRow) -> String {
    format!(
        "{};{};{};{};{};{};{};{}\n",
        format_tokens(summary.total_tokens),
        format_tokens(summary.input_tokens),
        format_tokens(summary.output_tokens),
        format_tokens(summary.cache_read_tokens),
        format_tokens(summary.cache_write_tokens),
        format_cost(summary.cost_usd),
        summary.request_count,
        summary.session_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oneline_format() {
        let row = AggregatedRow {
            total_tokens: 4700, input_tokens: 1000, output_tokens: 500,
            cache_write_tokens: 200, cache_read_tokens: 3000, cost_usd: 0.12,
            request_count: 3, session_count: 1, ..Default::default()
        };
        let line = render_oneline(&row);
        assert!(line.ends_with('\n'));
        let parts: Vec<&str> = line.trim().split(';').collect();
        assert_eq!(parts.len(), 8);
    }
}
