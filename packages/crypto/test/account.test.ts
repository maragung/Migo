/**
 * The TypeScript half of the account-root conformance suite.
 *
 * This file and `server/crates/migo-account/tests/vectors.rs` read the *same* JSON files in
 * `shared/protocol/vectors/crypto` and make the same assertions, for the same reason the message
 * suite does (see `vectors.test.ts`): cryptographic code can be wrong and still work, so the
 * expected values come from outside the implementation.
 *
 * The six files split by how their expected bytes were produced, a provenance each records:
 *
 * * `account-domains.json`, `account-evm.json`, `account-tx.json` and `account-eip712.json` are
 *   computed by an independent Python generator written from RFC 5869, BIP-32, EIP-55, the
 *   Ethereum specification's RLP appendix and EIP-712 — with those documents' own vectors as
 *   self-checks, so a failure means this port disagrees with the *standard*, not merely with the
 *   Rust crate. `account-tx.json` also carries one chain-sourced case: a real Avalanche C-Chain
 *   transaction whose sender recovery and hash are pinned to what the chain observed, because a
 *   port that is merely self-consistent can still disagree with the chain.
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

import { keccak_256 } from '@noble/hashes/sha3.js';

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

/**
 * A decimal-string integer field, the form every value past 2^53 arrives in: JSON numbers are
 * doubles and a wei amount read as `count()` would be silently corrupted, not loudly rejected.
 */
function bigintText(item: Case, key: string): bigint {
  const value = text(item, key);
  assert.ok(
    /^(0|[1-9][0-9]*)$/.test(value),
    `case \`${caseName(item)}\` needs a decimal \`${key}\``,
  );
  return BigInt(value);
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

// --- the transactions (independent-python + chain-sourced) -------------------

test('transaction bodies and signing hashes match the independent generator', () => {
  const file = load('account-tx.json');
  let signed = 0;
  for (const item of section(file, 'cases', 'account-tx.json')) {
    if (item.provenance === 'chain-sourced') {
      continue; // no root to sign with; its own test follows
    }
    const root = account.MigoRoot.fromBytes(bytesOf(item, 'root'));
    const wallet = account.EvmWallet.fromRoot(root, count(item, 'index'));

    // The integer fields arrive as decimal strings on purpose: wei values live far above 2^53 and
    // JSON numbers are doubles, so a `count()` read here would silently corrupt them.
    const tx = new account.Eip1559Tx({
      chainId: Number(bigintText(item, 'chain_id')),
      nonce: Number(bigintText(item, 'nonce')),
      maxPriorityFeePerGas: bigintText(item, 'max_priority_fee_per_gas'),
      maxFeePerGas: bigintText(item, 'max_fee_per_gas'),
      gasLimit: Number(bigintText(item, 'gas_limit')),
      to: bytesOf(item, 'recipient'),
      value: bigintText(item, 'value_wei'),
      data: bytesOf(item, 'data'),
    });
    assert.equal(hex(tx.bodyRlp()), text(item, 'body_rlp'), `body \`${caseName(item)}\``);
    assert.equal(hex(tx.signingHash()), text(item, 'signing_hash'), `digest \`${caseName(item)}\``);
    assert.equal(hex(wallet.address()), text(item, 'sender'), `sender \`${caseName(item)}\``);

    // The signature bytes are deliberately not pinned: each port signs with its own library and
    // nonce, and any valid low-s signature is the same transaction to the chain. Proving validity
    // is recovering the sender from the port's own raw transaction.
    const signedTx = tx.sign(wallet);
    assert.equal(
      hex(account.recoverSender(signedTx.raw())),
      hex(wallet.address()),
      `recovered sender \`${caseName(item)}\``,
    );
    // The raw transaction is a well-formed envelope on its way out, whatever broadcast does later.
    const envelope = account.rlpDecode(signedTx.raw().subarray(1));
    assert.ok(Array.isArray(envelope) && envelope.length === 12, `envelope \`${caseName(item)}\``);
    signed += 1;
  }
  assert.ok(signed > 0, 'account-tx.json carries no signable case');
});

test('the chain-sourced transaction recovers to its observed sender', () => {
  // A real Avalanche C-Chain mainnet transaction: the sender and hash are what the chain observed,
  // so this is the one case that can catch a port that is self-consistent and still wrong.
  const file = load('account-tx.json');
  const observed = section(file, 'cases', 'account-tx.json').filter(
    (item) => item.provenance === 'chain-sourced',
  );
  assert.equal(observed.length, 1, 'account-tx.json must carry exactly one chain-sourced case');
  const item = observed[0]!;

  const raw = bytesOf(item, 'raw');
  assert.equal(
    hex(account.recoverSender(raw)),
    text(item, 'sender'),
    'recovered sender disagrees with the chain',
  );
  assert.equal(
    hex(keccak_256(raw)),
    text(item, 'tx_hash'),
    'keccak256(raw) disagrees with the chain',
  );

  // Decode the envelope strictly and rebuild the transaction: every observed field must match the
  // case's record, and the re-encoded body must be the body the signature was made over.
  const envelope = account.rlpDecode(raw.subarray(1));
  assert.ok(Array.isArray(envelope) && envelope.length === 12, 'envelope must be a 12-item list');
  const body = envelope.slice(0, 9);
  const tx = new account.Eip1559Tx({
    chainId: Number(account.rlpAsUint(body[0]!)),
    nonce: Number(account.rlpAsUint(body[1]!)),
    maxPriorityFeePerGas: account.rlpAsUint(body[2]!),
    maxFeePerGas: account.rlpAsUint(body[3]!),
    gasLimit: Number(account.rlpAsUint(body[4]!)),
    to: body[5]! as Uint8Array,
    value: account.rlpAsUint(body[6]!),
    data: body[7]! as Uint8Array,
  });
  assert.equal(hex(tx.bodyRlp()), hex(account.rlpEncode(body)), 're-encoded body differs');
  assert.equal(tx.chainId, Number(text(item, 'chain_id')), 'chain id');
  assert.equal(tx.nonce, Number(text(item, 'nonce')), 'nonce');
  assert.equal(
    tx.maxPriorityFeePerGas,
    bigintText(item, 'max_priority_fee_per_gas'),
    'priority fee',
  );
  assert.equal(tx.maxFeePerGas, bigintText(item, 'max_fee_per_gas'), 'max fee');
  assert.equal(tx.gasLimit, Number(text(item, 'gas_limit')), 'gas limit');
  assert.equal(hex(tx.to), text(item, 'recipient'), 'recipient');
  assert.equal(tx.value, bigintText(item, 'value_wei'), 'value');
  assert.equal(hex(tx.data), text(item, 'data'), 'call data');
});

test('the RLP codec is strict in both directions', () => {
  // The four shapes a tolerant decoder accepts and a canonical one must not, pinned from the
  // specification's own strictness rules — this parser reads bytes that arrived over a network.
  for (const [bytes, why] of [
    ['8105', 'single byte below 0x80 must encode as itself'],
    ['b8012a', 'length written in long form for a short-form payload'],
    ['b9002a2a', 'length has a leading zero byte'],
    ['c001', 'trailing bytes after a complete item'],
  ] as const) {
    assert.throws(
      () => account.rlpDecode(unhex(bytes)),
      (error: unknown) =>
        error instanceof AccountError && error.kind === 'MalformedRlp' && error.detail.what === why,
      `${bytes} must be rejected`,
    );
  }

  // And canonical on the way in: the specification's appendix examples, plus the two integer
  // rules hand-rolled encoders get wrong — zero is the empty string, and 1024 needs two bytes.
  assert.equal(hex(account.rlpEncode(unhex('646f67'))), '83646f67');
  assert.equal(hex(account.rlpEncode([unhex('636174'), unhex('646f67')])), 'c88363617483646f67');
  assert.equal(hex(account.rlpEncode(account.rlpUint(0n))), '80');
  assert.equal(hex(account.rlpEncode(account.rlpUint(1024n))), '820400');
  assert.equal(hex(account.rlpEncode(unhex('00'))), '00');
  assert.equal(hex(account.rlpEncode([[], []])), 'c2c0c0');

  // Round trip: whatever the decoder accepts, the encoder hands back byte for byte — the identity
  // that makes "re-encode the body" a valid strictness check in the chain-sourced test above.
  const observed = load('account-tx.json').cases as Case[];
  const raw = unhex(
    text(
      observed.find((item) => item.provenance === 'chain-sourced')!,
      'raw',
    ),
  );
  assert.equal(hex(account.rlpEncode(account.rlpDecode(raw.subarray(1)))), hex(raw.subarray(1)));
});

test('parseAddress accepts lowercase and checks EIP-55 on mixed case', () => {
  // The send flow's last line of defense before funds move: a typo in a checksummed recipient must
  // fail here, not on the chain.
  const evm = load('account-evm.json');
  const withLetters = section(evm, 'cases', 'account-evm.json').find((item) =>
    /[a-fA-F]/.test(text(item, 'address_checksummed')),
  );
  assert.ok(withLetters !== undefined, 'account-evm.json carries no address with a letter in it');
  const checksummed = text(withLetters, 'address_checksummed');
  const lowercase = checksummed.toLowerCase().replace(/^0x/, '');
  assert.equal(hex(account.parseAddress(checksummed)), lowercase);
  assert.equal(hex(account.parseAddress(lowercase)), lowercase);
  assert.equal(hex(account.parseAddress(`0x${lowercase}`)), lowercase);

  // One flipped letter: still mixed case, still valid hex, and no longer the checksum the EIP-55
  // casing encodes — the exact shape a pasted-address typo produces.
  const flipped = checksummed.replace(/[a-fA-F]/, (c) =>
    c === c.toLowerCase() ? c.toUpperCase() : c.toLowerCase(),
  );
  assert.notEqual(flipped, checksummed);
  assert.throws(
    () => account.parseAddress(flipped),
    (error: unknown) => error instanceof AccountError && error.kind === 'AddressChecksumFailed',
  );
  assert.throws(
    () => account.parseAddress('not-an-address'),
    (error: unknown) => error instanceof AccountError && error.kind === 'BadAddress',
  );
  assert.throws(
    () => account.parseAddress(lowercase.slice(0, 39)),
    (error: unknown) => error instanceof AccountError && error.kind === 'BadAddress',
  );
});

// --- EIP-712 (spec example + independent-python) -----------------------------

/**
 * Converts the vector file's recursive value model into this package's typed values. Structs
 * become their own hashStruct — the EIP-712 rule that makes the type recursive — and a struct is
 * recognized *before* its fields are read: an array node carries `values`, not `value`, and the
 * eager read is the bug this suite's Python and Rust siblings each hit once.
 */
function eip712Value(node: unknown, path: string): account.Eip712Value {
  assert.ok(typeof node === 'object' && node !== null, `${path} must be an object`);
  const item = { name: path } as Case;
  Object.assign(item, node as Record<string, string>);
  if (item.struct !== undefined) {
    const struct = item.struct as Record<string, unknown>;
    const typeHash = account.eip712TypeHash(
      stringAt(struct, 'primary_type', path),
      stringsAt(struct, 'referenced_types', path),
    );
    const values = listAt(struct, 'values', path).map((child, i) =>
      eip712Value(child, `${path}[${i}]`),
    );
    return { type: 'bytes32', value: account.eip712HashStruct(typeHash, values) };
  }
  const type = stringAt(item, 'type', path);
  switch (type) {
    case 'address':
    case 'bytes32':
    case 'bytes':
      return { type, value: unhex(stringAt(item, 'value', path)) };
    case 'uint256':
      // The file writes uint256 as hex (shorter than 32 bytes when the value is small); the
      // padding to 32 is the port's job.
      return {
        type,
        value: BigInt(`0x${stringAt(item, 'value', path)}`),
      };
    case 'string':
      return { type, value: stringAt(item, 'value', path) };
    case 'array':
      return {
        type: 'array',
        values: listAt(item as Record<string, unknown>, 'values', path).map((child, i) =>
          eip712Value(child, `${path}[${i}]`),
        ),
      };
    default:
      assert.fail(`${path}: unknown EIP-712 value type \`${String(type)}\``);
  }
}

function stringAt(node: Record<string, unknown>, key: string, path: string): string {
  assert.equal(typeof node[key], 'string', `${path}.${key} must be a string`);
  return node[key] as string;
}

function stringsAt(node: Record<string, unknown>, key: string, path: string): readonly string[] {
  const values = listAt(node, key, path);
  return values.map((value, i) => stringAt({ [key]: value }, key, `${path}[${i}]`));
}

function listAt(node: Record<string, unknown>, key: string, path: string): readonly unknown[] {
  assert.ok(Array.isArray(node[key]), `${path}.${key} must be a list`);
  return node[key] as unknown[];
}

test('EIP-712 digests match the independent generator and the specification example', () => {
  const file = load('account-eip712.json');
  const cases = section(file, 'cases', 'account-eip712.json');

  // The first case is the EIP-712 specification's own worked example, its expected values pinned
  // to the EIP's published digest. A port cannot pass that by agreeing with the generator on a
  // shared mistake, which is why its presence is asserted rather than assumed.
  assert.equal(cases[0]!.name, 'eip712-spec-example');
  assert.equal(cases[0]!.provenance, 'eip712-spec-example');

  for (const item of cases) {
    const domainNode = item.domain as Record<string, unknown>;
    const domain = new account.Eip712Domain({
      name: domainNode.name as string | undefined,
      version: domainNode.version as string | undefined,
      chainId: domainNode.chain_id as number | undefined,
      verifyingContract:
        domainNode.verifying_contract !== undefined
          ? unhex(domainNode.verifying_contract as string)
          : undefined,
      salt: domainNode.salt !== undefined ? unhex(domainNode.salt as string) : undefined,
    });

    const message = (item.message as Record<string, unknown>).struct as Record<string, unknown>;
    const primary = stringAt(message, 'primary_type', caseName(item));
    const referenced = stringsAt(message, 'referenced_types', caseName(item));
    const values = listAt(message, 'values', caseName(item)).map((child, i) =>
      eip712Value(child, `${caseName(item)}[${i}]`),
    );

    // The encodeType appendix — referenced declarations, sorted by name — is the part of EIP-712
    // every hand-rolled implementation gets wrong, so the string itself is pinned before any hash.
    assert.equal(
      account.eip712EncodeType(primary, referenced),
      text(item, 'encode_type'),
      `encodeType \`${caseName(item)}\``,
    );

    const expected = item.expected as Record<string, string>;
    const typeHash = account.eip712TypeHash(primary, referenced);
    assert.equal(hex(typeHash), expected.type_hash, `type hash \`${caseName(item)}\``);
    assert.equal(
      hex(domain.separator()),
      expected.domain_separator,
      `separator \`${caseName(item)}\``,
    );
    const structHash = account.eip712HashStruct(typeHash, values);
    assert.equal(hex(structHash), expected.struct_hash, `hashStruct \`${caseName(item)}\``);
    assert.equal(
      hex(account.eip712Digest(domain.separator(), structHash)),
      expected.digest,
      `digest \`${caseName(item)}\``,
    );
  }
});

test('every account vector file is present and populated', () => {
  const expected: ReadonlyArray<readonly [string, readonly string[]]> = [
    ['account-domains.json', ['cases']],
    ['account-evm.json', ['cases']],
    ['account-tx.json', ['cases']],
    ['account-eip712.json', ['cases']],
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
  assert.ok(total >= 36, `only ${total} account vector cases, expected at least 36`);
});
