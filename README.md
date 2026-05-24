# tkstat

A terminal-based token usage monitor for Claude Code and Codex, inspired by [vnstat](https://github.com/vergoh/vnstat).

Claude Code and Codex write session logs as JSONL files, but there's no built-in way to see how many tokens you're burning across days, weeks, or months. `tkstat` parses JSONL logs into a local SQLite database, then queries it.

## Install

```
cargo install --path .
```

Or build from source:

```
cargo build --release
./target/release/tkstat
```

## Usage

```
tkstat              # daily usage (default)
tkstat -5           # 5-minute resolution
tkstat -h           # hourly
tkstat -m           # monthly
tkstat -y           # yearly
tkstat -t 10        # top 10 days by token volume
tkstat -s           # short summary
tkstat --heatmap    # GitHub-style contribution calendar
tkstat --chart      # braille time-series chart
tkstat --by-model   # usage grouped by exact model id
tkstat --by-provider # usage grouped by provider
tkstat --by-project # usage grouped by project
tkstat --provider codex --by-model  # Codex usage by exact model id
```

Daily:
```
$ tkstat -d --limit 10
 claude-code / daily
                input |    output |  cache rd |  cache cr |     total |      cost
 ---------------------+-----------+-----------+-----------+-----------+----------
 2026-03-30     1.2 K |    80.2 K |    36.8 M |     1.6 M |    38.4 M |     $84.5
 2026-03-31     4.2 K |     151 K |    66.3 M |     4.3 M |    70.7 M |      $185
 2026-04-01    23.3 K |     207 K |    29.4 M |     2.4 M |    32.0 M |     $87.2
 2026-04-02     5.3 K |    30.2 K |     3.9 M |     605 K |     4.6 M |     $16.1
 2026-04-03        90 |    62.2 K |     3.3 M |     346 K |     3.7 M |     $15.8
 2026-04-04         - |         - |         - |         - |         - |         -
 2026-04-05         - |         - |         - |         - |         - |         -
 2026-04-06     1.6 K |     182 K |    36.4 M |     3.1 M |    39.6 M |      $117
 2026-04-07    14.8 K |     612 K |     150 M |     7.4 M |     158 M |      $389
 2026-04-08     1.2 K |     269 K |    77.7 M |     8.2 M |    86.1 M |      $286
 ---------------------+-----------+-----------+-----------+-----------+----------
      total    51.8 K |     1.6 M |     404 M |    27.8 M |     434 M |     $1181
```


Hourly:
```
$ tkstat -h --limit 12
 claude-code / hourly
                input |    output |  cache rd |  cache cr |     total |      cost
 ---------------------+-----------+-----------+-----------+-----------+----------
 2026-04-08
      03:00         - |         - |         - |         - |         - |         -
      04:00         - |         - |         - |         - |         - |         -
      05:00         - |         - |         - |         - |         - |         -
      06:00         - |         - |         - |         - |         - |         -
      07:00        14 |     8.3 K |     116 K |    29.3 K |     154 K |     $1.34
      08:00         - |         - |         - |         - |         - |         -
      09:00       301 |    68.4 K |    12.9 M |     1.8 M |    14.8 M |     $58.0
      10:00       177 |    32.6 K |    28.4 M |     2.9 M |    31.4 M |     $95.3
      11:00        38 |     8.7 K |    14.7 M |     963 K |    15.7 M |     $40.8
      12:00         4 |       131 |     947 K |       292 |     948 K |     $1.44
      13:00       421 |     6.2 K |     3.8 M |     909 K |     4.7 M |     $23.2
      14:00        65 |    45.8 K |     3.1 M |     148 K |     3.3 M |     $10.8
 ---------------------+-----------+-----------+-----------+-----------+----------
      total     1.0 K |     170 K |    64.1 M |     6.7 M |    70.9 M |      $231
```

Top Days:

```
$ tkstat -t 5
 claude-code / top days
                input |    output |  cache rd |  cache cr |     total |      cost
 ---------------------+-----------+-----------+-----------+-----------+----------
 2026-04-07    14.8 K |     612 K |     150 M |     7.4 M |     158 M |      $389
 2026-03-20     5.4 K |     250 K |    90.7 M |     5.5 M |    96.5 M |      $245
 2026-03-31     4.2 K |     151 K |    66.3 M |     4.3 M |    70.7 M |      $185
 2026-03-18    14.0 K |     184 K |    50.5 M |     3.1 M |    53.8 M |      $131
 2026-03-17    12.0 K |     138 K |    39.1 M |     3.2 M |    42.5 M |      $117
 ---------------------+-----------+-----------+-----------+-----------+----------
      total    50.5 K |     1.3 M |     397 M |    23.5 M |     422 M |     $1067
```

### Filters

```
tkstat --model opus         # only opus usage (family alias)
tkstat --model claude-sonnet-4-5-20250929  # exact model id
tkstat --model-family sonnet  # explicit family filter
tkstat --by-model           # compare exact model ids
tkstat --by-provider        # compare providers
tkstat --by-project         # compare projects
tkstat --model sonnet       # only sonnet usage
tkstat --provider claude-code  # only Claude Code usage (`claude` is accepted as an alias)
tkstat --provider codex        # only Codex usage
tkstat -p myproject         # filter by project name (substring match)
tkstat -b 2026-03-01 -e 2026-03-31   # date range
tkstat --no-subagents       # exclude subagent usage
```

Report buckets and `--begin`/`--end` date filters use UTC dates, regardless of the host machine's local timezone. This keeps daily/hourly output deterministic across machines and daylight-saving transitions.

### Column selection

Default columns: `input`, `output`, `cache_rd`, `cache_cr`, `total`, `cost`.

Use `--columns` to pick exactly which columns to show:

```
tkstat --columns cost,reqs,sessions
tkstat --columns in,out,total,cost,reqs
tkstat --provider codex --columns input,cached_input,output,reasoning_output,total,cost
```

Available columns: `input` (`in`), `output` (`out`), `cache_rd` (`crd`), `cache_cr` (`ccr`), `cached_input` (`cached`), `reasoning_output` (`reason`), `total` (`tot`), `cost`, `reqs` (`req`), `sessions` (`sess`).

These map directly to fields in the [Anthropic Messages API usage object](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching#tracking-cache-performance):

| Column | API field | What it is |
|--------|-----------|------------|
| `input` | `input_tokens` | Tokens sent to Claude — prompts, system messages, tool results. Small in Claude Code because most input is cached. |
| `output` | `output_tokens` | Tokens Claude generates — responses, code, tool calls, chain-of-thought. |
| `cache rd` | `cache_read_input_tokens` | Input tokens served from Anthropic's [prompt cache](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching). 90% cheaper than regular input. This is the bulk of Claude Code token volume — the full conversation is resent each turn, but unchanged portions hit cache. |
| `cache cr` | `cache_creation_input_tokens` | Input tokens written to cache for the first time. 25% more expensive than regular input. |
| `cached in` | `cached_input_tokens` | Codex/OpenAI input tokens served from prompt cache; this is a subset of `input_tokens`. |
| `reason` | `reasoning_output_tokens` | Codex/OpenAI reasoning tokens; this is a subset of `output_tokens`. |
| `total` | — | Display total. Claude Code sums input, output, cache read, and cache creation; Codex/OpenAI totals use input plus output so cached and reasoning subcategories are not double-counted. |
| `cost` | — | Estimated cost in USD, calculated from token counts and [Anthropic's published pricing](https://docs.anthropic.com/en/docs/about-claude/models). |
| `reqs` | — | Number of API requests. |
| `sessions` | — | Number of distinct Claude Code sessions. |


### Output formats

```
tkstat --json -d        # JSON array
tkstat --csv -d         # CSV rows with raw numeric values for table-shaped reports
tkstat --oneline        # semicolon-delimited single line
tkstat -s               # short summary
```

### Budget warnings

```
tkstat --daily-budget-usd 5.00
tkstat --monthly-budget-usd 100.00 --provider codex
tkstat --budget --daily-budget-usd 5.00 --monthly-budget-usd 100.00
```

Budget warnings are printed to stderr and use the active provider/model/project/date filters. Structured stdout formats such as JSON and CSV remain machine-readable.

## How it works

Claude Code stores session logs under its projects directory. Codex stores session logs under its dated sessions directory. Each provider adapter normalizes its own token records into the local database.

`tkstat` maintains a SQLite database (at `~/.local/share/tkstat/tkstat.db`) that caches parsed token records. On each run it checks which JSONL files have changed since the last read (by file size and mtime) and only parses the new bytes.

Use `--force-update` to wipe the database and re-ingest everything (e.g., after changing pricing config).

## Database

```
tkstat --db /path/to/my.db      # custom database path
tkstat --data-dir /path/to/logs # custom Claude log directory
tkstat --provider all           # ingest/query all discoverable providers
tkstat --provider codex         # ingest/query Codex only
tkstat --pricing-seed           # install bundled offline pricing intervals
tkstat --pricing-refresh        # refresh local pricing intervals
tkstat --pricing-audit          # audit local pricing coverage
tkstat --force-update           # full re-ingest
```

The default database location is `~/.local/share/tkstat/tkstat.db`. You can also set the `TKSTAT_DB` environment variable.

Schema v8 stores provider plus exact model identity for every usage row and uses a local effective-dated pricing catalog. Provider ids are canonical storage keys (`claude-code`, `codex`); friendly CLI aliases such as `--provider claude` are normalized before querying. Because `tkstat` is pre-1.0, upgrading from an older schema rebuilds the usage cache and migrates legacy Claude pricing keys; run `tkstat --force-update` if you need to force a clean reingest.

### Provider and pricing examples

```
tkstat --pricing-seed
tkstat --provider claude-code -d
tkstat --provider codex --by-model
tkstat --provider all --by-model --json
tkstat --pricing-refresh
tkstat --pricing-audit --json
```

Cost-bearing reports fail closed when pricing coverage is missing. If you see an error naming a provider, model id, token category, and date range, run `tkstat --pricing-audit` to list all local pricing findings, then run `tkstat --pricing-seed` for bundled fallback pricing or `tkstat --pricing-refresh` to refresh the local catalog.

`--force-update` clears cached usage rows and file offsets, but keeps locally cached pricing intervals. If pricing was never seeded or refreshed, run one of the pricing commands before cost-bearing reports.
