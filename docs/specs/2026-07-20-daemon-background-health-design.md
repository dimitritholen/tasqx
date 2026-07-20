# Daemon background health — design

**Date:** 2026-07-20
**Status:** implemented and verified 2026-07-20
**Scope:** Medium #3 only: make daemon event-pump, listener, and background-thread failures observable. Connection admission and query performance remain separate branches.

## Decision

The daemon fails closed when a component required for correct service becomes permanently unhealthy.

- `max_event_rowid` and `pump` return errors. Missing schema, query failure, and row-decode failure are never converted to zero or skipped.
- `pump` collects and validates a full batch before broadcasting it. Its watermark advances only through successfully decoded rows.
- Poller and reminder threads report fatal failures to the serve loop through a supervisor channel.
- The serve loop checks that channel on every nonblocking-accept iteration and returns an `io::Error` with component context.
- `WouldBlock` is the normal nonblocking accept state. Any other listener error terminates serving instead of retrying forever.
- Reminder delivery/write failures remain transient because the store retains their source data and the scheduler already invalidates its watermark for retry. Repeated identical failures are logged once until recovery or a changed failure.

No degraded health endpoint is added: the process does not intentionally remain alive when event propagation or scheduler store reads are broken. This is simpler and gives service managers an unambiguous restart/failure signal.

## Alternatives

A permanent degraded mode plus health RPC was rejected because clients could continue receiving successful request responses while watch/reminders were known stale. Retrying all SQLite failures forever was rejected for the same reason and because corruption/schema damage is not transient. A logging dependency was rejected; a small component-tagged supervisor error and transition logger are sufficient.

## Verification

- A damaged `events` table makes `pump` return an error without advancing its watermark.
- A running daemon whose event table is damaged exits through `serve_with_notifier` with poller context within a bounded test deadline.
- Existing reminder retry tests continue proving transient fire failures are retried.
- Full workspace tests, Clippy with warnings denied, and diff checks pass.
