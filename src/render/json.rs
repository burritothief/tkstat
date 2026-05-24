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

    #[test]
    fn test_json_includes_model_group_metadata_when_present() {
        let rows = vec![AggregatedRow {
            period: "claude/claude-sonnet-4-5-20250929".into(),
            provider: Some("claude".into()),
            model_id: Some("claude-sonnet-4-5-20250929".into()),
            request_count: 1,
            ..Default::default()
        }];
        let json = render_json(&rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["provider"], "claude");
        assert_eq!(parsed[0]["model_id"], "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn test_json_includes_provider_group_metadata_when_present() {
        let rows = vec![AggregatedRow {
            period: "codex".into(),
            provider: Some("codex".into()),
            request_count: 2,
            ..Default::default()
        }];
        let json = render_json(&rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["period"], "codex");
        assert_eq!(parsed[0]["provider"], "codex");
    }

    #[test]
    fn test_json_includes_project_group_metadata_when_present() {
        let rows = vec![AggregatedRow {
            period: "my-project".into(),
            project: Some("my-project".into()),
            request_count: 2,
            ..Default::default()
        }];
        let json = render_json(&rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["period"], "my-project");
        assert_eq!(parsed[0]["project"], "my-project");
    }
}
