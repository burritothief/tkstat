use anyhow::Result;
use rusqlite::Connection;

use crate::domain::usage::TokenRecord;

/// Batch insert records using INSERT OR IGNORE for dedup.
/// Returns the count of newly inserted rows.
pub fn batch_insert(conn: &Connection, records: &[TokenRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0;

    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO token_usage
                (request_id, session_id, uuid, timestamp, model_family, model_raw,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cost_usd, project, source_file, is_subagent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;

        for r in records {
            let changed = stmt.execute(rusqlite::params![
                r.request_id,
                r.session_id,
                r.uuid,
                r.timestamp.to_rfc3339(),
                r.model.as_str(),
                r.model_raw,
                r.input_tokens as i64,
                r.output_tokens as i64,
                r.cache_creation_tokens as i64,
                r.cache_read_tokens as i64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::usage::ModelFamily;
    use chrono::Utc;

    fn make_record(request_id: &str, output_tokens: u64) -> TokenRecord {
        TokenRecord {
            request_id: request_id.into(),
            session_id: "sess1".into(),
            uuid: "uuid1".into(),
            timestamp: Utc::now(),
            model: ModelFamily::Opus,
            model_raw: "claude-opus-4-6".into(),
            input_tokens: 10,
            output_tokens,
            cache_creation_tokens: 100,
            cache_read_tokens: 500,
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

        let (output, cost): (i64, f64) = db
            .conn()
            .query_row(
                "SELECT output_tokens, cost_usd FROM token_usage WHERE request_id = 'r1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(output, 42);
        assert!((cost - 0.05).abs() < 0.001);
    }
}
