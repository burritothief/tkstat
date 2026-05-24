#!/usr/bin/env bash
set -euo pipefail

# Print a compact, human-readable summary of a tkstat SQLite database.
#
# Use this when debugging a local database, a copied user database, or a temp DB
# left behind by KEEP_TMP=1. The script never mutates the database. If TKSTAT_BIN
# is set, or a tkstat binary is available on PATH, it also runs pricing audit
# against the same database.
#
# Examples:
#   scripts/db_inspect.sh ~/.local/share/tkstat/tkstat.db
#   TKSTAT_BIN=target/debug/tkstat scripts/db_inspect.sh /tmp/tkstat.db
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

usage() {
  cat <<'USAGE'
Usage: scripts/db_inspect.sh DB_PATH

Environment:
  TKSTAT_BIN  Optional tkstat binary used for --pricing-audit.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

db="$1"

if ! command -v sqlite3 >/dev/null 2>&1; then
  printf 'sqlite3 is required for database inspection\n' >&2
  exit 1
fi

if [[ ! -f "$db" ]]; then
  printf 'database does not exist: %s\n' "$db" >&2
  exit 1
fi

section() {
  printf '\n== %s ==\n' "$1"
}

sql() {
  sqlite3 -header -column "$db" "$1"
}

scalar() {
  sqlite3 "$db" "$1"
}

table_exists() {
  [[ "$(scalar "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '$1';")" != "0" ]]
}

section "Database"
printf 'path: %s\n' "$db"
printf 'size: %s bytes\n' "$(wc -c <"$db" | tr -d ' ')"

section "Timezone Note"
printf 'raw token_usage.timestamp and pricing_intervals effective_* values are UTC RFC3339 instants\n'
printf 'tkstat report periods render in the system local timezone by default; use --utc for UTC report buckets\n'

if table_exists schema_version; then
  section "Schema"
  sql "SELECT version FROM schema_version LIMIT 1;"
else
  section "Schema"
  printf 'schema_version table is missing\n'
fi

section "Tables"
sql "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'index') ORDER BY type, name;"

if table_exists token_usage; then
  section "Usage Overview"
  sql "SELECT
         COUNT(*) AS requests,
         COUNT(DISTINCT provider || ':' || session_id) AS sessions,
         COUNT(DISTINCT provider) AS providers,
         COUNT(DISTINCT provider || ':' || model_id) AS models,
         MIN(timestamp) AS first_usage,
         MAX(timestamp) AS last_usage,
         SUM(total_tokens) AS total_tokens,
         printf('%.6f', COALESCE(SUM(cost_usd), 0)) AS cost_usd
       FROM token_usage;"

  section "Providers"
  sql "SELECT
         provider,
         COUNT(*) AS requests,
         COUNT(DISTINCT session_id) AS sessions,
         COUNT(DISTINCT model_id) AS models,
         SUM(total_tokens) AS tokens,
         printf('%.6f', COALESCE(SUM(cost_usd), 0)) AS cost_usd,
         MIN(timestamp) AS first_usage,
         MAX(timestamp) AS last_usage
       FROM token_usage
       GROUP BY provider
       ORDER BY provider;"

  section "Top Models"
  sql "SELECT
         provider,
         model_id,
         COUNT(*) AS requests,
         SUM(total_tokens) AS tokens,
         printf('%.6f', COALESCE(SUM(cost_usd), 0)) AS cost_usd
       FROM token_usage
       GROUP BY provider, model_id
       ORDER BY tokens DESC, provider ASC, model_id ASC
       LIMIT 20;"

  section "Recent Usage Rows"
  sql "SELECT
         timestamp,
         provider,
         model_id,
         project,
         total_tokens,
         printf('%.6f', cost_usd) AS cost_usd
       FROM token_usage
       ORDER BY timestamp DESC
       LIMIT 10;"
else
  section "Usage"
  printf 'token_usage table is missing\n'
fi

if table_exists pricing_intervals; then
  section "Pricing Overview"
  sql "SELECT
         provider,
         model_id,
         token_category,
         COUNT(*) AS intervals,
         MIN(effective_from) AS first_effective_from,
         COALESCE(MAX(effective_to), 'open') AS last_effective_to,
         GROUP_CONCAT(DISTINCT source) AS sources
       FROM pricing_intervals
       GROUP BY provider, model_id, token_category
       ORDER BY provider, model_id, token_category
       LIMIT 50;"
else
  section "Pricing"
  printf 'pricing_intervals table is missing\n'
fi

audit_bin="${TKSTAT_BIN:-}"
if [[ -z "$audit_bin" ]]; then
  if command -v tkstat >/dev/null 2>&1; then
    audit_bin="$(command -v tkstat)"
  elif [[ -x "$repo_root/target/debug/tkstat" ]]; then
    audit_bin="$repo_root/target/debug/tkstat"
  elif [[ -x "$repo_root/target/release/tkstat" ]]; then
    audit_bin="$repo_root/target/release/tkstat"
  fi
fi

if [[ -n "$audit_bin" && -x "$audit_bin" ]]; then
  section "Pricing Audit"
  "$audit_bin" --db "$db" --pricing-audit || true
else
  section "Pricing Audit"
  printf 'skipped; set TKSTAT_BIN or put tkstat on PATH\n'
fi
