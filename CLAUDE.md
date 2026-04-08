# CLAUDE.md

## Project

`tkstat` is a Rust CLI that monitors Claude Code token usage in the visual style of vnstat. It parses Claude Code's JSONL session logs, stores them in a local SQLite database for fast queries, and renders tables/charts to the terminal.

## Build & Test

```
cargo build --release
cargo test
```

The release binary lands at `target/release/tkstat`. Tests run in-memory (no disk or network).

## Architecture

```
src/
  main.rs          - Entry point: parse CLI, ingest, dispatch renderer
  cli.rs           - clap derive structs, flag resolution
  config.rs        - Data dir / DB path resolution
  error.rs         - Error types
  db/              - SQLite layer (schema, batch insert, aggregation queries)
  ingest/          - JSONL file discovery, parsing, dedup, incremental ingestion
  model/           - Domain types (TokenRecord, ModelFamily, TimePeriod, cost pricing)
  render/          - Output renderers (table, heatmap, braille chart, json, summary, oneline)
    columns.rs     - Column enum, parsing, default set
```

## Key Design Decisions

- **Incremental ingestion**: The `file_state` table tracks byte offsets per JSONL file. Only new bytes are parsed on subsequent runs. This is why warm starts are ~40ms.
- **Dedup by request_id**: Claude Code writes multiple JSONL entries per API request (streaming). We keep the one with the highest `output_tokens` per `request_id`. The DB uses `INSERT OR IGNORE` on `request_id` as PRIMARY KEY.
- **memchr pre-filter**: Before deserializing each JSONL line, we check for `"type":"assistant"` with a raw byte scan. This skips ~60% of lines without touching serde.
- **Gap-filling**: After SQL aggregation, missing time slots are filled with zero-rows so tables show continuous time series. The SQL fetches all matching rows (no LIMIT), gap-fills, then truncates to the requested limit from the end.
- **Cost at ingest time**: `estimated_cost_usd` is computed from compiled-in pricing and stored per row. If pricing changes, `--force-update` re-ingests everything.

## Data Source

Claude Code logs live at `~/.claude/projects/*/UUID.jsonl`. Only `"type":"assistant"` entries with a `usage` object and non-empty `requestId` are relevant. The `<synthetic>` model is skipped.

## Conventions

- Run `cargo test` before committing. All tests should pass with zero warnings.
- Keep the test count high — every module has inline `#[cfg(test)]` tests.
- No async — this is a sync CLI that aims for <50ms response times.
- Prefer `anyhow::Result` in application code, `thiserror` for the error enum.
- Don't add dependencies without a strong reason. The binary is 2.8MB stripped.
