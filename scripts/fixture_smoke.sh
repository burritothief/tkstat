#!/usr/bin/env bash
set -euo pipefail

# Verify the committed fixture corpus through the real tkstat CLI.
#
# Unlike e2e_smoke.sh, this script does not generate inline JSONL data. It uses
# only files under tests/fixtures, copies them into a temporary provider layout,
# and checks that ingest, pricing, provider grouping, model grouping, JSON, and
# CSV output still look coherent. Run this after editing fixtures or provider
# parsers.
#
# Examples:
#   scripts/fixture_smoke.sh
#   TKSTAT_BIN=target/release/tkstat scripts/fixture_smoke.sh
#   KEEP_TMP=1 scripts/fixture_smoke.sh
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
fixture_root="$repo_root/tests/fixtures"

usage() {
  cat <<'USAGE'
Usage: scripts/fixture_smoke.sh

Environment:
  TKSTAT_BIN  Use this tkstat binary instead of building target/debug/tkstat.
  KEEP_TMP    Set to 1 to keep the temporary database and provider trees.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
elif [[ $# -gt 0 ]]; then
  printf 'unknown argument: %s\n\n' "$1" >&2
  usage >&2
  exit 2
fi

if [[ ! -d "$fixture_root/claude" || ! -f "$fixture_root/codex/synthetic-codex-session.jsonl" ]]; then
  printf 'missing expected fixture files under %s\n' "$fixture_root" >&2
  exit 1
fi

bin="${TKSTAT_BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
  bin="$repo_root/target/debug/tkstat"
fi

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/tkstat-fixture-smoke.XXXXXX")"
db="$tmp_root/tkstat.db"
claude_projects="$tmp_root/claude/projects"
codex_home="$tmp_root/codex-home"
export HOME="$tmp_root/home"
export CLAUDE_CONFIG_DIR="$tmp_root/claude-config"
export CODEX_HOME="$codex_home"
export NO_COLOR=1

cleanup() {
  if [[ "${KEEP_TMP:-0}" == "1" ]]; then
    printf 'kept temp root: %s\n' "$tmp_root"
    printf 'kept database: %s\n' "$db"
  else
    rm -rf "$tmp_root"
  fi
}
trap cleanup EXIT

log_cmd() {
  {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  } >&2
}

run_capture() {
  log_cmd "$@"
  "$@"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" <<<"$haystack"; then
    printf 'expected output to contain: %s\noutput was:\n%s\n' "$needle" "$haystack" >&2
    exit 1
  fi
}

mkdir -p "$claude_projects" "$codex_home/sessions/2026/05/24"
cp -R "$fixture_root/claude/." "$claude_projects/"
cp "$fixture_root/codex/synthetic-codex-session.jsonl" \
  "$codex_home/sessions/2026/05/24/rollout-synthetic-codex-session.jsonl"

seed_out="$(run_capture "$bin" --pricing-seed --db "$db" --data-dir "$claude_projects")"
assert_contains "$seed_out" "seeded"

audit_out="$(run_capture "$bin" --pricing-audit --db "$db" --data-dir "$claude_projects")"
assert_contains "$audit_out" "pricing audit: no findings"

by_provider="$(run_capture "$bin" --force-update --provider all --db "$db" --data-dir "$claude_projects" --by-provider --no-color)"
assert_contains "$by_provider" "all providers / by provider"
assert_contains "$by_provider" "claude-code"
assert_contains "$by_provider" "codex"

by_model="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --by-model --limit 50 --no-color)"
assert_contains "$by_model" "claude-opus"
assert_contains "$by_model" "gpt-5.5"

json_model="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --by-model --json --limit 50)"
if command -v python3 >/dev/null 2>&1; then
  JSON_MODEL="$json_model" python3 - <<'PY'
import json
import os

rows = json.loads(os.environ["JSON_MODEL"])
assert any(row["provider"] == "claude-code" and row["cost_usd"] > 0 for row in rows)
assert any(row["provider"] == "codex" and row["model_id"] == "gpt-5.5" for row in rows)
PY
else
  assert_contains "$json_model" '"provider": "claude-code"'
  assert_contains "$json_model" '"provider": "codex"'
fi

csv_model="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --by-model --csv)"
assert_contains "$csv_model" "period,provider,model_id"
assert_contains "$csv_model" "codex/gpt-5.5,codex,gpt-5.5"

printf 'tkstat committed fixture smoke passed\n'
