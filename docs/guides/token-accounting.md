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

**Log-parse** — the fallback. Name the calling tool on `start`/`done` and the
daemon reads the tool's own transcript afterwards:

```console
tasqx done 4 --client "claude-code 2.1" --session-id "$SID" --transcript-path "$TP"
```

Parsers exist for Claude Code, Codex, Gemini CLI and GitHub Copilot CLI, picked
from `--client` by substring — without it no transcript is ever read.

**Telemetry** — the opt-in OTLP receiver below. Samples are matched to the task
by session id, and beat log-parsing when both are available.

Every measurement carries a confidence grading the correlation, never the
counts: `high` means the samples provably belong to the task (an explicit
transcript with the session id confirmed against it, or telemetry matched by
session id); `medium` is plausible but unproven — every self-report, and
Gemini/Copilot transcripts, which carry no per-session anchor; `low` means the
transcript was discovered by scanning and matched on time overlap alone.

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

## Repairing old history

```console
tasqx tokens recompute             # dry-run: per-task delta, writes nothing
tasqx tokens recompute --apply     # write the repair
```

The one verb in the API built to delete measurement rows, so the dry-run
default is the safety, not a convenience. It parses transcripts and runs
in-process only — a daemon refuses it over the socket; stop the daemon and run
`tasqx --no-daemon tokens recompute`.
