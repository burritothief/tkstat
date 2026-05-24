use anyhow::Result;
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 8;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;

    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        )",
    )?;

    let current: Option<i64> = tx
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .ok();

    match current {
        Some(v) if v >= SCHEMA_VERSION => {}
        Some(v) => {
            eprintln!(
                "tkstat database schema {v} is older than {SCHEMA_VERSION}; rebuilding usage cache. Run `tkstat --force-update` if you need to force a clean reingest, and run `tkstat --pricing-seed` or `tkstat --pricing-refresh` if pricing is missing."
            );
            // Drop old tables and recreate (pre-1.0, no migration path needed)
            tx.execute_batch("DROP TABLE IF EXISTS token_usage; DROP TABLE IF EXISTS file_state;")?;
            create_tables(&tx)?;
            migrate_provider_ids(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
        None => {
            create_tables(&tx)?;
            migrate_provider_ids(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
    }

    tx.commit()?;
    Ok(())
}

fn migrate_provider_ids(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE OR IGNORE pricing_intervals
            SET provider = 'claude-code'
          WHERE provider = 'claude';
         DELETE FROM pricing_intervals
          WHERE provider = 'claude';",
    )?;
    Ok(())
}

fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            provider            TEXT NOT NULL,
            request_id          TEXT NOT NULL,
            session_id          TEXT NOT NULL,
            uuid                TEXT NOT NULL,
            timestamp           TEXT NOT NULL,
            model_family        TEXT NOT NULL,
            model_id            TEXT NOT NULL,
            input_tokens        INTEGER NOT NULL,
            output_tokens       INTEGER NOT NULL,
            cache_creation_tokens  INTEGER NOT NULL,
            cache_read_tokens   INTEGER NOT NULL,
            cached_input_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens        INTEGER GENERATED ALWAYS AS
                (input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens) STORED,
            cost_usd            REAL NOT NULL,
            project             TEXT NOT NULL,
            source_file         TEXT NOT NULL,
            is_subagent         INTEGER NOT NULL DEFAULT 0,
            UNIQUE(provider, request_id)
        );

        CREATE INDEX IF NOT EXISTS idx_usage_provider ON token_usage(provider);
        CREATE INDEX IF NOT EXISTS idx_usage_timestamp ON token_usage(timestamp);
        CREATE INDEX IF NOT EXISTS idx_usage_date ON token_usage(date(timestamp));
        CREATE INDEX IF NOT EXISTS idx_usage_date_model ON token_usage(date(timestamp), model_family);
        CREATE INDEX IF NOT EXISTS idx_usage_provider_model_id ON token_usage(provider, model_id);
        CREATE INDEX IF NOT EXISTS idx_usage_date_provider_model ON token_usage(date(timestamp), provider, model_id);
        CREATE INDEX IF NOT EXISTS idx_usage_session ON token_usage(session_id);
        CREATE INDEX IF NOT EXISTS idx_usage_project ON token_usage(project);

        CREATE TABLE IF NOT EXISTS file_state (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            provider          TEXT NOT NULL,
            path              TEXT NOT NULL,
            size_bytes        INTEGER NOT NULL,
            mtime_secs        INTEGER NOT NULL,
            last_byte_offset  INTEGER NOT NULL,
            last_ingested_at  TEXT NOT NULL,
            UNIQUE(provider, path)
        );

        CREATE TABLE IF NOT EXISTS pricing_intervals (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            provider            TEXT NOT NULL CHECK(provider <> ''),
            model_id            TEXT NOT NULL CHECK(model_id <> ''),
            token_category      TEXT NOT NULL CHECK(token_category <> ''),
            currency            TEXT NOT NULL DEFAULT 'USD' CHECK(currency <> ''),
            rate_per_1m_tokens  REAL NOT NULL CHECK(rate_per_1m_tokens >= 0),
            effective_from      TEXT NOT NULL CHECK(effective_from <> ''),
            effective_to        TEXT,
            source              TEXT NOT NULL CHECK(source <> ''),
            CHECK(effective_to IS NULL OR effective_to > effective_from),
            UNIQUE(provider, model_id, token_category, currency, effective_from)
        );

        CREATE INDEX IF NOT EXISTS idx_pricing_lookup
            ON pricing_intervals(provider, model_id, token_category, currency, effective_from, effective_to);
        CREATE INDEX IF NOT EXISTS idx_pricing_model
            ON pricing_intervals(provider, model_id);",
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
    fn test_old_schema_version_rebuilds_usage_cache_and_adds_pricing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (1);
             CREATE TABLE token_usage (request_id TEXT PRIMARY KEY);
             INSERT INTO token_usage (request_id) VALUES ('legacy');",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let usage_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count, 0);

        let pricing_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'pricing_intervals'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pricing_exists, 1);
    }

    #[test]
    fn test_old_claude_pricing_provider_id_migrates_to_claude_code() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (7);
             CREATE TABLE token_usage (request_id TEXT PRIMARY KEY);
             CREATE TABLE file_state (path TEXT PRIMARY KEY);
             CREATE TABLE pricing_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                token_category TEXT NOT NULL,
                currency TEXT NOT NULL DEFAULT 'USD',
                rate_per_1m_tokens REAL NOT NULL,
                effective_from TEXT NOT NULL,
                effective_to TEXT,
                source TEXT NOT NULL,
                UNIQUE(provider, model_id, token_category, currency, effective_from)
             );
             INSERT INTO pricing_intervals
                (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES
                ('claude', 'claude-opus-4-6', 'input', 'USD', 5.0, '2026-01-01T00:00:00Z', NULL, 'legacy'),
                ('claude-code', 'claude-opus-4-6', 'input', 'USD', 5.0, '2026-01-01T00:00:00Z', NULL, 'canonical'),
                ('codex', 'gpt-5.5', 'input', 'USD', 2.5, '2026-01-01T00:00:00Z', NULL, 'seed');",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let old_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude-code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0);
        assert_eq!(canonical_count, 1);
    }

    #[test]
    fn test_migration_rolls_back_table_rebuild_and_provider_rewrite_on_failure() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (7);
             CREATE TABLE token_usage (request_id TEXT PRIMARY KEY);
             INSERT INTO token_usage (request_id) VALUES ('legacy-usage');
             CREATE TABLE file_state (path TEXT PRIMARY KEY);
             INSERT INTO file_state (path) VALUES ('legacy.jsonl');
             CREATE TABLE pricing_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                token_category TEXT NOT NULL,
                currency TEXT NOT NULL DEFAULT 'USD',
                rate_per_1m_tokens REAL NOT NULL,
                effective_from TEXT NOT NULL,
                effective_to TEXT,
                source TEXT NOT NULL,
                UNIQUE(provider, model_id, token_category, currency, effective_from)
             );
             INSERT INTO pricing_intervals
                (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES
                ('claude', 'claude-opus-4-6', 'input', 'USD', 5.0, '2026-01-01T00:00:00Z', NULL, 'legacy'),
                ('claude-code', 'claude-opus-4-6', 'input', 'USD', 5.0, '2026-01-01T00:00:00Z', NULL, 'canonical'),
                ('codex', 'gpt-5.5', 'input', 'USD', 2.5, '2026-01-01T00:00:00Z', NULL, 'seed');
             CREATE TRIGGER fail_schema_version_insert
             BEFORE INSERT ON schema_version
             BEGIN
                SELECT RAISE(FAIL, 'injected schema version failure');
             END;",
        )
        .unwrap();

        let err = run_migrations(&conn).unwrap_err().to_string();
        assert!(err.contains("injected schema version failure"));

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        let legacy_usage: String = conn
            .query_row("SELECT request_id FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy_usage, "legacy-usage");
        let legacy_file: String = conn
            .query_row("SELECT path FROM file_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy_file, "legacy.jsonl");

        let old_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude-code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 1);
        assert_eq!(canonical_count, 1);
    }

    #[test]
    fn test_generated_column_works() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO token_usage (provider, request_id, session_id, uuid, timestamp, model_family, model_id, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent)
             VALUES ('claude-code', 'r1', 's1', 'u1', '2026-04-07T10:00:00Z', 'opus', 'claude-opus-4-6', 100, 200, 300, 400, 0, 0, 1.5, 'test', '/test.jsonl', 0)",
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

    #[test]
    fn test_provider_and_exact_model_schema_present_and_indexed() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(token_usage)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.contains(&"provider".to_string()));
        assert!(columns.contains(&"model_id".to_string()));
        assert!(columns.contains(&"cached_input_tokens".to_string()));
        assert!(columns.contains(&"reasoning_output_tokens".to_string()));

        let indexes: Vec<String> = conn
            .prepare("PRAGMA index_list(token_usage)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(indexes.contains(&"idx_usage_provider".to_string()));
        assert!(indexes.contains(&"idx_usage_provider_model_id".to_string()));
        assert!(indexes.contains(&"idx_usage_date_provider_model".to_string()));
    }

    #[test]
    fn test_file_state_schema_is_provider_aware() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(file_state)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.contains(&"provider".to_string()));
        assert!(columns.contains(&"path".to_string()));

        conn.execute(
            "INSERT INTO file_state (provider, path, size_bytes, mtime_secs, last_byte_offset, last_ingested_at)
             VALUES ('claude-code', '/same.jsonl', 1, 1, 1, datetime('now')),
                    ('codex', '/same.jsonl', 2, 2, 2, datetime('now'))",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_pricing_intervals_schema_present_and_indexed() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(pricing_intervals)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for column in [
            "provider",
            "model_id",
            "token_category",
            "currency",
            "rate_per_1m_tokens",
            "effective_from",
            "effective_to",
            "source",
        ] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }

        let indexes: Vec<String> = conn
            .prepare("PRAGMA index_list(pricing_intervals)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(indexes.contains(&"idx_pricing_lookup".to_string()));
        assert!(indexes.contains(&"idx_pricing_model".to_string()));
    }
}
