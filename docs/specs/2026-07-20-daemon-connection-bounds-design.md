# Daemon connection bounds — design

**Date:** 2026-07-20
**Status:** approved for implementation
**Scope:** Medium #4 only: bound local-socket client, worker-thread, and stalled-I/O resources.

## Decision

The blocking thread-per-connection model remains. The daemon admits at most 64 concurrent clients through an atomic scoped permit acquired in the accept loop. A rejected stream never creates a reader thread, writer thread, or 1,024-entry outbound queue. It receives one id-less structured `unavailable` response with the configured limit and is closed; rejection diagnostics are logged on the first and power-of-two occurrences to remain observable without enabling log amplification.

Every admitted stream has a 30-second receive polling timeout, a 5-second send timeout, and a 15-minute inbound-idle deadline. Receive timeouts check shutdown and the idle deadline, so dead clients release permits and daemon shutdown does not leave connection readers blocked forever. A partial frame that stalls for a full poll interval is refused; legitimate local newline-framed requests are expected to arrive atomically relative to that generous interval.

The permit lives through writer-thread shutdown and releases on every return/panic unwind. With two threads per admitted client, connection workers are therefore bounded at 128 and outbound queue capacity at 65,536 lines.

## Local transport threat model

On Unix, the bound socket file is forced to owner-only mode `0600`, including custom socket paths. The containing directory must also be controlled by the daemon user; a caller-selected directory with unsafe ownership can still permit replacement or denial of service. A Unix mode regression test pins the supported guarantee.

Windows named-pipe access remains governed by the OS/interprocess security descriptor. This slice does not claim peer authentication on either platform: socket access is a local trust boundary, while API/MCP capability checks remain the authorization boundary. Cross-platform ACL hardening requires a transport API that exposes explicit descriptors and is deferred rather than approximated.

## Alternatives

Tokio or a worker pool was rejected because the measured concurrency requirement is small and admission/timeouts bound the existing model. Blocking in the accept loop until capacity returns was rejected because it hides overload and can fill the OS backlog. Dropping rejected streams without a frame was rejected because clients and operators could not distinguish overload from a crash.

## Verification

- Unit tests pin permit saturation and release.
- A stress integration test fills all 64 slots, attempts excess connections, observes structured overload, and proves an admitted client still dispatches successfully.
- A timeout-focused unit test proves an expired connection releases its permit.
- Unix integration pins socket mode `0600`.
- Full workspace tests, Clippy with warnings denied, and diff checks.
