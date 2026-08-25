/**
 * The TypeScript half of the cross-language conformance suite.
 *
 * This file and `server/crates/migo-wire/tests/vectors.rs` read the *same* JSON files and
 * make the same assertions. That is the point: two independent implementations of one
 * binary protocol, checked against expected bytes that came from neither of them. When
 * this suite and the Rust suite disagree, one of the two codecs is wrong and the vectors
 * say which.
 *
 * Where this runner asserts *more* than the Rust one, it is because TypeScript has a risk
 * Rust does not: there is no `u64`, so every integer above 2^53 goes through a split
 * accumulator or a `bigint`. Those two paths have to produce identical bytes, and the
 * vectors are the only place that is checked.
 */

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

import {
  MAX_NESTING_DEPTH,
  Reader,
  WireError,
  Writer,
  decodeFrame,
  decodeFrameLengthPrefixed,
  encodeFrame,
  encodeFrameLengthPrefixed,
  fromWire,
  idFromBytes,
  toWire,
  varint,
  type Frame,
  type FrameHeader,
  type Fragment,
  type Id,
  type TraceContext,
} from '../src/index.js';

// --- plumbing ---------------------------------------------------------------

const VECTORS_DIR = resolve(import.meta.dirname, '../../../../shared/protocol/vectors/wire');

interface Case {
  readonly name?: string;
  readonly [key: string]: unknown;
}

function load(file: string): Record<string, unknown> {
  const path = join(VECTORS_DIR, file);
  if (!existsSync(path)) {
    throw new Error(
      `${path} is missing; run python3 tools/vectors/generate_wire_vectors.py from the repo root`,
    );
  }
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
}

/** Pulls one section out of a vector file, refusing an empty one. */
function section(file: Record<string, unknown>, name: string, path: string): Case[] {
  const found = file[name];
  assert.ok(Array.isArray(found), `${path} has no \`${name}\` section`);
  assert.ok(found.length > 0, `${path} \`${name}\` is empty`);
  return found as Case[];
}

function caseName(item: Case): string {
  return typeof item.name === 'string' ? item.name : '<unnamed>';
}

function text(item: Case, key: string): string {
  const value = item[key];
  assert.equal(typeof value, 'string', `case \`${caseName(item)}\` needs a string \`${key}\``);
  return value as string;
}

function unhex(value: string): Uint8Array {
  assert.equal(value.length % 2, 0, `hex string of odd length: ${value}`);
  const out = new Uint8Array(value.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    const byte = Number.parseInt(value.slice(i * 2, i * 2 + 2), 16);
    assert.ok(Number.isInteger(byte), `not hex: ${value}`);
    out[i] = byte;
  }
  return out;
}

function hex(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}

function bytesOf(item: Case, key: string): Uint8Array {
  return unhex(text(item, key));
}

/** Vector integers are decimal *strings*, because `u64::MAX` is not a JSON number. */
function big(item: Case, key: string): bigint {
  const raw = item[key];
  if (typeof raw === 'string') return BigInt(raw);
  if (typeof raw === 'number') return BigInt(raw);
  throw new Error(`case \`${caseName(item)}\` needs an integer \`${key}\``);
}

function small(item: Case, key: string): number {
  const value = big(item, key);
  assert.ok(value <= 0xffffffffn, `case \`${caseName(item)}\` \`${key}\` does not fit u32`);
  return Number(value);
}

function idOf(hexText: string): Id {
  return idFromBytes(unhex(hexText));
}

/**
 * The failure name a vector file uses.
 *
 * These strings are the Rust enum's variant names. Comparing to `err.kind` rather than to a
 * message is what lets both languages read one file: a message can be reworded, a variant
 * name is part of the protocol's vocabulary.
 */
function kindOf(error: unknown): string {
  assert.ok(error instanceof WireError, `expected a WireError, got ${String(error)}`);
  return error.kind;
}

/** Asserts that `run` fails, and fails in the exact way the vector predicted. */
function expectError(item: Case, run: () => unknown, context: string): void {
  const expected = text(item, 'error');
  let outcome: unknown;
  try {
    outcome = run();
  } catch (error) {
    const actual = kindOf(error);
    assert.equal(
      actual,
      expected,
      `${context} case \`${caseName(item)}\` failed with ${actual}, vector says ${expected}` +
        (typeof item.why === 'string' ? ` (${item.why})` : ''),
    );
    return;
  }
  assert.fail(
    `${context} case \`${caseName(item)}\` was accepted (${String(outcome)}), vector says it must fail with ${expected}`,
  );
}

// --- varint -----------------------------------------------------------------

test('varints encode and decode as the vectors say', () => {
  const file = load('varint.json');
  for (const item of section(file, 'cases', 'varint.json')) {
    const value = big(item, 'value');
    const expected = bytesOf(item, 'hex');
    const label = caseName(item);

    const encoded: number[] = [];
    varint.encodeU64(value, encoded);
    assert.equal(hex(Uint8Array.from(encoded)), hex(expected), `encoding case \`${label}\``);
    assert.equal(varint.encodedLen(value), expected.length, `predicted length for \`${label}\``);

    const decoded = varint.decodeU64(expected, 0);
    assert.equal(decoded.value, value, `decoding case \`${label}\``);
    assert.equal(decoded.used, expected.length, `bytes consumed by \`${label}\``);

    // The `number` path has to agree with the `bigint` path byte for byte. This is the
    // assertion the Rust runner has no need for and the one most likely to catch a bug
    // here: the split accumulator is arithmetic nobody else in the protocol performs.
    if (value <= BigInt(Number.MAX_SAFE_INTEGER)) {
      const asNumber = Number(value);
      const viaNumber: number[] = [];
      varint.encodeU64(asNumber, viaNumber);
      assert.equal(hex(Uint8Array.from(viaNumber)), hex(expected), `number encoding \`${label}\``);
      assert.equal(varint.encodedLen(asNumber), expected.length, `number length \`${label}\``);
      const safe = varint.decodeU64Safe(expected, 0);
      assert.equal(safe.value, asNumber, `safe decoding \`${label}\``);
      assert.equal(safe.used, expected.length, `safe bytes consumed by \`${label}\``);
    }
    if (value <= 0xffffffffn) {
      const narrow = varint.decodeU32(expected, 0);
      assert.equal(narrow.value, Number(value), `u32 decoding \`${label}\``);
    } else {
      assert.throws(
        () => varint.decodeU32(expected, 0),
        (error: unknown) => kindOf(error) === 'LengthOverflow',
        `\`${label}\` does not fit u32 and must be refused as one`,
      );
    }
  }
});

test('the zigzag mapping matches the vectors', () => {
  const file = load('varint.json');
  for (const item of section(file, 'zigzag', 'varint.json')) {
    const signed = BigInt(text(item, 'value'));
    const encoded = big(item, 'encoded');
    const label = caseName(item);
    assert.equal(varint.zigzagEncode(signed), encoded, `zigzagEncode for \`${label}\``);
    assert.equal(varint.zigzagDecode(encoded), signed, `zigzagDecode for \`${label}\``);

    // And the encoded form is what actually goes on the wire.
    const bytes: number[] = [];
    varint.encodeU64(encoded, bytes);
    assert.equal(hex(Uint8Array.from(bytes)), text(item, 'hex'), `zigzag bytes for \`${label}\``);
  }
});

test('malformed varints are rejected', () => {
  const file = load('varint.json');
  for (const item of section(file, 'invalid', 'varint.json')) {
    const input = bytesOf(item, 'hex');
    expectError(item, () => varint.decodeU64(input, 0), 'varint');
  }
});

// --- frames -----------------------------------------------------------------

function headerFromCase(spec: Case): FrameHeader {
  const rawTrace = spec.trace;
  let trace: TraceContext | null = null;
  if (rawTrace !== null && typeof rawTrace === 'object') {
    const entry = rawTrace as Case;
    trace = { traceId: bytesOf(entry, 'trace_id'), spanId: bytesOf(entry, 'span_id') };
  }

  const rawFragment = spec.fragment;
  let fragment: Fragment | null = null;
  if (rawFragment !== null && typeof rawFragment === 'object') {
    const entry = rawFragment as Case;
    fragment = { index: small(entry, 'index'), total: small(entry, 'total') };
  }

  return {
    version: small(spec, 'version'),
    flags: small(spec, 'flags'),
    opcode: small(spec, 'opcode'),
    correlation: small(spec, 'correlation'),
    trace,
    fragment,
  };
}

test('frames encode and decode as the vectors say', () => {
  const file = load('frames.json');
  for (const item of section(file, 'cases', 'frames.json')) {
    const spec = item.frame as Case;
    assert.ok(spec, `case \`${caseName(item)}\` has a frame`);
    const expected = bytesOf(item, 'hex');
    const header = headerFromCase(spec);
    const payload = bytesOf(spec, 'payload');
    const label = caseName(item);

    const frame: Frame = { header, payload };
    assert.equal(hex(encodeFrame(frame)), hex(expected), `encoding case \`${label}\``);

    const decoded = decodeFrame(expected);
    assert.deepEqual(decoded.header, header, `decoded header for \`${label}\``);
    assert.equal(hex(decoded.payload), hex(payload), `decoded payload for \`${label}\``);
  }
});

test('length-prefixed frames match the vectors', () => {
  const file = load('frames.json');
  for (const item of section(file, 'length_prefixed', 'frames.json')) {
    const body = bytesOf(item, 'frame_hex');
    const expected = bytesOf(item, 'hex');
    const label = caseName(item);

    const frame = decodeFrame(body);
    assert.equal(
      hex(encodeFrameLengthPrefixed(frame)),
      hex(expected),
      `length-prefixed encoding of \`${label}\``,
    );

    const parsed = decodeFrameLengthPrefixed(expected);
    assert.ok(parsed !== null, `case \`${label}\` must be a complete frame`);
    assert.equal(parsed.consumed, expected.length, `case \`${label}\` consumed`);
    assert.equal(hex(encodeFrame(parsed.frame)), hex(body), `case \`${label}\` round trip`);

    // One byte short is not an error on a stream transport, it is Tuesday. A reader that
    // treated it as one would drop a connection every time a frame straddled two TCP
    // segments.
    assert.equal(
      decodeFrameLengthPrefixed(expected.subarray(0, expected.length - 1)),
      null,
      `case \`${label}\` must be incomplete when a byte is missing`,
    );
  }
});

test('malformed frames are rejected', () => {
  const file = load('frames.json');
  for (const item of section(file, 'invalid', 'frames.json')) {
    const input = bytesOf(item, 'hex');
    expectError(item, () => decodeFrame(input), 'frame');
  }
});

// --- MSE --------------------------------------------------------------------

const SAFE_MAX = BigInt(Number.MAX_SAFE_INTEGER);

/**
 * Replays a writer program from a vector file.
 *
 * One interpreter covers every struct shape in the schema, which is why the vectors
 * describe programs rather than named types: neither this runner nor the Rust one has to
 * be regenerated when a struct is added.
 */
function writeOps(writer: Writer, ops: readonly Case[]): void {
  for (const op of ops) {
    switch (text(op, 'op')) {
      case 'enter':
        writer.enter();
        break;
      case 'leave':
        writer.leave();
        break;
      case 'bool':
        assert.equal(typeof op.value, 'boolean', 'bool op needs a boolean value');
        writer.bool(op.value as boolean);
        break;
      case 'u32':
        writer.u32(small(op, 'value'));
        break;
      case 'u64': {
        const value = big(op, 'value');
        // Both encoders, chosen the way a generated struct chooses: `bitmask64` fields go
        // through `u64big`, everything else through `u64`.
        if (value <= SAFE_MAX) writer.u64(Number(value));
        else writer.u64big(value);
        break;
      }
      case 'timestamp':
        writer.timestamp(fromWire(Number(big(op, 'value'))));
        break;
      case 'id':
        writer.id(idOf(text(op, 'value')));
        break;
      case 'string':
        writer.str(text(op, 'value'));
        break;
      case 'bytes':
        writer.bytes(bytesOf(op, 'value'));
        break;
      case 'list_len':
        writer.listLen(Number(big(op, 'value')));
        break;
      case 'optional': {
        const nested = (op.ops ?? []) as Case[];
        writer.optional(small(op, 'id'), (inner) => {
          writeOps(inner, nested);
        });
        break;
      }
      default:
        throw new Error(`unknown write op \`${text(op, 'op')}\``);
    }
  }
}

/**
 * Replays the same program as reads.
 *
 * An op that carries a `value` is asserted against it; an op without one is read and
 * discarded, which is what the malformed-input cases need. An op marked `unknown` is the
 * forward-compatibility path: the field is skipped by its length instead of being decoded,
 * exactly as a generated decoder does with a field id from a newer peer.
 */
function readOps(reader: Reader, ops: readonly Case[], label: string): void {
  for (const op of ops) {
    const has = (key: string): boolean => op[key] !== undefined;
    switch (text(op, 'op')) {
      case 'enter':
        reader.enter();
        break;
      case 'leave':
        reader.leave();
        break;
      case 'bool': {
        const got = reader.bool();
        if (has('value')) assert.equal(got, op.value, `bool in case \`${label}\``);
        break;
      }
      case 'u32': {
        const got = reader.u32();
        if (has('value')) assert.equal(got, small(op, 'value'), `u32 in case \`${label}\``);
        break;
      }
      case 'u64': {
        const expected = has('value') ? big(op, 'value') : null;
        if (expected !== null && expected <= SAFE_MAX) {
          assert.equal(reader.u64(), Number(expected), `u64 in case \`${label}\``);
        } else {
          const got = reader.u64big();
          if (expected !== null) assert.equal(got, expected, `u64 in case \`${label}\``);
        }
        break;
      }
      case 'timestamp': {
        const got = reader.timestamp();
        if (has('value')) {
          assert.equal(toWire(got), Number(big(op, 'value')), `timestamp in case \`${label}\``);
        }
        break;
      }
      case 'id': {
        const got = reader.id();
        if (has('value')) assert.equal(got, idOf(text(op, 'value')), `id in case \`${label}\``);
        break;
      }
      case 'string': {
        const got = reader.str();
        if (has('value')) assert.equal(got, text(op, 'value'), `string in case \`${label}\``);
        break;
      }
      case 'bytes': {
        const got = reader.bytes();
        if (has('value')) assert.equal(hex(got), text(op, 'value'), `bytes in case \`${label}\``);
        break;
      }
      case 'list_len': {
        const got = reader.listLen();
        if (has('value')) {
          assert.equal(got, Number(big(op, 'value')), `list_len in case \`${label}\``);
        }
        break;
      }
      case 'optional': {
        const [fieldId, inner] = reader.optional();
        if (has('id')) assert.equal(fieldId, small(op, 'id'), `field id in case \`${label}\``);
        const nested = op.ops as Case[] | undefined;
        // An unknown field is dropped along with its sub-reader. That the outer position is
        // already past it is the property under test, and the outer `finish` checks it.
        if (nested !== undefined && op.unknown !== true) {
          readOps(inner, nested, label);
          inner.finish();
        }
        break;
      }
      default:
        throw new Error(`unknown read op \`${text(op, 'op')}\``);
    }
  }
}

test('MSE programs encode and decode as the vectors say', () => {
  const file = load('mse.json');
  for (const item of section(file, 'cases', 'mse.json')) {
    const ops = item.ops as Case[];
    assert.ok(Array.isArray(ops), `case \`${caseName(item)}\` has ops`);
    const expected = bytesOf(item, 'hex');
    const label = caseName(item);

    const writer = new Writer();
    writeOps(writer, ops);
    assert.equal(hex(writer.finish()), hex(expected), `encoding case \`${label}\``);

    const reader = new Reader(expected);
    readOps(reader, ops, label);
    reader.finish();
  }
});

test('malformed MSE is rejected', () => {
  const file = load('mse.json');
  for (const item of section(file, 'invalid', 'mse.json')) {
    const input = bytesOf(item, 'hex');
    const ops = item.read_ops as Case[];
    assert.ok(Array.isArray(ops), `case \`${caseName(item)}\` has read_ops`);
    expectError(
      item,
      () => {
        const reader = new Reader(input);
        readOps(reader, ops, caseName(item));
        reader.finish();
      },
      'mse',
    );
  }
});

// --- the suite is present at all --------------------------------------------

test('every vector file is present and populated', () => {
  // Guards against the failure this whole directory exists to prevent being itself
  // defeated by a missing file: without this, deleting `mse.json` turns three tests into
  // three errors that a long log could bury, and renaming a section turns them into silent
  // no-ops.
  const expected: ReadonlyArray<readonly [string, readonly string[]]> = [
    ['varint.json', ['cases', 'zigzag', 'invalid']],
    ['frames.json', ['cases', 'length_prefixed', 'invalid']],
    ['mse.json', ['cases', 'invalid']],
  ];
  let total = 0;
  for (const [file, sections] of expected) {
    const loaded = load(file);
    assert.equal(
      typeof loaded.provenance,
      'string',
      `${file} must record where its expected bytes came from`,
    );
    for (const name of sections) {
      total += section(loaded, name, file).length;
    }
  }
  assert.ok(total >= 60, `only ${total} wire vector cases, expected at least 60`);
});

test('the depth limit both sides enforce is the same number', () => {
  // A limit that drifted between the two implementations would not fail any vector above:
  // the deep-nesting case stops at 16 on both sides today. Pinning the constant is what
  // makes a future edit to one side visible.
  assert.equal(MAX_NESTING_DEPTH, 16);
});
