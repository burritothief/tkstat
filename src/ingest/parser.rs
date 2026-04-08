use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::usage::{ModelFamily, TokenRecord};
use crate::ingest::walker::SourceFile;

// -- Serde types for JSONL deserialization --

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonlEntry {
    #[serde(rename = "type")]
    entry_type: String,
    message: Option<JsonlMessage>,
    request_id: Option<String>,
    uuid: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonlMessage {
    model: Option<String>,
    usage: Option<JsonlUsage>,
}

#[derive(Debug, Deserialize)]
struct JsonlUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

const ASSISTANT_TYPE_MARKER: &[u8] = b"\"type\":\"assistant\"";

/// Parse a JSONL file starting at the given byte offset.
/// Returns deduplicated TokenRecords (one per request_id, keeping max output_tokens).
pub fn parse_jsonl_file(
    path: &Path,
    offset: u64,
    file_info: &SourceFile,
) -> Result<Vec<TokenRecord>> {
    let mut file = File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;

    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    Ok(parse_jsonl_bytes(&buf, file_info))
}

/// Parse raw JSONL bytes into deduplicated TokenRecords.
pub fn parse_jsonl_bytes(bytes: &[u8], file_info: &SourceFile) -> Vec<TokenRecord> {
    let mut seen: HashMap<String, TokenRecord> = HashMap::new();

    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }

        if memchr::memmem::find(line, ASSISTANT_TYPE_MARKER).is_none() {
            continue;
        }

        let entry: JsonlEntry = match serde_json::from_slice(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.entry_type != "assistant" {
            continue;
        }

        let Some(msg) = entry.message else { continue };
        let Some(usage) = msg.usage else { continue };
        let Some(ref request_id) = entry.request_id else { continue };
        if request_id.is_empty() {
            continue;
        }
        let model_str = msg.model.unwrap_or_default();
        if model_str.is_empty() || model_str == "<synthetic>" {
            continue;
        }
        let Some(timestamp) = entry.timestamp else { continue };

        let record = TokenRecord {
            request_id: request_id.clone(),
            session_id: entry.session_id.unwrap_or_default(),
            uuid: entry.uuid.unwrap_or_default(),
            timestamp,
            model: ModelFamily::classify(&model_str),
            model_raw: model_str,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cost_usd: 0.0,
            project: file_info.project_name.clone(),
            source_file: file_info.path.to_string_lossy().to_string(),
            is_subagent: file_info.is_subagent,
        };

        seen.entry(request_id.clone())
            .and_modify(|existing| {
                if record.output_tokens > existing.output_tokens {
                    *existing = record.clone();
                }
            })
            .or_insert(record);
    }

    seen.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_source_file() -> SourceFile {
        SourceFile {
            path: PathBuf::from("/test/projects/-Users-test-src-myproj/abc.jsonl"),
            project_name: "myproj".into(),
            is_subagent: false,
            size_bytes: 0,
            mtime_secs: 0,
        }
    }

    fn assistant_line(request_id: &str, model: &str, output_tokens: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":500,"output_tokens":{output_tokens}}}}},"requestId":"{request_id}","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}}"#
        )
    }

    #[test]
    fn test_parse_single_assistant_entry() {
        let line = assistant_line("req1", "claude-opus-4-6", 42);
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.request_id, "req1");
        assert_eq!(r.model, ModelFamily::Opus);
        assert_eq!(r.input_tokens, 10);
        assert_eq!(r.output_tokens, 42);
        assert_eq!(r.cache_creation_tokens, 100);
        assert_eq!(r.cache_read_tokens, 500);
    }

    #[test]
    fn test_dedup_keeps_max_output_tokens() {
        let lines = format!(
            "{}\n{}\n{}",
            assistant_line("req1", "claude-opus-4-6", 10),
            assistant_line("req1", "claude-opus-4-6", 50),
            assistant_line("req1", "claude-opus-4-6", 30),
        );
        let records = parse_jsonl_bytes(lines.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].output_tokens, 50);
    }

    #[test]
    fn test_skips_non_assistant_lines() {
        let mut all = format!(
            r#"{{"type":"user","message":{{"role":"user"}},"uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}}"#
        );
        all.push('\n');
        all.push_str(&assistant_line("req1", "claude-sonnet-4-6", 20));
        let records = parse_jsonl_bytes(all.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, ModelFamily::Sonnet);
    }

    #[test]
    fn test_skips_synthetic_model() {
        let line = assistant_line("req1", "<synthetic>", 10);
        assert_eq!(parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(), 0);
    }

    #[test]
    fn test_skips_empty_request_id() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        assert_eq!(parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(), 0);
    }

    #[test]
    fn test_skips_missing_request_id() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        assert_eq!(parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(), 0);
    }

    #[test]
    fn test_multiple_requests_kept_separately() {
        let lines = format!(
            "{}\n{}",
            assistant_line("req1", "claude-opus-4-6", 10),
            assistant_line("req2", "claude-haiku-4-5-20251001", 20),
        );
        assert_eq!(parse_jsonl_bytes(lines.as_bytes(), &make_source_file()).len(), 2);
    }

    #[test]
    fn test_handles_malformed_json_gracefully() {
        let lines = format!(
            "this is not json\n{}\n{{broken json\n",
            assistant_line("req1", "claude-opus-4-6", 10),
        );
        assert_eq!(parse_jsonl_bytes(lines.as_bytes(), &make_source_file()).len(), 1);
    }

    #[test]
    fn test_source_file_info_propagated() {
        let fi = SourceFile {
            path: PathBuf::from("/x/projects/-Users-me-src-coolproj/sess.jsonl"),
            project_name: "coolproj".into(),
            is_subagent: true,
            size_bytes: 100,
            mtime_secs: 12345,
        };
        let line = assistant_line("req1", "claude-opus-4-6", 10);
        let records = parse_jsonl_bytes(line.as_bytes(), &fi);
        assert_eq!(records[0].project, "coolproj");
        assert!(records[0].is_subagent);
    }
}
