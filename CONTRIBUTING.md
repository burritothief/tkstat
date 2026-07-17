# Contributing to tkstat

Thanks for improving tkstat. The project targets stable Rust with a minimum supported Rust
version of 1.88.

## Development workflow

1. Fork and clone the repository.
2. Build the CLI with `cargo build --locked`.
3. Run `scripts/dev_check.sh` before opening a pull request.

The full check runs formatting, a build without network pricing, Clippy with all targets and
features, the Rust test suite, and black-box smoke tests. Keep commits focused and include tests
for externally visible behavior.

## Provider fixtures and pricing

Provider log formats and pricing documents are external inputs. New parser behavior should include
the smallest representative fixture and a regression test. Fixtures must be synthetic or sanitized:
never commit prompts, responses, credentials, session identifiers, usernames, or personal paths.

Pricing changes must preserve exact provider model identifiers, effective dates, billing dimensions,
and source metadata. Run `scripts/pricing_check.sh` after changing the bundled catalog or pricing
parser.

## Compatibility

`tkstat` is primarily a CLI. Preserve documented flags, structured output fields, database migration
paths, and exit behavior. Public Rust modules are pre-1.0 and may evolve, but avoid unnecessary API
breakage in patch releases.
