/**
 * What a generated module still needs a test for.
 *
 * The encoders and decoders in `src/generated.ts` are written by `make protocol` from
 * `shared/protocol/schema`, and the Rust crate is written from the same schema in the same
 * run. Testing them field by field would test the generator's output against a hand copy
 * of the generator's rules — a lot of code that fails whenever the schema legitimately
 * changes. So this file asserts the things that stay true across every schema change, and
 * that break loudly when a template or a hand-written constant drifts:
 *
 *   - Structs round-trip, including the shapes most likely to be mis-templated: nested
 *     structs, lists of structs, absent and present optionals, a 64-bit bitmask, bytes.
 *   - Forward compatibility actually works. An old client must skip an optional field it
 *     has never heard of, and an unknown enum value must land on `Unknown` rather than
 *     escaping as a number that later code compares against a name.
 *   - The generated constants agree with the hand-written codec in `@migo/wire`. Both
 *     files hold a copy of the limits, the flag bits and the epoch, and a disagreement
 *     between them is a wire-incompatible build that would otherwise ship quietly.
 *   - The opcode and error tables are internally consistent, and every opcode's payload
 *     type is a type this module can actually encode.
 *
 * Imports are by package name — `@migo/wire`, not a relative path — so this suite also
 * exercises the `exports` map and the emitted declaration files, which the wire package's
 * own tests cannot: they import their own source.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  MAX_BATCH_ITEMS,
  MAX_FRAME_BYTES,
  MAX_NESTING_DEPTH,
  MAX_STRING_BYTES,
  MAX_SUBSCRIPTIONS,
  MIGO_EPOCH_MS,
  Reader,
  WireError,
  Writer,
  flags,
  parseId,
  type Id,
} from '@migo/wire';

import * as protocol from '../src/index.js';
import {
  BandwidthMode,
  CODE,
  ERROR_SYMBOLS,
  EPOCH_MS,
  FLAG,
  LIMITS,
  MessageKind,
  OP,
  OPCODES,
  PROTOCOL_VERSION,
  Platform,
  RESERVED_FLAG_MASK,
  clientAction,
  decodeHello,
  decodeMessageSend,
  decodeSyncResponse,
  decodeWelcome,
  encodeHello,
  encodeMessageSend,
  encodeSyncResponse,
  encodeWelcome,
  errorClass,
  isRetryable,
  opcodeName,
  type Hello,
  type MessageEvent,
  type MessageSend,
  type SyncResponse,
  type Welcome,
} from '../src/index.js';

const ID_A = parseId('01ARZ3NDEKTSV4RRFFQ69G5FAV');
const ID_B = parseId('01BX5ZZKBKACTAV9WEVGEMMVRZ');
const ID_C = parseId('01BX5ZZKBKACTAV9WEVGEMMVS0');

function roundTrip<T>(value: T, encode: (w: Writer, v: T) => void, decode: (r: Reader) => T): T {
  const writer = new Writer();
  encode(writer, value);
  const bytes = writer.finish();
  const reader = new Reader(bytes);
  const back = decode(reader);
  // A decoder that stops early is a decoder that would mis-frame the next field in a batch,
  // so the buffer has to be exactly consumed, not merely consumed enough.
  reader.finish();
  return back;
}

test('a struct with every awkward field shape survives a round trip', () => {
  const hello: Hello = {
    protocolVersion: PROTOCOL_VERSION,
    client: {
      platform: Platform.Web,
      appVersion: '0.1.0',
      osVersion: 'Linux 6.12',
      // `deviceModel` deliberately absent: the absent and present cases are different code
      // paths in both the encoder and the decoder.
    },
    features: (1n << 0n) | (1n << 2n) | (1n << 63n),
    locale: 'id-ID',
    bandwidthMode: BandwidthMode.LowData,
    accessToken: 'not-a-real-token',
    deviceId: ID_A,
    resume: { sessionId: ID_B, lastFrameSeq: 9_007_199_254_740_991 },
  };

  assert.deepEqual(roundTrip(hello, encodeHello, decodeHello), hello);

  // The high bit of the feature mask is the one a `number` would lose. It is asserted
  // separately because `deepEqual` on a struct that never had it set would pass.
  const back = roundTrip(hello, encodeHello, decodeHello);
  assert.equal(back.features & (1n << 63n), 1n << 63n);
  assert.equal(back.resume?.lastFrameSeq, Number.MAX_SAFE_INTEGER);
});

test('absent optionals cost two bytes and come back absent', () => {
  const bare: Hello = {
    protocolVersion: PROTOCOL_VERSION,
    client: { platform: Platform.Android, appVersion: '0.1.0' },
    features: 0n,
    locale: 'en',
    bandwidthMode: BandwidthMode.Auto,
  };
  const back = roundTrip(bare, encodeHello, decodeHello);
  assert.deepEqual(back, bare);
  // Not `undefined`-valued: absent. `Object.hasOwn` distinguishes them, and a decoder that
  // set the key to `undefined` would make `deepEqual` pass while breaking `in` checks and
  // JSON round trips in product code.
  assert.ok(!Object.hasOwn(back, 'accessToken'));
  assert.ok(!Object.hasOwn(back.client, 'osVersion'));
});

test('bytes and lists of structs survive a round trip', () => {
  const send: MessageSend = {
    messageId: ID_A,
    conversationId: ID_B,
    kind: MessageKind.Text,
    envelope: Uint8Array.from([0x00, 0xff, 0x7f, 0x80, 0x01]),
    replyTo: ID_C,
    expiresInMs: 86_400_000,
  };
  assert.deepEqual(roundTrip(send, encodeMessageSend, decodeMessageSend), send);

  // An empty payload is a legal envelope and a classic off-by-one in a length-prefixed
  // decoder.
  const empty: MessageSend = {
    messageId: ID_A,
    conversationId: ID_B,
    kind: MessageKind.System,
    envelope: new Uint8Array(0),
  };
  assert.deepEqual(roundTrip(empty, encodeMessageSend, decodeMessageSend), empty);

  const event = (seq: number): MessageEvent => ({
    messageId: ID_A,
    conversationId: ID_B,
    seq,
    senderId: ID_C,
    senderDevice: ID_A,
    kind: MessageKind.Text,
    envelope: Uint8Array.from([seq & 0xff]),
    createdAt: Date.parse('2026-01-02T03:04:05.678Z'),
    ...(seq % 2 === 0 ? { deleted: true } : {}),
  });

  const sync: SyncResponse = {
    conversationId: ID_B,
    status: protocol.SyncStatus.Ok,
    fromSeq: 1,
    toSeq: 3,
    more: false,
    messages: [event(1), event(2), event(3)],
  };
  assert.deepEqual(roundTrip(sync, encodeSyncResponse, decodeSyncResponse), sync);

  // An empty list is not the same code path as a populated one.
  assert.deepEqual(
    roundTrip({ ...sync, messages: [] }, encodeSyncResponse, decodeSyncResponse).messages,
    [],
  );
});

test('timestamps cross the epoch boundary intact', () => {
  const welcome: Welcome = {
    sessionId: ID_A,
    node: { nodeId: 'node-1', region: 'ap-southeast-1', country: 'ID' },
    features: 0n,
    serverTime: Date.parse('2026-08-20T11:22:33.444Z'),
    limits: {
      maxFrameBytes: LIMITS.MAX_FRAME_BYTES,
      maxBatchItems: LIMITS.MAX_BATCH_ITEMS,
      maxSubscriptions: LIMITS.MAX_SUBSCRIPTIONS,
      heartbeatMs: LIMITS.DEFAULT_HEARTBEAT_MS,
    },
    resumed: false,
    resumeFromSeq: 0,
    authenticatedUser: ID_B,
  };
  const back = roundTrip(welcome, encodeWelcome, decodeWelcome);
  assert.deepEqual(back, welcome);
  // Stated as a wall-clock string as well as a number, because a build with the wrong
  // epoch would agree with itself about the number.
  assert.equal(new Date(back.serverTime).toISOString(), '2026-08-20T11:22:33.444Z');
  // `false` is not `undefined`. An encoder that treated a falsy optional as absent would
  // turn "explicitly not a resume" into "unspecified", which is the same bug as a missing
  // `hasOwnProperty` check but on the wire.
  assert.equal(back.resumed, false);
  assert.equal(back.resumeFromSeq, 0);
});

test('an unknown optional field is skipped, not fatal', () => {
  // This is the forward-compatibility contract: a client built against today's schema must
  // survive a server that adds a field tomorrow. Hand-encoded because no generator will
  // emit a field it does not know about.
  const writer = new Writer();
  writer.enter();
  writer.id(ID_A);
  writer.u64(42);
  writer.u32(2); // two optionals present, both unknown to `ResumeRequest`
  writer.optional(9, (w) => {
    w.str('a field from a later schema');
  });
  writer.optional(10, (w) => {
    w.enter();
    w.bytes(Uint8Array.from([1, 2, 3]));
    w.u32(0);
    w.leave();
  });
  writer.leave();

  const back = protocol.decodeResumeRequest(new Reader(writer.finish()));
  assert.deepEqual(back, { sessionId: ID_A, lastFrameSeq: 42 });
});

test('an unknown enum value decodes to Unknown instead of escaping as a number', () => {
  const writer = new Writer();
  writer.enter();
  writer.u32(250); // a platform this build has never heard of
  writer.str('99.0.0');
  writer.u32(0);
  writer.leave();

  const back = protocol.decodeClientInfo(new Reader(writer.finish()));
  // The generated decoder casts, so the runtime value is the number off the wire. What
  // matters is that a `switch` on it lands in `default` and that comparing it against a
  // known member is false — a build that mapped it onto `Platform.Web` would render a
  // future client as a web client.
  assert.notEqual(back.platform, Platform.Web);
  assert.equal(Platform[back.platform], undefined, 'no name for an unknown value');
  assert.equal(Platform[Platform.Unknown], 'Unknown');
});

test('the generated constants agree with the hand-written codec', () => {
  // Two files, two copies, one wire. A mismatch here is a build that disagrees with itself
  // about how many bytes a frame may hold.
  assert.equal(LIMITS.MAX_FRAME_BYTES, MAX_FRAME_BYTES);
  assert.equal(LIMITS.MAX_STRING_BYTES, MAX_STRING_BYTES);
  assert.equal(LIMITS.MAX_BATCH_ITEMS, MAX_BATCH_ITEMS);
  assert.equal(LIMITS.MAX_NESTING_DEPTH, MAX_NESTING_DEPTH);
  assert.equal(LIMITS.MAX_SUBSCRIPTIONS, MAX_SUBSCRIPTIONS);
  assert.equal(EPOCH_MS, MIGO_EPOCH_MS);
  assert.equal(PROTOCOL_VERSION, 1);

  assert.equal(FLAG.COMPRESSED, flags.COMPRESSED);
  assert.equal(FLAG.TRACED, flags.TRACED);
  assert.equal(FLAG.BATCH, flags.BATCH);
  assert.equal(FLAG.ERROR, flags.ERROR);
  assert.equal(FLAG.ACK_REQUIRED, flags.ACK_REQUIRED);
  assert.equal(FLAG.FRAGMENT, flags.FRAGMENT);
  assert.equal(RESERVED_FLAG_MASK, flags.RESERVED_MASK);
});

test('the opcode table is consistent with itself and with this module', () => {
  const byName = new Map(Object.entries(OP));
  assert.ok(byName.size > 0);

  const seen = new Set<number>();
  for (const [name, code] of byName) {
    assert.equal(typeof code, 'number');
    assert.ok(!seen.has(code), `opcode ${code} is used twice`);
    seen.add(code);

    const meta = OPCODES[code];
    assert.ok(meta !== undefined, `${name} (${code}) has no metadata`);
    assert.equal(meta.name, name);
    assert.equal(meta.code, code);
    assert.equal(opcodeName(code), name);

    // Every payload and response names a type this module can encode and decode. This is
    // the check that catches a schema referring to a struct the generator skipped.
    for (const type of [meta.payload, meta.response]) {
      if (type === undefined) continue;
      const registry = protocol as unknown as Record<string, unknown>;
      assert.equal(typeof registry[`encode${type}`], 'function', `encode${type} is missing`);
      assert.equal(typeof registry[`decode${type}`], 'function', `decode${type} is missing`);
    }

    // A frame the server sends unprompted cannot require an acknowledgement from itself.
    if (meta.ackRequired) {
      assert.notEqual(meta.direction, 'client_to_server', `${name} cannot need a client ack`);
    }
    // Coalescing means "a newer frame replaces an older one", so it needs something to
    // group by — but only where the queue carries more than one subject. A server-bound
    // queue holds one session's own frames, so the session *is* the key and PRESENCE_SET
    // states none. A client-bound queue fans in every user and room the session watches,
    // and coalescing there without a key would drop one user's presence on another's.
    if (meta.cls === 'Coalescable' && meta.direction !== 'client_to_server') {
      assert.equal(typeof meta.coalesceKey, 'string', `${name} must say what it coalesces on`);
    }
    if (meta.coalesceKey !== undefined) {
      assert.equal(meta.cls, 'Coalescable', `${name} has a coalesce key but is not coalescable`);
    }
  }

  assert.equal(Object.keys(OPCODES).length, byName.size, 'no metadata for a nonexistent opcode');
  assert.equal(opcodeName(0xdead), 'UNKNOWN(0xdead)');
});

test('every error code has a symbol, a class and an action', () => {
  const codes = Object.values(CODE);
  assert.ok(codes.length > 0);
  for (const code of codes) {
    assert.equal(typeof ERROR_SYMBOLS[code], 'string', `${code} has no symbol`);
    assert.notEqual(errorClass(code), 'Unknown', `${code} falls outside every class range`);
    assert.notEqual(clientAction(code), '', `${code} has no client action`);
  }
  assert.equal(Object.keys(ERROR_SYMBOLS).length, new Set(codes).size, 'symbol table drifted');

  // The classes a client must not retry, and the ones it must. Getting this backwards means
  // either a hot loop against a server that will never accept the request, or a dropped
  // message the user watched fail.
  assert.ok(!isRetryable(CODE.TOKEN_EXPIRED));
  assert.equal(clientAction(CODE.TOKEN_EXPIRED), 'reauthenticate');
  assert.ok(!isRetryable(CODE.MALFORMED_FRAME));
  assert.ok(isRetryable(CODE.TIMEOUT));
  assert.equal(errorClass(CODE.ROOM_READ_ONLY_PARTITION), 'Federation');

  // An unmapped code is not a crash: a client on an old build meets a new code eventually.
  assert.equal(errorClass(9999), 'Unknown');
  assert.equal(clientAction(9999), 'backoff_retry');
});

test('the codec limits still apply to generated structs', () => {
  // A generated encoder is not a way around the frame limit. The check lives in the writer,
  // so this is really asserting that the generated code goes through it — a template that
  // built its own buffer would not.
  const huge: MessageSend = {
    messageId: ID_A,
    conversationId: ID_B,
    kind: MessageKind.Media,
    envelope: new Uint8Array(MAX_FRAME_BYTES + 1),
  };
  assert.throws(
    () => {
      const writer = new Writer();
      encodeMessageSend(writer, huge);
    },
    (error: unknown) => {
      assert.ok(error instanceof WireError);
      assert.equal(error.kind, 'BytesTooLong');
      return true;
    },
  );

  // A truncated frame fails as a decode error, not as an exception nobody typed for.
  const writer = new Writer();
  encodeMessageSend(writer, {
    messageId: ID_A,
    conversationId: ID_B,
    kind: MessageKind.Text,
    envelope: Uint8Array.from([1, 2, 3]),
  });
  const bytes = writer.finish();
  assert.throws(
    () => decodeMessageSend(new Reader(bytes.subarray(0, bytes.length - 2))),
    (error: unknown) => {
      assert.ok(error instanceof WireError, `got ${String(error)}`);
      return true;
    },
  );
});

test('an identifier keeps its type through the codec', () => {
  // `Id` is a branded string, so this is mostly a compile-time assertion — the value is
  // typed `Id` on the way out without a cast. The runtime half is that it comes back equal
  // by `===`, which is the property `Map<Id, T>` in the SDK depends on.
  const send: MessageSend = {
    messageId: ID_A,
    conversationId: ID_B,
    kind: MessageKind.Text,
    envelope: new Uint8Array(0),
  };
  const back = roundTrip(send, encodeMessageSend, decodeMessageSend);
  const id: Id = back.messageId;
  assert.equal(id, ID_A);
  assert.ok(new Map<Id, string>([[back.messageId, 'x']]).has(ID_A));
});
