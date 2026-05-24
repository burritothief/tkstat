use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::db::schema::SCHEMA_VERSION;
use crate::ingest::{self, CodexAdapter, ProviderAdapter, ProviderSources};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SchemaStatus {
    Current,
    Old,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TableStatus {
    Available,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SourceStatus {
    NotConfigured,
    Missing,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PricingStatus {
    MissingTable,
    Empty,
    Available,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsInventory {
    pub db_path: PathBuf,
    pub schema: SchemaInventory,
    pub providers: Vec<ProviderSourceInventory>,
    pub usage: UsageInventory,
    pub file_state: FileStateInventory,
    pub pricing: PricingInventory,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaInventory {
    pub status: SchemaStatus,
    pub version: Option<i64>,
    pub expected_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderSourceInventory {
    pub provider: &'static str,
    pub path: Option<PathBuf>,
    pub status: SourceStatus,
    pub discovered_files: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageInventory {
    pub table_status: TableStatus,
    pub total_rows: u64,
    pub by_provider: Vec<ProviderUsageInventory>,
    pub model_count: u64,
    pub latest_timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderUsageInventory {
    pub provider: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStateInventory {
    pub table_status: TableStatus,
    pub total_rows: u64,
    pub by_provider: Vec<ProviderFileStateInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderFileStateInventory {
    pub provider: String,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingInventory {
    pub status: PricingStatus,
    pub interval_count: u64,
    pub open_interval_count: u64,
    pub model_count: u64,
}

pub fn gather_inventory(
    db_path: impl Into<PathBuf>,
    conn: &Connection,
    sources: &ProviderSources,
) -> DiagnosticsInventory {
    DiagnosticsInventory {
        db_path: db_path.into(),
        schema: schema_inventory(conn),
        providers: provider_inventories(sources),
        usage: usage_inventory(conn),
        file_state: file_state_inventory(conn),
        pricing: pricing_inventory(conn),
    }
}

impl DiagnosticsInventory {
    pub fn blocking_issues(&self) -> Vec<String> {
        match self.schema.status {
            SchemaStatus::Old => vec![format!(
                "database schema version {} is older than expected version {}",
                self.schema
                    .version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                self.schema.expected_version
            )],
            SchemaStatus::Current | SchemaStatus::Missing => Vec::new(),
        }
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.schema.status == SchemaStatus::Missing {
            warnings.push("database schema is not initialized".into());
        }
        for provider in &self.providers {
            match provider.status {
                SourceStatus::Missing => {
                    warnings.push(format!("{} source path is missing", provider.provider));
                }
                SourceStatus::NotConfigured => {
                    warnings.push(format!(
                        "{} source path is not configured",
                        provider.provider
                    ));
                }
                SourceStatus::Available => {}
            }
        }
        match self.pricing.status {
            PricingStatus::MissingTable => warnings.push("pricing table is missing".into()),
            PricingStatus::Empty => warnings.push("pricing table has no intervals".into()),
            PricingStatus::Available => {}
        }
        warnings
    }
}

fn schema_inventory(conn: &Connection) -> SchemaInventory {
    let version = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get::<_, i64>(0)
        })
        .optional()
        .ok()
        .flatten();
    let status = match version {
        Some(version) if version >= SCHEMA_VERSION => SchemaStatus::Current,
        Some(_) => SchemaStatus::Old,
        None => SchemaStatus::Missing,
    };
    SchemaInventory {
        status,
        version,
        expected_version: SCHEMA_VERSION,
    }
}

fn provider_inventories(sources: &ProviderSources) -> Vec<ProviderSourceInventory> {
    vec![
        provider_inventory(
            ingest::claude::CLAUDE_PROVIDER,
            sources.claude_data_dir.as_deref(),
            |path| {
                let adapter = ingest::ClaudeCodeAdapter::new(path);
                adapter.discover().map(|files| files.len()).ok()
            },
        ),
        provider_inventory(
            ingest::codex::CODEX_PROVIDER,
            sources.codex_home.as_deref(),
            |path| {
                let adapter = CodexAdapter::new(path);
                adapter.discover().map(|files| files.len()).ok()
            },
        ),
    ]
}

fn provider_inventory(
    provider: &'static str,
    path: Option<&Path>,
    discover: impl FnOnce(&Path) -> Option<usize>,
) -> ProviderSourceInventory {
    let Some(path) = path else {
        return ProviderSourceInventory {
            provider,
            path: None,
            status: SourceStatus::NotConfigured,
            discovered_files: None,
        };
    };

    if !path.is_dir() {
        return ProviderSourceInventory {
            provider,
            path: Some(path.to_path_buf()),
            status: SourceStatus::Missing,
            discovered_files: None,
        };
    }

    ProviderSourceInventory {
        provider,
        path: Some(path.to_path_buf()),
        status: SourceStatus::Available,
        discovered_files: discover(path),
    }
}

fn usage_inventory(conn: &Connection) -> UsageInventory {
    if !table_exists(conn, "token_usage") {
        return UsageInventory {
            table_status: TableStatus::Missing,
            total_rows: 0,
            by_provider: Vec::new(),
            model_count: 0,
            latest_timestamp: None,
        };
    }

    UsageInventory {
        table_status: TableStatus::Available,
        total_rows: count_query(conn, "SELECT COUNT(*) FROM token_usage"),
        by_provider: provider_usage_counts(conn),
        model_count: count_query(
            conn,
            "SELECT COUNT(DISTINCT provider || '/' || model_id) FROM token_usage",
        ),
        latest_timestamp: optional_string_query(conn, "SELECT MAX(timestamp) FROM token_usage"),
    }
}

fn file_state_inventory(conn: &Connection) -> FileStateInventory {
    if !table_exists(conn, "file_state") {
        return FileStateInventory {
            table_status: TableStatus::Missing,
            total_rows: 0,
            by_provider: Vec::new(),
        };
    }

    FileStateInventory {
        table_status: TableStatus::Available,
        total_rows: count_query(conn, "SELECT COUNT(*) FROM file_state"),
        by_provider: provider_file_state_counts(conn),
    }
}

fn pricing_inventory(conn: &Connection) -> PricingInventory {
    if !table_exists(conn, "pricing_intervals") {
        return PricingInventory {
            status: PricingStatus::MissingTable,
            interval_count: 0,
            open_interval_count: 0,
            model_count: 0,
        };
    }

    let interval_count = count_query(conn, "SELECT COUNT(*) FROM pricing_intervals");
    PricingInventory {
        status: if interval_count == 0 {
            PricingStatus::Empty
        } else {
            PricingStatus::Available
        },
        interval_count,
        open_interval_count: count_query(
            conn,
            "SELECT COUNT(*) FROM pricing_intervals WHERE effective_to IS NULL",
        ),
        model_count: count_query(
            conn,
            "SELECT COUNT(DISTINCT provider || '/' || model_id) FROM pricing_intervals",
        ),
    }
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn count_query(conn: &Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
}

fn optional_string_query(conn: &Connection, sql: &str) -> Option<String> {
    conn.query_row(sql, [], |row| row.get::<_, Option<String>>(0))
        .ok()
        .flatten()
}

fn provider_usage_counts(conn: &Connection) -> Vec<ProviderUsageInventory> {
    let Ok(mut stmt) = conn
        .prepare("SELECT provider, COUNT(*) FROM token_usage GROUP BY provider ORDER BY provider")
    else {
        return Vec::new();
    };
    stmt.query_map([], |row| {
        Ok(ProviderUsageInventory {
            provider: row.get(0)?,
            rows: row.get::<_, i64>(1)?.max(0) as u64,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn provider_file_state_counts(conn: &Connection) -> Vec<ProviderFileStateInventory> {
    let Ok(mut stmt) = conn
        .prepare("SELECT provider, COUNT(*) FROM file_state GROUP BY provider ORDER BY provider")
    else {
        return Vec::new();
    };
    stmt.query_map([], |row| {
        Ok(ProviderFileStateInventory {
            provider: row.get(0)?,
            files: row.get::<_, i64>(1)?.max(0) as u64,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::db::Database;
    use crate::domain::usage::{ModelFamily, TokenRecord};

    fn temp_root(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tkstat-diagnostics-{test_name}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn sources(claude_data_dir: Option<PathBuf>, codex_home: Option<PathBuf>) -> ProviderSources {
        ProviderSources {
            claude_data_dir,
            codex_home,
        }
    }

    fn record(provider: &str, request_id: &str, model_id: &str) -> TokenRecord {
        TokenRecord {
            provider: provider.into(),
            request_id: request_id.into(),
            session_id: format!("s-{request_id}"),
            uuid: format!("u-{request_id}"),
            timestamp: "2026-04-07T10:00:00Z".parse().unwrap(),
            model: ModelFamily::classify(model_id),
            model_id: model_id.into(),
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
            cost_usd: 0.0,
            project: "demo".into(),
            source_file: "/tmp/session.jsonl".into(),
            is_subagent: false,
        }
    }

    #[test]
    fn test_inventory_empty_setup_reports_explicit_empty_statuses() {
        let db = Database::open_in_memory().unwrap();
        let inventory = gather_inventory("/tmp/tkstat.db", db.conn(), &sources(None, None));

        assert_eq!(inventory.schema.status, SchemaStatus::Current);
        assert_eq!(inventory.usage.table_status, TableStatus::Available);
        assert_eq!(inventory.usage.total_rows, 0);
        assert_eq!(inventory.pricing.status, PricingStatus::Empty);
        assert!(
            inventory
                .providers
                .iter()
                .all(|provider| provider.status == SourceStatus::NotConfigured)
        );
    }

    #[test]
    fn test_inventory_reports_claude_source_usage_file_state_and_pricing() {
        let root = temp_root("claude");
        let projects = root.join("claude").join("projects");
        let project_dir = projects.join("-home-tester-work-demo");
        fs::create_dir_all(&project_dir).unwrap();
        let path = project_dir.join("session.jsonl");
        fs::write(&path, "{}\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        db.insert_records(&[record("claude-code", "req-1", "claude-sonnet-4-5-20250929")])
            .unwrap();
        db.update_file_state("claude-code", &path, 3, 1, 3).unwrap();

        let inventory = gather_inventory(
            root.join("tkstat.db"),
            db.conn(),
            &sources(Some(projects.clone()), None),
        );
        let claude = inventory
            .providers
            .iter()
            .find(|provider| provider.provider == "claude-code")
            .unwrap();
        assert_eq!(claude.status, SourceStatus::Available);
        assert_eq!(claude.discovered_files, Some(1));
        assert_eq!(inventory.usage.total_rows, 1);
        assert_eq!(
            inventory.usage.by_provider,
            vec![ProviderUsageInventory {
                provider: "claude-code".into(),
                rows: 1
            }]
        );
        assert_eq!(
            inventory.file_state.by_provider,
            vec![ProviderFileStateInventory {
                provider: "claude-code".into(),
                files: 1
            }]
        );
        assert_eq!(inventory.pricing.status, PricingStatus::Available);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_inventory_reports_multi_provider_usage_and_model_counts() {
        let db = Database::open_in_memory().unwrap();
        db.insert_records(&[
            record("claude-code", "req-1", "claude-opus-4-6"),
            record("codex", "req-2", "gpt-5.5"),
        ])
        .unwrap();

        let inventory = gather_inventory("/tmp/tkstat.db", db.conn(), &sources(None, None));
        assert_eq!(inventory.usage.total_rows, 2);
        assert_eq!(inventory.usage.model_count, 2);
        assert_eq!(
            inventory.usage.by_provider,
            vec![
                ProviderUsageInventory {
                    provider: "claude-code".into(),
                    rows: 1
                },
                ProviderUsageInventory {
                    provider: "codex".into(),
                    rows: 1
                }
            ]
        );
        assert_eq!(
            inventory.latest_timestamp(),
            Some("2026-04-07T10:00:00+00:00")
        );
    }

    #[test]
    fn test_inventory_reports_missing_provider_paths_without_panicking() {
        let db = Database::open_in_memory().unwrap();
        let root = temp_root("missing");
        let inventory = gather_inventory(
            root.join("tkstat.db"),
            db.conn(),
            &sources(
                Some(root.join("missing-claude")),
                Some(root.join("missing-codex")),
            ),
        );
        assert!(
            inventory
                .providers
                .iter()
                .all(|provider| provider.status == SourceStatus::Missing)
        );
    }

    #[test]
    fn test_inventory_tolerates_old_schema_without_creating_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (1);",
        )
        .unwrap();

        let inventory = gather_inventory("/tmp/old.db", &conn, &sources(None, None));
        assert_eq!(inventory.schema.status, SchemaStatus::Old);
        assert_eq!(inventory.schema.version, Some(1));
        assert_eq!(inventory.usage.table_status, TableStatus::Missing);
        assert_eq!(inventory.file_state.table_status, TableStatus::Missing);
        assert_eq!(inventory.pricing.status, PricingStatus::MissingTable);
        assert!(!table_exists(&conn, "token_usage"));
    }

    impl DiagnosticsInventory {
        fn latest_timestamp(&self) -> Option<&str> {
            self.usage.latest_timestamp.as_deref()
        }
    }
}
