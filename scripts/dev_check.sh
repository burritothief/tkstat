#!/usr/bin/env bash
set -euo pipefail

# Run the local quality gate used before commits and releases.
#
# This script keeps the common development checks in one place: formatting,
# clippy with warnings denied, the Rust test suite, and the black-box E2E smoke
# test. Use --skip-e2e when iterating on a narrow unit-level change and run the
# full command before committing or tagging.
#
# Examples:
#   scripts/dev_check.sh
#   scripts/dev_check.sh --skip-e2e
#   TKSTAT_BIN=target/release/tkstat scripts/dev_check.sh
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
skip_e2e=0

usage() {
  cat <<'USAGE'
Usage: scripts/dev_check.sh [--skip-e2e]

Options:
  --skip-e2e   Run fmt, clippy, and cargo test only.
  -h, --help   Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-e2e)
      skip_e2e=1
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

run() {
  {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  } >&2
  "$@"
}

cd "$repo_root"

run cargo fmt -- --check
run cargo clippy --all-targets --all-features -- -D warnings
run cargo test

if [[ "$skip_e2e" == "0" ]]; then
  run "$script_dir/e2e_smoke.sh"
fi

printf 'tkstat development checks passed\n'
