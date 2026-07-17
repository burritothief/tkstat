use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::provider::ProviderId;
use crate::domain::usage::{ModelFamily, TokenRecord};
use crate::ingest::ParsedFile;
use crate::ingest::jsonl;
use crate::ingest::walker::SourceFile;

pub const CLAUDE_PROVIDER: &str = crate::domain::provider::CLAUDE_CODE_PROVIDER;

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
    service_tier: Option<String>,
    speed: Option<String>,
    inference_geo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonlMessage {
    model: Option<String>,
    usage: Option<JsonlUsage>,
    service_tier: Option<String>,
    speed: Option<String>,
    inference_geo: Option<String>,
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
    cache_creation: Option<JsonlCacheCreation>,
    service_tier: Option<String>,
    speed: Option<String>,
    inference_geo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonlCacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

/// Parse a JSONL file starting at the given byte offset.
/// Returns deduplicated TokenRecords (one per request_id, keeping max output_tokens).
pub fn parse_jsonl_file(path: &Path, offset: u64, file_info: &SourceFile) -> Result<ParsedFile> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;

    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let parsed = parse_jsonl_bytes_incremental(&buf, file_info);
    Ok(ParsedFile {
        safe_byte_offset: offset + parsed.safe_byte_offset,
        records: parsed.records,
        parse_errors: parsed.parse_errors,
    })
}

/// Parse raw JSONL bytes into deduplicated TokenRecords.
pub fn parse_jsonl_bytes(bytes: &[u8], file_info: &SourceFile) -> Vec<TokenRecord> {
    parse_jsonl_lines(bytes.split(|&b| b == b'\n'), file_info)
}

/// Parse raw JSONL bytes that may end with an incomplete trailing line.
pub fn parse_jsonl_bytes_incremental(bytes: &[u8], file_info: &SourceFile) -> ParsedFile {
    let safe_len = jsonl::safe_prefix_len::<JsonlEntry>(bytes);
    let parsed = parse_jsonl_lines_with_errors(bytes[..safe_len].split(|&b| b == b'\n'), file_info);
    ParsedFile {
        records: parsed.records,
        safe_byte_offset: safe_len as u64,
        parse_errors: parsed.parse_errors,
    }
}

fn parse_jsonl_lines<'a>(
    lines: impl IntoIterator<Item = &'a [u8]>,
    file_info: &SourceFile,
) -> Vec<TokenRecord> {
    parse_jsonl_lines_with_errors(lines, file_info).records
}

struct ParsedLines {
    records: Vec<TokenRecord>,
    parse_errors: u64,
}

fn parse_jsonl_lines_with_errors<'a>(
    lines: impl IntoIterator<Item = &'a [u8]>,
    file_info: &SourceFile,
) -> ParsedLines {
    let mut seen: HashMap<String, TokenRecord> = HashMap::new();
    let mut parse_errors = 0;

    for line in lines {
        if line.is_empty() {
            continue;
        }

        let entry: JsonlEntry = match serde_json::from_slice(line) {
            Ok(e) => e,
            Err(_) => {
                parse_errors += 1;
                continue;
            }
        };

        if entry.entry_type != "assistant" {
            continue;
        }

        let Some(msg) = entry.message else { continue };
        let Some(usage) = msg.usage else { continue };
        let Some(ref request_id) = entry.request_id else {
            continue;
        };
        if request_id.is_empty() {
            continue;
        }
        let model_str = msg.model.unwrap_or_default();
        if model_str.is_empty() || model_str == "<synthetic>" {
            continue;
        }
        let Some(timestamp) = entry.timestamp else {
            continue;
        };

        let cache_creation_5m_tokens = usage
            .cache_creation
            .as_ref()
            .map_or(0, |cache| cache.ephemeral_5m_input_tokens);
        let cache_creation_1h_tokens = usage
            .cache_creation
            .as_ref()
            .map_or(0, |cache| cache.ephemeral_1h_input_tokens);
        let split_cache_creation_tokens =
            cache_creation_5m_tokens.saturating_add(cache_creation_1h_tokens);
        let cache_creation_tokens = if usage.cache_creation_input_tokens > 0 {
            usage.cache_creation_input_tokens
        } else {
            split_cache_creation_tokens
        };
        let service_tier =
            jsonl::first_present([usage.service_tier, msg.service_tier, entry.service_tier]);
        let speed = jsonl::first_present([usage.speed, msg.speed, entry.speed]);
        let region =
            jsonl::first_present([usage.inference_geo, msg.inference_geo, entry.inference_geo]);

        let record = TokenRecord {
            provider: ProviderId::ClaudeCode,
            request_id: request_id.clone(),
            session_id: entry.session_id.unwrap_or_default(),
            uuid: entry.uuid.unwrap_or_default(),
            timestamp,
            model: ModelFamily::classify(&model_str),
            model_id: model_str,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
            cache_creation_5m_tokens,
            cache_creation_1h_tokens,
            service_tier,
            speed,
            region,
            processing_mode: None,
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

    ParsedLines {
        records: seen.into_values().collect(),
        parse_errors,
    }
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
        assert_eq!(r.provider, ProviderId::ClaudeCode);
        assert_eq!(r.request_id, "req1");
        assert_eq!(r.model, ModelFamily::Opus);
        assert_eq!(r.model_id, "claude-opus-4-6");
        assert_eq!(r.input_tokens, 10);
        assert_eq!(r.output_tokens, 42);
        assert_eq!(r.cache_creation_tokens, 100);
        assert_eq!(r.cache_read_tokens, 500);
    }

    #[test]
    fn test_parse_claude_pricing_modifiers_and_cache_creation_ttls() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"cache_creation_input_tokens":150,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":50},"cache_read_input_tokens":500,"output_tokens":42,"service_tier":"standard","speed":"fast","inference_geo":"us"}},"requestId":"req1","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.cache_creation_tokens, 150);
        assert_eq!(record.cache_creation_5m_tokens, 100);
        assert_eq!(record.cache_creation_1h_tokens, 50);
        assert_eq!(record.service_tier.as_deref(), Some("standard"));
        assert_eq!(record.speed.as_deref(), Some("fast"));
        assert_eq!(record.region.as_deref(), Some("us"));
    }

    #[test]
    fn test_parse_claude_cache_creation_total_from_split_when_aggregate_missing() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":50},"cache_read_input_tokens":500,"output_tokens":42}},"requestId":"req1","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cache_creation_tokens, 150);
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
        let mut all = r#"{"type":"user","message":{"role":"user"},"uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#.to_string();
        all.push('\n');
        all.push_str(&assistant_line("req1", "claude-sonnet-4-6", 20));
        let records = parse_jsonl_bytes(all.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, ModelFamily::Sonnet);
    }

    #[test]
    fn test_skips_synthetic_model() {
        let line = assistant_line("req1", "<synthetic>", 10);
        assert_eq!(
            parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(),
            0
        );
    }

    #[test]
    fn test_skips_empty_request_id() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        assert_eq!(
            parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(),
            0
        );
    }

    #[test]
    fn test_skips_missing_request_id() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        assert_eq!(
            parse_jsonl_bytes(line.as_bytes(), &make_source_file()).len(),
            0
        );
    }

    #[test]
    fn test_multiple_requests_kept_separately() {
        let lines = format!(
            "{}\n{}",
            assistant_line("req1", "claude-opus-4-6", 10),
            assistant_line("req2", "claude-haiku-4-5-20251001", 20),
        );
        assert_eq!(
            parse_jsonl_bytes(lines.as_bytes(), &make_source_file()).len(),
            2
        );
    }

    #[test]
    fn test_handles_malformed_json_gracefully() {
        let lines = format!(
            "this is not json\n{}\n{{broken json\n",
            assistant_line("req1", "claude-opus-4-6", 10),
        );
        assert_eq!(
            parse_jsonl_bytes(lines.as_bytes(), &make_source_file()).len(),
            1
        );
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

    #[test]
    fn test_preserves_exact_model_id_separately_from_family() {
        let line = assistant_line("req1", "claude-sonnet-4-5-20250929", 10);
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, ModelFamily::Sonnet);
        assert_eq!(records[0].model_id, "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn test_accepts_assistant_type_with_whitespace() {
        let line = r#"{"type": "assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"req1","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "req1");
    }

    #[test]
    fn test_accepts_assistant_type_after_message() {
        let line = r#"{"message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"req1","uuid":"u1","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1","type":"assistant","extra":{"ignored":true}}"#;
        let records = parse_jsonl_bytes(line.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model_id, "claude-opus-4-6");
    }

    #[test]
    fn test_mixed_file_keeps_all_valid_assistant_json_formats() {
        let compact = assistant_line("req1", "claude-sonnet-4-5-20250929", 10);
        let spaced = r#"{"type": "assistant","message":{"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"req2","uuid":"u2","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        let reordered = r#"{"message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":10,"output_tokens":7}},"requestId":"req3","uuid":"u3","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1","type":"assistant"}"#;
        let user = r#"{"type": "user","message":{"role":"user"},"uuid":"u4","timestamp":"2026-04-07T10:00:00Z","sessionId":"s1"}"#;
        let lines = format!("not json\n{compact}\n{spaced}\n{user}\n{reordered}\n");
        let records = parse_jsonl_bytes(lines.as_bytes(), &make_source_file());
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn test_incremental_parse_preserves_trailing_fragment() {
        let complete = assistant_line("req1", "claude-sonnet-4-5-20250929", 10);
        let fragment = r#"{"type":"assistant","message":{"#;
        let bytes = format!("{complete}\n{fragment}");
        let parsed = parse_jsonl_bytes_incremental(bytes.as_bytes(), &make_source_file());
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0].request_id, "req1");
        assert_eq!(parsed.safe_byte_offset, complete.len() as u64 + 1);
    }

    #[test]
    fn test_incremental_parse_accepts_valid_final_line_without_newline() {
        let line = assistant_line("req1", "claude-sonnet-4-5-20250929", 10);
        let parsed = parse_jsonl_bytes_incremental(line.as_bytes(), &make_source_file());
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0].request_id, "req1");
        assert_eq!(parsed.safe_byte_offset, line.len() as u64);
    }
}
