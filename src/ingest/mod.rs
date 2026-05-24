pub mod claude;
pub mod codex;
pub mod walker;

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::OptionalExtension;

use crate::db::Database;
use crate::domain::provider::ProviderId;
use crate::domain::usage::TokenRecord;
use crate::ingest::walker::SourceFile;

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub records: Vec<TokenRecord>,
    pub safe_byte_offset: u64,
    pub parse_errors: u64,
}

/// Provider-specific source discovery and parsing.
pub trait ProviderAdapter {
    fn provider(&self) -> ProviderId;
    fn discover(&self) -> Result<Vec<SourceFile>>;
    fn parse_file(&self, path: &Path, offset: u64, file_info: &SourceFile) -> Result<ParsedFile>;
}

/// Claude Code JSONL provider adapter.
pub struct ClaudeCodeAdapter {
    data_dir: PathBuf,
}

impl ClaudeCodeAdapter {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }
}

impl ProviderAdapter for ClaudeCodeAdapter {
    fn provider(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    fn discover(&self) -> Result<Vec<SourceFile>> {
        walker::discover_jsonl_files(&self.data_dir)
    }

    fn parse_file(&self, path: &Path, offset: u64, file_info: &SourceFile) -> Result<ParsedFile> {
        claude::parse_jsonl_file(path, offset, file_info)
    }
}

/// Codex session JSONL provider adapter.
pub struct CodexAdapter {
    codex_home: PathBuf,
}

impl CodexAdapter {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
        }
    }
}

impl ProviderAdapter for CodexAdapter {
    fn provider(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn discover(&self) -> Result<Vec<SourceFile>> {
        codex::discover_session_files(&self.codex_home)
    }

    fn parse_file(&self, path: &Path, offset: u64, file_info: &SourceFile) -> Result<ParsedFile> {
        codex::parse_session_file(path, offset, file_info)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSelection {
    All,
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone)]
pub struct ProviderSources {
    pub claude_data_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderIngestStatus {
    NotConfigured,
    Missing,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestFindingSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestFindingKind {
    MalformedLine,
    FileError,
}

#[derive(Debug, Clone)]
pub struct IngestFinding {
    pub provider: ProviderId,
    pub path: PathBuf,
    pub severity: IngestFindingSeverity,
    pub kind: IngestFindingKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ProviderIngestReport {
    pub provider: ProviderId,
    pub path: Option<PathBuf>,
    pub status: ProviderIngestStatus,
    pub discovered_files: usize,
    pub processed_files: usize,
    pub inserted_records: usize,
    pub parse_errors: u64,
    pub findings: Vec<IngestFinding>,
    pub last_ingested_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IngestReport {
    pub providers: Vec<ProviderIngestReport>,
}

impl IngestReport {
    pub fn inserted_records(&self) -> usize {
        self.providers
            .iter()
            .map(|provider| provider.inserted_records)
            .sum()
    }
}

/// Run the full ingestion pipeline: walk → parse → dedup → insert.
/// Returns the number of new records inserted.
pub fn sync(
    db: &Database,
    sources: &ProviderSources,
    selection: ProviderSelection,
    force: bool,
) -> Result<usize> {
    Ok(sync_with_report(db, sources, selection, force)?.inserted_records())
}

/// Run the full ingestion pipeline and return provider-level status metadata.
pub fn sync_with_report(
    db: &Database,
    sources: &ProviderSources,
    selection: ProviderSelection,
    force: bool,
) -> Result<IngestReport> {
    let mut report = IngestReport::default();
    if matches!(
        selection,
        ProviderSelection::All | ProviderSelection::ClaudeCode
    ) {
        match &sources.claude_data_dir {
            Some(data_dir) => {
                let adapter = ClaudeCodeAdapter::new(data_dir);
                report.providers.push(sync_provider_with_report(
                    db,
                    &adapter,
                    Some(data_dir),
                    force,
                )?);
            }
            None => report
                .providers
                .push(unconfigured_provider_report(db, ProviderId::ClaudeCode)?),
        }
    }

    if matches!(selection, ProviderSelection::All | ProviderSelection::Codex) {
        match &sources.codex_home {
            Some(codex_home) => {
                let adapter = CodexAdapter::new(codex_home);
                report.providers.push(sync_provider_with_report(
                    db,
                    &adapter,
                    Some(codex_home),
                    force,
                )?);
            }
            None => report
                .providers
                .push(unconfigured_provider_report(db, ProviderId::Codex)?),
        }
    }

    Ok(report)
}

/// Run ingestion for a single provider adapter.
pub fn sync_provider(db: &Database, adapter: &dyn ProviderAdapter, force: bool) -> Result<usize> {
    Ok(sync_provider_with_report(db, adapter, None, force)?.inserted_records)
}

/// Run ingestion for a single provider adapter and return provider status metadata.
pub fn sync_provider_with_report(
    db: &Database,
    adapter: &dyn ProviderAdapter,
    path: Option<&Path>,
    force: bool,
) -> Result<ProviderIngestReport> {
    let provider = adapter.provider();
    if let Some(path) = path
        && !path.is_dir()
    {
        return Ok(ProviderIngestReport {
            provider,
            path: Some(path.to_path_buf()),
            status: ProviderIngestStatus::Missing,
            discovered_files: 0,
            processed_files: 0,
            inserted_records: 0,
            parse_errors: 0,
            findings: Vec::new(),
            last_ingested_at: latest_ingested_at(db, provider)?,
        });
    }

    let files = adapter.discover()?;
    let mut total_inserted = 0;
    let mut processed_files = 0;
    let mut parse_errors = 0;
    let mut findings = Vec::new();

    for file in &files {
        let state = if force {
            None
        } else {
            db.get_file_state(provider, &file.path)?
        };

        if let Some(ref st) = state
            && st.size_bytes >= 0
            && st.size_bytes as u64 == file.size_bytes
            && st.mtime_secs == file.mtime_secs
            && st.last_byte_offset >= st.size_bytes
        {
            continue;
        }

        let offset = match &state {
            Some(st) => {
                let stored_offset = st.last_byte_offset.max(0) as u64;
                let stored_size = st.size_bytes.max(0) as u64;
                if file.size_bytes < stored_offset || file.size_bytes < stored_size {
                    0
                } else if file.size_bytes > stored_offset {
                    stored_offset
                } else {
                    0
                }
            }
            None => 0,
        };

        let parsed = match adapter.parse_file(&file.path, offset, file) {
            Ok(parsed) => parsed,
            Err(err) => {
                findings.push(IngestFinding {
                    provider,
                    path: file.path.clone(),
                    severity: IngestFindingSeverity::Error,
                    kind: IngestFindingKind::FileError,
                    message: err.to_string(),
                });
                continue;
            }
        };
        processed_files += 1;
        parse_errors += parsed.parse_errors;
        if parsed.parse_errors > 0 {
            findings.push(IngestFinding {
                provider,
                path: file.path.clone(),
                severity: IngestFindingSeverity::Warning,
                kind: IngestFindingKind::MalformedLine,
                message: format!("{} malformed JSONL line(s)", parsed.parse_errors),
            });
        }

        let count = db.insert_records(&parsed.records)?;
        total_inserted += count;

        db.update_file_state(
            provider,
            &file.path,
            file.size_bytes,
            file.mtime_secs,
            parsed.safe_byte_offset,
        )?;
    }

    Ok(ProviderIngestReport {
        provider,
        path: path.map(Path::to_path_buf),
        status: ProviderIngestStatus::Available,
        discovered_files: files.len(),
        processed_files,
        inserted_records: total_inserted,
        parse_errors,
        findings,
        last_ingested_at: latest_ingested_at(db, provider)?,
    })
}

fn unconfigured_provider_report(
    db: &Database,
    provider: ProviderId,
) -> Result<ProviderIngestReport> {
    Ok(ProviderIngestReport {
        provider,
        path: None,
        status: ProviderIngestStatus::NotConfigured,
        discovered_files: 0,
        processed_files: 0,
        inserted_records: 0,
        parse_errors: 0,
        findings: Vec::new(),
        last_ingested_at: latest_ingested_at(db, provider)?,
    })
}

fn latest_ingested_at(db: &Database, provider: ProviderId) -> Result<Option<String>> {
    Ok(db
        .conn()
        .query_row(
            "SELECT MAX(last_ingested_at) FROM file_state WHERE provider = ?1",
            [provider.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_projects_dir(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("tkstat-{test_name}-{}-{nanos}", std::process::id()));
        root.join(".claude").join("projects")
    }

    fn assistant_line(request_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4-5-20250929","usage":{{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":500,"output_tokens":20}}}},"requestId":"{request_id}","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}}"#
        )
    }

    fn codex_session_lines() -> String {
        [
            r#"{"timestamp":"2026-05-24T00:40:02.000Z","type":"session_meta","payload":{"id":"synthetic-codex-session","cwd":"/home/tester/work/tkstat","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-24T00:40:02.192Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/home/tester/work/tkstat","model":"gpt-5.5"}}"#,
            r#"{"timestamp":"2026-05-24T00:40:04.988Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":13066,"cached_input_tokens":4480,"output_tokens":213,"reasoning_output_tokens":0,"total_tokens":13279},"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":20,"reasoning_output_tokens":7,"total_tokens":120},"model_context_window":258400}},"rate_limits":null}"#,
        ]
        .join("\n")
    }

    struct FailingAdapter {
        files: Vec<SourceFile>,
    }

    impl ProviderAdapter for FailingAdapter {
        fn provider(&self) -> crate::domain::provider::ProviderId {
            crate::domain::provider::ProviderId::Codex
        }

        fn discover(&self) -> Result<Vec<SourceFile>> {
            Ok(self.files.clone())
        }

        fn parse_file(
            &self,
            path: &Path,
            _offset: u64,
            _file_info: &SourceFile,
        ) -> Result<ParsedFile> {
            if path.to_string_lossy().contains("unreadable") {
                Err(anyhow::anyhow!("permission denied"))
            } else {
                Err(anyhow::anyhow!("provider parser failed"))
            }
        }
    }

    fn discovered_file(path: PathBuf) -> SourceFile {
        SourceFile {
            path,
            project_name: "fixture".into(),
            is_subagent: false,
            size_bytes: 10,
            mtime_secs: 1,
        }
    }

    #[test]
    fn test_claude_code_adapter_discovers_and_parses_jsonl() {
        let projects_dir = temp_projects_dir("adapter-parse");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        fs::write(&path, format!("{}\n", assistant_line("req1"))).unwrap();

        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        let files = adapter.discover().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].project_name, "demo");

        let parsed = adapter.parse_file(&files[0].path, 0, &files[0]).unwrap();
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(
            parsed.records[0].provider,
            crate::domain::provider::ProviderId::ClaudeCode
        );
        assert_eq!(parsed.records[0].model_id, "claude-sonnet-4-5-20250929");

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_uses_provider_aware_file_state() {
        let projects_dir = temp_projects_dir("sync-provider");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        fs::write(&path, format!("{}\n", assistant_line("req1"))).unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 1);
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 0);
        assert!(
            db.get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
                .unwrap()
                .is_some()
        );

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_report_tracks_parse_errors_and_freshness() {
        let projects_dir = temp_projects_dir("sync-report-parse-error");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("bad.jsonl");
        fs::write(&path, "not-json\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        let report = sync_provider_with_report(&db, &adapter, Some(&projects_dir), false).unwrap();

        assert_eq!(report.status, ProviderIngestStatus::Available);
        assert_eq!(report.discovered_files, 1);
        assert_eq!(report.processed_files, 1);
        assert_eq!(report.inserted_records, 0);
        assert_eq!(report.parse_errors, 1);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].provider,
            crate::domain::provider::ProviderId::ClaudeCode
        );
        assert_eq!(report.findings[0].path, path);
        assert_eq!(report.findings[0].severity, IngestFindingSeverity::Warning);
        assert_eq!(report.findings[0].kind, IngestFindingKind::MalformedLine);
        assert!(report.findings[0].message.contains("malformed JSONL"));
        assert!(report.last_ingested_at.is_some());
        assert!(
            db.get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
                .unwrap()
                .is_some()
        );

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_report_preserves_file_level_errors() {
        let root = std::env::temp_dir().join(format!(
            "tkstat-file-errors-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let unreadable = root.join("unreadable.jsonl");
        let parser_failure = root.join("parser-failure.jsonl");
        let adapter = FailingAdapter {
            files: vec![
                discovered_file(unreadable.clone()),
                discovered_file(parser_failure.clone()),
            ],
        };
        let db = Database::open_in_memory().unwrap();
        let report = sync_provider_with_report(&db, &adapter, Some(&root), false).unwrap();

        assert_eq!(report.status, ProviderIngestStatus::Available);
        assert_eq!(report.discovered_files, 2);
        assert_eq!(report.processed_files, 0);
        assert_eq!(report.inserted_records, 0);
        assert_eq!(report.parse_errors, 0);
        assert_eq!(report.findings.len(), 2);
        assert!(report.findings.iter().any(|finding| {
            finding.provider == ProviderId::Codex
                && finding.path == unreadable
                && finding.severity == IngestFindingSeverity::Error
                && finding.kind == IngestFindingKind::FileError
                && finding.message.contains("permission denied")
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.provider == ProviderId::Codex
                && finding.path == parser_failure
                && finding.severity == IngestFindingSeverity::Error
                && finding.kind == IngestFindingKind::FileError
                && finding.message.contains("provider parser failed")
        }));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_sync_provider_preserves_partial_trailing_jsonl_until_complete() {
        let projects_dir = temp_projects_dir("partial-trailing-line");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        let first = assistant_line("req1");
        let second = assistant_line("req2");
        let split_at = 40;
        let initial = format!("{first}\n{}", &second[..split_at]);
        fs::write(&path, &initial).unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 1);

        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_byte_offset, first.len() as i64 + 1);
        assert!(state.last_byte_offset < state.size_bytes);

        fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 1);
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 0);

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_retries_unconsumed_trailing_bytes_with_same_size_and_mtime() {
        let projects_dir = temp_projects_dir("retry-trailing-fragment");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        let complete = assistant_line("req1");
        let fragment = r#"{"type":"assistant","message":{"#;
        fs::write(&path, format!("{complete}\n{fragment}")).unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        let first = sync_provider_with_report(&db, &adapter, Some(&projects_dir), false).unwrap();
        assert_eq!(first.inserted_records, 1);

        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_byte_offset, complete.len() as i64 + 1);
        assert!(state.last_byte_offset < state.size_bytes);

        let second = sync_provider_with_report(&db, &adapter, Some(&projects_dir), false).unwrap();
        assert_eq!(second.processed_files, 1);
        assert_eq!(second.inserted_records, 0);

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_ingests_valid_final_line_without_trailing_newline() {
        let projects_dir = temp_projects_dir("final-line-no-newline");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        let line = assistant_line("req1");
        fs::write(&path, &line).unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        let first = sync_provider_with_report(&db, &adapter, Some(&projects_dir), false).unwrap();
        assert_eq!(first.inserted_records, 1);

        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_byte_offset, state.size_bytes);

        let second = sync_provider_with_report(&db, &adapter, Some(&projects_dir), false).unwrap();
        assert_eq!(second.processed_files, 0);
        assert_eq!(second.inserted_records, 0);

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_provider_restarts_when_tracked_file_shrinks() {
        let projects_dir = temp_projects_dir("truncate-reparse");
        let session_dir = projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("abc.jsonl");
        fs::write(
            &path,
            format!("{}\n{}\n", assistant_line("req1"), assistant_line("req2")),
        )
        .unwrap();

        let db = Database::open_in_memory().unwrap();
        let adapter = ClaudeCodeAdapter::new(&projects_dir);
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 2);

        let replacement = assistant_line("req3");
        fs::write(&path, format!("{replacement}\n")).unwrap();
        assert_eq!(sync_provider(&db, &adapter, false).unwrap(), 1);

        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_byte_offset, replacement.len() as i64 + 1);

        fs::remove_dir_all(projects_dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn test_sync_all_ingests_claude_and_codex_then_queries_by_provider() {
        let claude_projects_dir = temp_projects_dir("sync-all-claude");
        let claude_session_dir = claude_projects_dir.join("-home-tester-work-demo");
        fs::create_dir_all(&claude_session_dir).unwrap();
        fs::write(
            claude_session_dir.join("abc.jsonl"),
            format!("{}\n", assistant_line("req1")),
        )
        .unwrap();

        let codex_home = std::env::temp_dir().join(format!(
            "tkstat-sync-all-codex-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let codex_session_dir = codex_home.join("sessions/2026/05/23");
        fs::create_dir_all(&codex_session_dir).unwrap();
        fs::write(
            codex_session_dir.join("synthetic-codex-session.jsonl"),
            format!("{}\n", codex_session_lines()),
        )
        .unwrap();

        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let sources = ProviderSources {
            claude_data_dir: Some(claude_projects_dir.clone()),
            codex_home: Some(codex_home.clone()),
        };
        assert_eq!(
            sync(&db, &sources, ProviderSelection::All, false).unwrap(),
            2
        );

        let claude = crate::db::query::query_summary(
            db.conn(),
            &crate::db::query::QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::ClaudeCode),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        let codex = crate::db::query::query_summary(
            db.conn(),
            &crate::db::query::QueryFilter {
                provider: Some(crate::domain::provider::ProviderId::Codex),
                include_subagents: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(claude.request_count, 1);
        assert_eq!(codex.request_count, 1);
        assert_eq!(codex.input_tokens, 100);
        assert_eq!(codex.cached_input_tokens, 40);
        assert_eq!(codex.reasoning_output_tokens, 7);

        fs::remove_dir_all(claude_projects_dir.parent().unwrap().parent().unwrap()).unwrap();
        fs::remove_dir_all(codex_home).unwrap();
    }
}
