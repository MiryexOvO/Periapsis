# Validation record — 2026-11-02

Environment: Windows, Node.js 24.21.0, Rust 1.99.1 MSVC.

| Check | Observed result |
|---|---|
| `npm run check` | Pass |
| `pnpm install --frozen-lockfile --ignore-scripts` | Pass with pnpm 11.18.0 |
| `pnpm run check` | Pass |
| `pnpm test` | 22 passed, 0 failed |
| `npm test` | Earlier 12-test baseline passed; current 22-test invocation shares the same Node command |
| `npm run handshake` | Pass: offer/accept/ack over MOQT-22 control tracks; both peers ACTIVE |
| `npm run demo` | Pass: MOQT-22 on QMux/WebSocket, 65,536 bytes received, SHA-256 matches |
| `npm audit --omit=dev` | 0 known vulnerabilities at validation time |
| `cargo check --workspace --all-targets` | Pass, including Rust test code type checking |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass |
| `cargo test --workspace` | Cannot link tests: `link.exe` not found; Rust tests not executed |

Network test uses two independent subscriber connections and compares every byte
against the original random payload. It also checks negotiated version and transport,
object write bounds and missing-credential rejection. It does not use an independent
MOQT implementation: this is an internal integration test, not a conformance claim.

Other executable tests cover optional/required extension compatibility, malformed and
oversized hello data, dependency ordering, expired reference recovery, queue bounds,
asynchronous broadcast discovery and cancellation/timeout behavior.

New tests cover delayed activation until ACK, fabricated acceptance, session mismatch,
replay, JSON property ordering, invalidated agreement after close, failed capability
negotiation and missing-peer timeout. The network handshake test sends 32 KiB after
activation and compares every byte. Rust now has 15 behavior tests that type-check;
they have not executed because the MSVC linker remains unavailable.

Upstream libraries currently print diagnostic errors during normal shutdown. Test
assertions and processes exit successfully; log severity alone is not the result.

No glass-to-glass media latency, LOC/MSF interoperability, QUIC network performance,
browser playback or WebRTC improvement has been measured in this increment.
