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

| Purpose            | Rust                                    | TypeScript                       |
| ------------------ | --------------------------------------- | -------------------------------- |
| Signing            | `ed25519-dalek`                         | `@noble/curves/ed25519`          |
| Key agreement      | `x25519-dalek`                          | `@noble/curves/ed25519` (X25519) |
| AEAD               | `chacha20poly1305` (XChaCha20-Poly1305) | `@noble/ciphers/chacha`          |
| KDF                | `hkdf` + `sha2`                         | `@noble/hashes/hkdf`             |
| Passphrase hashing | `argon2` (Argon2id)                     | server-side only                 |
| CSPRNG             | `getrandom` / OS                        | `crypto.getRandomValues`         |

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

- Passphrase hashing: **Argon2id**, tuned per deployment, at least 19 MiB / t=2 / p=1,
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
  "no such account" and "wrong passphrase" cost the same wall-clock time and return the
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

| Threat                                        | Impact                                         | Detection                                                           | Mitigation                                                                                                                                          | Recovery                                                                                                     |
| --------------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Stolen phone (unlocked)                       | account + E2EE + wallet access                 | audit `LOGIN_SUCCESS` from new IP/device context                    | device revoke kills sessions, refresh, gateway auth, E2EE keys; vault sealed by Keystore/Argon2id                                                   | revoke device, rotate identity if suspicion persists                                                         |
| Root secret compromise                        | all derived domains assumed compromised        | impossible to detect cryptographically — this is stated, not hidden | root never transmitted/logged/plaintext-stored; per-device credentials mean a root alone cannot log in as an existing device                        | emergency root rotation: new root, new identity, new wallet domain, revoke all devices, fresh `.migo` backup |
| `.migo` backup theft (Google Drive breach)    | ciphertext only                                | tamper = AEAD failure                                               | Argon2id over the recovery credential resists offline guessing; Drive is transport, not trust root                                                  | recovery credential change re-encrypts; rotate if the credential is suspected too                            |
| Identity key compromise (ML-DSA)              | login impersonation                            | audit `LOGIN_FAILURE` anomalies, lockout ladder                     | two-signature login (identity + device credential); 5-minute single-use challenges                                                                  | `purpose=rotate`: new key ACTIVE, old ROTATED, sessions unaffected                                           |
| EVM wallet key compromise                     | funds on that address only                     | on-chain activity (out of scope this release)                       | wallet domain is isolated from identity domain; server stores address only                                                                          | mark wallet `COMPROMISED`, generate new wallet; identity is NOT auto-rotated                                 |
| Challenge replay                              | duplicate login                                | challenge `consumed_at`                                             | single-use rows, 5-minute expiry, purpose + device binding, identical error for reuse and expiry                                                    | none needed — reuse fails closed                                                                             |
| Server database breach                        | public keys, addresses, hashes                 | existing audit/monitoring                                           | no private material exists server-side (§182); tokens are HMAC'd, refresh stored as tags                                                            | rotate `MIGO_AUTH__TOKEN_KEY`, force re-login                                                                |
| Phished recovery credential                   | offline guessing of a stolen backup            | repeated container-open failures are local, invisible               | Argon2id parameters in header are a floor, raised per format version                                                                                | trusted-device flow re-encrypts under a new credential                                                       |
| Quantum adversary (harvest-now-decrypt-later) | future forgery of past-issued login challenges | n/a                                                                 | login signatures are ML-DSA-65 (FIPS 204); session tokens are HMAC with short TTL                                                                   | EVM/secp256k1 remains non-PQ and is labelled as such in the UI — never claimed otherwise                     |
| Malicious/compromised dApp or RPC             | misleading sign requests                       | not applicable this release (no dApp surface shipped)               | no wallet API exposure exists in this release; rule stands for the future: never expose root/seed/mnemonic, require explicit approval per signature | n/a until the surface exists                                                                                 |

Rules that the table implies and code must keep true: the root and every domain
seed are zeroized on drop and have no `Debug`/`Display`; the `.migo` open path
returns one error for wrong-credential and tampered-file alike; challenge
verification is charged against the same lockout and rate-limit scopes as
passphrase login; and every security-relevant state change emits an audit event
without secret material.

## 12. The full system threat model, grounded and pinned (brief section 162)

This section is the "model ancaman penuh beserta pengujiannya" that the brief's
section 162 tracks as its remaining SPEC item. Sections 1–11 above are the
policy; this section walks the real system — `migo-crypto`, `migo-wire`,
`migo-gateway`, the mesh between nodes, and the clients — and states, for every
security-relevant claim, where it lives in the code and what holds it true.

Every claim below is in exactly one of two states:

- **Pinned** — a named test fails if the claim stops being true. Tests are cited
  as `file::test_name`; `tests/threat_model.rs` is
  `server/crates/migo-crypto/tests/threat_model.rs`, written for this section.
- **Unverified** — no test holds the claim today. The reason is stated, and the
  claim is treated as aspiration, not fact, until a test exists.

### 12.1 Trust boundaries

| #   | Boundary                                               | What crosses it                                       | Who we do not trust beyond it          |
| --- | ------------------------------------------------------ | ----------------------------------------------------- | -------------------------------------- |
| 1   | The device                                             | Private keys, plaintext, media capture                | The server, the network, other members |
| 2   | Device ↔ node (MWP/1 over WebSocket or QUIC)           | Frames, opaque tokens                                 | Anyone on the network path             |
| 3   | Node process (`migod`: gateway + domain crates)        | Client input of every kind                            | Every client, without exception        |
| 4   | Node ↔ node (the mesh)                                 | Federated events, room traffic, presence              | The peer node, the network between     |
| 5   | Node ↔ durable state (Postgres, Redis, object storage) | Ciphertext, public keys, hashed credentials, MAC tags | Anyone who obtains a dump              |

The assets of §1 cross these boundaries as follows: private-key material exists
only inside boundary 1 (pinned for the web/desktop clients by
`packages/sdk/test/key-secrecy.test.ts`, which transmits a full conversation
flow and asserts no private seed appears in any byte of it); message plaintext
exists inside boundaries 1 and 2-in-transit only, never at 3, 4, or 5 (the
mechanism is §12.3 C1–C25); node private keys exist only inside boundary 3.

### 12.2 Adversaries, restated for this codebase

| Adversary                           | What they actually hold in this system                                                | Where the model answers them                                                                                                             |
| ----------------------------------- | ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Passive network attacker            | Captured WebSocket/QUIC bytes                                                         | TLS on QUIC; **the WebSocket path is plaintext in the shipped development stack — finding F1 below**                                     |
| Active network attacker             | Inject, reorder, duplicate, replay frames                                             | Replay refusal and state-commit-after-success in the ratchets (C4–C8); wire canonicality (W2)                                            |
| Malicious server / compromised node | The full database, every published bundle, every frame; can serve any bundle it likes | X3DH signature check (C2), whole-record test (C25), MAC domain separation (C19); the _residual_ risk is whole-identity substitution (C3) |
| Malicious group member              | The chain key, every member's public identity                                         | Per-sender signatures (C10), group-bound AAD (C11), rotation on membership change (C13); no PCS within a chain (C16)                     |
| Stolen device                       | The device's private keys and ratchet state                                           | DH-ratchet healing (C5), sender-key rotation bound (C16), remote session revocation (auth crate, §4)                                     |
| Compromised peer node               | Its own node key, captured handshakes                                                 | Mesh mutual authentication, freshness, reflection refusal (C24, M1–M5); allow-list revocation                                            |
| Curious insider / DB dump           | Boundary-5 contents in full                                                           | Nothing at boundary 5 is a working credential: passphrases are Argon2id (C20), tokens are HMAC tags (C19), messages are ciphertext (C25) |

### 12.3 Claims and the tests that pin them

#### The crypto core (`server/crates/migo-crypto`)

| #   | Claim                                                                                                                                                                      | Pinned by                                                                                                                                                                                                                                         |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | A substituted or forged prekey is refused before any Diffie-Hellman runs                                                                                                   | `x3dh.rs::a_bundle_with_a_forged_prekey_is_refused_before_any_dh`, `identity.rs::a_prekey_from_another_identity_is_refused`                                                                                                                       |
| C2  | A prekey signature does not transfer to another key or key id                                                                                                              | `identity.rs::a_prekey_signature_does_not_transfer_to_another_key{,_id}`                                                                                                                                                                          |
| C3  | A _wholly_ substituted bundle (identity included) is **not** detectable by X3DH; the safety number is the detection                                                        | `tests/threat_model.rs::a_wholly_substituted_bundle_is_cryptographically_valid` (pins the boundary); `identity.rs::fingerprints_differ_between_identities_and_are_stable`; the human comparison step is **unverified** — no test can pin a person |
| C4  | 1:1 forward secrecy: used keys are deleted; a replayed frame re-delivers nothing                                                                                           | `ratchet.rs::a_replayed_message_is_refused`, `a_replayed_out_of_order_message_is_refused_too`                                                                                                                                                     |
| C5  | Post-compromise security: the root key changes on every DH turn                                                                                                            | `ratchet.rs::the_root_key_changes_on_every_ratchet_turn`                                                                                                                                                                                          |
| C6  | An injected frame with a forged ratchet key cannot destroy a working session                                                                                               | `ratchet.rs::a_forged_ratchet_key_cannot_destroy_the_session`, `a_tampered_ciphertext_is_refused_and_leaves_the_session_usable`                                                                                                                   |
| C7  | A message claiming an absurd position is refused without deriving keys (CPU bound)                                                                                         | `ratchet.rs::an_absurd_message_number_is_refused_without_deriving_keys`, `sender_key.rs::an_absurd_message_number_is_refused`                                                                                                                     |
| C8  | Skipped-key retention is bounded (memory bound)                                                                                                                            | `ratchet.rs::skipped_keys_are_bounded`, `sender_key.rs::skipped_keys_are_bounded`                                                                                                                                                                 |
| C9  | One group ciphertext serves every member; the server sees it exactly once                                                                                                  | `sender_key.rs::one_ciphertext_serves_every_member`                                                                                                                                                                                               |
| C10 | A member who holds the chain key cannot forge a message as another sender                                                                                                  | `sender_key.rs::another_member_cannot_forge_a_message_as_this_sender`                                                                                                                                                                             |
| C11 | A ciphertext cannot be replayed into another group                                                                                                                         | `sender_key.rs::a_message_cannot_be_replayed_into_another_group`                                                                                                                                                                                  |
| C12 | A member who joins mid-conversation cannot read history                                                                                                                    | `sender_key.rs::a_new_member_cannot_read_history`                                                                                                                                                                                                 |
| C13 | A member who left cannot read after the rekey                                                                                                                              | `sender_key.rs::a_member_who_left_cannot_read_after_the_rekey`, `tests/threat_model.rs::a_stolen_group_chain_key_reads_forward_only_until_the_rotation`                                                                                           |
| C14 | The group epoch rises monotonically and saturates instead of rolling over                                                                                                  | `sender_key.rs::the_epoch_saturates_instead_of_rolling_over`, `rotation_raises_the_epoch_and_mints_a_fresh_chain`                                                                                                                                 |
| C15 | A receiver refuses a sender-key distribution whose epoch does not advance                                                                                                  | **Unverified — finding F2 below.** `ReceiverKeyState::accept` carries no epoch at all; the refusal belongs to the runtime distribution wiring that brief section 163 still marks SPEC                                                             |
| C16 | Sender keys have forward secrecy but **no** post-compromise security within a chain; the window is bounded by rotation (2000 messages) and closed by any membership change | `tests/threat_model.rs::a_stolen_group_chain_key_reads_forward_only_until_the_rotation` (pins both halves, including the thief reading forward); `sender_key.rs::a_chain_refuses_to_run_past_its_rotation_bound`                                  |
| C17 | A call's media key is derived from the session secret, bound to its call id, and never equals a message key                                                                | `call_key.rs::both_sides_derive_the_same_key_from_one_session`, `a_call_key_is_bound_to_its_call`, `a_call_key_is_not_a_message_key`, plus the independent RFC 5869 pin `the_derivation_is_pinned_to_an_independent_vector`                       |
| C18 | A call-key update must open under the current key and advance the epoch; replays and rollbacks are refused and leave the working key intact                                | `call_key.rs::an_update_that_does_not_advance_the_epoch_is_refused`, `a_tampered_update_is_refused`, `an_update_bound_to_a_different_epoch_is_refused`                                                                                            |
| C19 | MAC keys are domain-separated per purpose, compared in constant time, and refuse tags below the 16-byte floor                                                              | `mac.rs::a_different_purpose_is_a_different_key`, `an_edited_tag_is_refused` (all 256 first-byte values), `a_tag_shorter_than_the_floor_is_refused`                                                                                               |
| C20 | Passphrases are Argon2id at the OWASP baseline with per-hash salts; absurd inputs are refused before hashing                                                               | `passphrase.rs::the_encoded_hash_names_argon2id_and_its_parameters`, `an_absurdly_long_passphrase_is_refused_before_hashing`                                                                                                                      |
| C21 | Decryption failures are uniform — wrong key, tampered tag, and wrong context are the same error, so there is no oracle                                                     | `tests/threat_model.rs::aead_failures_are_indistinguishable`; the unit tests of `aead.rs` assert the same variant per cause                                                                                                                       |
| C22 | Small-order and invalid points are refused at parse, not at first use                                                                                                      | `identity.rs::small_order_public_keys_are_refused`, `a_small_order_key_cannot_be_published_as_an_identity`                                                                                                                                        |
| C23 | Signatures never transfer between the mesh, prekey, and group-message domains, even when one key serves all three                                                          | `tests/threat_model.rs::signatures_never_transfer_between_the_mesh_prekey_and_group_domains`                                                                                                                                                      |
| C24 | The mesh handshake refuses reflection, cross-peer replay, stale and future timestamps, and version mismatch                                                                | `node.rs` tests (all of them), `migo-federation/tests/federation.rs::a_replayed_nonce_is_refused_even_within_the_clock_window`, `a_proof_whose_timestamp_is_outside_the_skew_window_is_refused`                                                   |
| C25 | The server's complete record of a conversation — both bundles, the initial message, every frame — decrypts nothing, even replayed against an attacker's own private keys   | `tests/threat_model.rs::everything_the_server_records_of_a_conversation_decrypts_nothing`                                                                                                                                                         |

Key publication is the server-side half of the same boundary:
`migo-keys` refuses an unverified prekey signature and an already-expired
signed prekey at publication (`INVALID_KEY_MATERIAL`), hands out a one-time
prekey at most once, and never serves a revoked device — all four rules in
`server/crates/migo-keys/src/service.rs`, with the signature check delegated to
`migo_crypto` so there is one definition of what a prekey signature covers.

#### The wire protocol (`server/crates/migo-wire`)

| #   | Claim                                                                                                                                        | Pinned by                                                                                                                                                               |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | A length from the network is checked against `MAX_FRAME_BYTES` before any allocation                                                         | `frame.rs` (three checks on encode/decode paths), `migo-gateway/tests/gateway.rs::an_oversize_frame_is_refused_before_any_allocation`                                   |
| W2  | Encodings are canonical: non-minimal varints rejected, trailing bytes rejected, unknown optional fields skipped, reserved flag bits rejected | `varint.rs`, `lib.rs::trailing_bytes_are_rejected`, `a_newer_peers_unknown_field_is_skipped`; cross-language by `tests/vectors.rs` over `shared/protocol/vectors/wire/` |
| W3  | No truncated or corrupted input can panic the decoder                                                                                        | `lib.rs::a_truncated_payload_never_panics`, `a_corrupted_byte_never_panics`                                                                                             |
| W4  | A decompression bomb is refused against the frame budget                                                                                     | `compress.rs::a_decompression_bomb_is_refused`                                                                                                                          |

#### The gateway (`server/crates/migo-gateway`)

| #   | Claim                                                                                                           | Pinned by                                                                                                                                                                                                                                               |
| --- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| G1  | An oversize frame is refused before allocation and the session survives                                         | `gateway.rs::an_oversize_frame_is_refused_before_any_allocation`                                                                                                                                                                                        |
| G2  | A server-only opcode from a client socket is refused and the session is closed as a protocol violation          | `gateway.rs::a_server_to_client_opcode_from_a_client_closes_the_session`, `the_wire_is_push_only_and_has_no_request_or_response_opcode`                                                                                                                 |
| G3  | Application opcodes never run before authentication                                                             | `gateway.rs::the_first_frame_must_be_a_hello_or_the_connection_is_refused` and the phase-gate tests; the mechanism is the `AuthLevel` phase gate in `connection.rs`                                                                                     |
| G4  | `SUBSCRIBE` authorization is read from the domain, never from the frame; the null dispatcher grants nothing     | `gateway.rs::a_subscribe_keeps_only_the_topics_that_belong_to_the_caller`, `a_subscribe_on_a_null_dispatcher_grants_nothing`                                                                                                                            |
| G5  | Error frames carry only the public face of a fault; internal text and ids never reach the client or the metrics | `gateway.rs::error_frames_carry_only_their_public_face`                                                                                                                                                                                                 |
| G6  | Backpressure drops droppable and coalescable frames but never Critical ones                                     | `gateway.rs::backpressure_drops_droppable_and_coalescable_but_never_critical`                                                                                                                                                                           |
| G7  | The exact ordering of the pre-dispatch checks (version, flags, length, opcode, phase, auth, rate, decode)       | **Unverified as an ordering.** Each check is individually pinned (W1, G2, G3); no test asserts the cheap rejections happen before the payload decode. The ordering is a cost property, and a refactor that decodes first would pass every existing test |
| G8  | No plaintext transport, including in development                                                                | **Contradicted — finding F1 below**                                                                                                                                                                                                                     |

#### Federation, the mesh between nodes (`server/crates/migo-federation`)

| #   | Claim                                                                                                               | Pinned by                                                                                                                                                                   |
| --- | ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M1  | A peer is looked up in the allow-list before its proof is examined; an unnamed node never federates                 | `federation.rs::a_handshake_from_a_node_not_in_the_allow_list_is_refused`; revocation is the same path (delete the row)                                                     |
| M2  | Every handshake refusal is one opaque error, so a prober learns nothing; only metrics tell the reasons apart        | `federation.rs::every_handshake_refusal_is_the_same_opaque_error`, `the_metrics_tell_the_four_refusal_reasons_apart`                                                        |
| M3  | A nonce window shorter than twice the accepted clock skew is refused at construction                                | `federation.rs::a_nonce_window_one_below_twice_the_skew_is_refused`                                                                                                         |
| M4  | Link sequences are monotonic; replays, gaps, and sequence zero are refused                                          | `federation.rs::a_non_advancing_sequence_is_a_replay_and_does_not_move_the_link`, `sequence_zero_is_never_accepted`, `a_gap_resets_the_link_so_the_next_packet_starts_over` |
| M5  | A node's events are authenticated by its node key; stealing that key means impersonating that node and nothing else | `node.rs::a_proof_for_one_peer_does_not_work_for_another`, `federation.rs::a_peer_cannot_authenticate_as_another_peer_whose_key_it_lacks`                                   |

#### The clients

| #   | Claim                                                                                                             | Pinned by                                                                                                                                                                                 |
| --- | ----------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| L1  | No private key, ratchet secret, or chain key ever leaves the device in a frame                                    | `packages/sdk/test/key-secrecy.test.ts` (drives the real session and group crypto, captures every transmitted byte, and includes a positive control proving the search works)             |
| L2  | The TypeScript and Rust implementations agree byte for byte                                                       | `packages/crypto/test/vectors.test.ts` and `server/crates/migo-crypto/tests/vectors.rs` over the same `shared/protocol/vectors/crypto/` files, generated by an independent implementation |
| L3  | The desktop mirrors the same session and group flows                                                              | `clients/desktop/src/crypto/{session,group}.rs` in-module test suites                                                                                                                     |
| L4  | Platform key storage — Android Keystore, WebCrypto non-extractable keys, never `localStorage`/`SharedPreferences` | **Unverified.** No test in this repository pins where a platform client stores key material; §3's table is policy. This is the largest untested surface in the model                      |

### 12.4 What Migo explicitly does not protect against

Stated plainly, because a guarantee overstated is worse than one not made
(brief §8):

1. **Metadata.** Who talks to whom, when, roughly how large. The server routes
   messages and must see this; the brief's §10 list is the honest inventory.
   No unlinkability, no cover traffic, no defence against a global passive
   adversary performing traffic analysis.
2. **Public and Managed room content.** Transport-protected, not end-to-end;
   the server reads them for moderation. The UI must say "encrypted transport",
   never "end-to-end".
3. **Whole-identity substitution, absent out-of-band verification.** A server
   can always present a consistent, fully attacker-owned bundle (C3). X3DH
   cannot refuse it; the safety number compared in person is the entire
   defence, and it is a human step.
4. **A compromised member device reading a group forward** until the next
   membership change or the 2000-message rotation bound (C16).
5. **A compromised client OS or malicious platform keystore** (§2).
6. **Harvest-now-decrypt-later against the message crypto.** X25519,
   XChaCha20-Poly1305, and Ed25519 are classical; a future quantum adversary
   decrypts today's captured ciphertexts. Only login is post-quantum (ML-DSA,
   §11), and the EVM/secp256k1 wallet domain is non-PQ and labelled so.
7. **The server's power to refuse service.** E2EE is confidentiality and
   integrity, not availability: the server can drop, delay, or reorder anything,
   and the client's recourse is the reconnect/resume machinery, not cryptography.
8. **Denial of service at the network layer.** The layered limits of §7 make
   attacks expensive, not impossible.

### 12.5 Findings — where the code and the brief disagree

These were found while grounding this section, are reported rather than papered
over, and should each close as either a code change or a brief amendment:

- **F1 — plaintext WebSocket transport.** Brief section 162 requires "no plaintext
  transport, including in development". The gateway's WebSocket listener binds
  a plain TCP socket (`migod/src/serve.rs`; the `Transport` trait carries no
  TLS), the development stack advertises `ws://localhost:8080/ws`
  (`infra/compose/docker-compose.yml`), and `Environment::Staging` is documented
  as "Production checks apply, **minus TLS**" (`migo-core/src/config.rs`). Only
  the QUIC listener terminates TLS (rustls). As written, the claim holds for no
  WebSocket deployment in this repository. Until TLS (in-process or a mandated
  terminating proxy) exists on the WS path, G8 above stays unverified.
- **F2 — receiver-side epoch enforcement is not built.** Brief section 163 says every
  membership change raises the group epoch _and_ distributes a fresh chain, and
  the epoch "tidak pernah mundur" (never regresses). The sender half is built
  and pinned (C14); the receiver half is not: `ReceiverKeyState::accept` takes
  any distribution, has no epoch field to compare against, and nothing above
  the crypto layer rejects a stale one. Consequence today: a malicious group
  member can hand a receiver an old distribution and strand it on a dead chain
  — a message-delivery denial of service until re-sync, not a confidentiality
  loss (the old chain cannot read the new one). The brief already marks the
  runtime distribution wiring SPEC, so this is an unbuilt requirement, not a
  quiet divergence; C15 stays unverified until it lands.
- **F3 — "recorded as an incident" is a counter, not a record.** Brief section 162
  says a Server-auth-level frame from a client socket "WAJIB ditolak dan
  dicatat sebagai insiden" (must be refused and recorded as an incident). The
  gateway closes the session with reason `protocol_violation` and counts it in
  the metrics registry; there is no durable incident record beyond that
  counter. If an operator needs an alertable incident trail, that is missing.
- **F4 — the mid-call joiner's first key is caller wiring.** Brief section 163's
  "sealed for them at join" for a participant joining a group call is a
  convention documented in `call_key.rs`, not code: `migo-crypto` provides the
  sealed-rotation mechanism (C18) but nothing distributes the epoch-0 key to a
  joiner. Same status as F2 — SPEC, and marked unverified here.

### 12.6 Verification index

Pinned claims live in five places, all run by CI:

| Suite                          | File                                                                                 | Holds                                               |
| ------------------------------ | ------------------------------------------------------------------------------------ | --------------------------------------------------- |
| Threat-model composition tests | `server/crates/migo-crypto/tests/threat_model.rs`                                    | C3, C13, C16, C21, C23, C25 (new with this section) |
| Crypto unit tests              | `server/crates/migo-crypto/src/*.rs`, `#[cfg(test)]`                                 | C1, C2, C4–C14, C17–C20, C22, C24                   |
| Cross-language vectors         | `server/crates/migo-crypto/tests/vectors.rs`, `packages/crypto/test/vectors.test.ts` | L2, and the byte-level construction of KDF/AEAD/MAC |
| Gateway integration tests      | `server/crates/migo-gateway/tests/gateway.rs`                                        | G1–G6, W1                                           |
| Federation integration tests   | `server/crates/migo-federation/tests/federation.rs`                                  | C24 (runtime half), M1–M5                           |

Currently unverified: C15 (receiver epoch), G7 (check ordering), G8 (WS TLS,
contradicted by F1), L4 (platform key storage). Everything else in this
section names its test.

Report a vulnerability: see [`../SECURITY.md`](../SECURITY.md).
