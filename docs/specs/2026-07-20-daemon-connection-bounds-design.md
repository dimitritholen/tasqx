# Daemon connection bounds — design

**Date:** 2026-07-20
**Status:** implemented and verified 2026-07-20
**Scope:** Medium #4 only: bound local-socket client, worker-thread, and stalled-I/O resources.

## Decision

The blocking thread-per-connection model remains. The daemon admits at most 64 concurrent clients through an atomic scoped permit acquired in the accept loop. A rejected stream never creates a reader thread, writer thread, or 1,024-entry outbound queue. It receives one id-less structured `unavailable` response with the configured limit and is closed; rejection diagnostics are logged on the first and power-of-two occurrences to remain observable without enabling log amplification.

Every admitted stream keeps efficient blocking I/O, a 5-second write deadline, and a 15-minute inbound-idle deadline. Unix uses native receive/send timeouts. Interprocess exposes no native named-pipe timeouts on Windows, so one watchdog per admitted Windows client monitors shutdown, last inbound activity, and in-progress writes, then uses `CancelIoEx` to interrupt the blocking handle. Dead clients therefore release permits and daemon shutdown does not leave connection readers or writers blocked forever.

The permit lives through writer/watchdog shutdown and releases on every return/panic unwind. Connection workers are bounded at 128 threads on Unix and 192 on Windows, and outbound queue capacity at 65,536 lines on either platform.

## Local transport threat model

On Unix, the bound socket file is forced to owner-only mode `0600`, including custom socket paths. The containing directory must also be controlled by the daemon user; a caller-selected directory with unsafe ownership can still permit replacement or denial of service. A Unix mode regression test pins the supported guarantee.

Windows named-pipe access remains governed by the OS/interprocess security descriptor. This slice does not claim peer authentication on either platform: socket access is a local trust boundary, while API/MCP capability checks remain the authorization boundary. Cross-platform ACL hardening requires a transport API that exposes explicit descriptors and is deferred rather than approximated.

## Alternatives

Tokio or a worker pool was rejected because the measured concurrency requirement is small and admission/timeouts bound the existing model. Blocking in the accept loop until capacity returns was rejected because it hides overload and can fill the OS backlog. Dropping rejected streams without a frame was rejected because clients and operators could not distinguish overload from a crash.

Native stream timeouts remain the Unix policy. The first Windows fallback—nonblocking polling—was rejected by the existing concurrent-client stress test: `PIPE_NOWAIT` reduced local request throughput from sub-second to more than a minute for the same workload. A bounded watchdog plus `CancelIoEx` preserves blocking performance and supplies the missing Windows deadlines explicitly.

## Verification

- Unit tests pin permit saturation/release and the idle-deadline boundary.
- `excess_clients_are_refused_without_disrupting_admitted_clients` fills all 64 slots, observes structured overload for eight excess connections, and proves an admitted client still dispatches successfully.
- The existing two-client/100-request integration completes in 0.26 seconds with blocking I/O, guarding against the rejected Windows polling regression.
- Unix integration pins socket mode `0600` where supported.
- Full workspace tests, Clippy with warnings denied, and diff checks pass.
