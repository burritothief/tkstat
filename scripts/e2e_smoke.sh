#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

bin="${TKSTAT_BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
  bin="$repo_root/target/debug/tkstat"
fi

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/tkstat-e2e-smoke.XXXXXX")"
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

mkdir -p "$claude_projects/-home-tester-work-demo"
cat >"$claude_projects/-home-tester-work-demo/main.jsonl" <<'JSONL'
{"type":"assistant","message":{"model":"claude-opus-4-5-20251101","usage":{"input_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":200,"output_tokens":10}},"requestId":"smoke-claude-opus","uuid":"smoke-claude-opus-uuid","timestamp":"2026-01-31T21:20:19.858Z","sessionId":"smoke-claude-session"}
{"type":"assistant","message":{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":30,"cache_creation_input_tokens":0,"cache_read_input_tokens":50,"output_tokens":40}},"requestId":"smoke-claude-sonnet","uuid":"smoke-claude-sonnet-uuid","timestamp":"2026-02-01T01:44:51.951Z","sessionId":"smoke-claude-session"}
JSONL

mkdir -p "$codex_home/sessions/2026/05/24"
cat >"$codex_home/sessions/2026/05/24/rollout-smoke-2026-05-24.jsonl" <<'JSONL'
{"timestamp":"2026-05-24T00:40:02.000Z","type":"session_meta","payload":{"id":"smoke-codex-session","cwd":"/home/tester/work/tkstat","model_provider":"openai"}}
{"timestamp":"2026-05-24T00:40:02.192Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/home/tester/work/tkstat","model":"gpt-5.5"}}
{"timestamp":"2026-05-24T00:40:04.988Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":13066,"cached_input_tokens":4480,"output_tokens":213,"reasoning_output_tokens":0,"total_tokens":13279},"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":20,"reasoning_output_tokens":7,"total_tokens":120},"model_context_window":258400}}}
JSONL

seed_out="$(run_capture "$bin" --pricing-seed --db "$db" --data-dir "$claude_projects")"
assert_contains "$seed_out" "seeded"

refresh_out="$(run_capture "$bin" --pricing-refresh --db "$db" --data-dir "$claude_projects")"
assert_contains "$refresh_out" "refreshed pricing catalog"

by_provider="$(run_capture "$bin" --force-update --provider all --db "$db" --data-dir "$claude_projects" --by-provider --no-color)"
assert_contains "$by_provider" "all providers / by provider"
assert_contains "$by_provider" "claude"
assert_contains "$by_provider" "codex"

daily="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" -d --limit 200 --no-color)"
assert_contains "$daily" "all providers / daily"
assert_contains "$daily" "2026-01-31"
assert_contains "$daily" "2026-05-24"

by_model="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --by-model --limit 50 --no-color)"
assert_contains "$by_model" "claude-opus-4-5-20251101"
assert_contains "$by_model" "gpt-5.5"

json_daily="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --json -d --limit 200)"
if command -v python3 >/dev/null 2>&1; then
  JSON_DAILY="$json_daily" python3 - <<'PY'
import json
import os

rows = json.loads(os.environ["JSON_DAILY"])
assert any(row["period"] == "2026-05-24" and row["cost_usd"] > 0 for row in rows)
assert any(row["period"] == "2026-01-31" and row["cost_usd"] > 0 for row in rows)
PY
else
  assert_contains "$json_daily" '"period": "2026-05-24"'
  assert_contains "$json_daily" '"cost_usd"'
fi

csv_model="$(run_capture "$bin" --provider all --db "$db" --data-dir "$claude_projects" --by-model --csv)"
assert_contains "$csv_model" "period,provider,model_id"
assert_contains "$csv_model" "codex/gpt-5.5,codex,gpt-5.5"

printf 'tkstat e2e smoke passed\n'
