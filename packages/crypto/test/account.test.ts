/**
 * The TypeScript half of the account-root conformance suite.
 *
 * This file and `server/crates/migo-account/tests/vectors.rs` read the *same* JSON files in
 * `shared/protocol/vectors/crypto` and make the same assertions, for the same reason the message
 * suite does (see `vectors.test.ts`): cryptographic code can be wrong and still work, so the
 * expected values come from outside the implementation.
 *
 * The four files split by how their expected bytes were produced, a provenance each records:
 *
 * * `account-domains.json` and `account-evm.json` are computed by an independent Python generator
 *   written from RFC 5869, BIP-32 and EIP-55 with those documents' own vectors as self-checks — a
 *   failure means this port disagrees with the *standard*, not merely with the Rust crate.
 * * `account-mldsa.json` and `account-container.json` are `rust-reference`: ML-DSA-65 has no
 *   script-reproducible published vectors and the container is the house composition, so the Rust
 *   crate is the reference and these files exist to hold this port to it byte for byte. This
 *   runner re-asserts the `rust-reference` provenance, because a test that silently trusts its own
 *   output as truth is worse than no test.
 */

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

import { AccountError, account } from '../src/index.js';

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
      `${path} is missing; run python3 tools/vectors/generate_account_vectors.py from the repo root`,
    );
  }
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
}

/**
 * Pulls one section out of a vector file, refusing an empty one: a suite that runs zero cases
 * reports the strongest guarantee in the codebase and checks nothing.
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
 * The failure name a vector file uses. These strings are the Rust enum's variant names, so
 * comparing to `error.kind` rather than to a message is what lets both languages read one file.
 */
function kindOf(error: unknown): string {
  assert.ok(error instanceof AccountError, `expected an AccountError, got ${String(error)}`);
  return error.kind;
}

/** Runs `body`, asserting it rejects with an {@link AccountError} of exactly `expected` kind. */
async function rejectsKind(
  expected: string,
  body: () => Promise<unknown>,
  context: string,
): Promise<void> {
  let outcome: unknown;
  try {
    outcome = await body();
  } catch (error) {
    const actual = kindOf(error);
    assert.equal(actual, expected, `${context} failed with ${actual}, expected ${expected}`);
    return;
  }
  assert.fail(`${context} was accepted (${String(outcome)}), expected ${expected}`);
}

// --- label table ------------------------------------------------------------
//
// A vector file names a domain as text, but the runner resolves it through this package's own
// constants and fails on an unknown name. Passing the file's string straight to the KDF would make
// the test pass for a file that says `MIGO/IDENTITY/V2`, which is precisely the change that must
// not go unnoticed: renaming a domain silently re-keys every account. The table is also the
// TypeScript↔Rust bridge for the domain *set* — a domain the Rust crate gains and this one does
// not regenerates a case this runner cannot resolve, and it says so by name.

const DOMAIN_LABELS: ReadonlyMap<string, string> = new Map([
  [account.DOMAIN_IDENTITY, account.DOMAIN_IDENTITY],
  [account.DOMAIN_EVM, account.DOMAIN_EVM],
  [account.DOMAIN_E2EE, account.DOMAIN_E2EE],
  [account.DOMAIN_BACKUP, account.DOMAIN_BACKUP],
  [account.DOMAIN_DEVICE, account.DOMAIN_DEVICE],
]);

function domainLabel(item: Case): string {
  const named = text(item, 'label');
  const resolved = DOMAIN_LABELS.get(named);
  assert.ok(resolved !== undefined, `\`${named}\` is not an account domain this build defines`);
  return resolved;
}

// --- the domains (independent-python) ---------------------------------------

test('account domain seeds match the independent Python generator', () => {
  const file = load('account-domains.json');
  for (const item of section(file, 'cases', 'account-domains.json')) {
    const root = account.MigoRoot.fromBytes(bytesOf(item, 'root'));
    const seed = root.domainSeed(domainLabel(item));
    assert.equal(hex(seed), text(item, 'seed'), `domain seed for \`${caseName(item)}\``);
  }
});

test('founding-device E2EE seeds match the independent Python generator', () => {
  // The E2EE domain seed is expanded once more per key (Ed25519 signing, X25519 exchange); those
  // sub-seeds ride on the E2EE case beside the domain seed itself.
  const file = load('account-domains.json');
  let checked = 0;
  for (const item of section(file, 'cases', 'account-domains.json')) {
    if (typeof item.e2ee_signing_seed !== 'string') {
      continue;
    }
    const root = account.MigoRoot.fromBytes(bytesOf(item, 'root'));
    const { signing, exchange } = account.foundingDeviceE2eeSeeds(root);
    assert.equal(
      hex(signing),
      text(item, 'e2ee_signing_seed'),
      `signing seed \`${caseName(item)}\``,
    );
    assert.equal(
      hex(exchange),
      text(item, 'e2ee_exchange_seed'),
      `exchange seed \`${caseName(item)}\``,
    );
    checked += 1;
  }
  assert.ok(checked > 0, 'account-domains.json carries no E2EE sub-seed case');
});

test('every live account domain is pinned', () => {
  // A future generator change that drops a domain silently must fail here rather than pass by
  // running one fewer case.
  const file = load('account-domains.json');
  const labels = new Set(
    section(file, 'cases', 'account-domains.json').map((item) =>
      typeof item.label === 'string' ? item.label : '',
    ),
  );
  for (const domain of [
    account.DOMAIN_IDENTITY,
    account.DOMAIN_EVM,
    account.DOMAIN_E2EE,
    account.DOMAIN_BACKUP,
  ]) {
    assert.ok(labels.has(domain), `account-domains.json is missing the ${domain} domain`);
  }
});

// --- the EVM wallets (independent-python) -----------------------------------

test('EVM wallets match the independent Python generator', () => {
  const file = load('account-evm.json');
  for (const item of section(file, 'cases', 'account-evm.json')) {
    const root = account.MigoRoot.fromBytes(bytesOf(item, 'root'));
    const wallet = account.EvmWallet.fromRoot(root, count(item, 'index'));

    // The address is the derived-from-public-key value the server stores; the checksum is display
    // material with its own nibble-indexing failure mode, so it is pinned separately in EIP-55
    // canonical casing.
    assert.equal(hex(wallet.address()), text(item, 'address'), `address \`${caseName(item)}\``);
    assert.equal(
      wallet.addressChecksummed(),
      text(item, 'address_checksummed'),
      `EIP-55 checksum \`${caseName(item)}\``,
    );
    // The private key and chain code are intermediates the vectors pin for the ports: a wallet
    // that lands on the right address from a wrong private key is a bug the address alone hides.
    assert.equal(
      hex(wallet.privateKeyBytes()),
      text(item, 'private_key'),
      `private key \`${caseName(item)}\``,
    );
    assert.equal(
      hex(wallet.chainCode()),
      text(item, 'chain_code'),
      `chain code \`${caseName(item)}\``,
    );
  }
});

// --- ML-DSA (rust-reference) -------------------------------------------------

test('ML-DSA identity keys and signatures reproduce the reference vectors', () => {
  const file = load('account-mldsa.json');
  for (const item of section(file, 'cases', 'account-mldsa.json')) {
    assert.equal(
      text(item, 'provenance'),
      'rust-reference',
      `\`${caseName(item)}\`: unexpected provenance — this test consumes rust-reference files`,
    );

    const seed = bytesOf(item, 'seed');
    const publicKey = bytesOf(item, 'public_key');
    const payload = bytesOf(item, 'payload');
    const signature = bytesOf(item, 'signature');
    const context = text(item, 'context');
    assert.equal(
      publicKey.length,
      account.PUBLIC_KEY_LEN,
      `\`${caseName(item)}\` public key length`,
    );
    assert.equal(signature.length, account.SIGNATURE_LEN, `\`${caseName(item)}\` signature length`);

    // The key under test is whichever kind its context says it is: the context is the purpose, and
    // the purpose picks both the key type and the signing method. Signing the device case under
    // the identity's login context is exactly the cross-purpose confusion the contexts prevent, so
    // the routing here is deliberately explicit — as it is in the Rust test.
    let derivedPublic: Uint8Array;
    let derivedSignature: Uint8Array;
    let contextBytes: Uint8Array;
    if (context === 'migo-auth-login-v1') {
      const identity = account.IdentityKey.fromSeed(seed);
      derivedPublic = identity.publicKey();
      derivedSignature = identity.signLogin(payload);
      contextBytes = account.CONTEXT_LOGIN;
    } else if (context === 'migo-auth-rotate-v1') {
      const identity = account.IdentityKey.fromSeed(seed);
      derivedPublic = identity.publicKey();
      derivedSignature = identity.signRotate(payload);
      contextBytes = account.CONTEXT_ROTATE;
    } else if (context === 'migo-auth-device-v1') {
      const credential = account.DeviceCredential.fromSeed(seed);
      derivedPublic = credential.publicKey();
      derivedSignature = credential.signLogin(payload);
      contextBytes = account.CONTEXT_LOGIN_DEVICE;
    } else {
      assert.fail(`\`${caseName(item)}\`: unknown context ${context}`);
    }

    // This port's context constant must equal the vector's context string, or the routing above
    // used the right key with the wrong domain separator.
    assert.equal(
      new TextDecoder().decode(contextBytes),
      context,
      `\`${caseName(item)}\` context constant disagrees with the vector`,
    );

    // Byte for byte: deterministic signing is what makes this portable, so the pinned bytes must
    // reproduce exactly, not merely verify.
    assert.equal(
      hex(derivedPublic),
      text(item, 'public_key'),
      `\`${caseName(item)}\` public key reproduces`,
    );
    assert.equal(
      hex(derivedSignature),
      text(item, 'signature'),
      `\`${caseName(item)}\` signature bytes reproduce`,
    );

    // And the pinned signature verifies against the pinned key under the pinned context, which also
    // proves the file self-consistent. `verifyIdentity` throws on failure, so reaching the next
    // line is the assertion.
    account.verifyIdentity(publicKey, payload, contextBytes, signature);
  }
});

test('a device credential seed reproduces its vector public key', () => {
  // Device credentials are reconstructed from their stored seed, never derived; the vector pins
  // that relation.
  const file = load('account-mldsa.json');
  const deviceCases = section(file, 'cases', 'account-mldsa.json').filter(
    (item) => item.context === 'migo-auth-device-v1',
  );
  assert.ok(deviceCases.length > 0, 'account-mldsa.json carries no device credential case');
  for (const item of deviceCases) {
    const credential = account.DeviceCredential.fromSeed(bytesOf(item, 'seed'));
    assert.equal(
      hex(credential.publicKey()),
      text(item, 'public_key'),
      `device credential public key \`${caseName(item)}\``,
    );
  }
});

// --- the container (rust-reference) ------------------------------------------

test('containers reproduce byte for byte and open', async () => {
  const file = load('account-container.json');
  for (const item of section(file, 'cases', 'account-container.json')) {
    assert.equal(
      text(item, 'provenance'),
      'rust-reference',
      `\`${caseName(item)}\`: unexpected provenance`,
    );

    const root = account.MigoRoot.fromBytes(bytesOf(item, 'root'));
    const credential = text(item, 'credential');
    const salt = bytesOf(item, 'salt');
    const nonce = bytesOf(item, 'nonce');
    const params = new account.ContainerParams(
      count(item, 'memory_kib'),
      count(item, 'time_cost'),
      count(item, 'lanes'),
    );
    const createdAt = count(item, 'created_at');
    const expected = bytesOf(item, 'container');

    // Reseating the same inputs must produce the same file: the format has no hidden entropy beyond
    // the salt and nonce, which are pinned here.
    const payload = account.AccountFile.forRoot(root, createdAt);
    const actual = await account.sealContainerWith(credential, payload, params, salt, nonce);
    assert.equal(
      hex(actual),
      text(item, 'container'),
      `\`${caseName(item)}\` container bytes reproduce`,
    );

    // And the pinned file opens back to the same root with its credential.
    const opened = await account.openContainer(credential, expected);
    assert.ok(opened.rootSecret().equals(root), `\`${caseName(item)}\` opened root differs`);

    // A wrong credential is refused identically to a tampered byte: both `OpenFailed`, so a caller
    // cannot tell how far a guess got.
    await rejectsKind(
      'OpenFailed',
      () => account.openContainer('wrong-credential-x', expected),
      `\`${caseName(item)}\` wrong credential`,
    );
    const tampered = expected.slice();
    const last = tampered.length - 1;
    // `noUncheckedIndexedAccess` is on, so the read is spelled out rather than hidden in a `^=`.
    tampered[last] = (tampered[last] ?? 0) ^ 0x01;
    await rejectsKind(
      'OpenFailed',
      () => account.openContainer(credential, tampered),
      `\`${caseName(item)}\` tampered byte`,
    );
  }
});

// --- the suite is present at all --------------------------------------------

test('every account vector file is present and populated', () => {
  const expected: ReadonlyArray<readonly [string, readonly string[]]> = [
    ['account-domains.json', ['cases']],
    ['account-evm.json', ['cases']],
    ['account-mldsa.json', ['cases']],
    ['account-container.json', ['cases']],
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
  assert.ok(total >= 24, `only ${total} account vector cases, expected at least 24`);
});
