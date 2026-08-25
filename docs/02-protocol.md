# 02 — Migo Wire Protocol (MWP/1)

> The normative definition is the IDL in [`shared/protocol/schema`](../shared/protocol/schema).
> This document explains the framing, the encoding rules and the session lifecycle.
> Cross-language conformance is enforced by the vectors in
> [`shared/protocol/vectors`](../shared/protocol/vectors) — Rust and TypeScript must
> produce byte-identical output.

## 1. Why not JSON / protobuf / gRPC

| Option             | Verdict                                                                                                                                 |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------- |
| JSON               | 3–6× larger for our payloads; no binary blobs without base64 (+33%). Rejected on the hot path (brief §11)                               |
| protobuf           | Good, but pulls a compiler into every toolchain, and its field-tag overhead is paid on _every_ field. We pay it only on optional fields |
| gRPC               | HTTP/2 framing + headers per call, poor fit for a long-lived bidirectional session with tiny frames                                     |
| MessagePack / CBOR | Self-describing = field names on the wire. Fine for cold config, wasteful per chat message                                              |

MWP is ~40 lines of codec logic, generated from one IDL, with the evolution properties
we need and a **4-byte minimum frame header**. JSON is still used for the REST admin
surface where humans and curl matter more than bytes.

## 2. Transport bindings

| Transport  | Binding                                                                                    |
| ---------- | ------------------------------------------------------------------------------------------ |
| WebSocket  | One MWP frame per **binary** WS message. WS supplies the length, so the frame carries none |
| QUIC / TCP | Stream of `u32be length` + frame                                                           |
| HTTP       | REST fallback carries the same payload structs as JSON (development, admin, bots)          |

`permessage-deflate` is **disabled**; MWP does its own per-frame compression decision (§6).

## 3. Frame layout

```
 0        1        2..       ..        ..                       end
┌────────┬────────┬─────────┬──────────┬───────────────────────────┐
│version │ flags  │ opcode  │ correl.  │ [flag headers] │ payload  │
│  u8    │  u8    │ varint  │  varint  │                │  bytes   │
└────────┴────────┴─────────┴──────────┴───────────────────────────┘
```

- `version` — `1` for MWP/1. A frame with an unknown version is answered with
  `ERROR{PROTOCOL_VERSION_UNSUPPORTED}` and the connection is closed. Servers must
  never panic on a bad byte (brief §71).
- `flags` — see below.
- `opcode` — see the registry in `shared/protocol/schema/opcodes.json`. Hot opcodes are
  deliberately `< 0x80` so they encode in a single byte.
- `correlation` — request/response pairing. `0` means server-initiated event (no reply
  expected). Clients allocate correlations monotonically per connection.
- `payload` — the remainder of the frame, encoded with MSE (§4).

Minimum header: **4 bytes** (`version`, `flags`, 1-byte opcode, 1-byte correlation).

### Flags

| Bit    | Name           | Meaning                                                                                  |
| ------ | -------------- | ---------------------------------------------------------------------------------------- |
| `0x01` | `COMPRESSED`   | Payload is `deflate-raw`. Only set when it actually shrinks (§6)                         |
| `0x02` | `TRACED`       | 16-byte trace id + 8-byte span id precede the payload                                    |
| `0x04` | `BATCH`        | Payload is `varint count` then `count × (varint len, sub-frame)` (§7)                    |
| `0x08` | `ERROR`        | Payload is `Error` instead of the opcode's normal response type                          |
| `0x10` | `ACK_REQUIRED` | Receiver must acknowledge by watermark (§8)                                              |
| `0x20` | `FRAGMENT`     | Followed by `varint index`, `varint total`; payload is a slice of a larger logical frame |
| `0x40` | _reserved_     | Must be 0; receivers reject unknown flag bits                                            |
| `0x80` | `FLAGS_EXT`    | A second flags byte follows (reserved for MWP/2)                                         |

Rejecting unknown flag bits is intentional: silently ignoring them is how you ship a
protocol you can never extend safely.

## 4. MSE — Migo Struct Encoding

A struct is encoded as **required fields positionally**, then an **optional section**:

```
struct = required_field* , varint optional_count , optional_entry*
optional_entry = varint field_id , varint byte_len , bytes
```

Consequences, all deliberate:

- Required fields cost **zero** framing overhead — no tag, no length.
- Optional fields cost 2 bytes of overhead and can be **added at any time**; an old
  receiver skips unknown ids using `byte_len`. This is the entire forward-compatibility
  story and it needs no negotiation.
- Required fields are **frozen** for the life of a protocol version. Changing one is a
  new major version, with both supported during migration (brief §71).

### Primitives

| Type             | Encoding                                                                   |
| ---------------- | -------------------------------------------------------------------------- |
| `bool`           | 1 byte, `0` or `1`; any other value is a decode error                      |
| `u8 u16 u32 u64` | LEB128 unsigned varint, max 10 bytes                                       |
| `i32 i64`        | zig-zag then LEB128                                                        |
| `f32 f64`        | fixed 4 / 8 bytes little-endian                                            |
| `string`         | `varint len` + UTF-8; invalid UTF-8 is a decode error                      |
| `bytes`          | `varint len` + raw                                                         |
| `id`             | fixed **16 bytes** (ULID / UUIDv7), no length prefix                       |
| `timestamp`      | varint milliseconds since **Migo epoch** `2024-01-01T00:00:00Z`            |
| `duration_ms`    | varint milliseconds                                                        |
| `enum`           | varint discriminant; unknown values decode to the enum's `Unknown` variant |
| `list<T>`        | `varint count` + items                                                     |
| `map<string,T>`  | `varint count` + `(string, T)` pairs, keys sorted for determinism          |
| `struct`         | nested, as above                                                           |

Using a Migo epoch instead of the Unix epoch saves one byte per timestamp for the next
~40 years. At millions of messages per second that byte is a real line item.

### Hard limits (enforced by the codec, not by callers)

| Limit               | Value   | Reason                                       |
| ------------------- | ------- | -------------------------------------------- |
| `MAX_FRAME_BYTES`   | 262 144 | Anything larger belongs in object storage    |
| `MAX_STRING_BYTES`  | 65 536  | Bounded before allocation                    |
| `MAX_LIST_ITEMS`    | 4 096   | Prevents `varint count` allocation bombs     |
| `MAX_NESTING_DEPTH` | 16      | Prevents stack exhaustion via nested structs |
| `MAX_BATCH_ITEMS`   | 256     | Bounded work per frame                       |
| `MAX_VARINT_BYTES`  | 10      | Rejects non-canonical / infinite varints     |

Every limit is checked **before** allocating. A decoder that reads `varint count = 2^32`
and calls `Vec::with_capacity` is a remote OOM; ours returns `Error::LimitExceeded`.
These are the first things the fuzzers attack.

## 5. Session lifecycle

```
    client                                     gateway
      │  ── HELLO { protocol, client, features, locale, [auth], [resume] } ──▶
      │                                                                      │
      │  ◀── WELCOME { session, node, features, heartbeat_ms, limits, time } ─┤
      │        or ERROR{…} / RECONNECT_HINT{ endpoint, after_ms }             │
      │                                                                      │
      │  ── AUTHENTICATE { access_token, device } ──▶  (if not in HELLO)      │
      │  ◀── AUTHENTICATED { user, device, capabilities } ────────────────────┤
      │                                                                      │
      │  ── SUBSCRIBE { topics[] } ──▶       ◀── events … ────────────────────┤
      │  ── PING ──▶  ◀── PONG ──                                            │
      │  ── ACK { watermark } ──▶                                            │
```

- **Feature negotiation** (brief §72): `HELLO.features` and `WELCOME.features` are
  `u64` bitmasks; the session uses the **intersection**. A server never emits a frame
  for a feature the client did not advertise. Bit assignments live in
  `schema/meta.json`.
- **Heartbeat**: the server dictates the interval in `WELCOME`; the client sends `PING`
  at that cadence, adaptively backing off on battery saver / low-data mode. Missing
  2 intervals closes the socket. Both sides may ping.
- **Resume**: `HELLO.resume { session_id, last_frame_seq }`. The gateway keeps a small
  ring buffer of `Critical` frames per session for a short window, so a subway tunnel
  costs a few hundred bytes, not a resync. If the buffer no longer covers the client,
  the reply is `RESUME_REQUIRED` and the client falls back to cursor sync
  ([01-architecture.md](01-architecture.md) §6).
- **Reconnect** uses exponential backoff `1,2,4,8,16,30 s` with full jitter (brief §18).
  On planned drain the server sends `RECONNECT_HINT` with a target endpoint and a
  _randomised_ delay so 50 000 clients do not return in the same second.

## 6. Compression policy

Compress a payload only when **all** hold:

1. Both sides negotiated `FEATURE_COMPRESSION`.
2. Payload ≥ `COMPRESS_MIN_BYTES` (512).
3. The compressed form is at least 10 % smaller.

Otherwise send it raw. Compressing a 60-byte chat message makes it bigger and burns
battery on both ends (brief §12). `deflate-raw` is chosen because browsers implement it
natively (`CompressionStream`), so the web client pays **zero** bundle bytes for it.

## 7. Batching and coalescing

`BATCH` frames amortise the 4-byte header and, more importantly, one WebSocket message
and one TLS record across many events. The gateway batches with a **≤ 15 ms** linger —
long enough to group a burst, short enough to be invisible.

Coalescing is separate and applies to `Coalescable` classes: within a linger window,
only the newest presence/typing/count event per key survives. A room where 800 people
toggle presence emits one aggregated update, not 800.

## 8. Acknowledgement and redelivery

Frames flagged `ACK_REQUIRED` are held in the session's redelivery buffer until the
client sends `ACK { watermark }`. Watermarks are cumulative, so one ACK retires many
frames (a few bytes for hundreds of messages). On resume, everything above the last
acknowledged watermark is replayed — in order, with the original IDs, so client-side
dedup makes replay harmless.

## 9. Errors

`ERROR` payloads carry a **stable numeric code**, a machine-readable symbol, an optional
human message (already localised by the client, never by the server), and a
`retry_after_ms` when applicable. Codes are grouped so clients can act on the class
without knowing every member:

| Range       | Class                | Client behaviour                          |
| ----------- | -------------------- | ----------------------------------------- |
| `1000–1099` | Protocol             | Fatal. Upgrade or close                   |
| `1100–1199` | Auth                 | Refresh token, then re-login              |
| `1200–1299` | Permission           | Surface "not allowed", do not retry       |
| `1300–1399` | Validation           | Bug in the client. Log, do not retry      |
| `1400–1499` | Rate limit / quota   | Retry after `retry_after_ms`, with jitter |
| `1500–1599` | Not found / conflict | Reconcile local state                     |
| `1600–1699` | Server / transient   | Retry with backoff                        |
| `1700–1799` | Federation           | Retry; surface degraded state             |

The full list is generated from `schema/errors.json` into both languages, so a client
can never invent a code the server does not know.

## 10. Security properties of the transport

- TLS 1.3 (or QUIC) is mandatory; MWP defines no plaintext transport, not even in dev.
- MWP does **not** protect message confidentiality by itself — private content is
  already sealed by E2E ([03-security-threat-model.md](03-security-threat-model.md)).
  Transport encryption protects metadata and room content.
- Frames are validated before dispatch: version, flags, opcode existence, session state
  for that opcode, rate-limit cost, then payload decode. An unauthenticated session may
  only use `HELLO`, `AUTHENTICATE`, `PING`, `RESUME`.
- Every opcode has a rate-limit cost declared in the IDL, so a new opcode cannot be
  merged without one.

## 11. Versioning

`MWP/1` is frozen once the first public client ships. Additive change (new opcodes, new
optional fields, new enum variants, new feature bits) happens **inside** v1. Breaking
change means `MWP/2`, and servers speak both for at least one full client-deprecation
window. A server must never crash or hang because an old client connected (brief §71).
