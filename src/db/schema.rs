use anyhow::Result;
use rusqlite::Connection;

use crate::domain::pricing::billable_usage_components;
use crate::domain::provider::ProviderId;
use crate::domain::timestamp::parse_canonical_utc_rfc3339;
use crate::domain::usage::{ModelFamily, TokenRecord};

pub const SCHEMA_VERSION: i64 = 10;

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
        Some(v) if v >= 9 => {
            eprintln!(
                "tkstat database schema {v} is older than {SCHEMA_VERSION}; adding pricing modifier dimensions."
            );
            migrate_provider_ids(&tx)?;
            migrate_pricing_dimensions(&tx)?;
            create_tables(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
        Some(v) if v >= 8 => {
            eprintln!(
                "tkstat database schema {v} is older than {SCHEMA_VERSION}; adding normalized billing components and pricing modifier dimensions for existing usage rows."
            );
            migrate_provider_ids(&tx)?;
            migrate_pricing_dimensions(&tx)?;
            create_tables(&tx)?;
            backfill_usage_billing_components(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
        Some(v) => {
            eprintln!(
                "tkstat database schema {v} is older than {SCHEMA_VERSION}; rebuilding usage cache. Run `tkstat --force-update` if you need to force a clean reingest, and run `tkstat --pricing-seed` or `tkstat --pricing-refresh` if pricing is missing."
            );
            // Drop old tables and recreate (pre-1.0, no migration path needed)
            tx.execute_batch(
                "DROP TABLE IF EXISTS usage_billing_components;
                 DROP TABLE IF EXISTS token_usage;
                 DROP TABLE IF EXISTS file_state;",
            )?;
            migrate_provider_ids(&tx)?;
            migrate_pricing_dimensions(&tx)?;
            create_tables(&tx)?;
            backfill_usage_billing_components(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
        None => {
            create_tables(&tx)?;
            migrate_provider_ids(&tx)?;
            backfill_usage_billing_components(&tx)?;
            set_version(&tx, SCHEMA_VERSION)?;
        }
    }

    tx.commit()?;
    Ok(())
}

fn migrate_provider_ids(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "pricing_intervals")? {
        return Ok(());
    }
    conn.execute_batch(
        "UPDATE OR IGNORE pricing_intervals
            SET provider = 'claude-code'
          WHERE provider = 'claude';
         DELETE FROM pricing_intervals
          WHERE provider = 'claude';",
    )?;
    Ok(())
}

fn migrate_pricing_dimensions(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "pricing_intervals")? || pricing_intervals_has_dimensions(conn)? {
        return Ok(());
    }

    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_pricing_lookup;
         DROP INDEX IF EXISTS idx_pricing_model;
         ALTER TABLE pricing_intervals RENAME TO pricing_intervals_old;

         CREATE TABLE pricing_intervals (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            provider            TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
            model_id            TEXT NOT NULL CHECK(model_id <> ''),
            token_category      TEXT NOT NULL CHECK(token_category <> ''),
            service_tier        TEXT,
            speed               TEXT,
            region              TEXT,
            processing_mode     TEXT,
            source_detail       TEXT,
            currency            TEXT NOT NULL DEFAULT 'USD' CHECK(currency <> ''),
            rate_per_1m_tokens  REAL NOT NULL CHECK(rate_per_1m_tokens >= 0),
            effective_from      TEXT NOT NULL CHECK(effective_from <> ''),
            effective_to        TEXT,
            source              TEXT NOT NULL CHECK(source <> ''),
            CHECK(effective_to IS NULL OR effective_to > effective_from)
         );

         INSERT INTO pricing_intervals
            (provider, model_id, token_category, service_tier, speed, region, processing_mode,
             source_detail, currency, rate_per_1m_tokens, effective_from, effective_to, source)
         SELECT provider, model_id, token_category, NULL, NULL, NULL, NULL, NULL,
                currency, rate_per_1m_tokens, effective_from, effective_to, source
         FROM pricing_intervals_old;

         DROP TABLE pricing_intervals_old;",
    )?;
    create_pricing_indexes(conn)?;
    Ok(())
}

fn pricing_intervals_has_dimensions(conn: &Connection) -> Result<bool> {
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(pricing_intervals)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok([
        "service_tier",
        "speed",
        "region",
        "processing_mode",
        "source_detail",
    ]
    .iter()
    .all(|column| columns.iter().any(|existing| existing == column)))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            provider            TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
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

        CREATE TABLE IF NOT EXISTS usage_billing_components (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            usage_id            INTEGER NOT NULL,
            provider            TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
            request_id          TEXT NOT NULL CHECK(request_id <> ''),
            model_id            TEXT NOT NULL CHECK(model_id <> ''),
            timestamp           TEXT NOT NULL CHECK(timestamp <> ''),
            token_category      TEXT NOT NULL CHECK(token_category <> ''),
            tokens              INTEGER NOT NULL CHECK(tokens > 0),
            service_tier        TEXT,
            speed               TEXT,
            region              TEXT,
            processing_mode     TEXT,
            source_detail       TEXT,
            component_ordinal   INTEGER NOT NULL,
            UNIQUE(provider, request_id, component_ordinal)
        );

        CREATE INDEX IF NOT EXISTS idx_billing_components_usage
            ON usage_billing_components(provider, request_id);
        CREATE INDEX IF NOT EXISTS idx_billing_components_lookup
            ON usage_billing_components(provider, model_id, token_category, timestamp);

        CREATE TABLE IF NOT EXISTS file_state (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            provider          TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
            path              TEXT NOT NULL,
            size_bytes        INTEGER NOT NULL,
            mtime_secs        INTEGER NOT NULL,
            last_byte_offset  INTEGER NOT NULL,
            last_ingested_at  TEXT NOT NULL,
            UNIQUE(provider, path)
        );

        CREATE TABLE IF NOT EXISTS pricing_intervals (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            provider            TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
            model_id            TEXT NOT NULL CHECK(model_id <> ''),
            token_category      TEXT NOT NULL CHECK(token_category <> ''),
            service_tier        TEXT,
            speed               TEXT,
            region              TEXT,
            processing_mode     TEXT,
            source_detail       TEXT,
            currency            TEXT NOT NULL DEFAULT 'USD' CHECK(currency <> ''),
            rate_per_1m_tokens  REAL NOT NULL CHECK(rate_per_1m_tokens >= 0),
            effective_from      TEXT NOT NULL CHECK(effective_from <> ''),
            effective_to        TEXT,
            source              TEXT NOT NULL CHECK(source <> ''),
            CHECK(effective_to IS NULL OR effective_to > effective_from)
        );",
    )?;
    create_pricing_indexes(conn)?;
    Ok(())
}

fn create_pricing_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pricing_unique_key
            ON pricing_intervals(
                provider,
                model_id,
                token_category,
                currency,
                COALESCE(service_tier, ''),
                COALESCE(speed, ''),
                COALESCE(region, ''),
                COALESCE(processing_mode, ''),
                COALESCE(source_detail, ''),
                effective_from
            );
        CREATE INDEX IF NOT EXISTS idx_pricing_lookup
            ON pricing_intervals(
                provider,
                model_id,
                token_category,
                currency,
                service_tier,
                speed,
                region,
                processing_mode,
                source_detail,
                effective_from,
                effective_to
            );
        CREATE INDEX IF NOT EXISTS idx_pricing_model
            ON pricing_intervals(provider, model_id);",
    )?;
    Ok(())
}

fn backfill_usage_billing_components(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM usage_billing_components", [])?;
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT id, provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                    input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                    cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent
             FROM token_usage
             ORDER BY id ASC",
        )?;
        stmt.query_map([], row_to_token_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut insert = conn.prepare_cached(
        "INSERT INTO usage_billing_components
            (usage_id, provider, request_id, model_id, timestamp, token_category, tokens,
             service_tier, speed, region, processing_mode, source_detail, component_ordinal)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for (usage_id, record) in rows {
        insert_billing_components(&mut insert, usage_id, &record)?;
    }
    Ok(())
}

pub(crate) fn insert_billing_components(
    stmt: &mut rusqlite::CachedStatement<'_>,
    usage_id: i64,
    record: &TokenRecord,
) -> Result<()> {
    for (idx, component) in billable_usage_components(record).into_iter().enumerate() {
        stmt.execute(rusqlite::params![
            usage_id,
            component.provider.as_str(),
            record.request_id,
            component.model_id,
            crate::domain::timestamp::format_utc_rfc3339(component.timestamp),
            component.token_category.as_str(),
            crate::db::u64_to_sql_i64("billing component tokens", component.tokens)?,
            component.service_tier,
            component.speed,
            component.region,
            component.processing_mode,
            component.source_detail,
            idx as i64,
        ])?;
    }
    Ok(())
}

fn row_to_token_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, TokenRecord)> {
    let provider: String = row.get(1)?;
    let timestamp: String = row.get(5)?;
    let model_family: String = row.get(6)?;
    let provider = ProviderId::from_canonical(&provider).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unsupported provider id '{provider}'").into(),
        )
    })?;
    let timestamp = parse_canonical_utc_rfc3339(&timestamp).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, err.into())
    })?;
    Ok((
        row.get(0)?,
        TokenRecord {
            provider,
            request_id: row.get(2)?,
            session_id: row.get(3)?,
            uuid: row.get(4)?,
            timestamp,
            model: model_family.parse().unwrap_or(ModelFamily::Unknown),
            model_id: row.get(7)?,
            input_tokens: row.get::<_, i64>(8)?.max(0) as u64,
            output_tokens: row.get::<_, i64>(9)?.max(0) as u64,
            cache_creation_tokens: row.get::<_, i64>(10)?.max(0) as u64,
            cache_read_tokens: row.get::<_, i64>(11)?.max(0) as u64,
            cached_input_tokens: row.get::<_, i64>(12)?.max(0) as u64,
            reasoning_output_tokens: row.get::<_, i64>(13)?.max(0) as u64,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            service_tier: None,
            speed: None,
            region: None,
            processing_mode: None,
            cost_usd: row.get(14)?,
            project: row.get(15)?,
            source_file: row.get(16)?,
            is_subagent: row.get::<_, i64>(17)? != 0,
        },
    ))
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
    fn test_schema_v8_backfills_billing_components_without_losing_display_totals() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (8);
             CREATE TABLE token_usage (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                provider            TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
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
             CREATE TABLE file_state (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                provider          TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
                path              TEXT NOT NULL,
                size_bytes        INTEGER NOT NULL,
                mtime_secs        INTEGER NOT NULL,
                last_byte_offset  INTEGER NOT NULL,
                last_ingested_at  TEXT NOT NULL,
                UNIQUE(provider, path)
             );
             INSERT INTO token_usage
                (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent)
             VALUES
                ('claude-code', 'claude-r', 's1', 'u1', '2026-04-07T10:00:00+00:00',
                 'opus', 'claude-opus-4-6', 10, 20, 30, 40, 0, 0, 0.0, 'project', '/claude.jsonl', 0),
                ('codex', 'codex-r', 's2', 'u2', '2026-05-24T00:40:04+00:00',
                 'unknown', 'gpt-5.5', 100, 20, 0, 0, 40, 7, 0.0, 'project', '/codex.jsonl', 0);",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let (usage_count, display_total): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(total_tokens) FROM token_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(usage_count, 2);
        assert_eq!(display_total, 220);

        let component_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_billing_components", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(component_count, 7);

        let claude_components: Vec<(String, i64)> = conn
            .prepare(
                "SELECT token_category, tokens
                 FROM usage_billing_components
                 WHERE provider = 'claude-code' AND request_id = 'claude-r'
                 ORDER BY component_ordinal",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            claude_components,
            vec![
                ("input".into(), 10),
                ("output".into(), 20),
                ("cache_read".into(), 40),
                ("cache_creation".into(), 30),
            ]
        );

        let codex_components: Vec<(String, i64)> = conn
            .prepare(
                "SELECT token_category, tokens
                 FROM usage_billing_components
                 WHERE provider = 'codex' AND request_id = 'codex-r'
                 ORDER BY component_ordinal",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            codex_components,
            vec![
                ("input".into(), 60),
                ("output".into(), 20),
                ("cached_input".into(), 40),
            ]
        );
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
    fn test_schema_v9_adds_pricing_dimension_columns_without_losing_intervals() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (9);
             CREATE TABLE pricing_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL CHECK(provider IN ('claude-code', 'codex')),
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
                ('codex', 'gpt-5.5', 'input', 'USD', 2.5, '2026-01-01T00:00:00+00:00', NULL, 'legacy-v9');",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(pricing_intervals)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for column in [
            "service_tier",
            "speed",
            "region",
            "processing_mode",
            "source_detail",
        ] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }

        let (count, rate, speed): (i64, f64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MAX(rate_per_1m_tokens), MAX(speed) FROM pricing_intervals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(rate, 2.5);
        assert_eq!(speed, None);
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
            "service_tier",
            "speed",
            "region",
            "processing_mode",
            "source_detail",
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
        assert!(indexes.contains(&"idx_pricing_unique_key".to_string()));
        assert!(indexes.contains(&"idx_pricing_lookup".to_string()));
        assert!(indexes.contains(&"idx_pricing_model".to_string()));
    }

    #[test]
    fn test_usage_billing_components_schema_present_and_indexed() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(usage_billing_components)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for column in [
            "usage_id",
            "provider",
            "request_id",
            "model_id",
            "timestamp",
            "token_category",
            "tokens",
            "service_tier",
            "speed",
            "region",
            "processing_mode",
            "source_detail",
            "component_ordinal",
        ] {
            assert!(columns.contains(&column.to_string()), "missing {column}");
        }

        let indexes: Vec<String> = conn
            .prepare("PRAGMA index_list(usage_billing_components)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(indexes.contains(&"idx_billing_components_usage".to_string()));
        assert!(indexes.contains(&"idx_billing_components_lookup".to_string()));
    }

    #[test]
    fn test_new_schema_rejects_unsupported_provider_ids() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let usage_err = conn
            .execute(
                "INSERT INTO token_usage
                    (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                     input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                     cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent)
                 VALUES ('bogus', 'r1', 's1', 'u1', '2026-04-07T10:00:00Z', 'unknown', 'm1',
                         1, 0, 0, 0, 0, 0, 0.0, 'project', '/usage.jsonl', 0)",
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(usage_err.contains("CHECK"));

        let file_state_err = conn
            .execute(
                "INSERT INTO file_state
                    (provider, path, size_bytes, mtime_secs, last_byte_offset, last_ingested_at)
                 VALUES ('bogus', '/usage.jsonl', 1, 1, 1, '2026-04-07T10:00:00Z')",
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(file_state_err.contains("CHECK"));

        let pricing_err = conn
            .execute(
                "INSERT INTO pricing_intervals
                    (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
                 VALUES ('bogus', 'm1', 'input', 'USD', 1.0, '2026-01-01T00:00:00Z', NULL, 'test')",
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(pricing_err.contains("CHECK"));

        let component_err = conn
            .execute(
                "INSERT INTO usage_billing_components
                    (usage_id, provider, request_id, model_id, timestamp, token_category, tokens, component_ordinal)
                 VALUES (1, 'bogus', 'r1', 'm1', '2026-04-07T10:00:00+00:00', 'input', 1, 0)",
                [],
            )
            .unwrap_err()
            .to_string();
        assert!(component_err.contains("CHECK"));
    }
}
