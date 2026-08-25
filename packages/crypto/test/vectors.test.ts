/**
 * The TypeScript half of the cryptographic conformance suite.
 *
 * This file and `server/crates/migo-crypto/tests/vectors.rs` read the *same* JSON files in
 * `shared/protocol/vectors/crypto` and make the same assertions. Cryptographic code has a
 * failure mode ordinary code does not: it can be wrong and still work. A KDF called with the
 * salt and the secret transposed produces perfectly good-looking random bytes, round-trips
 * through its own `open`, and passes every property test — and then the server derives a
 * different key and nobody can read anything. Worse, the mistake is unfixable once shipped,
 * because the wrong bytes are now the format that a million stored messages were sealed under.
 *
 * So the expected values come from outside both implementations. The HKDF, HMAC and HChaCha20 in
 * `tools/vectors/generate_crypto_vectors.py` were written from RFC 5869, RFC 2104 and
 * draft-irtf-cfrg-xchacha, and that generator refuses to emit anything until it reproduces those
 * documents' own published vectors. Cases carried through from the RFCs are labelled with a
 * `provenance` of `rfc-5869` or `rfc-4231-construction`; a failure there means the composition
 * is wrong in a way that no amount of internal agreement between Rust and TypeScript would have
 * revealed.
 *
 * Where this runner asserts *more* than the Rust one, it is for hazards that only exist on this
 * side. Rust zeroizes a key on `Drop` and refuses to `Display` one because the type system says
 * so; here a key is an object that something will eventually pass to `JSON.stringify`, and a
 * `verify` that returned a boolean would be one forgotten `if` away from accepting every
 * forgery. Those are checked at the bottom of this file.
 */

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { inspect } from 'node:util';

import { CryptoError, MacKey, SymmetricKey, aead, kdf, mac } from '../src/index.js';

// --- plumbing ---------------------------------------------------------------

const VECTORS_DIR = resolve(import.meta.dirname, '../../../../shared/protocol/vectors/crypto');

interface Case {
  readonly name?: string;
  readonly [key: string]: unknown;
}

function load(file: string): Record<string, unknown> {
  const path = join(VECTORS_DIR, file);
  if (!existsSync(path)) {
    throw new Error(
      `${path} is missing; run python3 tools/vectors/generate_crypto_vectors.py from the repo root`,
    );
  }
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
}

/**
 * Pulls one section out of a vector file, refusing an empty one.
 *
 * Empty is a failure, not a no-op: a crypto suite that runs zero cases reports the strongest
 * guarantee in the codebase and checks nothing.
 */
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

function count(item: Case, key: string): number {
  const value = item[key];
  assert.ok(Number.isInteger(value), `case \`${caseName(item)}\` needs an integer \`${key}\``);
  return value as number;
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

/**
 * An absent `salt` field, or an explicit `null`, means RFC 5869's absent salt.
 *
 * Kept distinct from a zero-length salt on purpose. The two produce the same PRK — HMAC pads a
 * zero-length key and an absent one to the same block — and `kdf.json` carries both so that a
 * re-implementation which conflates them can be shown to be accidentally right rather than
 * assumed to be.
 */
function saltOf(item: Case): Uint8Array | null {
  const value = item.salt;
  if (value === undefined || value === null) {
    return null;
  }
  assert.equal(typeof value, 'string', `case \`${caseName(item)}\` salt must be hex or null`);
  return unhex(value as string);
}

/**
 * The failure name a vector file uses.
 *
 * These strings are the Rust enum's variant names. Comparing to `error.kind` rather than to a
 * message is what lets both languages read one file: a message can be reworded, a variant name
 * is part of the protocol's vocabulary.
 */
function kindOf(error: unknown): string {
  assert.ok(error instanceof CryptoError, `expected a CryptoError, got ${String(error)}`);
  return error.kind;
}

/** Runs `body`, asserting it throws a {@link CryptoError} of exactly `expected` kind. */
function throwsKind(expected: string, body: () => unknown, context: string): void {
  let outcome: unknown;
  try {
    outcome = body();
  } catch (error) {
    const actual = kindOf(error);
    assert.equal(actual, expected, `${context} failed with ${actual}, expected ${expected}`);
    return;
  }
  assert.fail(`${context} was accepted (${String(outcome)}), expected ${expected}`);
}

// --- label tables -----------------------------------------------------------
//
// A vector file names a label as text, but the runner resolves it through this package's own
// constants and fails on an unknown name. Passing the file's string straight to the KDF would
// make the test pass for a file that says `migo-x3dh-v2`, which is precisely the change that
// must not go unnoticed: renaming a label silently re-keys every session in the deployment.
//
// The tables are also the TypeScript↔Rust bridge for the label *set*. If the Rust crate gains a
// ninth KDF label and this package does not, the vectors regenerate with a case this runner
// cannot resolve, and it says so by name.

const KDF_LABELS: ReadonlyMap<string, string> = new Map([
  [kdf.LABEL_X3DH, kdf.LABEL_X3DH],
  [kdf.LABEL_RATCHET_ROOT, kdf.LABEL_RATCHET_ROOT],
  [kdf.LABEL_RATCHET_CHAIN, kdf.LABEL_RATCHET_CHAIN],
  [kdf.LABEL_MESSAGE_KEY, kdf.LABEL_MESSAGE_KEY],
  [kdf.LABEL_SENDER_CHAIN, kdf.LABEL_SENDER_CHAIN],
  [kdf.LABEL_SENDER_MESSAGE, kdf.LABEL_SENDER_MESSAGE],
  [kdf.LABEL_BACKUP, kdf.LABEL_BACKUP],
  [kdf.LABEL_RECOVERY, kdf.LABEL_RECOVERY],
]);

const MAC_LABELS: ReadonlyMap<string, string> = new Map([
  [mac.LABEL_SESSION_TOKEN, mac.LABEL_SESSION_TOKEN],
  [mac.LABEL_REFRESH_TOKEN, mac.LABEL_REFRESH_TOKEN],
  [mac.LABEL_RESUME_CURSOR, mac.LABEL_RESUME_CURSOR],
  [mac.LABEL_MEDIA_URL, mac.LABEL_MEDIA_URL],
  [mac.LABEL_PAGINATION, mac.LABEL_PAGINATION],
  [mac.LABEL_VERIFICATION, mac.LABEL_VERIFICATION],
  [mac.LABEL_WEBHOOK, mac.LABEL_WEBHOOK],
]);

function kdfLabel(item: Case): string {
  const named = text(item, 'label');
  const resolved = KDF_LABELS.get(named);
  assert.ok(resolved !== undefined, `\`${named}\` is not a KDF label this build defines`);
  return resolved;
}

function macLabel(item: Case): string {
  const named = text(item, 'label');
  const resolved = MAC_LABELS.get(named);
  assert.ok(resolved !== undefined, `\`${named}\` is not a MAC label this build defines`);
  return resolved;
}

// --- kdf --------------------------------------------------------------------

test('HKDF derivations match the vectors', () => {
  const file = load('kdf.json');
  for (const item of section(file, 'cases', 'kdf.json')) {
    const okm = kdf.derive(
      bytesOf(item, 'secret'),
      saltOf(item),
      kdfLabel(item),
      count(item, 'length'),
    );
    assert.equal(hex(okm), text(item, 'okm'), `derivation for case \`${caseName(item)}\``);
    assert.equal(okm.length, count(item, 'length'), `length for case \`${caseName(item)}\``);
  }
});

test('the RFC 5869 vectors pass through this KDF', () => {
  // The specification's own numbers. They are the difference between "this package agrees with
  // itself" and "this package implements HKDF-SHA256". The `info` is raw bytes here, not a Migo
  // label, which is the reason `derive` accepts both.
  const file = load('kdf.json');
  for (const item of section(file, 'rfc', 'kdf.json')) {
    const okm = kdf.derive(
      bytesOf(item, 'secret'),
      saltOf(item),
      bytesOf(item, 'label_hex'),
      count(item, 'length'),
    );
    assert.equal(hex(okm), text(item, 'okm'), `RFC 5869 case \`${caseName(item)}\``);
  }
});

test('an absent salt and an empty salt agree', () => {
  // Not a coincidence to be relied on blindly, but a documented consequence of RFC 5869 plus
  // HMAC's key padding. It is asserted because the two spellings appear at different call sites,
  // and a future refactor that "fixes" one of them into the other must not be able to change any
  // derived key.
  const file = load('kdf.json');
  const cases = section(file, 'cases', 'kdf.json');
  const find = (wanted: string): Case => {
    const found = cases.find((item) => caseName(item) === wanted);
    assert.ok(found !== undefined, `kdf.json must carry the \`${wanted}\` case`);
    return found;
  };
  const absent = find('ratchet_root_with_absent_salt');
  const empty = find('ratchet_root_with_empty_salt');
  assert.equal(saltOf(absent), null, 'the absent-salt case must have no salt');
  assert.equal(text(empty, 'salt'), '', 'the empty-salt case must be empty');
  assert.equal(
    text(absent, 'okm'),
    text(empty, 'okm'),
    'an absent salt is HashLen zero bytes, which HMAC pads identically to a zero-length key',
  );
  // And this implementation must actually produce that, rather than the file merely claiming it.
  const secret = bytesOf(absent, 'secret');
  const label = kdfLabel(absent);
  assert.equal(
    hex(kdf.derive(secret, null, label, count(absent, 'length'))),
    hex(kdf.derive(secret, new Uint8Array(0), label, count(empty, 'length'))),
  );
});

test('paired derivations split one expansion', () => {
  const file = load('kdf.json');
  for (const item of section(file, 'pairs', 'kdf.json')) {
    const secret = bytesOf(item, 'secret');
    const salt = saltOf(item);
    const label = kdfLabel(item);
    const firstLength = count(item, 'first_length');
    const secondLength = count(item, 'second_length');

    const { first, second } = kdf.derivePair(secret, salt, label, firstLength, secondLength);
    assert.equal(hex(first), text(item, 'first'), `first half of pair \`${caseName(item)}\``);
    assert.equal(hex(second), text(item, 'second'), `second half of pair \`${caseName(item)}\``);

    // The pair is one expansion of A+B bytes split at A, not two expansions. Asserting it here
    // means a "simplification" into two `derive` calls cannot pass, because two calls would
    // produce two copies of the same prefix instead of a contiguous stream.
    const combined = kdf.derive(secret, salt, label, firstLength + secondLength);
    assert.equal(
      hex(combined),
      `${text(item, 'first')}${text(item, 'second')}`,
      `pair \`${caseName(item)}\` must be one expansion split in two`,
    );
    assert.notEqual(hex(first), hex(second), 'a root key equal to its chain key is not a ratchet');
  }
});

test('every KDF label is distinct', () => {
  // Two derivations sharing a label is the one mistake in this module that produces no symptom:
  // both keys work, and they are the same key. The `Map` above cannot hold a duplicate, so the
  // count is the assertion.
  const labels = [
    kdf.LABEL_X3DH,
    kdf.LABEL_RATCHET_ROOT,
    kdf.LABEL_RATCHET_CHAIN,
    kdf.LABEL_MESSAGE_KEY,
    kdf.LABEL_SENDER_CHAIN,
    kdf.LABEL_SENDER_MESSAGE,
    kdf.LABEL_BACKUP,
    kdf.LABEL_RECOVERY,
  ];
  assert.equal(new Set(labels).size, labels.length, 'two derivations share a label');
  assert.equal(KDF_LABELS.size, labels.length, 'the label table is missing a label');
  for (const label of labels) {
    assert.match(label, /^migo-[a-z0-9-]+-v\d+$/, `\`${label}\` does not follow the label form`);
  }
});

// --- aead -------------------------------------------------------------------

test('sealed envelopes match the vectors', () => {
  const file = load('aead.json');
  for (const item of section(file, 'cases', 'aead.json')) {
    const key = SymmetricKey.fromBytes(bytesOf(item, 'key'));
    const nonce = bytesOf(item, 'nonce');
    const associatedData = bytesOf(item, 'aad');
    const plaintext = bytesOf(item, 'plaintext');

    const sealed = aead.sealWithNonce(key, nonce, associatedData, plaintext);
    assert.equal(hex(sealed), text(item, 'sealed'), `sealing case \`${caseName(item)}\``);
    assert.equal(
      sealed.length,
      aead.NONCE_LEN + plaintext.length + aead.TAG_LEN,
      `case \`${caseName(item)}\` layout is nonce || ciphertext || tag`,
    );

    const opened = aead.open(key, associatedData, sealed);
    assert.equal(hex(opened), hex(plaintext), `opening case \`${caseName(item)}\``);
  }
});

test('tampered envelopes are refused', () => {
  // Not decoration. `open` returning a plaintext for a message whose tag was flipped by one bit
  // is not a bug in a corner case; it is the absence of authentication, which is the only
  // property the AEAD was chosen for.
  const file = load('aead.json');
  for (const item of section(file, 'invalid', 'aead.json')) {
    const key = SymmetricKey.fromBytes(bytesOf(item, 'key'));
    const associatedData = bytesOf(item, 'aad');
    const sealed = bytesOf(item, 'sealed');
    const why = typeof item.why === 'string' ? item.why : '';
    throwsKind(
      text(item, 'error'),
      () => aead.open(key, associatedData, sealed),
      `case \`${caseName(item)}\` (${why})`,
    );
  }
});

test('a sealed envelope survives a round trip with a random nonce', () => {
  // The vectors can only exercise `sealWithNonce`, because a random nonce has no expected bytes.
  // This covers the function application code actually calls, and the property that matters:
  // two seals of one plaintext must differ, or the nonce is not being generated.
  const key = SymmetricKey.generate();
  const associatedData = new TextEncoder().encode('conversation:42');
  const plaintext = new TextEncoder().encode('halo dunia');

  const first = aead.seal(key, associatedData, plaintext);
  const second = aead.seal(key, associatedData, plaintext);
  assert.notEqual(hex(first), hex(second), 'two seals share a nonce: the RNG is not being used');
  assert.equal(hex(aead.open(key, associatedData, first)), hex(plaintext));
  assert.equal(hex(aead.open(key, associatedData, second)), hex(plaintext));

  // And the associated data is authenticated, not merely accepted.
  throwsKind(
    'DecryptionFailed',
    () => aead.open(key, new TextEncoder().encode('conversation:43'), first),
    'an envelope opened under the wrong associated data',
  );
});

test('keys and nonces of the wrong width are refused before use', () => {
  // Rust gets this from `[u8; 32]`. Here it is a runtime check, so it is worth a test: a 31-byte
  // key silently zero-padded to 32 would produce an entirely valid-looking envelope that no
  // other implementation could open.
  throwsKind('BadLength', () => SymmetricKey.fromBytes(new Uint8Array(31)), 'a 31-byte key');
  throwsKind('BadLength', () => SymmetricKey.fromBytes(new Uint8Array(33)), 'a 33-byte key');
  const key = SymmetricKey.fromBytes(new Uint8Array(aead.KEY_LEN));
  throwsKind(
    'BadLength',
    () => aead.sealWithNonce(key, new Uint8Array(12), new Uint8Array(0), new Uint8Array(0)),
    'an AES-GCM-sized nonce',
  );
});

// --- mac --------------------------------------------------------------------

test('token MACs match the vectors', () => {
  const file = load('mac.json');
  for (const item of section(file, 'cases', 'mac.json')) {
    const root = bytesOf(item, 'root');
    const label = macLabel(item);
    const message = bytesOf(item, 'message');

    // The subkey is checked separately from the tag so a failure names the half that broke
    // rather than leaving both suspect.
    assert.equal(
      hex(kdf.derive(root, null, label, 32)),
      text(item, 'key'),
      `subkey for case \`${caseName(item)}\``,
    );

    const key = MacKey.derive(root, label);
    const tag = key.tag(message);
    assert.equal(hex(tag), text(item, 'tag'), `tag for case \`${caseName(item)}\``);
    key.verify(message, tag);

    // A tag that verifies over the wrong message is not a MAC.
    const other = new Uint8Array(message.length + 1);
    other.set(message, 0);
    throwsKind(
      'BadSignature',
      () => key.verify(other, tag),
      `case \`${caseName(item)}\` over a different message`,
    );
  }
});

test('multi-part MACs are length-prefixed', () => {
  const file = load('mac.json');
  for (const item of section(file, 'parts', 'mac.json')) {
    const root = bytesOf(item, 'root');
    const label = macLabel(item);
    const raw = item.parts;
    assert.ok(Array.isArray(raw), `case \`${caseName(item)}\` has no parts`);
    const parts = raw.map((part) => {
      assert.equal(typeof part, 'string', 'a part is hex');
      return unhex(part as string);
    });

    assert.equal(
      hex(kdf.derive(root, null, label, 32)),
      text(item, 'key'),
      `subkey for case \`${caseName(item)}\``,
    );
    const key = MacKey.derive(root, label);
    const tag = key.tagParts(parts);
    assert.equal(hex(tag), text(item, 'tag'), `multi-part tag for case \`${caseName(item)}\``);
    key.verifyParts(parts, tag);
  }
});

test('a different split of the same bytes gets a different tag', () => {
  // The canonical HMAC footgun: without length prefixes, a token for user `1` device `23` is also
  // a valid token for user `12` device `3`. The named pairs in the file are exactly the collisions
  // a naive concatenation would produce.
  const file = load('mac.json');
  const parts = section(file, 'parts', 'mac.json');
  const tagNamed = (wanted: string): string => {
    const found = parts.find((item) => caseName(item) === wanted);
    assert.ok(found !== undefined, `mac.json \`parts\` must carry \`${wanted}\``);
    return text(found, 'tag');
  };
  for (const pair of section(file, 'distinct_pairs', 'mac.json')) {
    const left = text(pair, 'left');
    const right = text(pair, 'right');
    const why = typeof pair.why === 'string' ? pair.why : '';
    assert.notEqual(tagNamed(left), tagNamed(right), `\`${left}\` and \`${right}\`: ${why}`);
  }

  // The file's tags are this package's tags — the test above proved that — so the inequality
  // above is a statement about this implementation too. Stated directly as well, because reading
  // it only out of the file would leave a reader wondering.
  const key = MacKey.derive(new Uint8Array(32).fill(0xa1), mac.LABEL_PAGINATION);
  const abc = new TextEncoder().encode('abc');
  assert.notEqual(
    hex(key.tagParts([abc.subarray(0, 2), abc.subarray(2)])),
    hex(key.tagParts([abc.subarray(0, 1), abc.subarray(1)])),
  );
  // And the multi-part tag is not the single-part tag of the concatenation either, which is what
  // a caller would get if the length prefixes were dropped.
  assert.notEqual(hex(key.tagParts([abc])), hex(key.tag(abc)));
});

test('the RFC 4231 vectors pass through this HMAC', () => {
  const file = load('mac.json');
  for (const item of section(file, 'rfc', 'mac.json')) {
    const key = MacKey.fromBytes(bytesOf(item, 'key'));
    assert.equal(
      hex(key.tag(bytesOf(item, 'message'))),
      text(item, 'tag'),
      `RFC 4231 case \`${caseName(item)}\``,
    );
  }
});

test('tag truncation follows the documented floor', () => {
  const file = load('mac.json');
  for (const item of section(file, 'truncation', 'mac.json')) {
    const key = MacKey.derive(bytesOf(item, 'root'), macLabel(item));
    const message = bytesOf(item, 'message');
    const full = key.tag(message);
    const take = count(item, 'tag_len');
    const accepted = item.accepted;
    assert.equal(typeof accepted, 'boolean', `case \`${caseName(item)}\` needs \`accepted\``);
    const why = typeof item.why === 'string' ? item.why : '';
    const truncated = full.subarray(0, Math.min(take, full.length));

    if (accepted === true) {
      key.verify(message, truncated);
    } else {
      throwsKind(
        'BadLength',
        () => key.verify(message, truncated),
        `case \`${caseName(item)}\` must refuse a ${take}-byte tag (${why})`,
      );
    }
  }
  assert.equal(mac.MIN_TAG_LEN, 16, 'the floor is 128 bits of forgery margin');
  assert.equal(mac.TAG_LEN, 32, 'HMAC-SHA256 produces 32 bytes');
});

test('a tag longer than the MAC produces is refused', () => {
  // Not in the vectors because Rust cannot express it: `verify` there takes a slice and the
  // over-length case is caught by the same bound. Here a caller can pass a 64-byte array, and
  // truncating it to 32 for the comparison would accept a tag whose second half is anything at
  // all.
  const key = MacKey.derive(new TextEncoder().encode('root'), mac.LABEL_SESSION_TOKEN);
  const message = new TextEncoder().encode('migo');
  const padded = new Uint8Array(mac.TAG_LEN + 1);
  padded.set(key.tag(message), 0);
  throwsKind('BadLength', () => key.verify(message, padded), 'a 33-byte tag');
});

// --- the hazards that only exist on this side -------------------------------

test('verification reports failure by throwing, not by returning', () => {
  // A `verify` that returns `false` is one forgotten `if` away from accepting every forgery, and
  // the missing `if` is invisible in review. This test is the executable version of that rule:
  // if someone changes the signature to return a boolean, `assert.throws` fails.
  const key = MacKey.derive(new TextEncoder().encode('root'), mac.LABEL_RESUME_CURSOR);
  const message = new TextEncoder().encode('cursor:1');
  const tag = key.tag(message);
  assert.equal(key.verify(message, tag), undefined, 'a successful verify returns nothing');
  assert.throws(() => key.verify(new TextEncoder().encode('cursor:2'), tag), CryptoError);
});

test('key material never appears in a string, a log line, or JSON', () => {
  // The realistic leak on this side is not an attacker reading a private field. It is a developer
  // logging the object that holds one, or a session record being serialised into local storage
  // with a key still attached to it.
  const secret = 'deadbeef'.repeat(8);
  const symmetric = SymmetricKey.fromBytes(unhex(secret));
  const macKey = MacKey.fromBytes(unhex(secret));

  for (const [what, value] of [
    ['SymmetricKey', symmetric],
    ['MacKey', macKey],
  ] as const) {
    const rendered = [
      // `String(x)` and `` `${x}` `` are the same operation on an object — both call
      // `toString` through ToPrimitive — so one of them stands for both.
      String(value),
      JSON.stringify(value),
      JSON.stringify({ session: { id: 7, key: value } }),
      inspect(value),
      inspect({ nested: [value] }, { depth: 5 }),
    ].join('\n');
    assert.doesNotMatch(rendered, /deadbeef/i, `${what} leaked key bytes into text`);
    assert.doesNotMatch(rendered, /[0-9a-f]{16}/i, `${what} rendered something hex-shaped`);
    assert.match(rendered, /\*\*\*/, `${what} must render a redaction marker`);
  }

  // `Object.keys` and the spread operator must not find the bytes either: a `#private` field is
  // unreachable, an underscore-prefixed one is not, and the difference is the whole reason for
  // the choice.
  assert.deepEqual(Object.keys(symmetric), []);
  assert.deepEqual(Object.keys({ ...macKey }), []);
});

test('a key copies its bytes, so clearing one buffer cannot clear another', () => {
  // Rust takes `[u8; 32]` by value and gets this from the type system. Here the caller passes a
  // reference, and a key that aliased it would be silently zeroed by a caller doing exactly the
  // right thing with its own buffer.
  const bytes = new Uint8Array(32).fill(0x5a);
  const key = SymmetricKey.fromBytes(bytes);
  const nonce = new Uint8Array(aead.NONCE_LEN).fill(1);
  const sealed = aead.sealWithNonce(key, nonce, new Uint8Array(0), new TextEncoder().encode('hi'));

  bytes.fill(0);
  assert.equal(
    hex(aead.open(key, new Uint8Array(0), sealed)),
    hex(new TextEncoder().encode('hi')),
    'the key was aliased to the caller buffer',
  );

  const macKey = MacKey.fromBytes(new Uint8Array(32).fill(0x5a));
  const message = new TextEncoder().encode('m');
  const tag = macKey.tag(message);
  assert.notEqual(
    hex(tag),
    hex(MacKey.fromBytes(new Uint8Array(32)).tag(message)),
    'the key is all zeroes: the copy took the caller cleared buffer',
  );
});

test('a destroyed key throws instead of working under all zeroes', () => {
  // A zeroed key still produces perfectly plausible tags and envelopes, and every one of them
  // would verify against any other zeroed key in the deployment — a silent downgrade to no
  // authentication at all. So use after destruction is an error, not a weaker key.
  const macKey = MacKey.derive(new TextEncoder().encode('root'), mac.LABEL_WEBHOOK);
  const message = new TextEncoder().encode('body');
  const tag = macKey.tag(message);
  macKey.destroy();
  assert.throws(() => macKey.tag(message), TypeError);
  assert.throws(() => macKey.verify(message, tag), TypeError);

  const symmetric = SymmetricKey.generate();
  symmetric.destroy();
  assert.throws(() => symmetric.expose(), TypeError);
  assert.throws(
    () => aead.seal(symmetric, new Uint8Array(0), new Uint8Array(0)),
    TypeError,
    'sealing under a destroyed key must not fall back to zeroes',
  );
});

test('no error carries the bytes it was handed', () => {
  // The same rule `@migo/wire` follows, for the same reason and one more: these errors are
  // produced while processing an attacker-supplied ciphertext, they end up in logs, and a log
  // line is not a place to put a decryption failure's inputs. An error that quoted the plaintext
  // it failed to produce would also be a privacy incident on every dropped private message.
  const key = SymmetricKey.fromBytes(new Uint8Array(32).fill(0xc0));
  const sealed = aead.sealWithNonce(
    key,
    new Uint8Array(aead.NONCE_LEN).fill(2),
    new Uint8Array(0),
    unhex('cafebabecafebabe'),
  );
  const tampered = sealed.slice();
  const last = tampered.length - 1;
  // `noUncheckedIndexedAccess` is on, so the read is spelled out rather than hidden in a `^=`.
  tampered[last] = (tampered[last] ?? 0) ^ 0x01;

  try {
    aead.open(key, new Uint8Array(0), tampered);
    assert.fail('a tampered envelope opened');
  } catch (error) {
    assert.ok(error instanceof CryptoError);
    assert.equal(error.kind, 'DecryptionFailed');
    assert.doesNotMatch(error.message, /[0-9a-f]{8}/i, 'the error quoted a hex dump');
    assert.doesNotMatch(String(error.stack ?? ''), /cafebabe/i);
    // Lengths are the one number that is safe to state — the sender can already see them — and
    // `BadLength` is built out of exactly those.
    const short = new CryptoError('BadLength', 'x');
    assert.deepEqual(short.detail, {});
  }
});

// --- the suite is present at all --------------------------------------------

test('every vector file is present and populated', () => {
  const expected: ReadonlyArray<readonly [string, readonly string[]]> = [
    ['kdf.json', ['cases', 'rfc', 'pairs']],
    ['aead.json', ['cases', 'invalid']],
    ['mac.json', ['cases', 'parts', 'rfc', 'truncation', 'distinct_pairs']],
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
  assert.ok(total >= 40, `only ${total} crypto vector cases, expected at least 40`);
});
