use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

use crate::domain::provider::ProviderId;
use crate::domain::timestamp::format_utc_rfc3339;
use crate::domain::usage::{ModelFamily, TokenRecord};
use crate::ingest::ParsedFile;
use crate::ingest::walker::SourceFile;

pub const CODEX_PROVIDER: &str = crate::domain::provider::CODEX_PROVIDER;

#[derive(Debug, Deserialize)]
struct CodexEntry {
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "type")]
    entry_type: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

#[derive(Debug, Default)]
struct CodexSessionState {
    session_id: Option<String>,
    cwd: Option<String>,
    model_id: Option<String>,
}

/// Walk a Codex home directory and find session JSONL files.
pub fn discover_session_files(codex_home: &Path) -> Result<Vec<SourceFile>> {
    let sessions_dir = codex_home.join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(&sessions_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl")
            && let Some(file) = source_file(path)
        {
            files.push(file);
        }
    }
    Ok(files)
}

fn source_file(path: &Path) -> Option<SourceFile> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;

    Some(SourceFile {
        path: path.to_path_buf(),
        project_name: "unknown".into(),
        is_subagent: false,
        size_bytes: meta.len(),
        mtime_secs,
    })
}

/// Parse a Codex session JSONL file starting at the given byte offset.
pub fn parse_session_file(path: &Path, offset: u64, file_info: &SourceFile) -> Result<ParsedFile> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(parse_session_bytes_incremental(&buf, offset, file_info))
}

/// Parse raw Codex JSONL bytes into token records.
pub fn parse_session_bytes(bytes: &[u8], file_info: &SourceFile) -> Vec<TokenRecord> {
    parse_session_lines(bytes, 0, file_info).records
}

/// Parse Codex JSONL bytes while preserving a trailing incomplete line.
pub fn parse_session_bytes_incremental(
    bytes: &[u8],
    emit_after_offset: u64,
    file_info: &SourceFile,
) -> ParsedFile {
    let safe_len = safe_jsonl_prefix_len(bytes);
    parse_session_lines(&bytes[..safe_len], emit_after_offset, file_info)
}

fn complete_jsonl_prefix_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |pos| pos + 1)
}

fn safe_jsonl_prefix_len(bytes: &[u8]) -> usize {
    let newline_safe_len = complete_jsonl_prefix_len(bytes);
    if newline_safe_len < bytes.len()
        && serde_json::from_slice::<CodexEntry>(&bytes[newline_safe_len..]).is_ok()
    {
        bytes.len()
    } else {
        newline_safe_len
    }
}

fn parse_session_lines(bytes: &[u8], emit_after_offset: u64, file_info: &SourceFile) -> ParsedFile {
    let mut state = CodexSessionState::default();
    let mut seen: HashMap<String, TokenRecord> = HashMap::new();
    let mut line_start = 0usize;
    let mut parse_errors = 0;

    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            line_start += 1;
            continue;
        }
        let entry: CodexEntry = match serde_json::from_slice(line) {
            Ok(entry) => entry,
            Err(_) => {
                parse_errors += 1;
                line_start += line.len() + 1;
                continue;
            }
        };

        match entry.entry_type.as_str() {
            "session_meta" => update_from_session_meta(&mut state, &entry.payload),
            "turn_context" => update_from_turn_context(&mut state, &entry.payload),
            "event_msg" => {
                if line_start as u64 >= emit_after_offset
                    && let Some(record) = token_count_record(&state, entry, file_info)
                {
                    seen.entry(record.request_id.clone()).or_insert(record);
                }
            }
            _ => {}
        }
        line_start += line.len() + 1;
    }

    ParsedFile {
        records: seen.into_values().collect(),
        safe_byte_offset: bytes.len() as u64,
        parse_errors,
    }
}

fn update_from_session_meta(state: &mut CodexSessionState, payload: &Value) {
    if let Some(id) = payload.get("id").and_then(Value::as_str)
        && !id.is_empty()
    {
        state.session_id = Some(id.to_string());
    }
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
        && !cwd.is_empty()
    {
        state.cwd = Some(cwd.to_string());
    }
}

fn update_from_turn_context(state: &mut CodexSessionState, payload: &Value) {
    if let Some(model) = payload.get("model").and_then(Value::as_str)
        && !model.is_empty()
    {
        state.model_id = Some(model.to_string());
    }
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
        && !cwd.is_empty()
    {
        state.cwd = Some(cwd.to_string());
    }
}

fn token_count_record(
    state: &CodexSessionState,
    entry: CodexEntry,
    file_info: &SourceFile,
) -> Option<TokenRecord> {
    let payload_type = entry.payload.get("type").and_then(Value::as_str)?;
    if payload_type != "token_count" {
        return None;
    }

    let timestamp = entry.timestamp?;
    let model_id = state.model_id.clone()?;
    let usage: CodexUsage =
        serde_json::from_value(entry.payload.get("info")?.get("last_token_usage")?.clone()).ok()?;
    let session_id = state
        .session_id
        .clone()
        .or_else(|| session_id_from_path(&file_info.path))?;
    let request_id = format!(
        "{}:{}:{}:{}:{}:{}",
        session_id,
        format_utc_rfc3339(timestamp),
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.output_tokens,
        usage.reasoning_output_tokens
    );
    let project = state
        .cwd
        .as_deref()
        .map(project_name_from_cwd)
        .unwrap_or_else(|| file_info.project_name.clone());

    Some(TokenRecord {
        provider: ProviderId::Codex,
        request_id: request_id.clone(),
        session_id,
        uuid: request_id,
        timestamp,
        model: ModelFamily::classify(&model_id),
        model_id,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cached_input_tokens: usage.cached_input_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        cost_usd: 0.0,
        project,
        source_file: file_info.path.to_string_lossy().to_string(),
        is_subagent: false,
    })
}

fn session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    if stem.len() >= 36 {
        let suffix = &stem[stem.len() - 36..];
        if suffix.chars().filter(|&c| c == '-').count() == 4 {
            return Some(suffix.to_string());
        }
    }
    Some(stem.to_string())
}

fn project_name_from_cwd(cwd: &str) -> String {
    PathBuf::from(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_file() -> SourceFile {
        SourceFile {
            path: PathBuf::from(
                "/tmp/tkstat-fixtures/codex/sessions/2026/05/23/synthetic-codex-session.jsonl",
            ),
            project_name: "unknown".into(),
            is_subagent: false,
            size_bytes: 0,
            mtime_secs: 0,
        }
    }

    fn session_meta() -> &'static str {
        r#"{"timestamp":"2026-05-24T00:40:02.000Z","type":"session_meta","payload":{"id":"synthetic-codex-session","cwd":"/home/tester/work/tkstat","model_provider":"openai"}}"#
    }

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-05-24T00:40:02.192Z","type":"turn_context","payload":{{"turn_id":"turn-1","cwd":"/home/tester/work/tkstat","model":"{model}"}}}}"#
        )
    }

    fn token_count(ts: &str, output: u64) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":13066,"cached_input_tokens":4480,"output_tokens":213,"reasoning_output_tokens":0,"total_tokens":13279}},"last_token_usage":{{"input_tokens":100,"cached_input_tokens":40,"output_tokens":{output},"reasoning_output_tokens":7,"total_tokens":120}},"model_context_window":258400}}}},"rate_limits":null}}"#
        )
    }

    #[test]
    fn test_parse_codex_token_count_preserves_openai_token_categories() {
        let lines = format!(
            "{}\n{}\n{}",
            session_meta(),
            turn_context("gpt-5.5"),
            token_count("2026-05-24T00:40:04.988Z", 20)
        );
        let records = parse_session_bytes(lines.as_bytes(), &source_file());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.provider, ProviderId::Codex);
        assert_eq!(record.session_id, "synthetic-codex-session");
        assert_eq!(record.model_id, "gpt-5.5");
        assert_eq!(record.project, "tkstat");
        assert_eq!(record.input_tokens, 100);
        assert_eq!(record.cached_input_tokens, 40);
        assert_eq!(record.output_tokens, 20);
        assert_eq!(record.reasoning_output_tokens, 7);
        assert_eq!(record.cache_read_tokens, 0);
        assert_eq!(record.cache_creation_tokens, 0);
    }

    #[test]
    fn test_parse_codex_skips_token_count_without_model() {
        let lines = format!(
            "{}\n{}",
            session_meta(),
            token_count("2026-05-24T00:40:04.988Z", 20)
        );
        assert_eq!(
            parse_session_bytes(lines.as_bytes(), &source_file()).len(),
            0
        );
    }

    #[test]
    fn test_parse_codex_deduplicates_duplicate_source_events() {
        let event = token_count("2026-05-24T00:40:04.988Z", 20);
        let lines = format!(
            "{}\n{}\n{}\n{}",
            session_meta(),
            turn_context("gpt-5.5"),
            event,
            event
        );
        assert_eq!(
            parse_session_bytes(lines.as_bytes(), &source_file()).len(),
            1
        );
    }

    #[test]
    fn test_parse_codex_missing_optional_session_meta_uses_file_stem_session() {
        let lines = format!(
            "{}\n{}",
            turn_context("gpt-5.5"),
            token_count("2026-05-24T00:40:04.988Z", 20)
        );
        let records = parse_session_bytes(lines.as_bytes(), &source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, "synthetic-codex-session");
    }

    #[test]
    fn test_parse_codex_incremental_preserves_trailing_fragment() {
        let complete = format!("{}\n{}\n", session_meta(), turn_context("gpt-5.5"));
        let fragment = r#"{"timestamp":"2026-05-24T00:40:04.988Z","type":"event_msg","payload":{"#;
        let bytes = format!("{complete}{fragment}");
        let parsed = parse_session_bytes_incremental(bytes.as_bytes(), 0, &source_file());
        assert!(parsed.records.is_empty());
        assert_eq!(parsed.safe_byte_offset, complete.len() as u64);
    }

    #[test]
    fn test_parse_codex_incremental_accepts_valid_final_line_without_newline() {
        let bytes = format!(
            "{}\n{}\n{}",
            session_meta(),
            turn_context("gpt-5.5"),
            token_count("2026-05-24T00:40:04.988Z", 20)
        );
        let parsed = parse_session_bytes_incremental(bytes.as_bytes(), 0, &source_file());
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.safe_byte_offset, bytes.len() as u64);
    }

    #[test]
    fn test_parse_codex_replays_context_before_emit_offset() {
        let context = format!("{}\n{}\n", session_meta(), turn_context("gpt-5.5"));
        let event = token_count("2026-05-24T00:40:04.988Z", 20);
        let bytes = format!("{context}{event}\n");
        let parsed =
            parse_session_bytes_incremental(bytes.as_bytes(), context.len() as u64, &source_file());
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0].model_id, "gpt-5.5");
        assert_eq!(parsed.safe_byte_offset, bytes.len() as u64);
    }
}
