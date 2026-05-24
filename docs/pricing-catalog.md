# Pricing Catalog

`pricing/catalog.json` is the bundled offline pricing catalog used by
`tkstat --pricing-seed` and the current `tkstat --pricing-refresh`
implementation. It is a versioned, reviewable snapshot of official provider
pricing pages, not a live network lookup.

Each catalog source records its official URL, retrieval date, and notes about
how provider pricing is represented. Each catalog entry records provider,
model ids or aliases, token categories, pricing dimensions, currency,
effective interval, rates per 1M tokens, source label, source reference, and
notes. The application expands those entries into effective-dated SQLite
`pricing_intervals` rows.

Bundled prices can become stale when providers change prices or announce new
models. Cost-bearing reports fail closed when local pricing does not cover
observed usage. Use `tkstat --pricing-audit` to inspect gaps and reseed or
import a reviewed catalog update when pricing changes.
