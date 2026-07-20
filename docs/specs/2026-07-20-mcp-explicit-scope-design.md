# MCP explicit scope — design

**Date:** 2026-07-20
**Status:** implemented and verified
**Scope:** Low #1 only: replace misleading MCP token semantics without changing MCP protocol behavior.

## Decision

The bundled MCP server is a local stdio child process. Its read/write choice is operator intent, not authentication. The CLI therefore becomes `tasqx mcp serve [--scope read|write]`, with `read` as the least-privilege default. The `token` subcommand, `--token`, `TASQX_MCP_TOKEN`, token minting, and prefix-based token parsing are removed.

`Scope` remains the concrete capability enforced by `McpServer`: read scope hides and rejects write tools, while write scope exposes them. The CLI parses the closed vocabulary and passes the selected scope directly. No secret store, signing, hashing, or credential abstraction is introduced because stdio has no remote principal to authenticate.

## Compatibility and migration

- MCP JSON-RPC framing, tool schemas, dispatch methods, and read/write enforcement do not change.
- Existing default startup remains read-only.
- Old `mcp token` and `serve --token` invocations fail at clap parsing with exit code 2 instead of accepting forgeable credentials.
- Documentation and examples name scope as operator-selected process configuration, never as authentication.
- A future socket/network transport may not treat `Scope` or a caller-provided scope string as authentication; it requires a separately designed credential and peer-authentication boundary.

## Verification

- Core tests pin read/write capability behavior without token round-tripping.
- CLI parse/help tests accept both explicit scopes, retain the read default, and reject the removed token forms including truncated and random values.
- Existing MCP protocol integration tests remain unchanged.
- Full workspace tests, Clippy with warnings denied, and diff checks pass.
