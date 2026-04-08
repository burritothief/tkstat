use crate::domain::usage::AggregatedRow;

/// Render aggregated rows as a JSON array.
pub fn render_json(rows: &[AggregatedRow]) -> String {
    // AggregatedRow has only primitive Serialize fields — serialization is infallible.
    serde_json::to_string_pretty(rows).expect("AggregatedRow serialization is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_output_valid() {
        let rows = vec![AggregatedRow {
            period: "2026-04-07".into(),
            input_tokens: 100,
            total_tokens: 470,
            cost_usd: 0.12,
            request_count: 3,
            ..Default::default()
        }];
        let json = render_json(&rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["total_tokens"], 470);
    }

    #[test]
    fn test_json_empty() {
        let json = render_json(&[]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }
}
