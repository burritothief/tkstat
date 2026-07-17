# Pricing Catalog

`pricing/catalog.json` is the bundled offline pricing catalog used by
`tkstat --pricing-seed`. It is a versioned, reviewable snapshot of official provider
pricing pages.

Each catalog source records its official URL, retrieval date, and notes about
how provider pricing is represented. Each catalog entry records provider,
model ids or aliases, token categories, pricing dimensions, currency,
effective interval, rates per 1M tokens, source label, source reference, and
notes. The application expands those entries into effective-dated SQLite
`pricing_intervals` rows.

Bundled prices can become stale when providers change prices or announce new
models. `tkstat --pricing-refresh` explicitly fetches the official Anthropic
pricing and model-identity Markdown documents and OpenAI's structured pricing
document. Each provider is parsed and committed independently; changed source
shapes or invalid rates fail without replacing last-known-good data. Cost-bearing
reports fail closed when exact-model pricing does not cover observed usage.
