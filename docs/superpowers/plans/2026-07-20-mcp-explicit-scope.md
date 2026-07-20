# MCP explicit scope implementation plan

## Task 1: Pin the new CLI contract

- [ ] Add failing parse tests for `serve --scope read|write`.
- [ ] Add failing tests proving `mcp token`, `serve --token`, and random/truncated token-shaped inputs are rejected.
- [ ] Preserve read-only default startup semantics.

## Task 2: Remove false credential semantics

- [ ] Replace `McpAction::Token` and `Serve::token` with `Serve::scope`.
- [ ] Pass the explicit scope directly to `McpServer`.
- [ ] Remove `Scope::mint_token`, `Scope::from_token`, and token round-trip tests.

## Task 3: Align documentation

- [ ] Supersede D7 and update README, command help, manual, and generated docs.
- [ ] Remove `TASQX_MCP_TOKEN` from current configuration/environment documentation.
- [ ] State that future non-stdio transports require a separate authentication design.

## Task 4: Verify and integrate

- [ ] Run focused CLI help/parse and MCP protocol suites.
- [ ] Run full workspace tests, Clippy, and diff checks.
- [ ] Update Low #1 with verification evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
