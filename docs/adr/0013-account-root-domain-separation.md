# ADR-0013 — One account root, domain-separated keys, ML-DSA-65 login

- **Status:** Accepted · **Date:** 2026-09-01 · **Brief refs:** §46, §47, §138, §162, §182

## Context

A Migo account today is three unrelated credential stories the user has to
experience as one: a password the server verifies (§46), an E2EE identity key
pair generated per device with no recovery story at all (§8–11), and no wallet
domain. Lose the device and the E2EE history is gone by design — the anti-escrow
principle is correct, but it leaves the honest answer to "how do I move to a new
phone" as "you don't".

The product brief (`migo-update-1.md`, user spec) asks for one account, one root
secret, one backup, with multiple isolated cryptographic domains behind it:
ML-DSA-65 for login (post-quantum), a secp256k1 EVM wallet domain, E2EE, device
credentials, and an encrypted portable container (`.migo`) as the backup story.

Three constraints shape the decision:

1. **One key must never feed two algorithms.** Deriving an Ethereum key from an
   ML-DSA key (or vice versa) by hashing is cryptographically unsound and the
   brief forbids it outright.
2. **ML-DSA has no BIP-32.** There is no hierarchical derivation for lattice
   keys, so "derive subkeys from the identity key" is not an option; what FIPS
   204 does specify is deterministic key generation from a 32-byte seed
   (Algorithm 6).
3. **The E2EE protocol must not be rebuilt.** X3DH + Double Ratchet with
   per-device Ed25519+X25519 keys is established, reviewed, and ported to three
   languages; the brief says to preserve or improve it, not replace it.

## Options

| Option | Recovery story | Domain separation | Spec compliance |
| --- | --- | --- | --- |
| Keep per-device randomness, add server-side key escrow | server can restore | none | violates anti-escrow (§46, crypto crate docs) |
| One master keypair, derive everything from it | one secret | broken — one algorithm's key material feeds another | forbidden by the brief |
| Root secret + HKDF domain labels; each domain consumes its own seed through the standard construction of that domain | one secret, one container | enforced by construction | what the brief specifies |

## Decision

A 32-byte CSPRNG **Migo root secret**, generated on-device, never transmitted.
Every domain is HKDF-SHA-256 over the root under its own immutable label:

```text
MIGO/IDENTITY/V1 → 32-byte seed → ML-DSA-65 keygen (FIPS 204 Alg. 6)
MIGO/EVM/V1      → seed → BIP-32 master → BIP-44 m/44'/60'/0'/0/i → secp256k1 → Keccak-256 address
MIGO/E2EE/V1     → founding device's Ed25519 + X25519 seeds (X3DH/Double Ratchet unchanged)
MIGO/BACKUP/V1   → the .migo container's key schedule
MIGO/DEVICE/V1   → per-device credential seed — random per device, NOT root-derived
```

Two departures from "everything derives from the root" are deliberate:

- **Device credentials are random per device.** The root stays off additional
  devices unless the user restores it from a container; a leaked backup alone
  then cannot log in as a registered device, because `purpose=login` challenges
  require both the identity signature and the device-credential signature.
- **Additional devices' E2EE keys are fresh**, so a new device never inherits
  historical plaintext. Only the founding device's keys are root-derived, which
  is exactly the recovery story the container needs.

Login is a challenge–response over the existing REST surface: the server issues
a single-use, expiring, purpose-bound challenge in canonical MSE encoding; the
client signs those exact bytes with context `migo-auth-login-v1`; the server
verifies against the account's ACTIVE `identity_keys` row and opens an ordinary
session (ADR-0011 tokens, §46 lockout and rate limits apply unchanged). Password
login remains as a legacy path, and legacy accounts upgrade in place.

The `.migo` container is versioned (format + crypto versions in a fixed header),
keyed by Argon2id over a recovery credential through `MIGO/BACKUP/V1`, sealed
with XChaCha20-Poly1305 with the raw header as associated data. Tampering and a
wrong credential both fail authentication with the same error — the file is not
an oracle.

## Consequences

The server stores only public material (`identity_keys.public_key`,
`device.public_credential`, `wallet.address`) — the anti-escrow principle of
`migo-crypto` now extends from message keys to the account itself. Algorithm
agility is schema, not code: `algorithm` and `key_version` are columns, so a
future signature scheme is a new row, not a migration. The EVM wallet in this
release is address display and registration only; on-chain balances, RPC, and
transaction signing are explicitly out of scope and the UI must not imply
otherwise. The honest quantum story is stated once, in the product surface:
Migo *authentication* is post-quantum, the EVM wallet is not, because EOA
secp256k1 is not — and no amount of ML-DSA upstream changes that.

Reference implementation: `server/crates/migo-account` (Rust, consumed by the
server for verification and by desktop in full), ported byte-for-byte to
`packages/crypto` (TypeScript) and `clients/android/core` (Kotlin), with
cross-port test vectors pinning the derivations.
