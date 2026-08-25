/**
 * The assertions the shared vectors structurally cannot make.
 *
 * A vector case proves that two implementations agree with a third, independent
 * generator about a byte string. That is the strongest guarantee in this repository, and
 * it has a blind spot: a value that only ever appears *inside* a round trip is invisible
 * to it. Two examples, both found by mutating this package and watching the suite stay
 * green:
 *
 *   - The timestamp cases encode `toWire(x)` and compare against `fromWire` of the
 *     result. The epoch cancels. Change `MIGO_EPOCH_MS` by a second and every vector
 *     still passes, while every timestamp Migo has ever stored moves by a second.
 *   - The identifier cases compare `idFromBytes(hex)` against `idFromBytes(hex)`. A
 *     self-consistent but wrong base32 alphabet — or the right alphabet with the wrong
 *     bit shift — passes them all, and produces text nobody else can parse. Ids appear
 *     in URLs, log lines and support tickets, so the text form is a compatibility
 *     surface whether or not it crosses the wire.
 *
 * So the constants below are pinned against sources outside this package: the epoch
 * against `Date.parse`, the identifier against the canonical ULID from that
 * specification's own documentation. Where a value is written out longhand here, that is
 * the point — a test that derives its expectation from the code under test proves only
 * that the code is consistent with itself.
 *
 * The rest of the file covers the two subsystems that have no vector file yet, batching
 * and compression, and the one encoder property no small case can reach: that the frame
 * limit is enforced against the whole frame rather than one field at a time.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  BATCH_OPCODE,
  COMPRESS_MIN_BYTES,
  ID_BYTE_LEN,
  ID_TEXT_LEN,
  MAX_BATCH_ITEMS,
  MAX_FRAME_BYTES,
  MIGO_EPOCH_MS,
  NIL_ID,
  WireError,
  Writer,
  decodeBatchPayload,
  deflateRaw,
  encodeBatch,
  encodeFrame,
  flags,
  frameHeader,
  fromWire,
  idFromBytes,
  idToBytes,
  idUnixMs,
  inflateRaw,
  isCompressionAvailable,
  isId,
  maybeDeflate,
  parseId,
  toWire,
  tryParseId,
  unpackFrame,
  varint,
  type Frame,
  type WireErrorKind,
} from '../src/index.js';

/** The alphabet, written out again on purpose. See the module docs. */
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** The example identifier from the ULID specification. */
const ULID_TEXT = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
const ULID_HEX = '01563e3ab5d3d6764c61efb99302bd5b';
const ULID_UNIX_MS = 1469922850259;

function unhex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function hex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function kindOf(error: unknown): WireErrorKind {
  assert.ok(error instanceof WireError, `expected a WireError, got ${String(error)}`);
  return error.kind;
}

function throwsKind(run: () => unknown, kind: WireErrorKind, context: string): void {
  assert.throws(run, (error: unknown) => {
    assert.equal(kindOf(error), kind, context);
    return true;
  });
}

async function rejectsKind(
  run: () => Promise<unknown>,
  kind: WireErrorKind,
  context: string,
): Promise<void> {
  await assert.rejects(run, (error: unknown) => {
    assert.equal(kindOf(error), kind, context);
    return true;
  });
}

/**
 * Deterministic bytes that DEFLATE cannot shrink. `Math.random` would work and would
 * make a failure unreproducible, which for a size-threshold test is the difference
 * between a bug report and a shrug.
 */
function incompressible(length: number): Uint8Array {
  const out = new Uint8Array(length);
  let state = 0x2f6e2b1 >>> 0;
  for (let i = 0; i < length; i += 1) {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    out[i] = state & 0xff;
  }
  return out;
}

function frameOf(opcode: number, payload: string): Frame {
  return { header: frameHeader(opcode, opcode * 10), payload: new TextEncoder().encode(payload) };
}

test('the Migo epoch is 2024-01-01T00:00:00Z and nothing else', () => {
  // Pinned against the platform's own date parser rather than against a second copy of
  // the same number. This is the assertion an epoch mutation has to survive.
  assert.equal(MIGO_EPOCH_MS, Date.parse('2024-01-01T00:00:00.000Z'));
  assert.equal(new Date(fromWire(0)).toISOString(), '2024-01-01T00:00:00.000Z');

  // The mapping in both directions, at a distance where an off-by-a-second is visible.
  assert.equal(toWire(Date.parse('2024-01-02T00:00:00.000Z')), 86_400_000);
  assert.equal(fromWire(86_400_000), Date.parse('2024-01-02T00:00:00.000Z'));
  assert.equal(toWire(fromWire(1_234_567)), 1_234_567);

  // Before the epoch is not representable. Clamped, not rejected, and not wrapped into a
  // huge unsigned value — `Timestamp::to_wire` on the Rust side does the same, and a
  // default-constructed zero must not become the year 2554.
  assert.equal(toWire(0), 0);
  assert.equal(toWire(MIGO_EPOCH_MS - 1), 0);
  assert.equal(toWire(-1e12), 0);

  // Sub-millisecond input truncates towards zero instead of producing a fractional
  // varint, which would throw from `encodeU64` at the far end of a call stack.
  assert.equal(toWire(MIGO_EPOCH_MS + 1.9), 1);
});

test('the identifier text form matches the ULID specification', () => {
  assert.equal(ID_TEXT_LEN, 26);
  assert.equal(ID_BYTE_LEN, 16);

  const bytes = unhex(ULID_HEX);
  assert.equal(bytes.length, ID_BYTE_LEN);
  assert.equal(idFromBytes(bytes), ULID_TEXT);
  assert.equal(hex(idToBytes(parseId(ULID_TEXT))), ULID_HEX);

  // The time prefix is six big-endian bytes of Unix milliseconds — not Migo-epoch
  // milliseconds. Ids are generated by clients and sorted by databases that know nothing
  // about the Migo epoch, so this one place counts from 1970.
  assert.equal(idUnixMs(parseId(ULID_TEXT)), ULID_UNIX_MS);
  assert.equal(new Date(ULID_UNIX_MS).toISOString(), '2016-07-30T23:54:10.259Z');

  // Both ends of the range.
  assert.equal(NIL_ID, '00000000000000000000000000');
  assert.equal(idFromBytes(new Uint8Array(ID_BYTE_LEN)), NIL_ID);
  assert.equal(hex(idToBytes(NIL_ID)), '00'.repeat(ID_BYTE_LEN));
  assert.equal(idUnixMs(NIL_ID), 0);
  assert.equal(idFromBytes(new Uint8Array(ID_BYTE_LEN).fill(0xff)), '7ZZZZZZZZZZZZZZZZZZZZZZZZZ');

  // Every symbol, in order, checked through the low five bits of the last byte. Pins the
  // alphabet *and* the shift: a rotated alphabet or an off-by-one shift fails here even
  // though it would round-trip perfectly.
  for (let value = 0; value < CROCKFORD.length; value += 1) {
    const probe = new Uint8Array(ID_BYTE_LEN);
    probe[ID_BYTE_LEN - 1] = value;
    assert.equal(idFromBytes(probe).at(-1), CROCKFORD[value], `symbol ${value}`);
  }
  assert.equal(new Set(CROCKFORD).size, 32, 'the alphabet must have no repeats');
  for (const excluded of 'ILOU') {
    assert.ok(!CROCKFORD.includes(excluded), `${excluded} must not be in the alphabet`);
  }

  assert.throws(
    () => idFromBytes(new Uint8Array(15)),
    (error: unknown) => {
      assert.equal(kindOf(error), 'FieldOverflow');
      return true;
    },
  );
});

test('identifiers parse the way a human retypes them, and come back canonical', () => {
  // The three confusions Crockford documents, each applied to the canonical example. The
  // Rust `decode_char` accepts exactly these, and the two sides have to agree on which
  // strings are valid or a link that works in the app 404s in the browser.
  assert.equal(parseId(ULID_TEXT.toLowerCase()), ULID_TEXT);
  assert.equal(parseId('0IARZ3NDEKTSV4RRFFQ69G5FAV'), ULID_TEXT, 'I reads as 1');
  assert.equal(parseId('0lARZ3NDEKTSV4RRFFQ69G5FAV'), ULID_TEXT, 'l reads as 1');
  assert.equal(parseId('O1ARZ3NDEKTSV4RRFFQ69G5FAV'), ULID_TEXT, 'O reads as 0');
  assert.equal(parseId('o1arz3ndektsv4rrffq69g5fav'), ULID_TEXT, 'both at once');

  // Lenient parsing without canonicalisation would be worse than strictness: two strings
  // for one identifier, and `Map<Id, T>` quietly holding both.
  assert.ok(isId(ULID_TEXT));
  assert.ok(!isId(ULID_TEXT.toLowerCase()), 'a lenient spelling is parseable, not an id');
  assert.ok(!isId('O1ARZ3NDEKTSV4RRFFQ69G5FAV'));
  assert.ok(!isId(ULID_TEXT.slice(1)));
  assert.ok(!isId(123));
  assert.ok(!isId(null));

  // The three ways a candidate can fail, each reported specifically enough to fix.
  assert.deepEqual(tryParseId(''), { ok: false, why: { kind: 'length', actual: 0 } });
  assert.deepEqual(tryParseId(`${ULID_TEXT}A`), {
    ok: false,
    why: { kind: 'length', actual: 27 },
  });
  assert.deepEqual(tryParseId(`${ULID_TEXT.slice(0, 25)}!`), {
    ok: false,
    why: { kind: 'character', position: 25 },
  });
  assert.deepEqual(tryParseId(`U${ULID_TEXT.slice(1)}`), {
    ok: false,
    why: { kind: 'character', position: 0 },
  });
  // 26 characters carry 130 bits; an id is 128. A leading symbol above 7 would decode to
  // a value no 16-byte id can hold, and accepting it would make the text form ambiguous.
  assert.deepEqual(tryParseId(`8${ULID_TEXT.slice(1)}`), { ok: false, why: { kind: 'overflow' } });
  assert.deepEqual(tryParseId('ZZZZZZZZZZZZZZZZZZZZZZZZZZ'), {
    ok: false,
    why: { kind: 'overflow' },
  });

  assert.throws(() => parseId('nope'), TypeError);
});

test('batches round-trip and refuse every shape that would let a peer lie', async () => {
  const frames = [frameOf(1, 'first'), frameOf(2, 'second'), frameOf(3, 'third')];
  const batch = encodeBatch(frames);
  assert.equal(batch.header.opcode, BATCH_OPCODE);
  assert.equal(batch.header.flags, flags.BATCH);
  assert.equal(batch.header.correlation, 0, 'the envelope answers no request of its own');
  assert.deepEqual(await unpackFrame(batch), frames);

  // A one-element batch is the bare frame, byte for byte and object for object.
  const only = frameOf(7, 'alone');
  assert.equal(encodeBatch([only]), only);

  // An empty batch is legal and unpacks to nothing. A transport that coalesces on a timer
  // can reach this when the queue drains between the wake-up and the read.
  assert.deepEqual(decodeBatchPayload(encodeBatch([]).payload), []);

  // A frame that is not a batch still comes back as a one-element list, so the dispatch
  // path above this never has to ask which shape arrived.
  assert.deepEqual(await unpackFrame(only), [only]);

  throwsKind(() => encodeBatch([batch, only]), 'NestedBatch', 'nesting refused when encoding');
  throwsKind(
    () => encodeBatch(Array.from({ length: MAX_BATCH_ITEMS + 1 }, (_, i) => frameOf(i + 1, 'x'))),
    'BatchTooLarge',
    'too many elements refused when encoding',
  );

  // Decoding a nested batch: hand-built, because the encoder refuses to produce one.
  const inner = encodeFrame(batch);
  const nested: number[] = [];
  varint.encodeU64(1, nested);
  varint.encodeU64(inner.length, nested);
  for (const byte of inner) nested.push(byte);
  throwsKind(
    () => decodeBatchPayload(Uint8Array.from(nested)),
    'NestedBatch',
    'nesting refused when decoding',
  );

  // A count larger than the bytes can hold. This is the check that stops a four-byte
  // frame from asking for a 256-element array.
  throwsKind(() => decodeBatchPayload(Uint8Array.from([5, 0])), 'BatchTooLarge', 'lying count');
  throwsKind(
    () => decodeBatchPayload(Uint8Array.from([0xff, 0x01, 0x00])),
    'BatchTooLarge',
    'count above the item limit',
  );

  // An element length that runs off the end of the payload.
  const truncated: number[] = [];
  varint.encodeU64(1, truncated);
  varint.encodeU64(64, truncated);
  for (const byte of encodeFrame(only)) truncated.push(byte);
  throwsKind(
    () => decodeBatchPayload(Uint8Array.from(truncated)),
    'UnexpectedEnd',
    'element length past the end',
  );

  // Bytes after the last element. Silently ignoring them would let two different payloads
  // decode to the same batch, which for a signed handshake frame is a forgery primitive.
  const trailing = [...encodeBatch(frames).payload, 0x00];
  throwsKind(
    () => decodeBatchPayload(Uint8Array.from(trailing)),
    'TrailingBytes',
    'trailing bytes after the last element',
  );
});

test('compression is skipped unless it pays for itself', async () => {
  assert.ok(isCompressionAvailable(), 'Node has had CompressionStream since 18');

  // Below the threshold the answer is no regardless of how compressible the bytes are —
  // a 40-byte payload of zeros would shrink, and the flag byte plus the round trip
  // through two streams costs more than the saving.
  const tiny = new Uint8Array(COMPRESS_MIN_BYTES - 1);
  assert.equal(await maybeDeflate(tiny), null, 'below the size floor');

  // Above the threshold but incompressible: DEFLATE adds framing, so this is the case
  // where a naive encoder makes every frame bigger.
  assert.equal(await maybeDeflate(incompressible(4096)), null, 'no gain to be had');

  // Above the threshold and compressible.
  const compressible = new Uint8Array(4096).fill(0x61);
  const deflated = await maybeDeflate(compressible);
  assert.ok(deflated !== null, 'a 4 KiB run of one byte must compress');
  assert.ok(deflated.length * 10 < compressible.length, 'and by far more than the 10% floor');
  assert.deepEqual(await inflateRaw(deflated), compressible);

  // Round trip through the unconditional entry point, including the empty payload.
  assert.deepEqual(await inflateRaw(await deflateRaw(new Uint8Array(0))), new Uint8Array(0));

  // BTYPE 11 is reserved, so this is a stream no raw-DEFLATE decoder can accept.
  await rejectsKind(
    () => inflateRaw(Uint8Array.from([0xff, 0xff, 0xff, 0xff])),
    'DecompressFailed',
    'garbage',
  );

  // A decompression bomb: 64 KiB of zeros in a couple of hundred bytes, inflated under a
  // 1 KiB ceiling. The limit has to bite while reading, not after — by the time the last
  // chunk arrives the memory has already been spent.
  const bomb = await deflateRaw(new Uint8Array(64 * 1024));
  assert.ok(bomb.length < 1024, 'the bomb must be smaller than the limit it beats');
  await rejectsKind(() => inflateRaw(bomb, 1024), 'DecompressedTooLarge', 'bomb');

  // Neither error may quote the payload: these run on attacker-supplied bytes, and a hex
  // dump in a log line is both a log-injection vector and, for a private message, a
  // privacy incident.
  for (const error of [WireError.decompressFailed(), WireError.decompressedTooLarge(1024)]) {
    assert.doesNotMatch(error.message, /[0-9a-fA-F]{8}/, 'no payload bytes in the message');
  }
});

test('the frame limit is enforced against the whole frame, not one field at a time', () => {
  // Each field is comfortably legal; the frame is not. A writer that reset its length
  // counter for every nested scope would encode this happily and hand a 320 KiB frame to
  // a transport that has to reject it — or worse, to a peer whose reader rejects it, one
  // hop too late to say anything useful.
  const chunk = new Uint8Array(8192).fill(0x41);
  throwsKind(
    () => {
      const writer = new Writer();
      writer.u32(40);
      for (let field = 0; field < 40; field += 1) {
        writer.optional(field, (inner) => inner.bytes(chunk));
      }
      writer.finish();
    },
    'FrameTooLarge',
    'the sum of legal fields is not itself legal',
  );

  // The same total in one field is refused at the field's own limit instead, which is the
  // smaller number and so the one that must win.
  throwsKind(
    () => new Writer().bytes(new Uint8Array(MAX_FRAME_BYTES + 1)),
    'BytesTooLong',
    'one oversized field',
  );
});
