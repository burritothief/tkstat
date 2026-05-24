#!/usr/bin/env bash
set -euo pipefail

# Run non-mutating preflight checks before creating a tkstat release tag.
#
# This script verifies the package version, checks that the git tree is clean,
# ensures the release tag does not already exist, and optionally runs the full
# development quality gate. It deliberately does not commit, tag, or push; it
# prints the exact commands to run after the checks pass.
#
# Examples:
#   scripts/release_check.sh 0.2.1
#   scripts/release_check.sh --skip-dev-check 0.2.1
#

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
skip_dev_check=0

usage() {
  cat <<'USAGE'
Usage: scripts/release_check.sh [--skip-dev-check] VERSION

Arguments:
  VERSION           Expected Cargo.toml package version, for example 0.2.1.

Options:
  --skip-dev-check  Do not run scripts/dev_check.sh.
  -h, --help        Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-dev-check)
      skip_dev_check=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      printf 'unknown argument: %s\n\n' "$1" >&2
      usage >&2
      exit 2
      ;;
    *)
      expected_version="$1"
      shift
      ;;
  esac
done

if [[ -z "${expected_version:-}" ]]; then
  usage >&2
  exit 2
fi

cd "$repo_root"

actual_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
tag="v$expected_version"

if [[ "$actual_version" != "$expected_version" ]]; then
  printf 'Cargo.toml version mismatch: expected %s, found %s\n' "$expected_version" "$actual_version" >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  printf 'git working tree is not clean; commit or stash changes before release\n' >&2
  git status --short >&2
  exit 1
fi

if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
  printf 'tag already exists locally: %s\n' "$tag" >&2
  exit 1
fi

if git ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1; then
  printf 'tag already exists on origin: %s\n' "$tag" >&2
  exit 1
fi

if [[ "$skip_dev_check" == "0" ]]; then
  "$script_dir/dev_check.sh"
fi

cat <<EOF
release preflight passed for $tag

Suggested release commands:
  git tag -a $tag -m "Release $tag"
  git push origin main
  git push origin $tag
EOF
