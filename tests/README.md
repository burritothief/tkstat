# Integration Test Layout

The `e2e_*.rs` files are black-box CLI tests grouped by concern:

- `e2e_fixtures.rs`: committed fixture parsing, fixture coverage, and privacy scanning.
- `e2e_setup_smoke.rs`: recommended setup/reset flow and local smoke-script execution.
- `e2e_output_modes.rs`: report rendering, filters, CSV/JSON, and budget report output.
- `e2e_pricing.rs`: pricing remediation and pricing-audit behavior.
- `e2e_provider_diagnostics.rs`: provider selection, ingestion warnings, and doctor diagnostics.

Shared fixture builders, command execution, and assertions live in `support/mod.rs`.
