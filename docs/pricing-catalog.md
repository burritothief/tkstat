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

The September 10, 2026 additions cover GPT-6 Astra, GPT-5.6 Terra and Luna,
GPT-5.4 mini, GPT-5.3 Codex, Claude Opus 4.8 and 5, Sonnet 5, and Fable/Mythos
5 and 5.1. Claude entries include both cache-write TTLs and US inference;
Fable/Mythos 5.1 use their published $0.25/MTok cache-read price.

The October 8, 2026 additions cover GPT-6.1 Sol and Claude Opus 5.5. GPT-6.1
Sol standard short-context rates are $2 input, $0.10 cached input, and $10
output per million tokens. Claude Opus 5.5 rates are $4 input, $20 output,
$5 for 5-minute cache writes, $8 for 1-hour cache writes, and $0.20 for cache
reads per million tokens. Claude entries also cover US inference at 1.1x.

After upgrading, run `tkstat --pricing-seed` to add bundled coverage, or
`tkstat --pricing-refresh` to fetch current prices for both providers. Both
commands extend brand-new, previously unpriced exact keys to their earliest
observed usage. Existing priced history is preserved. Older bundled snapshots
remain dated as originally reviewed; refresh to pick up subsequent price changes.

OpenAI refresh accepts the current Markdown pricing table and the earlier
structured table. It selects standard short-context rates explicitly. Codex
estimates currently exclude long-context premiums and separately billed cache
writes because ingestion does not retain those billing dimensions.
