use crate::diagnostics::{DiagnosticsInventory, PricingStatus, SchemaStatus, SourceStatus};

pub fn render_doctor(inventory: &DiagnosticsInventory) -> String {
    let mut out = String::new();
    out.push_str("tkstat doctor\n\n");
    out.push_str("Database\n");
    out.push_str(&format!("  path: {}\n", inventory.db_path.display()));
    out.push_str(&format!(
        "  schema: {}\n",
        schema_label(
            &inventory.schema.status,
            inventory.schema.version,
            inventory.schema.expected_version
        )
    ));
    out.push('\n');

    out.push_str("Providers\n");
    for provider in &inventory.providers {
        let path = provider
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".into());
        let files = provider
            .discovered_files
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "  {}: {}  files: {}  path: {}\n",
            provider.provider,
            source_label(&provider.status),
            files,
            path
        ));
    }
    out.push('\n');

    out.push_str("Ingestion State\n");
    out.push_str(&format!("  usage rows: {}\n", inventory.usage.total_rows));
    out.push_str(&format!("  model count: {}\n", inventory.usage.model_count));
    out.push_str(&format!(
        "  latest usage: {}\n",
        inventory.usage.latest_timestamp.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  file-state rows: {}\n",
        inventory.file_state.total_rows
    ));
    for provider in &inventory.usage.by_provider {
        out.push_str(&format!(
            "  provider {}: {} usage rows\n",
            provider.provider, provider.rows
        ));
    }
    for provider in &inventory.file_state.by_provider {
        out.push_str(&format!(
            "  provider {}: {} tracked files\n",
            provider.provider, provider.files
        ));
    }
    out.push('\n');

    out.push_str("Pricing\n");
    out.push_str(&format!(
        "  status: {}\n",
        pricing_label(&inventory.pricing.status)
    ));
    out.push_str(&format!(
        "  intervals: {}  open: {}  models: {}\n",
        inventory.pricing.interval_count,
        inventory.pricing.open_interval_count,
        inventory.pricing.model_count
    ));

    let blocking = inventory.blocking_issues();
    if !blocking.is_empty() {
        out.push_str("\nBlocking Issues\n");
        for issue in blocking {
            out.push_str(&format!("  - {issue}\n"));
        }
        out.push_str("  Run `tkstat --force-update` after opening tkstat normally, or rebuild the database if the schema cannot be migrated.\n");
    }

    let warnings = inventory.warnings();
    if !warnings.is_empty() {
        out.push_str("\nWarnings\n");
        for warning in warnings {
            out.push_str(&format!("  - {warning}\n"));
        }
    }

    out
}

fn schema_label(status: &SchemaStatus, version: Option<i64>, expected: i64) -> String {
    match status {
        SchemaStatus::Current => format!("current v{}", version.unwrap_or(expected)),
        SchemaStatus::Old => format!(
            "old v{} (expected v{expected})",
            version.unwrap_or_default()
        ),
        SchemaStatus::Missing => format!("missing (expected v{expected})"),
    }
}

fn source_label(status: &SourceStatus) -> &'static str {
    match status {
        SourceStatus::NotConfigured => "not configured",
        SourceStatus::Missing => "missing",
        SourceStatus::Available => "available",
    }
}

fn pricing_label(status: &PricingStatus) -> &'static str {
    match status {
        PricingStatus::MissingTable => "missing table",
        PricingStatus::Empty => "empty",
        PricingStatus::Available => "available",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{
        FileStateInventory, PricingInventory, ProviderSourceInventory, SchemaInventory,
        SourceStatus, TableStatus, UsageInventory,
    };
    use std::path::PathBuf;

    #[test]
    fn test_render_doctor_includes_sections_and_warnings() {
        let inventory = DiagnosticsInventory {
            db_path: PathBuf::from("/tmp/tkstat.db"),
            schema: SchemaInventory {
                status: SchemaStatus::Missing,
                version: None,
                expected_version: 7,
            },
            providers: vec![ProviderSourceInventory {
                provider: "claude-code",
                path: Some(PathBuf::from("/missing")),
                status: SourceStatus::Missing,
                discovered_files: None,
            }],
            usage: UsageInventory {
                table_status: TableStatus::Missing,
                total_rows: 0,
                by_provider: Vec::new(),
                model_count: 0,
                latest_timestamp: None,
            },
            file_state: FileStateInventory {
                table_status: TableStatus::Missing,
                total_rows: 0,
                by_provider: Vec::new(),
            },
            pricing: PricingInventory {
                status: PricingStatus::MissingTable,
                interval_count: 0,
                open_interval_count: 0,
                model_count: 0,
            },
        };
        let output = render_doctor(&inventory);
        assert!(output.contains("Database"));
        assert!(output.contains("Providers"));
        assert!(output.contains("Pricing"));
        assert!(output.contains("Warnings"));
        assert!(output.contains("claude-code source path is missing"));
    }
}
