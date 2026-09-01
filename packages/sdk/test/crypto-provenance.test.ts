/**
 * The crypto under the SDK is audited third-party code, and this is provable without running it.
 *
 * ADR-0003 permits only audited implementations of cryptographic primitives, for a reason behavioural
 * tests cannot catch: a hand-rolled cipher that is subtly wrong still produces random-looking bytes,
 * still round-trips against itself, and still passes every functional test — the flaw surfaces years
 * later in someone else's cryptanalysis, against messages already sent. The only defence is
 * provenance, checked structurally at the import boundary. So this file reads the source of
 * `@migo/crypto` and of the SDK from disk and proves three things by inspection: the crypto package
 * pulls its primitives from the three audited `@noble` libraries and nothing else, the SDK layer
 * contains no cryptography of its own but reaches every primitive through `@migo/crypto`, and the one
 * client entropy source is the platform CSPRNG. These are the checks a code reviewer would run by
 * eye; pinning them means a future edit that quietly adds `crypto-js`, reaches for `node:crypto`, or
 * inlines a primitive fails here instead of in production.
 */

import assert from 'node:assert/strict';
import test from 'node:test';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * The audited libraries the crypto package may depend on. The ADR-0003 trio carried the E2EE
 * stack; ADR-0013's account port added three more, each for a primitive the trio does not carry
 * and each from the same audited shelf: `@noble/post-quantum` for ML-DSA-65 (same authors and
 * audit posture as the rest of `@noble`), `@scure/bip32` for BIP-32/44 walking, and `hash-wasm`
 * for Argon2id. Nothing else — a fifth family fails the manifest test below.
 */
const AUDITED = new Set([
  '@noble/ciphers',
  '@noble/curves',
  '@noble/hashes',
  '@noble/post-quantum',
  '@scure/bip32',
  'hash-wasm',
]);

/** What the SDK is allowed to import from outside its own tree: the workspace packages, no more. */
const SDK_ALLOWED = new Set(['@migo/crypto', '@migo/protocol', '@migo/wire']);

/** Walks up from this compiled test file until it finds the pnpm workspace root. */
function workspaceRoot(): string {
  let dir = path.dirname(fileURLToPath(import.meta.url));
  for (;;) {
    if (existsSync(path.join(dir, 'pnpm-workspace.yaml'))) {
      return dir;
    }
    const parent = path.dirname(dir);
    assert.notEqual(parent, dir, 'reached the filesystem root without finding the workspace');
    dir = parent;
  }
}

const ROOT = workspaceRoot();
const CRYPTO_SRC = path.join(ROOT, 'packages', 'crypto', 'src');
const SDK_SRC = path.join(ROOT, 'packages', 'sdk', 'src');

/** Removes block and line comments so didactic prose (which names `Math.random`, `node:crypto`, …)
 * is never mistaken for real code. The `//` strip spares `://` so URLs in strings survive. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

/** The absolute paths of every `.ts` source file under `dir`, subdirectories included — the
 * account port lives in `src/account/`, and a policy that skips it is not a policy. */
function tsFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...tsFiles(full));
    } else if (entry.name.endsWith('.ts')) {
      out.push(full);
    }
  }
  return out;
}

/** The npm package name an import specifier resolves to (`@noble/ciphers/chacha.js` -> `@noble/ciphers`). */
function packageOf(specifier: string): string {
  const parts = specifier.split('/');
  return specifier.startsWith('@') ? `${parts[0]}/${parts[1]}` : (parts[0] ?? specifier);
}

/** Every non-relative module specifier imported by `file`, comments removed first. */
function externalImports(file: string): string[] {
  const source = stripComments(readFileSync(file, 'utf8'));
  const out: string[] = [];
  // Covers `import … from 'x'`, `export … from 'x'`, and bare `import 'x'` side-effect imports.
  for (const match of source.matchAll(/\bfrom\s*['"]([^'"]+)['"]/g)) {
    out.push(match[1] ?? '');
  }
  for (const match of source.matchAll(/\bimport\s*['"]([^'"]+)['"]/g)) {
    out.push(match[1] ?? '');
  }
  return out.filter((specifier) => specifier.length > 0 && !specifier.startsWith('.'));
}

/** The concatenated, comment-stripped source of every crypto module, for whole-package assertions. */
function cryptoSource(): string {
  return tsFiles(CRYPTO_SRC)
    .map((file) => stripComments(readFileSync(file, 'utf8')))
    .join('\n');
}

test('the crypto package declares only the audited libraries as dependencies', () => {
  const manifest = JSON.parse(
    readFileSync(path.join(ROOT, 'packages', 'crypto', 'package.json'), 'utf8'),
  ) as { dependencies?: Record<string, string> };
  const deps = Object.keys(manifest.dependencies ?? {}).sort();
  // Exactly the audited set — no unaudited crypto dependency slipped in, and no audited
  // library the package does not actually use (a stale entry hides a removed import).
  assert.deepEqual(deps, [...AUDITED].sort());
});

test('every external import in the crypto package resolves to an audited @noble library', () => {
  // The structural heart of ADR-0003: if a primitive came from node:crypto, a hand-rolled module,
  // or any unaudited package, its import would appear here and fail this check.
  for (const file of tsFiles(CRYPTO_SRC)) {
    for (const specifier of externalImports(file)) {
      assert.ok(
        AUDITED.has(packageOf(specifier)),
        `${path.basename(file)} imports ${specifier}, which is not an audited @noble library`,
      );
    }
  }
});

test('each named primitive is the audited implementation, not a look-alike', () => {
  const source = cryptoSource();
  // Pin the specific algorithm-to-library bindings a reviewer would verify by hand. Each regex stays
  // within one import statement (no semicolon between the symbol and its source).
  const bindings: Array<[string, RegExp]> = [
    ['XChaCha20-Poly1305 AEAD', /xchacha20poly1305[^;]*from\s*['"]@noble\/ciphers/],
    ['HKDF', /hkdf[^;]*from\s*['"]@noble\/hashes/],
    ['SHA-256', /sha256[^;]*from\s*['"]@noble\/hashes/],
    ['HMAC', /hmac[^;]*from\s*['"]@noble\/hashes/],
    ['Keccak-256 (EVM addresses)', /keccak_256[^;]*from\s*['"]@noble\/hashes/],
    ['Ed25519', /ed25519[^;]*from\s*['"]@noble\/curves/],
    ['X25519', /x25519[^;]*from\s*['"]@noble\/curves/],
    ['secp256k1 (EVM keys)', /secp256k1[^;]*from\s*['"]@noble\/curves/],
    ['ML-DSA-65 (account identity)', /ml_dsa65[^;]*from\s*['"]@noble\/post-quantum/],
    ['BIP-32/44 (EVM derivation)', /HDKey[^;]*from\s*['"]@scure\/bip32/],
    ['Argon2id (container key stretch)', /argon2id[^;]*from\s*['"]hash-wasm/],
  ];
  for (const [name, pattern] of bindings) {
    assert.match(source, pattern, `${name} is not imported from its audited @noble library`);
  }
});

test('the SDK layer imports no cryptography of its own, only the workspace packages', () => {
  // The SDK is policy over primitives: it must never import @noble directly, reach for node:crypto,
  // or pull in a crypto library — every primitive comes through @migo/crypto.
  for (const file of tsFiles(SDK_SRC)) {
    for (const specifier of externalImports(file)) {
      assert.ok(
        SDK_ALLOWED.has(packageOf(specifier)),
        `${path.basename(file)} imports ${specifier}; the SDK may only import the workspace packages`,
      );
    }
  }
});

test('the sole client entropy source is the platform CSPRNG', () => {
  // All key material traces back to one module; it must draw from globalThis.crypto.getRandomValues
  // and nothing weaker. (The refusal-to-fall-back behaviour itself is exercised in ids.test.ts.)
  const random = readFileSync(path.join(CRYPTO_SRC, 'random.ts'), 'utf8');
  assert.match(
    random,
    /globalThis\.crypto/,
    'the entropy source is not the platform crypto object',
  );
  assert.match(random, /getRandomValues/, 'entropy is not drawn from getRandomValues');
});
