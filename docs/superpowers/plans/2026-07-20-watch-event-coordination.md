# Watch event coordination implementation plan

**Goal:** Prevent `tasqx watch` from losing the only notification that can refresh a stale snapshot, while enforcing response-ID correlation.

**Architecture:** Split raw wire parsing from queued delivery. `Conn::request` searches retained responses and reads raw frames until its own ID arrives, queueing events and unrelated responses. `next_frame` drains retained events first so the existing TTY loop refreshes again and non-TTY mode emits each event.

## Task 1: Prove the dropped-event and uncorrelated-response behavior

- [ ] Add a test-only scripted local-socket server in `daemon.rs` tests.
- [ ] Send event -> unrelated response -> requested response for the first request.
- [ ] Assert the old client incorrectly accepts the unrelated response or loses the event (RED).
- [ ] Extend the script with a second list response containing the second change.

## Task 2: Implement correlated dual inboxes

- [ ] Rename the existing response queue and add a pending-event queue.
- [ ] Extract raw frame reading that never consumes queued frames.
- [ ] Make `request` first find a queued matching response, then queue events/unrelated responses until its own response arrives.
- [ ] Make `next_frame` drain events, then responses, then the wire.
- [ ] Update comments to state the actual retention and ordering guarantees.

## Task 3: Verify watch semantics and contracts

- [ ] Make the scripted test assert a retained event triggers the second request and the final state includes the second change.
- [ ] Run the focused daemon tests and CLI watch-related tests.
- [ ] Run `cargo test --workspace --all-targets --no-fail-fast`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings` and `git diff --check`.
- [ ] Mark Medium #2 verified, record evidence, commit, fast-forward merge into `main`, verify the merged checkout, and delete the branch.

