/**
 * The envelope byte codec round-trips exactly and refuses to read past the end of a buffer.
 *
 * The opaque `envelope` inside a `MessageSend` is a flat, scheme-specific byte layout — fixed-width
 * key and ciphertext blocks, single bytes, and LEB128 varints — assembled and parsed by this
 * reader/writer pair rather than by MSE, because section 11 forbids JSON there. Two properties are
 * load-bearing. The write-then-read round trip must be exact to the byte: a varint that widened or a
 * block that shifted by one would hand the ratchet the wrong key and turn every message into
 * undecryptable noise. And the reader must treat a short or malformed envelope as an error, never
 * read uninitialised memory past the end: a peer (or the server relaying a peer's bytes) can send a
 * truncated envelope, and a reader that ran off the end would at best crash and at worst read
 * adjacent bytes as key material. So these tests pin the round trip, the LEB128 width boundaries, the
 * interleaving of scalars and blocks, and every bounds check the reader promises.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { EnvelopeReader, EnvelopeWriter, SdkError } from '../src/index.js';

test('a mixed envelope of bytes, varints, and blocks reads back exactly and in order', () => {
  const writer = new EnvelopeWriter();
  writer.u8(0x01);
  writer.varint(300);
  writer.bytes(new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
  writer.u8(0x7f);
  writer.varint(1);
  const encoded = writer.finish();

  const reader = new EnvelopeReader(encoded);
  assert.equal(reader.u8(), 0x01);
  assert.equal(reader.varint(), 300);
  assert.deepEqual(reader.take(4), new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
  assert.equal(reader.u8(), 0x7f);
  assert.equal(reader.varint(), 1);
});

test('u8 keeps only the low eight bits, so a scheme tag can never bleed into the next field', () => {
  const writer = new EnvelopeWriter();
  writer.u8(0x1ff); // 511: the high bit must be dropped, not carried into a second byte
  const encoded = writer.finish();
  assert.equal(encoded.length, 1, 'a single byte wrote more than one byte');
  assert.equal(new EnvelopeReader(encoded).u8(), 0xff);
});

test('varints use LEB128 widths, one byte below the boundary and two above it', () => {
  // 127 is the largest one-byte varint; 128 is the smallest two-byte one. Pinning the widths proves
  // this is real LEB128 and not a fixed-width int that would desync the Rust reader.
  const below = new EnvelopeWriter();
  below.varint(127);
  assert.equal(below.finish().length, 1);

  const above = new EnvelopeWriter();
  above.varint(128);
  assert.equal(above.finish().length, 2);
});

test('varints round-trip across every width boundary up to the u32 ceiling', () => {
  for (const value of [0, 1, 127, 128, 16_383, 16_384, 2_097_151, 2_097_152, 0xffffffff]) {
    const writer = new EnvelopeWriter();
    writer.varint(value);
    const decoded = new EnvelopeReader(writer.finish()).varint();
    assert.equal(decoded, value, `varint ${value} did not round-trip`);
  }
});

test('a fixed block sits between its neighbouring scalars without shifting either', () => {
  // bytes() flushes the pending scalars as their own chunk before appending the block, so the byte
  // before and after the block must land exactly adjacent to it.
  const writer = new EnvelopeWriter();
  writer.u8(0x11);
  writer.bytes(new Uint8Array([0x22, 0x33]));
  writer.u8(0x44);
  assert.deepEqual(writer.finish(), new Uint8Array([0x11, 0x22, 0x33, 0x44]));
});

test('reading one byte past the end fails cleanly instead of returning undefined', () => {
  const reader = new EnvelopeReader(new Uint8Array([0x05]));
  assert.equal(reader.u8(), 0x05);
  // The buffer is spent; the next read must throw, not hand back a garbage byte.
  assert.throws(() => reader.u8(), SdkError);
});

test('taking a block longer than what remains throws and leaves the reader where it was', () => {
  const reader = new EnvelopeReader(new Uint8Array([0x01, 0x02, 0x03]));
  assert.equal(reader.u8(), 0x01);
  // Only two bytes remain; asking for three must fail rather than read past the end.
  assert.throws(() => reader.take(3), SdkError);
  // And the failed take must not have consumed anything: the two real bytes are still readable.
  assert.deepEqual(reader.take(2), new Uint8Array([0x02, 0x03]));
});

test('taking exactly the remaining bytes succeeds, and one more then fails', () => {
  const reader = new EnvelopeReader(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
  assert.deepEqual(reader.take(4), new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
  assert.throws(() => reader.take(1), SdkError);
});

test('rest returns everything left once and nothing the second time', () => {
  const reader = new EnvelopeReader(new Uint8Array([0x01, 0x02, 0x03, 0x04]));
  assert.equal(reader.u8(), 0x01);
  assert.deepEqual(reader.rest(), new Uint8Array([0x02, 0x03, 0x04]));
  assert.deepEqual(reader.rest(), new Uint8Array([]), 'rest returned bytes after being consumed');
});

test('a varint that cannot fit a u32 is rejected on read rather than silently truncated', () => {
  // The writer accepts any safe integer, but envelope counts are u32; a value beyond that must fail
  // to decode rather than wrap around to a smaller, wrong count.
  const writer = new EnvelopeWriter();
  writer.varint(2 ** 40);
  const reader = new EnvelopeReader(writer.finish());
  assert.throws(() => reader.varint());
});

test('an empty writer finishes to an empty buffer', () => {
  assert.deepEqual(new EnvelopeWriter().finish(), new Uint8Array([]));
});
