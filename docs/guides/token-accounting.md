# Counting AI token spend per task

tasqx attributes AI token usage to the task the spend served — always as four
separate buckets (input, output, cache read, cache write), never a blended
total: cache-read tokens cost a fraction of fresh ones, so one number would
destroy exactly the split a cost report needs. The terminal report's TOKENS
column names the largest bucket and its own count (`cacheR 13.6M`);
`tasqx report --json` and the HTML report carry all four.

## Turn it on

```console
tasqx config set tokens.enabled true
```

That switch lets the daemon parse tool transcripts after a completion — nothing
is measured through it without a running daemon (`tasqx daemon`). Self-reports
on completion need no daemon and no switch.

## Three channels

**Self-report** — the primary channel. An agent completing a task over the API
or MCP passes what it spent: `input_tokens`, `output_tokens`,
`cache_read_tokens`, `cache_creation_tokens` (any one count is enough) plus an
optional `tool` and `model`. The agent is the only party that knows which task
the spend served; completing without counts earns a `tokens_hint` saying so.

A harness that reports only one number passes `total_tokens` instead of the
four, never beside them. It is stored as its own kind, shown as
`total (unsplit)`, and never split into input and output. A count that
arrives after the task was completed goes through `tasqx_add_tokens` over MCP,
or from a shell:

```console
tasqx tokens add 602 --total 37898
tasqx tokens add 602 --in 18400 --out 2600 --tool claude-code
```

**Log-parse** — the fallback. Name the calling tool on `start`/`done` and the
daemon reads the tool's own transcript afterwards:

```console
tasqx done 4 --client "claude-code 2.1" --session-id "$SID" --transcript-path "$TP"
```

Parsers exist for Claude Code, Codex, Gemini CLI and GitHub Copilot CLI, picked
from `--client` by substring — without it no transcript is ever read.

**Telemetry** — the opt-in OTLP receiver below. Samples are matched to the task
by session id, and beat log-parsing when both are available.

## How much to trust a figure

Every measurement carries a confidence. It grades how firmly the tokens are
tied to this task, not how precise the counts are: every channel reads
per-request figures.

| Channel | Earns | When |
|---|---|---|
| Telemetry (`otel`) | `high` | Always: samples are matched to the task by session id |
| Log-parse | `high` | An explicit `--transcript-path`, with the completion's session id confirmed in the file |
| Log-parse | `medium` | An explicit path to a file with no per-session anchor (Gemini, Copilot) |
| Log-parse | `low` | No path: the transcript was found by scanning and matched on time overlap alone |
| Log-parse | `low` | `tokens recompute` found the transcript gone, so it kept the counts and lowered the grade |
| Self-report | `medium` | Every report on `done`, and `tokens add`; `token.add` also accepts `low` |

A self-report is `medium` and can never be `high`. Only the agent knows which
task the spend served, so self-report is the primary channel. But nothing
tasqx holds can check the number, and the grade describes how checkable a
figure is, not how much the reporter is trusted. A report stays unverifiable
however much you trust whoever sent it.

## The OTLP receiver

`tasqx config set otlp.enabled true` makes the daemon listen on
`127.0.0.1:4318` (`otlp.port` to change it) — loopback only, never exposed.
Point Claude Code at it with four environment variables:

```console
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/json
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

The endpoint is the base URL — the exporter appends `/v1/logs` itself, which is
the path tasqx parses (`/v1/metrics` is accepted so exporters do not retry, but
never parsed). Receiver and exporter are both off by default, so no telemetry
moves until you turn on both.

## The rules

One task never mixes channels: a self-report is authoritative, and log-parsing
stands down for a task the agent already measured. A transcript sample banked
by one task is claimed globally — no other task can earn it, whatever its
re-read timestamp later says — and a sample falling inside two tasks' windows
is contested and banked for no one. Only contest ever removes tokens: a
transcript that goes missing or re-reads differently keeps its counts with
confidence downgraded to `low`, never deleted blind.

## Budgets: the other direction

Accounting tells you what a task cost once it is over. A budget is the same
number read before that:

```console
tasqx add Port the payment adapter --budget-tokens 200000
```

It counts **fresh** tokens — input, output and cache creation — and ignores
cache reads, because a budget dominated by re-reads measures how often the agent
re-read its own context rather than how big the work was. An unsplit
`total_tokens` counts in full: nothing can say how much of it was cache reads,
and counting none of it would let an unsplit report hide an overrun. That is not a blend of
the four buckets; those stay split everywhere they are reported.

**It stops nothing.** tasqx is a store an agent calls between turns: it never
sees the turn and could not interrupt one. An overrun is a signal, and usually
one signal in particular — the task was too big to hand over whole.
`tasqx report --outcomes` counts overruns so you can see which kinds of work
keep being cut too large.

## Repairing old history

| Command | What it does |
|---|---|
| `tasqx tokens recompute` | Dry-run: per-task delta, writes nothing |
| `tasqx tokens recompute --apply` | Write the repair |

The one verb in the API built to delete measurement rows, so the dry-run
default is the safety, not a convenience. It parses transcripts and runs
in-process only — a daemon refuses it over the socket; stop the daemon and run
`tasqx --no-daemon tokens recompute`.
