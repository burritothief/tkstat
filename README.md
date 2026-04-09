# tkstat

Terminal-based token usage monitor for Claude Code, inspired by [vnstat](https://github.com/vergoh/vnstat).

Claude Code writes session logs as JSONL files, but there's no built-in way to see how many tokens you're burning across days, weeks, or months. `tkstat` parses JSONL logs into a local SQLite database, then queries it.

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
```

Daily:
```
$ tkstat -d --limit 10
 claude / daily
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
 claude / hourly
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
 claude / top days
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
tkstat --model opus         # only opus usage
tkstat --model sonnet       # only sonnet usage
tkstat -p myproject         # filter by project name (substring match)
tkstat -b 2026-03-01 -e 2026-03-31   # date range
tkstat --no-subagents       # exclude subagent usage
```

### Column selection

Default columns: `input`, `output`, `cache_rd`, `cache_cr`, `total`, `cost`.

Use `--columns` to pick exactly which columns to show:

```
tkstat --columns cost,reqs,sessions
tkstat --columns in,out,total,cost,reqs
```

Available columns: `input` (`in`), `output` (`out`), `cache_rd` (`crd`), `cache_cr` (`ccr`), `total` (`tot`), `cost`, `reqs` (`req`), `sessions` (`sess`).

These map directly to fields in the [Anthropic Messages API usage object](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching#tracking-cache-performance):

| Column | API field | What it is |
|--------|-----------|------------|
| `input` | `input_tokens` | Tokens sent to Claude — prompts, system messages, tool results. Small in Claude Code because most input is cached. |
| `output` | `output_tokens` | Tokens Claude generates — responses, code, tool calls, chain-of-thought. |
| `cache rd` | `cache_read_input_tokens` | Input tokens served from Anthropic's [prompt cache](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching). 90% cheaper than regular input. This is the bulk of Claude Code token volume — the full conversation is resent each turn, but unchanged portions hit cache. |
| `cache cr` | `cache_creation_input_tokens` | Input tokens written to cache for the first time. 25% more expensive than regular input. |
| `total` | — | Sum of all four token types. |
| `cost` | — | Estimated cost in USD, calculated from token counts and [Anthropic's published pricing](https://docs.anthropic.com/en/docs/about-claude/models). |
| `reqs` | — | Number of API requests. |
| `sessions` | — | Number of distinct Claude Code sessions. |


### Output formats

```
tkstat --json -d        # JSON array
tkstat --oneline        # semicolon-delimited single line
tkstat -s               # short summary
```

## How it works

Claude Code stores session logs at `~/.claude/projects/*/UUID.jsonl`. Each API response is a JSON line with a `usage` object containing token counts.

`tkstat` maintains a SQLite database (at `~/.local/share/tkstat/tkstat.db`) that caches parsed token records. On each run it checks which JSONL files have changed since the last read (by file size and mtime) and only parses the new bytes.

Use `--force-update` to wipe the database and re-ingest everything (e.g., after changing pricing config).

## Database

```
tkstat --db /path/to/my.db      # custom database path
tkstat --data-dir /path/to/logs # custom Claude log directory
tkstat --force-update           # full re-ingest
```

The default database location is `~/.local/share/tkstat/tkstat.db`. You can also set the `TKSTAT_DB` environment variable.
