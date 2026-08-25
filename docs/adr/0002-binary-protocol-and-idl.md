# ADR-0002 — Custom binary protocol generated from one IDL

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §11, §12, §71, §72

## Context

Bandwidth is a top-three product property, and the platform must serve a Rust server, a
TypeScript web client, a Kotlin Android client and third-party bots. Two failure modes
dominate binary protocols: bloated encodings and client/server drift.

## Options

| Option                   | Pros                                                                 | Cons                                                                                                  |
| ------------------------ | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| JSON                     | Debuggable, universal                                                | 3–6× bytes, base64 for blobs, no schema enforcement                                                   |
| protobuf/gRPC            | Mature, codegen, evolution                                           | Toolchain per language, tag overhead on every field, gRPC framing unfit for tiny bidirectional frames |
| MessagePack/CBOR         | Compact-ish, self-describing                                         | Field names on the wire on every message                                                              |
| Custom MSE + IDL codegen | 4-byte frames, zero overhead on required fields, one source of truth | We own the codec and its fuzzing                                                                      |

## Decision

Define the protocol once in JSON IDL under `shared/protocol/schema`, generate Rust and
TypeScript. Encoding is MSE: required fields positional (no overhead), optional fields
tagged with a length so unknown ids are skippable. JSON remains for the REST/admin surface.

## Consequences

Adding an optional field is backward compatible without negotiation; changing a required
field is a protocol version. We must maintain a generator, fuzz the decoder, and keep
cross-language vectors green — that last one is what actually prevents drift.
