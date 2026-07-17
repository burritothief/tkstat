# Changelog

All notable changes to tkstat are documented here. The project follows Semantic Versioning while
pre-1.0 releases may still evolve their Rust library API.

## [0.4.2] - 2026-07-16

### Added

- Strict validation for conflicting report, period, and maintenance flags.
- Positive finite validation for limits and budget thresholds.
- An explicit Rust 1.88 MSRV check and cross-platform CI test matrix.
- Release artifact checksums and tag-to-package-version validation.

### Changed

- Upgraded direct dependencies, including the `ureq` 3.3 HTTP stack.
- Made CLI output gracefully handle broken Unix pipes.
- Enabled SQLite foreign-key enforcement and replaced delete-and-insert upserts.
- Documented platform-native database paths and the contributor workflow.

## [0.4.0] - 2026-07-16

- Simplified the pricing and reporting architecture while preserving fail-closed cost behavior.
- Materialized request costs and reduced normal report latency.
- Added robust provider pricing refresh and audit workflows.

[0.4.2]: https://github.com/burritothief/tkstat/compare/v0.4.0...v0.4.2
[0.4.0]: https://github.com/burritothief/tkstat/releases/tag/v0.4.0
