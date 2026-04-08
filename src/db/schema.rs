use anyhow::Result;
use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 3;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        )",
    )?;

    let current: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .ok();

    match current {
        Some(v) if v >= SCHEMA_VERSION => {}
        _ => {
            // Drop old tables and recreate (pre-1.0, no migration path needed)
            conn.execute_batch(
                "DROP TABLE IF EXISTS token_usage; DROP TABLE IF EXISTS file_state;",
            )?;
            create_tables(conn)?;
            set_version(conn, SCHEMA_VERSION)?;
        }
    }

    Ok(())
}

fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS token_usage (
            request_id          TEXT PRIMARY KEY,
            session_id          TEXT NOT NULL,
            uuid                TEXT NOT NULL,
            timestamp           TEXT NOT NULL,
            model_family        TEXT NOT NULL,
            model_raw           TEXT NOT NULL,
            input_tokens        INTEGER NOT NULL,
            output_tokens       INTEGER NOT NULL,
            cache_creation_tokens  INTEGER NOT NULL,
            cache_read_tokens   INTEGER NOT NULL,
            total_tokens        INTEGER GENERATED ALWAYS AS
                (input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens) STORED,
            cost_usd            REAL NOT NULL,
            project             TEXT NOT NULL,
            source_file         TEXT NOT NULL,
            is_subagent         INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON token_usage(timestamp);
        CREATE INDEX IF NOT EXISTS idx_usage_date ON token_usage(date(timestamp));
        CREATE INDEX IF NOT EXISTS idx_usage_date_model ON token_usage(date(timestamp), model_family);
        CREATE INDEX IF NOT EXISTS idx_usage_session ON token_usage(session_id);
        CREATE INDEX IF NOT EXISTS idx_usage_project ON token_usage(project);

        CREATE TABLE IF NOT EXISTS file_state (
            path              TEXT PRIMARY KEY,
            size_bytes        INTEGER NOT NULL,
            mtime_secs        INTEGER NOT NULL,
            last_byte_offset  INTEGER NOT NULL,
            last_ingested_at  TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn set_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        [version],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrations_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn test_schema_version_set() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn test_generated_column_works() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO token_usage (request_id, session_id, uuid, timestamp, model_family, model_raw, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cost_usd, project, source_file, is_subagent)
             VALUES ('r1', 's1', 'u1', '2026-04-07T10:00:00Z', 'opus', 'claude-opus-4-6', 100, 200, 300, 400, 1.5, 'test', '/test.jsonl', 0)",
            [],
        ).unwrap();

        let total: i64 = conn
            .query_row(
                "SELECT total_tokens FROM token_usage WHERE request_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 1000);
    }
}
