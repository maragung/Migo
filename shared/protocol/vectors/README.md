# Conformance vectors

These files are the contract between every Migo implementation. Rust reads them,
TypeScript reads them, Kotlin will read them. Each one pins **bytes**, not
behaviour, because the failure this directory exists to prevent is the one that
never shows up in a single-language test suite: two implementations that both
pass their own tests and disagree on the wire.

A vector is only worth what its expected value is worth. So each file records
where its expected bytes came from, and there are exactly two acceptable answers:

* **hand-authored from the specification.** The `wire/` files are computed by
  hand from `docs/02-protocol.md` §3–4 and written down before the code was run
  against them. They test the implementation against the spec, so a mismatch is
  a real question — is the code wrong, or is the document?
* **computed by an independent implementation.** The `crypto/` files are produced
  by `tools/vectors/generate_crypto_vectors.py`, which implements the same
  constructions from the RFCs using Python's `cryptography` and `hashlib` —
  a different codebase, a different language, different primitives.

What is *not* acceptable is pasting the Rust implementation's own output back in
as the expected value. That produces a test that passes forever and detects
nothing but accidental change, and it is worse than no test because it looks like
coverage. Where a case cannot be derived independently the file says so in its
`provenance` field, and such a case is honest about testing only cross-language
agreement, not correctness.

## Running them

```
make vectors          # Rust runners (and TypeScript, once packages/wire exists)
```

The Rust runners live in `server/crates/migo-wire/tests/vectors.rs`,
`server/crates/migo-crypto/tests/vectors.rs` and
`server/crates/migo-account/tests/vectors.rs`. They fail if a file is missing, if
a file is empty, or if a case does not parse — a vector suite that silently runs
zero cases is the most expensive kind of green build.

## File format

Every file is JSON with this envelope:

```json
{
  "$comment": "what this file covers",
  "provenance": "hand-authored from docs/02-protocol.md §4",
  "cases": [ ... ],
  "invalid": [ ... ]
}
```

`cases` are inputs that must encode to, and decode from, the given `hex`.
`invalid` are byte sequences that must be **rejected**, each naming the error
kind. Rejection cases matter as much as the happy path: a decoder that accepts a
malformed length prefix is a remote out-of-memory primitive, and no round-trip
test can find that.

All byte strings are lowercase hex with no separators. Integers that can exceed
2^53 are written as **decimal strings**, because JSON numbers in JavaScript are
doubles and `18446744073709551615` does not survive the trip.

### `wire/varint.json`

```json
{ "name": "one_hundred_fifty", "value": "150", "hex": "9601" }
```

`zigzag` holds signed cases as `{ "name", "value", "encoded" }` where `encoded`
is the unsigned value zig-zag maps onto — the mapping is tested on its own
because no MSE field uses a signed type yet (see *Gaps*).

### `wire/frames.json`

```json
{ "name": "traced",
  "frame": {
    "version": 1, "flags": 2, "opcode": 1, "correlation": 0,
    "trace": { "trace_id": "000102…", "span_id": "1011…" },
    "fragment": null,
    "payload": "" },
  "hex": "0102010000…" }
```

`flags` in a case is the value expected **after** decode. An encoder derives
`TRACED` and `FRAGMENT` from the presence of the blocks rather than from the
caller's bits, so those two are never set by hand.

A frame whose `flags` include `COMPRESSED` is not inflated by these vectors: raw
DEFLATE output is not byte-stable across implementations, so the payload is
carried opaquely and only the header is pinned. Compression policy is tested in
`migo-wire`'s own suite, where it belongs.

`length_prefixed` repeats a few frames with the 4-byte big-endian prefix used by
stream transports.

### `wire/mse.json`

Structs are described as a small program of writer operations, so one runner
covers any shape — including nesting and unknown-field skipping — without either
language needing generated types:

```json
{ "name": "struct_with_one_optional",
  "ops": [
    { "op": "enter" },
    { "op": "u64", "value": "42" },
    { "op": "u32", "value": "1" },
    { "op": "optional", "id": 1, "ops": [ { "op": "string", "value": "hello" } ] },
    { "op": "leave" }
  ],
  "hex": "2a0101060568656c6c6f" }
```

Ops: `enter`, `leave`, `bool`, `u32`, `u64`, `timestamp`, `id`, `string`,
`bytes`, `list_len`, `optional`. The runner replays the program through the
encoder and compares bytes, then replays it through the decoder over those same
bytes and compares values — both directions from one description.

Note that the optional *count* is an explicit `u32` op. It belongs to the
struct's own encoder, not to the `optional` operation, and writing it out keeps
the vector honest about what appears on the wire.

### `crypto/*.json`

`kdf.json` covers `kdf::derive` and `derive_pair`; `aead.json` covers
`aead::seal_with_nonce` and `open`; `mac.json` covers `MacKey::derive`, `tag` and
`tag_parts`. Each case carries its own `provenance`:

* `rfc-5869` — an HKDF test vector from the RFC, run through our function's
  parameter shape. Catches a genuine implementation bug.
* `rfc-8439` / `xchacha-draft` — an AEAD vector from the specification.
* `independent-python` — computed by the generator from the RFC construction.
  Catches an implementation bug *and* cross-language drift.

Regenerate with:

```
python3 tools/vectors/generate_crypto_vectors.py
```

The generator is committed and reviewable. If it and the Rust code ever agree on
something wrong, that is a design error, not a copied constant — and a reviewer
can see both derivations side by side.

### `crypto/account-*.json`

The unified-account vectors, consumed by `migo-account`'s runner (the Rust half;
TypeScript and Kotlin consumers land with their ports). Four files, two
provenances, and the split is the point:

* `account-domains.json`, `account-evm.json` — **independent-python**, from
  `tools/vectors/generate_account_vectors.py`. The generator implements HKDF
  from RFC 5869, BIP-32 from its specification and EIP-55 from the EIP, each
  self-checked against that document's own published vectors before it emits
  anything. Domain seeds, the founding device's E2EE sub-seeds, and
  `m/44'/60'/0'/0/i` addresses are checked against it.
* `account-mldsa.json`, `account-container.json` — **rust-reference**, written
  by `server/crates/migo-account/examples/write_reference_vectors.rs`. ML-DSA-65
  has no script-reproducible published vector set, and the container's bytes are
  the house composition, so the Rust implementation is the reference and these
  files test the *ports*, not the crate. The case-level `provenance` field says
  so, and the Rust runner re-asserts it before trusting a case.

Regenerate all four with `make vectors` (the two Python files also have a
`--check` mode in `vector-check`; the two rust-reference files are instead
re-derived byte for byte by `test-vectors-rust`, which is the same guarantee
expressed as a test).

## Gaps, stated deliberately

* **No signed or floating-point primitive cases.** `docs/02-protocol.md` §4
  specifies `i32`, `i64`, `f32`, `f64`, but no field in
  `shared/protocol/schema/structs.json` uses one, so `Writer` has no encoder for
  them. A vector for an encoder that does not exist would have to be written
  against an imagined implementation. The zig-zag mapping *is* covered, because
  that part does exist. When the first signed field lands, its vectors land in
  the same change.
* **No `map<string,T>` cases**, for the same reason: the schema has no map field.
* **No X3DH or ratchet chains yet.** They need fixed key material and a fixed
  sequence of steps; the file will be `crypto/session.json` and is the next
  thing added here.

## The rule

A new frame, a new optional field, or a change to any encoding does not merge
without a vector in the same change. `make vectors` runs in `make ci`; the CI
job runs it for both languages. That is the whole reason the directory exists.
