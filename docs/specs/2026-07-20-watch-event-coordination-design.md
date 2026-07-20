# Watch event coordination — design

**Date:** 2026-07-20  
**Status:** approved in advance 2026-07-20  
**Scope:** Medium #2 only: retain daemon events observed during requests, correlate responses by ID, and prove watch refreshes from the retained event. Daemon health, connection admission, and query performance remain separate branches.

## Problem

`daemon::Conn::request` currently calls `next_frame` and discards every event encountered before a response. A TTY watch reacts to an event by requesting a new task snapshot. If a second mutation commits after that snapshot but before its response reaches the client, the second event is discarded and the rendered snapshot can remain stale forever.

The same method also returns the first response frame without checking its ID, despite describing the response as correlated.

## Decision

`Conn` owns two explicit inboxes: pending event frames and pending response envelopes. Wire reads are separated from queued-frame delivery.

- `request` assigns an ID, sends the envelope, and returns only the response carrying that ID.
- Events encountered while waiting are appended to the event inbox.
- Responses for other IDs are retained in the response inbox.
- `next_frame` exposes retained events before retained responses, then reads the wire.
- TTY watch handles the retained event on its next loop iteration and refreshes again.
- Non-TTY watch continues to receive and print every event individually; no implicit coalescing is introduced.

This keeps the existing newline-delimited protocol and server unchanged. A generation counter was rejected because it would require a new cross-layer contract for behavior the client can provide locally. A single mixed queue was rejected because correlated lookup would require rotating or rescanning events on every request and would obscure the two delivery policies.

## Error handling

Response envelopes without the requested ID are not accepted as the current call's result. They remain queued. EOF and malformed JSON retain their existing `io::Error` behavior. IDs are compared as JSON values so the client remains compatible with the current numeric envelope IDs.

## Verification

A scripted local-socket test forces this exact sequence:

1. client sends a list request;
2. server sends an event for a second change;
3. server sends an unrelated response;
4. server sends the requested stale list response;
5. client returns only the correlated response and then exposes the retained event;
6. a second list request returns a snapshot containing the second change.

The test is deterministic and uses the real `Conn` framing rather than sleeps. Existing daemon integration, CLI, workspace, and Clippy suites must remain green.

