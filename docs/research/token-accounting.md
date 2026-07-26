# Per-task AI token accounting — research report

> **Implementation status (2026-07-24):** fully implemented on branch `feat/token-accounting` (backlog project `tasqx-token-accounting`, tasks #10–#19). Built exactly on the hybrid recommendation below: a `token_usage` table + `token.add`; correlation metadata in the start/done event payloads with MCP `clientInfo` capture; self-report params on `tasqx_complete_task`; local transcript parsers for Claude Code, Codex, Gemini CLI and Copilot CLI (`crates/tasqx-core/src/tokens/`); an async attribution engine in the daemon (`attribution.rs`, reminder-loop pattern, off the engine lock) that buckets per-request samples into each task's time window; an opt-in local OTLP/HTTP receiver (`otlp.rs`, `[otlp] enabled`, off by default) preferred over log-parsing when present; and five `tokens_*` metrics on the summary/HTML/terminal reports. Sources are stored separately (`log-parse` / `otel` / `self-report`) with a confidence grade; a task attributed by one source is never double-counted by another. The Codex `token_count` semantics question was resolved empirically (see the spike section). 715 tests pass.



*Deep-research run 2026-07-24 (106 agents, 24 sources, 25 claims adversarially verified: 21 confirmed, 4 refuted). Question: how can tasqx reliably capture tokens spent by arbitrary AI coding tools when a task is closed, for later reporting?*

## TL;DR

There is no universal channel, but there is a proven hybrid architecture. The anchor is **parsing each tool's local session/transcript files** — the only mechanism demonstrated to generalize (ccusage covers 15 coding agents this way, fully local, no provider APIs). On top of that: an opt-in **OTEL telemetry channel** for the three big CLIs, and **agent self-report on `tasqx_complete_task`** as a fallback for tools without local logs (e.g. Cursor).

## Per-tool local data sources (verified 3-0 unless noted)

| Tool | Local data | Detail |
|---|---|---|
| Claude Code | `~/.claude/projects/*.jsonl` (or `~/.config/claude/projects/`) | Per-message usage block: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`. Confirmed on this machine. |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (+ `archived_sessions/`) | `token_count` events, **persisted only since 2025-09-06** (openai/codex commit 0269096, PR #3221); earlier sessions carry no usage. Some early-Sept builds lack model metadata. |
| Copilot CLI | `~/.copilot/otel/*.jsonl` | Parsed by ccusage. |
| Gemini CLI | `${GEMINI_DATA_DIR:-~/.gemini/tmp}` or telemetry outfile `.gemini/telemetry.log` | Outfile and OTLP endpoint are mutually exclusive per session. |
| Cursor | **No usable local token logs** | Only account-level scraping (caut-style) or agent self-report. |

Prior art: [ccusage](https://ccusage.com/guide/) aggregates 15 agents (Claude Code, Codex, Gemini CLI, OpenCode, Copilot CLI, Amp, Goose, Kimi, Qwen, …) purely from these directories; its only network dependency is the LiteLLM pricing table for cost estimates. [caut](https://github.com/Dicklesworthstone/coding_agent_usage_tracker) covers 16+ providers (incl. Cursor) via a prioritized fallback chain: CLI-over-PTY → browser cookies → OAuth tokens → direct APIs → local JSONL — fragile and account-level, but the pattern for "no logs" tools.

## OTEL telemetry channel (higher fidelity, opt-in, off by default)

All three major CLIs emit per-request token data with timestamps — enough to bucket usage into a task's time window:

- **Claude Code**: `CLAUDE_CODE_ENABLE_TELEMETRY=1` + OTLP env vars → metric `claude_code.token.usage` (type: input/output/cacheRead/cacheCreation) and `claude_code.api_request` log events with `session.id` and `prompt_id`. OTEL support is beta; `cost_usd` is an estimate. ([docs](https://code.claude.com/docs/en/monitoring-usage))
- **Gemini CLI**: `gemini_cli.token.usage` metric and `gemini_cli.api_response` events with full breakdown incl. separate *thought* and *tool* tokens. ([docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/telemetry.md))
- **Codex**: `codex.api_request` events (total, input, cached_input, output, reasoning_output) with conversation IDs, enabled via the `[otel]` table in `~/.codex/config.toml`.

Caveat: the OTel GenAI semantic conventions (`gen_ai.usage.*`) are still Development status and do not standardize cache-token types; each tool uses its own namespace (`claude_code.*`, `gemini_cli.*`, `codex.*`). A collector in tasqx must handle per-tool schemas, not one universal schema.

## Task boundaries: hooks give correlation, not tokens

Claude Code hooks (`Stop`, `SubagentStop`, `SessionEnd`, `TaskCompleted`) deliver `transcript_path`, `session_id` and `prompt_id` (v2.1.196+) — ideal for correlating a task to a transcript — but the hook payload **contains no token or cost fields** (confirmed via open feature requests anthropics/claude-code #11008, #49588). `TaskCompleted` fires on Claude Code's internal task system, not on third-party MCP tools like `tasqx_complete_task`. Transcripts are written asynchronously and may lag when a hook fires: parse later, never synchronously in the hook.

## Recommended architecture for tasqx

1. **At task start/complete**: record timestamps plus every available correlation ID (session_id, prompt_id, transcript_path, cwd). The MCP server can try to identify the calling tool via `clientInfo`.
2. **Daemon computes tokens asynchronously after close** by parsing the detected tool's local logs and bucketing per-message usage into the task's time window — the ccusage-proven approach.
3. **Optional: local OTLP receiver in the daemon** plus documented opt-in configuration for Claude Code, Codex and Gemini CLI, for higher precision.
4. **Self-report as optional parameter on `tasqx_complete_task`** — last resort for tools with neither logs nor telemetry (Cursor).
5. **Store input/output/cacheRead/cacheCreation as separate fields**, never one blended total — cache tokens cost a fraction and every tool defines them differently.

## Key caveats

- **Log formats are undocumented internals** that change without notice (Codex only started persisting usage Sept 2025); the parser needs per-tool version tolerance.
- **Codex semantics unverified**: the claim that `token_count` events are cumulative and require delta computation was *refuted 1-2* in verification. Verify empirically against real rollout files before writing the parser (local material exists in `~/.codex/sessions/`).
- **Time-window bucketing is inherently fuzzy** when one session interleaves multiple tasks; consider having the agent declare an active-task marker per prompt.
- **Subscription plans (Claude Max, ChatGPT Plus) decouple tokens from real billing** — report tokens as tokens; show cost only as an estimate (ccusage uses the LiteLLM pricing table).

## Open questions before implementation

1. ~~Codex `token_count`: cumulative or per-turn?~~ **Resolved** — both, in one event; see spike result below.
2. Is time-window + prompt_id correlation sufficient, or must the agent declare its active task?
3. Exact log schemas for Aider, OpenCode, Copilot CLI (ccusage support suggests they are stable enough, but unverified here).
4. Can the MCP server reliably learn which tool/session is calling (`clientInfo`, environment inspection, or a session identifier passed at task start)?

## Spike result: Codex `token_count` semantics (2026-07-24, task #10)

Empirically verified against 9 local rollout files (240 token_count events, CLI versions 0.105.0 and 0.112.0). **Open question 1 is resolved.**

Each `token_count` event carries **both** representations:

```json
{"timestamp":"2026-03-10T10:47:41.050Z","type":"event_msg","payload":{"type":"token_count","info":{
  "total_token_usage":{"input_tokens":12811,"cached_input_tokens":3456,"output_tokens":341,"reasoning_output_tokens":116,"total_tokens":13152},
  "last_token_usage":{"input_tokens":12811,"cached_input_tokens":3456,"output_tokens":341,"reasoning_output_tokens":116,"total_tokens":13152},
  "model_context_window":258400},"rate_limits":null}}
```

Verified invariants (held in 9/9 files, both CLI versions):

1. `total_token_usage` is **cumulative** and monotonically non-decreasing across the session.
2. `last_token_usage` is the **per-request** usage of the most recent API call.
3. At every distinct step, `Δtotal_token_usage == last_token_usage` — the two representations are consistent.
4. **Events are duplicated** (~2× — e.g. 12 events for 6 distinct steps): the same totals are re-emitted multiple times per turn. Naively summing `last_token_usage` over all events double-counts.
5. `cached_input_tokens` is a subset of `input_tokens`; `reasoning_output_tokens` is a subset of `output_tokens`; `total_tokens = input_tokens + output_tokens` (OpenAI API semantics).

**Parser rule for tasqx (#15)**: iterate `token_count` events; keep only events where `total_token_usage.total_tokens` changed from the previous kept event (plus the first). Each kept event is one timestamped per-request sample with `last_token_usage`. Session total = `total_token_usage` of the last event. This supports both whole-session accounting and time-window bucketing.

Useful context fields: `session_meta` (first line) has session `id`, `cwd`, `cli_version`, `originator`; the **model lives in `turn_context.payload.model`** (e.g. `gpt-5.4`), not in `session_meta`. `event_msg/task_started` and `event_msg/task_complete` mark turn boundaries; `turn_context` also carries per-turn `cwd` — all useful for attribution.

## Refuted claims (for the record)

- "Codex token_count events are cumulative, requiring delta computation" — 1-2, unresolved; treat as unknown.
- "Only Codex and Claude expose parseable local logs" — 0-3; most agents do have local data.
- "Most providers expose no programmatic usage data" — 0-3.
- "Gemini CLI telemetry is disabled by default via `enabled: false` in settings.json" — 0-3 (the config-key detail was wrong; telemetry does need to be enabled, but not via that exact mechanism).

## Sources

Primary: [Claude Code monitoring](https://code.claude.com/docs/en/monitoring-usage) · [Claude Code hooks](https://code.claude.com/docs/en/hooks) · [Claude Code costs](https://code.claude.com/docs/en/costs) · [Gemini CLI telemetry](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/telemetry.md) · [ccusage](https://ccusage.com/guide/) ([Codex guide](https://ccusage.com/guide/codex/), [cost modes](https://ccusage.com/guide/cost-modes)) · [caut](https://github.com/Dicklesworthstone/coding_agent_usage_tracker) · [OTel GenAI observability](https://opentelemetry.io/blog/2026/genai-observability/) · [tokcat](https://github.com/handlecusion/tokcat)

Secondary/community: SigNoz Claude Code monitoring, claude-code-otel, anthropics/claude-code issues #49588 #11008 #33978 #25941, Vantage on Cursor costs, various blog analyses of Claude/Codex JSONL internals.
