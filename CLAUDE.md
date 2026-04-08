# CLAUDE.md

## Project

`tkstat` is a Rust CLI that monitors Claude Code token usage in the visual style of vnstat. It parses Claude Code's JSONL session logs, stores them in a local SQLite database for fast queries, and renders tables/charts to the terminal.

## Build & Test

```
cargo build --release
cargo test
cargo clippy -- -D warnings
```

The release binary lands at `target/release/tkstat`. Tests run in-memory (no disk, no network, no filesystem). Every PR must pass all three commands above with zero warnings.

## Architecture

```
src/
  lib.rs           - Public module declarations (this is a library + binary)
  main.rs          - Binary entry point only: parse CLI, call lib
  cli.rs           - clap derive structs, OutputMode enum, ChartMetric enum
  config.rs        - Data dir / DB path resolution
  error.rs         - thiserror error types
  domain/          - Core domain types, no IO
    usage.rs       - TokenRecord, AggregatedRow, ModelFamily, formatting
    pricing.rs     - Static per-model pricing lookup
    period.rs      - TimePeriod enum, SQL bucketing expressions
  db/              - SQLite layer
    schema.rs      - CREATE TABLE, migrations, version tracking
    ingest.rs      - Batch INSERT OR IGNORE
    query.rs       - Aggregation queries, gap-filling, WHERE builder
  ingest/          - JSONL file discovery and parsing
    walker.rs      - Filesystem traversal, SourceFile metadata
    parser.rs      - JSONL serde types, memchr pre-filter, dedup
  render/          - All output renderers
    table.rs       - vnstat-style ASCII table with configurable columns
    columns.rs     - Column enum with FromStr, parsing, default set
    heatmap.rs     - GitHub-style contribution calendar (Vega blues palette)
    braille.rs     - Braille-dot time series chart via drawille
    summary.rs     - Short summary mode
    json.rs        - JSON output (uses Serialize derive on AggregatedRow)
    oneline.rs     - Single-line semicolon-delimited output
```

## Key Design Decisions

- **lib.rs + main.rs split**: The library is reusable. main.rs is just CLI wiring. All logic lives in lib.rs modules.
- **Incremental ingestion**: The `file_state` table tracks byte offsets per JSONL file. Only new bytes are parsed on subsequent runs. Warm starts are ~30-40ms.
- **Dedup by request_id**: Claude Code writes multiple JSONL entries per API request (streaming). We keep the one with the highest `output_tokens` per `request_id`. The DB uses `INSERT OR IGNORE` on `request_id` as PRIMARY KEY.
- **memchr pre-filter**: Before deserializing each JSONL line, we check for `"type":"assistant"` with a raw byte scan. This skips ~60% of lines without touching serde.
- **Gap-filling**: After SQL aggregation, missing time slots are filled with zero-rows so tables show continuous time series. The SQL fetches all matching rows (no LIMIT), gap-fills, then truncates to the requested limit from the end.
- **Cost at ingest time**: `cost_usd` is computed from static pricing tables and stored per row. If pricing changes, `--force-update` re-ingests everything.
- **OutputMode enum**: CLI flags resolve to a single `OutputMode` variant, which is matched in main.rs. No implicit flag priority or silent conflicts.
- **ChartMetric enum**: Uses `clap::ValueEnum` so invalid values are rejected at parse time with proper help text.
- **Log-scale heatmap**: Heatmap colors use logarithmic normalization so low-usage days are still visually distinct from zero-usage days.

## Data Source

Claude Code logs live at `~/.claude/projects/*/UUID.jsonl`. Only `"type":"assistant"` entries with a `usage` object and non-empty `requestId` are relevant. The `<synthetic>` model is skipped.

## Code Standards

### Naming

- Use `creation`/`read` terminology for cache tokens, matching the Anthropic API. The API field `cache_creation_input_tokens` maps to our `cache_creation_tokens`. The column header abbreviation is `cache cr`.
- Field names should be concise: `cost_usd` not `estimated_cost_usd`, `period` not `period_label`.
- Module names should be specific: `domain/` not `model/`, `usage.rs` not `tokens.rs`, `pricing.rs` not `cost.rs`.
- Serde deserialization types for JSONL use the `Jsonl` prefix (`JsonlEntry`, `JsonlMessage`, `JsonlUsage`), not `Raw`.
- Public API functions should be descriptive at the call site: `ingest::sync()` not `ingest::run()`.

### Error Handling

- Use `anyhow::Result` in application/binary code (main.rs, CLI wiring).
- Use `thiserror` for the `TkstatError` enum in `error.rs`.
- **Never swallow errors**: no `.filter_map(|r| r.ok())` on iterators of Results. Use `.collect::<Result<Vec<_>, _>>()?` to propagate.
- **No `.expect()` or `.unwrap()` in non-test code** unless the invariant is trivially provable (e.g., `NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()`). Prefer returning `Option`/`Result` or using `match`-based static lookups.

### Types and Traits

- Implement `FromStr` instead of inherent `from_str` methods. This enables `.parse::<T>()` and clap integration.
- Implement `Display` for types that need string representation.
- Derive `Serialize` on types that need JSON output rather than manually constructing `serde_json::Value`.
- Use enums with `clap::ValueEnum` for CLI options with fixed choices. Never match on raw strings from CLI args.
- Prefer static dispatch (`&'static T`, match arms) over HashMap for small fixed lookup tables (e.g., pricing).

### Performance

- No async. This is a sync CLI targeting <50ms response time.
- Minimize allocations in hot paths. The JSONL parser processes thousands of lines — use `&[u8]` splitting and `memchr` pre-filtering.
- Let SQLite do aggregation (`GROUP BY`, `SUM`). Don't fetch all rows into Rust and aggregate in-memory.
- Pre-compute derived values at ingest time (e.g., `cost_usd`) so queries can just `SUM`.

### Dependencies

Don't add dependencies without a strong reason. The binary is ~3MB stripped. Current deps and why:

- `clap` (derive) — CLI parsing, the standard
- `serde` + `serde_json` — JSONL deserialization
- `rusqlite` (bundled) — embedded SQLite, sync, zero async overhead
- `chrono` — date/time parsing and local timezone conversion
- `owo-colors` — zero-alloc terminal colors, respects NO_COLOR
- `drawille` — braille canvas for chart rendering
- `walkdir` — directory traversal
- `memchr` — fast byte scanning for JSONL pre-filter
- `anyhow` + `thiserror` — error handling
- `dirs` — XDG-compliant path resolution

Do NOT add: `tokio`, `async-std`, `reqwest`, `config-rs`, `comfy-table`, `textplots`. If you need HTTP for pricing updates, add it behind a feature flag.

## Testing

- Every module has inline `#[cfg(test)]` tests. If you add a function, add tests for it.
- Tests must not touch the real filesystem, network, or user's Claude data. Use `Database::open_in_memory()` for DB tests. Use synthetic JSONL byte strings for parser tests.
- Tests must not contain personal information, real usernames, or real local paths. Use generic names like `/home/alice/.claude/projects/...`.
- Test edge cases: empty data, single row, malformed JSON lines, zero values, missing fields.
- Run `cargo test` before every commit. Zero failures, zero warnings.
- Run `cargo clippy -- -D warnings` before every commit.

## SQLite Schema

Schema version is tracked in a `schema_version` table. When the version changes, tables are dropped and recreated (pre-1.0, no migration path). Bump the version constant in `schema.rs` if you change the schema.

The `total_tokens` column is `GENERATED ALWAYS AS (...) STORED` — never write to it directly.

## Commit Style

- Write commits in imperative mood: "Add heatmap renderer" not "Added heatmap renderer".
- Keep commits focused. One logical change per commit.
- Run tests before committing.
