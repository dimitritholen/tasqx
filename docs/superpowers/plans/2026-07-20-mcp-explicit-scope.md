# MCP explicit scope implementation plan

## Task 1: Pin the new CLI contract

- [x] Add failing parse tests for `serve --scope read|write`.
- [x] Add failing tests proving `mcp token`, `serve --token`, and random/truncated token-shaped inputs are rejected.
- [x] Preserve read-only default startup semantics.

## Task 2: Remove false credential semantics

- [x] Replace `McpAction::Token` and `Serve::token` with `Serve::scope`.
- [x] Pass the explicit scope directly to `McpServer`.
- [x] Remove `Scope::mint_token`, `Scope::from_token`, and token round-trip tests.

## Task 3: Align documentation

- [x] Supersede D7 and update README, command help, manual, and generated docs.
- [x] Remove `TASQX_MCP_TOKEN` from current configuration/environment documentation.
- [x] State that future non-stdio transports require a separate authentication design.

## Task 4: Verify and integrate

- [x] Run focused CLI help/parse and MCP protocol suites.
- [x] Run full workspace tests, Clippy, and diff checks.
- [x] Update Low #1 with verification evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
