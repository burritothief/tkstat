use crate::domain::usage::AggregatedRow;
use crate::render::columns::Column;

pub fn render_csv(rows: &[AggregatedRow], columns: &[Column]) -> String {
    let include_provider = rows.iter().any(|row| row.provider.is_some());
    let include_model = rows.iter().any(|row| row.model_id.is_some());
    let include_project = rows.iter().any(|row| row.project.is_some());

    let mut headers = vec!["period".to_string()];
    if include_provider {
        headers.push("provider".into());
    }
    if include_model {
        headers.push("model_id".into());
    }
    if include_project {
        headers.push("project".into());
    }
    headers.extend(columns.iter().map(|column| csv_header(*column).to_string()));

    let mut out = String::new();
    write_csv_row(&mut out, headers);
    for row in rows {
        let mut fields = vec![row.period.clone()];
        if include_provider {
            fields.push(row.provider.clone().unwrap_or_default());
        }
        if include_model {
            fields.push(row.model_id.clone().unwrap_or_default());
        }
        if include_project {
            fields.push(row.project.clone().unwrap_or_default());
        }
        fields.extend(columns.iter().map(|column| raw_value(*column, row)));
        write_csv_row(&mut out, fields);
    }
    out
}

fn csv_header(column: Column) -> &'static str {
    match column {
        Column::Input => "input_tokens",
        Column::Output => "output_tokens",
        Column::CacheRead => "cache_read_tokens",
        Column::CacheCreation => "cache_creation_tokens",
        Column::CachedInput => "cached_input_tokens",
        Column::ReasoningOutput => "reasoning_output_tokens",
        Column::Total => "total_tokens",
        Column::Cost => "cost_usd",
        Column::Requests => "request_count",
        Column::Sessions => "session_count",
    }
}

fn raw_value(column: Column, row: &AggregatedRow) -> String {
    match column {
        Column::Input => row.input_tokens.to_string(),
        Column::Output => row.output_tokens.to_string(),
        Column::CacheRead => row.cache_read_tokens.to_string(),
        Column::CacheCreation => row.cache_creation_tokens.to_string(),
        Column::CachedInput => row.cached_input_tokens.to_string(),
        Column::ReasoningOutput => row.reasoning_output_tokens.to_string(),
        Column::Total => row.total_tokens.to_string(),
        Column::Cost => format!("{:.6}", row.cost_usd),
        Column::Requests => row.request_count.to_string(),
        Column::Sessions => row.session_count.to_string(),
    }
}

fn write_csv_row(out: &mut String, fields: Vec<String>) {
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&escape(field));
    }
    out.push('\n');
}

fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csv_escapes_commas_quotes_and_newlines() {
        let rows = vec![AggregatedRow {
            period: "proj, \"quoted\"\nnext".into(),
            input_tokens: 10,
            ..Default::default()
        }];
        let csv = render_csv(&rows, &[Column::Input]);
        assert!(csv.contains("\"proj, \"\"quoted\"\"\nnext\""));
    }

    #[test]
    fn test_csv_uses_raw_numeric_values() {
        let rows = vec![AggregatedRow {
            period: "2026-04-07".into(),
            input_tokens: 1500,
            cached_input_tokens: 400,
            reasoning_output_tokens: 70,
            cost_usd: 1.2,
            request_count: 3,
            ..Default::default()
        }];
        let csv = render_csv(
            &rows,
            &[
                Column::Input,
                Column::CachedInput,
                Column::ReasoningOutput,
                Column::Cost,
                Column::Requests,
            ],
        );
        assert_eq!(
            csv,
            "period,input_tokens,cached_input_tokens,reasoning_output_tokens,cost_usd,request_count\n2026-04-07,1500,400,70,1.200000,3\n"
        );
    }

    #[test]
    fn test_csv_includes_group_metadata() {
        let rows = vec![AggregatedRow {
            period: "claude/claude-opus-4-6".into(),
            provider: Some("claude".into()),
            model_id: Some("claude-opus-4-6".into()),
            project: Some("demo".into()),
            ..Default::default()
        }];
        let csv = render_csv(&rows, &[Column::Total]);
        assert!(csv.starts_with("period,provider,model_id,project,total_tokens\n"));
        assert!(csv.contains("claude,claude-opus-4-6,demo"));
    }

    #[test]
    fn test_csv_empty_rows_still_emit_header() {
        let csv = render_csv(&[], &[Column::Input]);
        assert_eq!(csv, "period,input_tokens\n");
    }
}
