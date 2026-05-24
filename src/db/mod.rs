pub mod ingest;
pub mod pricing;
pub mod query;
pub mod schema;

use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::Connection;

use crate::domain::pricing::PricingInterval;
use crate::domain::provider::ProviderId;
use crate::domain::usage::TokenRecord;

/// File tracking state for incremental ingestion.
#[derive(Debug, Clone)]
pub struct FileState {
    pub size_bytes: i64,
    pub mtime_secs: i64,
    pub last_byte_offset: i64,
}

/// Wrapper around a SQLite connection with domain-specific methods.
pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        schema::run_migrations(&self.conn)?;
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn insert_records(&self, records: &[TokenRecord]) -> Result<usize> {
        ingest::batch_insert(&self.conn, records)
    }

    pub fn insert_pricing_interval(&self, interval: &PricingInterval) -> Result<()> {
        pricing::validate_interval(interval)?;
        pricing::insert_interval(&self.conn, interval)
    }

    pub fn seed_pricing(&self) -> Result<usize> {
        pricing::seed_pricing(&self.conn)
    }

    pub fn refresh_pricing(&self, fetcher: &dyn pricing::PricingFetcher) -> Result<usize> {
        pricing::refresh_pricing(&self.conn, fetcher)
    }

    pub fn calculate_record_cost(&self, record: &TokenRecord) -> Result<f64> {
        pricing::calculate_record_cost(&self.conn, record)
    }

    pub fn get_file_state(&self, provider: ProviderId, path: &Path) -> Result<Option<FileState>> {
        let path_str = path.to_string_lossy();
        let mut stmt = self.conn.prepare_cached(
            "SELECT size_bytes, mtime_secs, last_byte_offset FROM file_state WHERE provider = ?1 AND path = ?2",
        )?;
        let mut rows = stmt.query_map(
            rusqlite::params![provider.as_str(), path_str.as_ref()],
            |row| {
                Ok(FileState {
                    size_bytes: row.get(0)?,
                    mtime_secs: row.get(1)?,
                    last_byte_offset: row.get(2)?,
                })
            },
        )?;
        match rows.next() {
            Some(Ok(state)) => Ok(Some(state)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn update_file_state(
        &self,
        provider: ProviderId,
        path: &Path,
        size_bytes: u64,
        mtime_secs: i64,
        last_byte_offset: u64,
    ) -> Result<()> {
        let size_bytes = u64_to_sql_i64("size_bytes", size_bytes)?;
        let last_byte_offset = u64_to_sql_i64("last_byte_offset", last_byte_offset)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO file_state (provider, path, size_bytes, mtime_secs, last_byte_offset, last_ingested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![
                provider.as_str(),
                path.to_string_lossy().as_ref(),
                size_bytes,
                mtime_secs,
                last_byte_offset,
            ],
        )?;
        Ok(())
    }

    pub fn reset(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM usage_billing_components;
             DELETE FROM token_usage;
             DELETE FROM file_state;",
        )?;
        Ok(())
    }
}

pub(crate) fn u64_to_sql_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| anyhow::anyhow!("{field} value {value} exceeds SQLite INTEGER range"))
}

pub(crate) fn ensure_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_file_state_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        assert!(
            db.get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
                .unwrap()
                .is_none()
        );
        db.update_file_state(
            crate::domain::provider::ProviderId::ClaudeCode,
            &path,
            1024,
            100000,
            1024,
        )
        .unwrap();
        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.size_bytes, 1024);
        assert_eq!(state.mtime_secs, 100000);
    }

    #[test]
    fn test_file_state_update() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        db.update_file_state(
            crate::domain::provider::ProviderId::ClaudeCode,
            &path,
            1024,
            100000,
            1024,
        )
        .unwrap();
        db.update_file_state(
            crate::domain::provider::ProviderId::ClaudeCode,
            &path,
            2048,
            200000,
            2048,
        )
        .unwrap();
        let state = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        assert_eq!(state.size_bytes, 2048);
    }

    #[test]
    fn test_file_state_provider_paths_do_not_collide() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/shared.jsonl");
        db.update_file_state(
            crate::domain::provider::ProviderId::ClaudeCode,
            &path,
            1024,
            100000,
            1024,
        )
        .unwrap();
        db.update_file_state(
            crate::domain::provider::ProviderId::Codex,
            &path,
            2048,
            200000,
            2048,
        )
        .unwrap();

        let claude = db
            .get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
            .unwrap()
            .unwrap();
        let codex = db
            .get_file_state(crate::domain::provider::ProviderId::Codex, &path)
            .unwrap()
            .unwrap();
        assert_eq!(claude.size_bytes, 1024);
        assert_eq!(codex.size_bytes, 2048);
    }

    #[test]
    fn test_file_state_rejects_values_above_sqlite_integer_range() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        let err = db
            .update_file_state(
                crate::domain::provider::ProviderId::ClaudeCode,
                &path,
                i64::MAX as u64 + 1,
                100000,
                1024,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("size_bytes"));
        assert!(err.contains("exceeds SQLite INTEGER range"));

        let err = db
            .update_file_state(
                crate::domain::provider::ProviderId::ClaudeCode,
                &path,
                1024,
                100000,
                i64::MAX as u64 + 1,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("last_byte_offset"));
    }

    #[test]
    fn test_reset_clears_data() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        db.insert_records(&[TokenRecord {
            provider: crate::domain::provider::ProviderId::ClaudeCode,
            request_id: "reset-r1".into(),
            session_id: "reset-s1".into(),
            uuid: "reset-u1".into(),
            timestamp: "2026-04-07T10:00:00+00:00".parse().unwrap(),
            model: crate::domain::usage::ModelFamily::Opus,
            model_id: "claude-opus-4-6".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: 30,
            cache_read_tokens: 40,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            service_tier: None,
            speed: None,
            region: None,
            processing_mode: None,
            cost_usd: 0.0,
            project: "test".into(),
            source_file: "/test/file.jsonl".into(),
            is_subagent: false,
        }])
        .unwrap();
        db.update_file_state(
            crate::domain::provider::ProviderId::ClaudeCode,
            &path,
            1024,
            100000,
            1024,
        )
        .unwrap();
        db.reset().unwrap();
        assert!(
            db.get_file_state(crate::domain::provider::ProviderId::ClaudeCode, &path)
                .unwrap()
                .is_none()
        );
        let usage_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count, 0);
        let component_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM usage_billing_components", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(component_count, 0);
    }

    #[test]
    fn test_reset_preserves_pricing_intervals() {
        use crate::domain::pricing::{PricingDimensions, PricingInterval, TokenCategory};

        let db = Database::open_in_memory().unwrap();
        db.seed_pricing().unwrap();
        let before: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(before > 0);

        let standard = PricingDimensions {
            processing_mode: Some("standard".into()),
            ..Default::default()
        };
        let interval = crate::db::pricing::applicable_interval_for_dimensions(
            db.conn(),
            crate::domain::provider::ProviderId::Codex,
            "gpt-5.4",
            TokenCategory::Input,
            "2026-05-24T00:00:00Z".parse().unwrap(),
            &standard,
        )
        .unwrap();
        assert_eq!(interval.model_id, "gpt-5.4");

        db.reset().unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after, before);

        let selected = crate::db::pricing::applicable_interval_for_dimensions(
            db.conn(),
            crate::domain::provider::ProviderId::Codex,
            "gpt-5.4",
            TokenCategory::Input,
            "2026-05-24T00:00:00Z".parse().unwrap(),
            &standard,
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, interval.rate_per_1m_tokens);

        let custom = PricingInterval::usd(
            crate::domain::provider::ProviderId::Codex,
            "gpt-reset-check",
            TokenCategory::Input,
            1.0,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "test",
        );
        db.insert_pricing_interval(&custom).unwrap();
        db.reset().unwrap();
        assert!(
            crate::db::pricing::applicable_interval(
                db.conn(),
                crate::domain::provider::ProviderId::Codex,
                "gpt-reset-check",
                TokenCategory::Input,
                "2026-05-24T00:00:00Z".parse().unwrap(),
            )
            .is_ok()
        );
    }
}
