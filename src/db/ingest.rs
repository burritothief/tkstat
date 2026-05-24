use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::db::{ensure_non_empty, u64_to_sql_i64};
use crate::domain::usage::TokenRecord;

/// Batch insert records, ignoring only duplicate provider/request_id source events.
/// Returns the count of newly inserted rows.
pub fn batch_insert(conn: &Connection, records: &[TokenRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0;

    {
        let mut duplicate_stmt = tx.prepare_cached(
            "SELECT 1 FROM token_usage WHERE provider = ?1 AND request_id = ?2 LIMIT 1",
        )?;
        let mut stmt = tx.prepare_cached(
            "INSERT INTO token_usage
                (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cached_input_tokens, reasoning_output_tokens,
                 cost_usd, project, source_file, is_subagent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        )?;

        for r in records {
            validate_record(r)?;
            let duplicate = duplicate_stmt
                .query_row(rusqlite::params![r.provider, r.request_id], |_| Ok(()))
                .optional()?
                .is_some();
            if duplicate {
                continue;
            }

            let changed = stmt.execute(rusqlite::params![
                r.provider,
                r.request_id,
                r.session_id,
                r.uuid,
                r.timestamp.to_rfc3339(),
                r.model.as_str(),
                r.model_id,
                u64_to_sql_i64("input_tokens", r.input_tokens)?,
                u64_to_sql_i64("output_tokens", r.output_tokens)?,
                u64_to_sql_i64("cache_creation_tokens", r.cache_creation_tokens)?,
                u64_to_sql_i64("cache_read_tokens", r.cache_read_tokens)?,
                u64_to_sql_i64("cached_input_tokens", r.cached_input_tokens)?,
                u64_to_sql_i64("reasoning_output_tokens", r.reasoning_output_tokens)?,
                r.cost_usd,
                r.project,
                r.source_file,
                r.is_subagent as i32,
            ])?;
            inserted += changed;
        }
    }

    tx.commit()?;
    Ok(inserted)
}

fn validate_record(record: &TokenRecord) -> Result<()> {
    ensure_non_empty("provider", &record.provider)?;
    ensure_non_empty("request_id", &record.request_id)?;
    ensure_non_empty("model_id", &record.model_id)?;
    ensure_non_empty("source_file", &record.source_file)?;
    if !record.cost_usd.is_finite() {
        bail!("cost_usd must be finite");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::usage::ModelFamily;
    use chrono::Utc;

    fn make_record(request_id: &str, output_tokens: u64) -> TokenRecord {
        TokenRecord {
            provider: "claude".into(),
            request_id: request_id.into(),
            session_id: "sess1".into(),
            uuid: "uuid1".into(),
            timestamp: Utc::now(),
            model: ModelFamily::Opus,
            model_id: "claude-opus-4-6".into(),
            input_tokens: 10,
            output_tokens,
            cache_creation_tokens: 100,
            cache_read_tokens: 500,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
            cost_usd: 0.05,
            project: "test".into(),
            source_file: "/test.jsonl".into(),
            is_subagent: false,
        }
    }

    #[test]
    fn test_batch_insert_basic() {
        let db = Database::open_in_memory().unwrap();
        let records = vec![make_record("r1", 10), make_record("r2", 20)];
        let count = db.insert_records(&records).unwrap();
        assert_eq!(count, 2);

        let total: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 2);
    }

    #[test]
    fn test_batch_insert_dedup_ignores_existing() {
        let db = Database::open_in_memory().unwrap();
        let records = vec![make_record("r1", 10)];
        assert_eq!(db.insert_records(&records).unwrap(), 1);
        assert_eq!(db.insert_records(&records).unwrap(), 0);
    }

    #[test]
    fn test_batch_insert_empty() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.insert_records(&[]).unwrap(), 0);
    }

    #[test]
    fn test_inserted_values_correct() {
        let db = Database::open_in_memory().unwrap();
        db.insert_records(&[make_record("r1", 42)]).unwrap();

        let (provider, model_id, output, cost): (String, String, i64, f64) = db
            .conn()
            .query_row(
                "SELECT provider, model_id, output_tokens, cost_usd FROM token_usage WHERE request_id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(provider, "claude");
        assert_eq!(model_id, "claude-opus-4-6");
        assert_eq!(output, 42);
        assert!((cost - 0.05).abs() < 0.001);
    }

    #[test]
    fn test_same_request_id_from_different_providers_do_not_collide() {
        let db = Database::open_in_memory().unwrap();
        let claude = make_record("shared-request", 42);
        let mut codex = make_record("shared-request", 84);
        codex.provider = "codex".into();
        codex.model = ModelFamily::Unknown;
        codex.model_id = "gpt-5.1-codex".into();

        assert_eq!(db.insert_records(&[claude, codex]).unwrap(), 2);

        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM token_usage WHERE request_id = 'shared-request'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_insert_allows_i64_max_token_value() {
        let db = Database::open_in_memory().unwrap();
        let mut record = make_record("max", 0);
        record.input_tokens = i64::MAX as u64;
        record.cache_creation_tokens = 0;
        record.cache_read_tokens = 0;
        assert_eq!(db.insert_records(&[record]).unwrap(), 1);

        let stored: i64 = db
            .conn()
            .query_row(
                "SELECT input_tokens FROM token_usage WHERE request_id = 'max'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, i64::MAX);
    }

    #[test]
    fn test_insert_rejects_token_values_above_i64_max() {
        let db = Database::open_in_memory().unwrap();
        let mut record = make_record("overflow", 0);
        record.input_tokens = i64::MAX as u64 + 1;

        let err = db.insert_records(&[record]).unwrap_err().to_string();
        assert!(err.contains("input_tokens"));
        assert!(err.contains("exceeds SQLite INTEGER range"));

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_insert_rejects_empty_required_identity_fields() {
        let db = Database::open_in_memory().unwrap();
        let mut record = make_record("bad", 10);
        record.provider.clear();

        let err = db.insert_records(&[record]).unwrap_err().to_string();
        assert!(err.contains("provider must not be empty"));

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
