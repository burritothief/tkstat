#!/usr/bin/env bash
set -euo pipefail

# Smoke-test maintained operational scripts without touching real user data,
# tags, or remotes.
#
# Examples:
#   scripts/script_smoke.sh
#   TKSTAT_BIN=target/debug/tkstat scripts/script_smoke.sh
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

usage() {
  cat <<'USAGE'
Usage: scripts/script_smoke.sh

Environment:
  TKSTAT_BIN  Use this tkstat binary instead of building target/debug/tkstat.
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

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/tkstat-script-smoke.XXXXXX")"
cleanup() {
  rm -rf "$tmp_root"
}
trap cleanup EXIT

log_cmd() {
  {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  } >&2
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" <<<"$haystack"; then
    printf 'expected output to contain: %s\noutput was:\n%s\n' "$needle" "$haystack" >&2
    exit 1
  fi
}

run_capture() {
  log_cmd "$@"
  "$@" 2>&1
}

expect_success_contains() {
  local needle="$1"
  shift
  local output
  output="$(run_capture "$@")"
  assert_contains "$output" "$needle"
}

expect_failure_contains() {
  local needle="$1"
  shift
  local output status
  set +e
  output="$(run_capture "$@")"
  status=$?
  set -e
  if [[ "$status" -eq 0 ]]; then
    printf 'expected command to fail: %q\noutput was:\n%s\n' "$*" "$output" >&2
    exit 1
  fi
  assert_contains "$output" "$needle"
}

checksum_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    return 1
  fi
}

bin="${TKSTAT_BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
  bin="$repo_root/target/debug/tkstat"
fi

expect_success_contains "Usage: scripts/db_inspect.sh" "$script_dir/db_inspect.sh" --help
expect_success_contains "Usage: scripts/fixture_smoke.sh" "$script_dir/fixture_smoke.sh" --help
expect_success_contains "Usage: scripts/pricing_check.sh" "$script_dir/pricing_check.sh" --help
expect_success_contains "Usage: scripts/dev_check.sh" "$script_dir/dev_check.sh" --help
expect_success_contains "Usage: scripts/release_check.sh" "$script_dir/release_check.sh" --help

expect_failure_contains "unknown argument" "$script_dir/fixture_smoke.sh" --bogus
expect_failure_contains "invalid provider" "$script_dir/pricing_check.sh" --provider invalid
expect_failure_contains "Usage: scripts/release_check.sh" "$script_dir/release_check.sh" --skip-dev-check
expect_failure_contains "unknown argument" "$script_dir/dev_check.sh" --bogus

fixture_out="$tmp_root/fixture_smoke.out"
TKSTAT_BIN="$bin" "$script_dir/fixture_smoke.sh" >"$fixture_out"
assert_contains "$(cat "$fixture_out")" "tkstat committed fixture smoke passed"

pricing_out="$tmp_root/pricing_check.out"
TKSTAT_BIN="$bin" "$script_dir/pricing_check.sh" --provider all >"$pricing_out"
assert_contains "$(cat "$pricing_out")" "tkstat pricing workflow check passed"

if command -v sqlite3 >/dev/null 2>&1; then
  db="$tmp_root/inspect.db"
  "$bin" --pricing-seed --db "$db" >/dev/null
  before_checksum="$(checksum_file "$db" || true)"
  inspect_out="$(TKSTAT_BIN="$bin" "$script_dir/db_inspect.sh" "$db")"
  assert_contains "$inspect_out" "== Database =="
  assert_contains "$inspect_out" "raw token_usage.timestamp and pricing_intervals effective_* values are UTC RFC3339 instants"
  assert_contains "$inspect_out" "== Pricing Audit =="
  if [[ -n "$before_checksum" ]]; then
    after_checksum="$(checksum_file "$db")"
    if [[ "$before_checksum" != "$after_checksum" ]]; then
      printf 'db_inspect.sh mutated %s\n' "$db" >&2
      exit 1
    fi
  fi
else
  printf 'skipping db_inspect.sh fixture-backed check; sqlite3 is not available\n' >&2
fi

printf 'tkstat operational script smoke passed\n'
