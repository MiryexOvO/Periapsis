# NSP Core — experimental reference 0.1

NSP is a media session and policy layer above MOQT. It targets live distribution,
interactive media and desktop/game streaming. A private broadcast is one use case,
not the scope of the protocol. Superiority over WebRTC remains a hypothesis to test.

## Implementation status

Implemented: strict draft-22 connection selection; upstream MOQT object transport;
local authenticated QMux/WebSocket relay; opaque object publisher/subscriber; hello
validation and deterministic capability selection; bounded advisory deadline queue.
Bidirectional offer/accept/ack exchange now runs on ordinary MOQT control tracks;
each accept is recomputed by the initiator before it can acknowledge and activate.

Rust `nsp-types`, `nsp-session`, `nsp-scheduler` provide the native structured capability
and scheduling core. Workspace check and strict Clippy pass; native executable tests
await MSVC linker installation. The Rust session crate does not yet parse JSON.

Not implemented: actual encoded A/V, LOC/MSF mapping, runtime enforcement of negotiated media policy, ABR, A/V clock mapping,
browser player, MSE gateway, OBS adapter, production quotas, native QUIC server,
browser WebTransport validation, P2P or WebRTC comparison.

The original `demo` sends a hello and computes acceptance locally. The separate
`handshake` demo completes the bidirectional exchange, with both peers ACTIVE.
The transport is still QMux/WebSocket; no native QUIC or media interoperability claim.

## Layer ownership

MOQT owns SETUP, request/track/group/object identifiers and wire serialization. QUIC
owns congestion control and retransmissions. NSP does not create new MOQT headers.
The reference uses upstream `@moq/net`; QMux uses upstream `@moq/qmux`.

NSP owns versioned application control, capabilities, media profiles, synchronization,
feedback and recovery conventions. Media payload formats must be pinned and validated
before a media profile is marked interoperable. Probe payloads here are opaque bytes.

## Proposed control contract

UTF-8 JSON hello, maximum 16 KiB, schema version 1:

```json
{"type":"nsp.hello","version":1,"profiles":["live"],"codecs":["application/octet-stream"],"extensions":[]}
```

Extension entries: `id`, integer `version >= 1`, boolean `required`. At most 32;
duplicate IDs are invalid. Unsupported required extensions on either side fail
negotiation. Unknown optional extensions are omitted from acceptance.

The responder selects the first locally preferred common profile and returns the
codec intersection. No common profile or codec, or mismatched schema, fails closed.
`application/octet-stream` is a diagnostic profile value, not a video codec.

The control envelope uses a 32-character lowercase hex `sessionId` supplied by a
trusted invitation. This correlates the exchange; it is not authentication or a
replacement for relay authorization. Envelopes are bounded to 16 KiB.

* `nsp.offer`: `sessionId`, initiator `hello`.
* `nsp.accept`: `sessionId`, responder `hello`, selected `agreement`.
* `nsp.ack`: `sessionId`, acknowledging the validated acceptance.
* `nsp.reject`: `sessionId`, rejecting negotiation without disclosing internal errors.

Control broadcasts: `nsp/{sessionId}/initiator` and `nsp/{sessionId}/responder`, each
with an `nsp.control` track. Both endpoints announce their outgoing track first.
These are application conventions, not standard MOQT extensions or LOC/MSF tracks.

Initiator: MOQ_READY -> AWAITING_ACCEPT -> ACK_PENDING -> ACTIVE. Responder:
MOQ_READY -> AWAITING_OFFER -> AWAITING_ACK -> ACTIVE. The initiator must hand ACK
to the transport before activation; the responder must actually receive it.

The initiator recomputes the agreement using the responder's preference order and
compares all selected codecs, extensions and buffer limits. Property ordering in
JSON is irrelevant. Wrong session IDs, forged agreements, unexpected/replayed
messages or failed negotiation reject the session and invalidate its agreement.
Transport close invalidates an active agreement. Failure/timeout closes resources;
reject notification is best-effort when the transport is already closing.

The `openPeer` helper exposes an agreement only while ACTIVE; lower-level raw track
helpers remain diagnostic primitives and do not enforce media policy. The Rust
state machine uses typed messages; JSON wire decoding remains TypeScript-only.
This remains an experimental schema rather than a stable wire API.

## Initial media policy defaults

| Profile | Target buffer | Maximum buffer |
|---|---:|---:|
| interactive | 35 ms | 100 ms |
| live | 100 ms | 300 ms |
| quality | 300 ms | 1000 ms |

These are initial local policy inputs. They are not measured latency promises.
Total latency also includes capture, encoding, network, decoding and display.

DeadlineQueue takes **local monotonic millisecond** deadlines. It refuses overflow
before changing accounting. Only dependency-ready units can be sent. Expired
disposable units can be dropped; expired reference/key/audio units request recovery.
The application must invalidate affected dependency chains and apply recovery.
The queue does not execute network sends or override congestion control.

## Transport capability levels

* `quic`: future native full transport, TLS security; HTTP is not required.
* `webtransport`: HTTPS browser connection using `moqt-22`; adapter exists, untested.
* `qmux-websocket`: current tested reference, `qmux-01.moqt-22` on WS; WSS requires
  a TLS gateway. It preserves MOQT objects but has TCP ordering and different loss
  behavior from QUIC. It is not evidence of QUIC performance or full standards conformance.
* `http-media`: planned MSE/HTTP compatibility gateway. Plain remote HTTP cannot use
  WebCodecs. QMux alone therefore does not provide HTTP video playback.

No transport is silently relabelled as another. Adapter rejects any negotiated
version other than `moq-transport-22`, including moq-lite.

## Reference relay limits and security boundary

Loopback only; distinct publisher/subscriber bearer tokens required. The handshake
demo also creates a separate trusted `peer` token allowing bidirectional control.
It is disabled unless explicitly configured. Credentials are
currently query parameters for the WebSocket upgrade and must not enter logs. This
is a local diagnostic convention, not the final NSP authentication protocol.

Browser Origin headers are refused until deployment origin policy is implemented.
The relay limits connections (32), QMux stream/window counts and WebSocket message
size. Probe writes are limited to 1 MiB per object. This does **not** enforce full
publisher broadcast/track/cache quotas; untrusted publishers are out of scope.
Do not expose this reference relay directly to the Internet. WSS termination alone
does not provide missing quotas or browser origin authorization.

## Development milestones

1. P0a (this increment): reproducible opaque object transfer, capability/policy core.
2. P0b: independently implemented MOQT peer interoperability and native QUIC test.
3. P1: LOC/MSF revision matrix, encoded video/audio, browser HTTPS playback and sync.
4. P2: strengthen the experimental bidirectional handshake, add feedback, bounded runtime buffers and recovery.
5. P3: actual HTTP/MSE gateway, TLS deployment, browser origin policy, scoped auth/quotas.
6. P4: ABR, native clients, OBS, repeatable WebRTC comparison; optional P2P later.

## Benchmark contract

Use matched input, codec settings, bitrate, hardware and network conditions. Record
glass-to-glass P50/P95/P99, freezes, recovery, A/V skew, throughput, CPU and memory.
Exercise clean links, 1/3/5% loss, RTT/jitter changes and abrupt bandwidth reduction.
Report native QUIC, WebTransport and TCP fallback separately. Object delivery tests
prove byte forwarding, not media latency, video quality or WebRTC superiority.
