#!/usr/bin/env bash
set -euo pipefail

# Exercise the pricing workflow against an isolated database.
#
# The default mode uses committed fixtures and a temp database, so it is safe to
# run without touching a user's real tkstat database. Pass --data-dir and
# CODEX_HOME if you want to validate pricing against local provider logs while
# still writing only to the temp database created by this script.
#
# Examples:
#   scripts/pricing_check.sh
#   scripts/pricing_check.sh --provider codex
#   CODEX_HOME=~/.codex scripts/pricing_check.sh --data-dir ~/.claude/projects --keep-db
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
fixture_root="$repo_root/tests/fixtures"
provider="all"
data_dir=""
keep_db=0

usage() {
  cat <<'USAGE'
Usage: scripts/pricing_check.sh [--provider all|claude-code|claude|codex] [--data-dir PATH] [--keep-db]

Options:
  --provider VALUE  Provider selection passed to tkstat; defaults to all.
  --data-dir PATH   Claude projects directory. Defaults to committed fixtures.
  --keep-db         Keep the temporary database and print its path.
  -h, --help        Show this help text.

Environment:
  TKSTAT_BIN  Use this tkstat binary instead of building target/debug/tkstat.
  CODEX_HOME  Codex home. Defaults to a temp tree populated from committed fixtures.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --provider)
      provider="${2:-}"
      shift 2
      ;;
    --data-dir)
      data_dir="${2:-}"
      shift 2
      ;;
    --keep-db)
      keep_db=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$provider" in
  all|claude-code|claude|codex) ;;
  *)
    printf 'invalid provider: %s\n' "$provider" >&2
    exit 2
    ;;
esac

bin="${TKSTAT_BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
  bin="$repo_root/target/debug/tkstat"
fi

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/tkstat-pricing-check.XXXXXX")"
db="$tmp_root/tkstat.db"
codex_home="${CODEX_HOME:-$tmp_root/codex-home}"
export HOME="$tmp_root/home"
export CLAUDE_CONFIG_DIR="$tmp_root/claude-config"
export CODEX_HOME="$codex_home"
export NO_COLOR=1

cleanup() {
  if [[ "$keep_db" == "1" ]]; then
    printf 'kept temp root: %s\n' "$tmp_root"
    printf 'kept database: %s\n' "$db"
  else
    rm -rf "$tmp_root"
  fi
}
trap cleanup EXIT

if [[ -z "$data_dir" ]]; then
  data_dir="$tmp_root/claude/projects"
  mkdir -p "$data_dir"
  cp -R "$fixture_root/claude/." "$data_dir/"
fi

if [[ ! -d "$codex_home" || "$codex_home" == "$tmp_root/codex-home" ]]; then
  mkdir -p "$codex_home/sessions/2026/05/24"
  cp "$fixture_root/codex/synthetic-codex-session.jsonl" \
    "$codex_home/sessions/2026/05/24/rollout-synthetic-codex-session.jsonl"
fi

run() {
  {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  } >&2
  "$@"
}

run "$bin" --db "$db" --data-dir "$data_dir" --pricing-seed
run "$bin" --db "$db" --data-dir "$data_dir" --pricing-refresh
run "$bin" --db "$db" --data-dir "$data_dir" --provider "$provider" --force-update --by-provider --no-color
run "$bin" --db "$db" --data-dir "$data_dir" --provider "$provider" --by-model --columns total,cost --no-color
run "$bin" --db "$db" --data-dir "$data_dir" --pricing-audit

printf 'tkstat pricing workflow check passed\n'
