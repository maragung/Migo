# ADR-0003 — X3DH + Double Ratchet for 1:1, sender-key for groups

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §8, §9, §106, §107

## Context

Private messaging is E2E by default with no user toggle. Groups need many recipients
without quadratic cost. Public/Managed rooms need server-side moderation, so they cannot be
end-to-end encrypted.

## Options

| Option                                            | Pros                                                                  | Cons                                                      |
| ------------------------------------------------- | --------------------------------------------------------------------- | --------------------------------------------------------- |
| Plain long-term key per user                      | Trivial                                                               | No forward secrecy; one key loss decrypts everything      |
| MLS                                               | Modern group crypto, good asymptotics                                 | Young ecosystem, heavy for phase 1, immature in TS/Kotlin |
| X3DH + Double Ratchet (1:1) + sender-key (groups) | Proven, forward secrecy + post-compromise security, linear group cost | Per-device sessions and key distribution to manage        |

## Decision

X3DH for asynchronous session setup (identity key, signed prekey, one-time prekeys, public
halves on the server only). Double Ratchet per 1:1 device pair. Groups use per-sender
symmetric ratchets whose keys are distributed over the 1:1 channels. Public/Managed rooms
use transport encryption and the UI says exactly that. Primitives from audited libraries
only (`*-dalek`, `chacha20poly1305`, `hkdf`, `@noble/*`); MLS revisited when its
multi-language ecosystem matures.

## Consequences

The server never holds a private key or plaintext. Losing all devices without recovery
material loses history — so recovery (recovery key, device sync, optional client-encrypted
backup) is a phase-1 requirement, not a later nicety. Both implementations must agree
byte-for-byte, enforced by shared vectors.
