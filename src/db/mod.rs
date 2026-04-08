pub mod ingest;
pub mod query;
pub mod schema;

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

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

    pub fn get_file_state(&self, path: &Path) -> Result<Option<FileState>> {
        let path_str = path.to_string_lossy();
        let mut stmt = self.conn.prepare_cached(
            "SELECT size_bytes, mtime_secs, last_byte_offset FROM file_state WHERE path = ?1",
        )?;
        let mut rows = stmt.query_map([path_str.as_ref()], |row| {
            Ok(FileState {
                size_bytes: row.get(0)?,
                mtime_secs: row.get(1)?,
                last_byte_offset: row.get(2)?,
            })
        })?;
        match rows.next() {
            Some(Ok(state)) => Ok(Some(state)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn update_file_state(
        &self,
        path: &Path,
        size_bytes: u64,
        mtime_secs: i64,
        last_byte_offset: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO file_state (path, size_bytes, mtime_secs, last_byte_offset, last_ingested_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            rusqlite::params![
                path.to_string_lossy().as_ref(),
                size_bytes as i64,
                mtime_secs,
                last_byte_offset,
            ],
        )?;
        Ok(())
    }

    pub fn reset(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM token_usage; DELETE FROM file_state;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db.conn()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_file_state_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        assert!(db.get_file_state(&path).unwrap().is_none());
        db.update_file_state(&path, 1024, 100000, 1024).unwrap();
        let state = db.get_file_state(&path).unwrap().unwrap();
        assert_eq!(state.size_bytes, 1024);
        assert_eq!(state.mtime_secs, 100000);
    }

    #[test]
    fn test_file_state_update() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        db.update_file_state(&path, 1024, 100000, 1024).unwrap();
        db.update_file_state(&path, 2048, 200000, 2048).unwrap();
        let state = db.get_file_state(&path).unwrap().unwrap();
        assert_eq!(state.size_bytes, 2048);
    }

    #[test]
    fn test_reset_clears_data() {
        let db = Database::open_in_memory().unwrap();
        let path = PathBuf::from("/test/file.jsonl");
        db.update_file_state(&path, 1024, 100000, 1024).unwrap();
        db.reset().unwrap();
        assert!(db.get_file_state(&path).unwrap().is_none());
    }
}
