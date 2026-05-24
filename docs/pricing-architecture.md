# Pricing Architecture Decision

## Status

Accepted for the accuracy-first pricing work that starts after tkstat 0.2.3.

## Decision

Cost calculation must use normalized billable usage components as the canonical cost input. Each component represents one priced line item with provider, exact model id, UTC usage timestamp, token category, token count, currency, and any pricing-relevant modifiers such as service tier, speed, region, processing mode, or source detail.

The existing wide columns on `token_usage` remain the display and compatibility surface. Reports can keep reading `input_tokens`, `output_tokens`, `cache_creation_tokens`, `cache_read_tokens`, `cached_input_tokens`, `reasoning_output_tokens`, and `total_tokens` for table, JSON, CSV, chart, heatmap, and budget display totals. Those wide columns are not the long-term source of truth for cost.

Effective-dated pricing intervals are the source of truth for rates. Price lookup must match each billable component by provider, model id, token category, UTC timestamp, and every pricing dimension that affects the rate. Missing or overlapping matches must fail closed before structured report output is emitted.

If retained, `token_usage.cost_usd` is a deprecated compatibility/cache field. It is not authoritative for new cost-bearing reports, because pricing refreshes and effective-dated corrections must be able to recompute costs from billing components and pricing intervals.

## Rationale

Wide token columns are practical for display, but they do not scale to provider-specific pricing details. Claude cache writes can have distinct TTL prices, and request modifiers such as service tier, speed, or inference geography can change rates. Codex/OpenAI usage can introduce processing-mode or data-residency dimensions. Adding one column per token category and modifier would keep increasing query complexity and make coverage validation fragile.

Raw metadata alone is also insufficient as the cost source because it makes SQL reporting and exact coverage validation harder. Parsed, normalized billable components keep the important provider facts while giving pricing lookup one stable shape.

## Table Roles

- `token_usage`: deduplicated request-level usage and display aggregates. This table preserves provider, exact model id, timestamps, project/session metadata, and wide token totals for user-facing reports.
- `usage_billing_components`: normalized priced line items derived from `token_usage` and provider logs. This is the planned cost source of truth for cost-bearing reports.
- `pricing_intervals`: effective-dated rates keyed by provider, model id, token category, currency, timestamp range, and pricing dimensions. This is the rate source of truth.

## Consequences

Cost-bearing queries must validate coverage over the full component price key. Token-only reports may continue to work without pricing coverage. Pricing audit and doctor diagnostics should explain missing catalog coverage, stale source data, unsupported modifiers, and assumptions used for default dimensions.

This architecture favors accuracy and explicit failures over silent zero cost, broad default matching, or provider-specific shortcuts.
