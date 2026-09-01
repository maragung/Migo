# 03 — Security & threat model

Security is an architectural property here, not a feature backlog item (brief §45).

## 1. Assets, ranked

1. Private message plaintext and the keys that protect it.
2. Account credentials, session tokens, passkeys, recovery material.
3. Node private keys (mesh identity) — compromise means impersonating a region.
4. Economy ledger integrity.
5. Moderation records and audit logs (non-repudiation).
6. Metadata: who talks to whom, when, from where.

## 2. Adversaries we design against

| Adversary                | Capability                           | Primary mitigation                                                                       |
| ------------------------ | ------------------------------------ | ---------------------------------------------------------------------------------------- |
| Network attacker         | Observe/modify traffic               | TLS 1.3 everywhere, E2E for private content, no plaintext transport                      |
| Malicious user           | Full control of a client             | Server-authoritative everything; client input is untrusted                               |
| Malicious room member    | Legitimate access to a room          | Granular permissions, rate limits, audit, moderation tooling                             |
| Malicious bot developer  | Runs code, holds a token             | Minimum-permission default, no DB access, sandbox, per-bot quotas                        |
| Compromised single node  | Reads its own DB, holds its node key | Private content is E2E; node keys are revocable; mesh allow-list                         |
| Curious insider / admin  | Database and log access              | E2E means no plaintext exists to read; admin actions are audit-logged                    |
| Stolen device            | Local storage access                 | Keys in platform keystore, app lock, remote session revocation                           |
| Automated abuse at scale | Many accounts, high rate             | Cost-based distributed rate limits, trust scoring, phone/email friction on abuse signals |

Explicitly **out of scope**: a compromised client OS, a malicious platform keystore, and
global-passive-adversary traffic analysis. We reduce metadata but do not claim
unlinkability.

## 3. End-to-end encryption

Automatic and non-optional for private communication (brief §8). No user-facing toggle,
because a security control that must be enabled is a security control that is not used.

| Surface               | Protection                                                                        | Rationale                                                                                                                                 |
| --------------------- | --------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| 1:1 chat              | X3DH key agreement + Double Ratchet, per-message keys                             | Forward secrecy and post-compromise security                                                                                              |
| Group chat            | Sender-key (per-sender symmetric ratchet), keys distributed over the 1:1 channels | Linear cost in members, not quadratic per message                                                                                         |
| Public / Managed Room | Transport encryption only                                                         | Server-side moderation, search and history are product requirements. The UI says **"Encrypted transport"**, never "end-to-end" (brief §8) |

The UI must state the actual guarantee, plainly. Overstating it is worse than not having it.

### Primitives — audited libraries only

| Purpose          | Rust                                    | TypeScript                       |
| ---------------- | --------------------------------------- | -------------------------------- |
| Signing          | `ed25519-dalek`                         | `@noble/curves/ed25519`          |
| Key agreement    | `x25519-dalek`                          | `@noble/curves/ed25519` (X25519) |
| AEAD             | `chacha20poly1305` (XChaCha20-Poly1305) | `@noble/ciphers/chacha`          |
| KDF              | `hkdf` + `sha2`                         | `@noble/hashes/hkdf`             |
| Password hashing | `argon2` (Argon2id)                     | server-side only                 |
| CSPRNG           | `getrandom` / OS                        | `crypto.getRandomValues`         |

Rules, without exception:

- **No hand-rolled primitives.** We compose audited constructions; we do not invent them.
- Nonces are never reused: XChaCha20's 192-bit random nonce plus a per-chain counter.
- Every ciphertext is authenticated; unauthenticated decryption is not exposed by our API.
- Both implementations are validated against the same JSON test vectors
  (`shared/protocol/vectors/crypto/`). A change that breaks cross-language agreement
  fails CI.

### Key storage

| Platform | Location                                                                                                                                                     |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Android  | Android Keystore (hardware-backed when available); local DB encrypted; never `SharedPreferences` (brief §109)                                                |
| Web      | WebCrypto **non-extractable** keys where possible; wrapped key material in IndexedDB under a KDF-derived wrapping key; **never** `localStorage` (brief §108) |
| iOS      | Keychain / Secure Enclave                                                                                                                                    |

### Backup and recovery (brief §106–107)

Users lose devices; E2E makes that unforgiving. Options offered, in this order:
recovery key (user-held, shown once, verified by re-entry), multi-device key sync over
the existing E2E channel, and optional encrypted cloud backup where the _client_
encrypts first with a key derived from a strong recovery passphrase. The server stores an
opaque blob. The consequence of losing recovery material is stated before the user opts in.

## 4. Authentication

- Password hashing: **Argon2id**, tuned per deployment, at least 19 MiB / t=2 / p=1,
  re-tuned as hardware improves and re-hashed transparently on login.
- Access token: short-lived (15 min default), 130 bytes of fixed layout tagged with
  HMAC-SHA-256, carrying account, device, session, capabilities, region, issued-at,
  expires-at and authenticated-at. **No algorithm field** and no JSON parser on the
  pre-authentication path (ADR-0011). Verification is one MAC and a length check, with
  no store read, so refusing an unauthenticated flood never touches the database.
- Refresh token: 32 opaque random bytes, **single-use, rotating**. Only a _keyed_ tag of
  it is persisted, so a database dump yields no working credential and cannot even
  confirm a candidate token offline. Reuse of a rotated refresh token is treated as
  theft: the whole session family is revoked, and that check is ranked above device
  mismatch and expiry so the theft signal is never spent on a more specific error. This
  turns token exfiltration from silent persistence into a detected incident.
- Revocation is honest about its bound: a signed token is valid until it expires, so a
  revocation takes effect on the next request that reads the session row and exposure is
  capped at `auth.access_ttl_seconds`. Callers that cannot accept that use the checked
  path.
- Device binding: tokens carry a device id and are refused when presented from another
  device; a refresh from the wrong device is treated as theft, not as a mismatch.
- Presence: each session stores its own `authenticated_at`, carried _forward_ across
  refreshes rather than reset, because a refresh is not proof a human is present. This
  is what `REAUTHENTICATION_REQUIRED` (1108) is decided from (brief §125).
- Account enumeration: an unknown identifier is verified against a placeholder hash, so
  "no such account" and "wrong password" cost the same wall-clock time and return the
  same code.
- There is deliberately **no per-account failure lockout.** Pricing is per network class,
  because a per-account counter lets a stranger who knows a username lock its owner out.
  A failed attempt is charged from the resolved anonymous bucket rather than a hardcoded
  number, so the charge is always actually collectable (ADR-0006).
- Passkeys / WebAuthn are first-class, and 2FA + single-use recovery codes are supported
  (brief §46).
- Device sessions are listable and revocable, with network class and last-active
  (brief §47). Listings mark the caller's own session; signing out of a session that
  belongs to somebody else reads as `NOT_FOUND`, never as "not yours".

## 5. Authorization

Permissions are granular capability strings (`CHAT_SEND`, `USER_BAN`, `ROOM_MANAGE`, …,
brief §48) resolved server-side per request from `(actor, scope, target)`. Non-negotiable:

- **Every** mutating handler calls the permission check. There is no "internal" path
  that skips it.
- The client's view of its permissions is a UI hint only (brief §119).
- Object references are authorised, never merely well-formed — the IDOR class of bug is
  a permission bug, and the integration tests assert it per endpoint.
- Sensitive actions (ownership transfer, mass deletion, account deletion, economy
  adjustments) require **re-authentication** and are audit-logged (brief §85).

## 6. Input handling

Untrusted by default: protocol frames, REST bodies, uploads, bot commands, deep links,
usernames, room names, and every string that will ever be rendered.

- Decode with limits (see [02-protocol.md](02-protocol.md) §4) before allocating.
- Validate structurally _and_ semantically; reject, do not coerce.
- Normalise Unicode (NFKC) for identifiers and run a confusable check —
  impersonation via homoglyph usernames is a real, common attack (brief §80).
- Uploads: never trust `Content-Type`. Sniff, validate extension against sniffed type,
  enforce per-type size limits, strip metadata, re-encode images server-side,
  serve from a separate origin with `Content-Disposition` and a restrictive CSP
  (brief §122).
- Web client: strict CSP, Trusted Types where supported, no `dangerouslySetInnerHTML`
  on user content, no `eval`.

## 7. Availability

Layered defence (brief §121): CDN/edge → connection admission → per-IP and per-user
cost buckets → per-opcode cost → per-room limits → bounded queues → circuit breakers.
Reject abnormal payloads as early and as cheaply as possible; the goal is that the most
expensive thing an attacker can make us do is close their socket.

## 8. Secrets

No secret in Git, ever (brief §103). Development uses `.env` (git-ignored); production
uses a secret manager with environment injection and rotation. `migod` refuses to start
in `production` with a development-default or empty secret — a loud failure at deploy
time instead of a quiet vulnerability forever.

## 9. Logging and privacy

- Structured logs with a redaction layer: tokens, keys, ciphertext, and message bodies
  are never logged, at any level (brief §117).
- Metadata retention is minimised: IPs are truncated and short-TTL, precise location is
  never collected, message metadata is kept only as long as routing requires (brief §78).
- Crash reports and analytics carry no message content (brief §116).

## 10. Verification

Security testing is not a phase, it is part of CI ([10-testing-strategy.md](10-testing-strategy.md)):
fuzzers on the codec and crypto envelope, property tests on permissions, negative
integration tests per endpoint (unauth / wrong-user / wrong-role / rate-limited /
replayed / oversized / malformed), and `cargo audit` + `pnpm audit` on every build.

Report a vulnerability: see [`../SECURITY.md`](../SECURITY.md).

## 11. Account root, ML-DSA identity, and the EVM wallet domain (brief section 182, ADR-0013)

The account root raises the value of a single secret: it controls login identity,
the founding device's E2EE keys, and the EVM wallet domain. Each row follows the
attack / impact / detection / mitigation / recovery shape required by the brief.

| Threat | Impact | Detection | Mitigation | Recovery |
| --- | --- | --- | --- | --- |
| Stolen phone (unlocked) | account + E2EE + wallet access | audit `LOGIN_SUCCESS` from new IP/device context | device revoke kills sessions, refresh, gateway auth, E2EE keys; vault sealed by Keystore/Argon2id | revoke device, rotate identity if suspicion persists |
| Root secret compromise | all derived domains assumed compromised | impossible to detect cryptographically — this is stated, not hidden | root never transmitted/logged/plaintext-stored; per-device credentials mean a root alone cannot log in as an existing device | emergency root rotation: new root, new identity, new wallet domain, revoke all devices, fresh `.migo` backup |
| `.migo` backup theft (Google Drive breach) | ciphertext only | tamper = AEAD failure | Argon2id over the recovery credential resists offline guessing; Drive is transport, not trust root | recovery credential change re-encrypts; rotate if the credential is suspected too |
| Identity key compromise (ML-DSA) | login impersonation | audit `LOGIN_FAILURE` anomalies, lockout ladder | two-signature login (identity + device credential); 5-minute single-use challenges | `purpose=rotate`: new key ACTIVE, old ROTATED, sessions unaffected |
| EVM wallet key compromise | funds on that address only | on-chain activity (out of scope this release) | wallet domain is isolated from identity domain; server stores address only | mark wallet `COMPROMISED`, generate new wallet; identity is NOT auto-rotated |
| Challenge replay | duplicate login | challenge `consumed_at` | single-use rows, 5-minute expiry, purpose + device binding, identical error for reuse and expiry | none needed — reuse fails closed |
| Server database breach | public keys, addresses, hashes | existing audit/monitoring | no private material exists server-side (§182); tokens are HMAC'd, refresh stored as tags | rotate `MIGO_AUTH__TOKEN_KEY`, force re-login |
| Phished recovery credential | offline guessing of a stolen backup | repeated container-open failures are local, invisible | Argon2id parameters in header are a floor, raised per format version | trusted-device flow re-encrypts under a new credential |
| Quantum adversary (harvest-now-decrypt-later) | future forgery of past-issued login challenges | n/a | login signatures are ML-DSA-65 (FIPS 204); session tokens are HMAC with short TTL | EVM/secp256k1 remains non-PQ and is labelled as such in the UI — never claimed otherwise |
| Malicious/compromised dApp or RPC | misleading sign requests | not applicable this release (no dApp surface shipped) | no wallet API exposure exists in this release; rule stands for the future: never expose root/seed/mnemonic, require explicit approval per signature | n/a until the surface exists |

Rules that the table implies and code must keep true: the root and every domain
seed are zeroized on drop and have no `Debug`/`Display`; the `.migo` open path
returns one error for wrong-credential and tampered-file alike; challenge
verification is charged against the same lockout and rate-limit scopes as
password login; and every security-relevant state change emits an audit event
without secret material.

