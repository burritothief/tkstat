use crate::domain::usage::{format_cost, format_tokens, AggregatedRow};

/// Render a short summary (like vnstat -s).
pub fn render_summary(summary: &AggregatedRow) -> String {
    format!(
        concat!(
            " claude / summary\n",
            "\n",
            "   Requests: {requests:>10}    Sessions: {sessions:>10}\n",
            "     Input:  {input:>10}    Output:   {output:>10}\n",
            "   Cache rd: {cache_rd:>10}    Cache wr: {cache_wr:>10}\n",
            "     Total:  {total:>10}    Cost:     {cost:>10}\n",
        ),
        requests = summary.request_count,
        sessions = summary.session_count,
        input = format_tokens(summary.input_tokens),
        output = format_tokens(summary.output_tokens),
        cache_rd = format_tokens(summary.cache_read_tokens),
        cache_wr = format_tokens(summary.cache_write_tokens),
        total = format_tokens(summary.total_tokens),
        cost = format_cost(summary.cost_usd),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summary_rendering() {
        let row = AggregatedRow {
            input_tokens: 50000, output_tokens: 12000, cache_write_tokens: 8000,
            cache_read_tokens: 200000, total_tokens: 270000, cost_usd: 5.42,
            request_count: 150, session_count: 12, ..Default::default()
        };
        let output = render_summary(&row);
        assert!(output.contains("150"));
        assert!(output.contains("$5.42"));
    }
}
