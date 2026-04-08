# tkstat

A terminal-based token usage monitor for [Claude Code](https://docs.anthropic.com/en/docs/claude-code), inspired by [vnstat](https://github.com/vergoh/vnstat).

Claude Code writes session logs as JSONL files, but there's no built-in way to see how many tokens you're burning across days, weeks, or months. [ccusage](https://github.com/ryoppippi/ccusage) solves this but re-parses all logs on every run, which gets slow. vnstat is instant because it maintains its own database.

tkstat takes the same approach: parse the JSONL logs into a local SQLite database, then query it. Cold start ingests ~8K records in under 50ms. Warm starts are ~30ms.

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

### Daily

```
$ tkstat -d --limit 10
 claude / daily
                         input |       output |     cache rd |     cache cr |        total |         cost
 ------------------------------+--------------+--------------+--------------+--------------+-------------
       2026-03-30        1.2 K |       80.2 K |       36.8 M |        1.6 M |       38.4 M |        $84.5
       2026-03-31        4.2 K |        151 K |       66.3 M |        4.3 M |       70.7 M |         $185
       2026-04-01       23.3 K |        207 K |       29.4 M |        2.4 M |       32.0 M |        $87.2
       2026-04-02        5.3 K |       30.2 K |        3.9 M |        605 K |        4.6 M |        $16.1
       2026-04-03           90 |       62.2 K |        3.3 M |        346 K |        3.7 M |        $15.8
       2026-04-04            - |            - |            - |            - |            - |            -
       2026-04-05            - |            - |            - |            - |            - |            -
       2026-04-06        1.6 K |        182 K |       36.4 M |        3.1 M |       39.6 M |         $117
       2026-04-07       14.8 K |        612 K |        150 M |        7.4 M |        158 M |         $389
       2026-04-08           19 |        3.1 K |        3.9 M |        760 K |        4.7 M |        $20.4
 ------------------------------+--------------+--------------+--------------+--------------+-------------
            total       50.6 K |        1.3 M |        330 M |       20.4 M |        352 M |         $915
```

Days with no activity show `-`. The time series is always continuous — no gaps.

### Hourly

Sub-daily views group by date to avoid repeating it on every line:

```
$ tkstat -h --limit 12
 claude / hourly
                         input |       output |     cache rd |     cache cr |        total |         cost
 ------------------------------+--------------+--------------+--------------+--------------+-------------
 2026-04-07
            13:00            - |            - |            - |            - |            - |            -
            14:00        8.8 K |       62.1 K |       25.3 M |        568 K |       25.9 M |        $47.5
            15:00          546 |       17.0 K |        3.1 M |        163 K |        3.2 M |        $4.87
            16:00          110 |       21.7 K |        5.2 M |        206 K |        5.4 M |        $13.3
            17:00          600 |       54.6 K |       26.7 M |        528 K |       27.3 M |        $47.2
            18:00          365 |        8.8 K |        3.6 M |        287 K |        3.9 M |        $11.5
            19:00            - |            - |            - |            - |            - |            -
            20:00        3.0 K |       17.8 K |        3.6 M |        462 K |        4.1 M |        $13.9
            21:00          194 |        141 K |        6.9 M |        979 K |        8.0 M |        $38.7
            22:00          656 |        145 K |       29.7 M |        1.1 M |       31.0 M |        $76.9
            23:00          388 |        109 K |       40.1 M |        2.7 M |       42.9 M |         $119
 2026-04-08
            00:00           19 |        3.1 K |        3.9 M |        760 K |        4.7 M |        $20.4
 ------------------------------+--------------+--------------+--------------+--------------+-------------
            total       14.7 K |        581 K |        148 M |        7.8 M |        156 M |         $393
```

### Top days

```
$ tkstat -t 5
 claude / top days
                         input |       output |     cache rd |     cache cr |        total |         cost
 ------------------------------+--------------+--------------+--------------+--------------+-------------
       2026-04-07       14.8 K |        612 K |        150 M |        7.4 M |        158 M |         $389
       2026-03-20        5.4 K |        250 K |       90.7 M |        5.5 M |       96.5 M |         $245
       2026-03-31        4.2 K |        151 K |       66.3 M |        4.3 M |       70.7 M |         $185
       2026-03-18       14.0 K |        184 K |       50.5 M |        3.1 M |       53.8 M |         $131
       2026-03-17       12.0 K |        138 K |       39.1 M |        3.2 M |       42.5 M |         $117
 ------------------------------+--------------+--------------+--------------+--------------+-------------
            total       50.5 K |        1.3 M |        397 M |       23.5 M |        422 M |        $1067
```

### Heatmap

```
$ tkstat --heatmap
```

Renders a year-long GitHub-style contribution calendar using the Vega/D3 blues color palette with continuous color interpolation and log-scale normalization.

### Braille chart

```
$ tkstat --chart
```

Renders a braille-dot time series of daily token usage. Use `--chart-metric cost` to chart by estimated cost instead of tokens.

## Filters

```
tkstat --model opus         # only opus usage
tkstat --model sonnet       # only sonnet usage
tkstat -p myproject         # filter by project name (substring match)
tkstat -b 2026-03-01 -e 2026-03-31   # date range
tkstat --no-subagents       # exclude subagent usage
```

## Column selection

Default columns: `input`, `output`, `cache_rd`, `cache_cr`, `total`, `cost`.

Use `--columns` to pick exactly which columns to show:

```
tkstat --columns cost,reqs,sessions
tkstat --columns in,out,total,cost,reqs
```

Available columns: `input` (`in`), `output` (`out`), `cache_rd` (`crd`), `cache_cr` (`ccr`), `total` (`tot`), `cost`, `reqs` (`req`), `sessions` (`sess`).

## Output formats

```
tkstat --json -d        # JSON array
tkstat --oneline        # semicolon-delimited single line
tkstat -s               # short summary
```

## Columns explained

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

## How it works

Claude Code stores session logs at `~/.claude/projects/*/UUID.jsonl`. Each API response is a JSON line with a `usage` object containing token counts.

tkstat maintains a SQLite database (at `~/.local/share/tkstat/tkstat.db`) that caches parsed token records. On each run it checks which JSONL files have changed since the last read (by file size and mtime) and only parses the new bytes. This is why it's fast:

- **Cold start** (first run, full ingest): ~50ms for ~8K records
- **Warm start** (no new data): ~30ms

Use `--force-update` to wipe the database and re-ingest everything (e.g., after changing pricing config).

## Database

```
tkstat --db /path/to/my.db      # custom database path
tkstat --data-dir /path/to/logs # custom Claude log directory
tkstat --force-update           # full re-ingest
```

The default database location is `~/.local/share/tkstat/tkstat.db`. You can also set the `TKSTAT_DB` environment variable.

## License

MIT
