//! One Double Ratchet per remote device, and the X3DH policy that starts them.
//!
//! A conversation is not a session. Every *device* a peer signs in on has its own long-term identity
//! and its own ratchet, so a two-person chat where each side has a laptop and a phone is four
//! ratchets, and a message is sealed once per recipient device. That is what makes a compromised
//! phone unable to read what the laptop received, and it is why this store is keyed by device id
//! rather than by account or conversation.
//!
//! # When X3DH runs
//!
//! Once per session, on the first send to a device we have never spoken to. The resulting preamble
//! then rides on *every* message until that device replies, because until a reply arrives we have no
//! evidence it ever received the first one — and a peer that missed the preamble cannot derive the
//! session, so a message without it would be permanently undecryptable rather than merely late.
//!
//! # What the tag authenticates
//!
//! The ratchet binds both device identities (through X3DH's associated data, `IK_initiator ||
//! IK_responder`) and the ratchet header into every message's AEAD associated data. The identity
//! binding is the anti-unknown-key-share protection: the server cannot swap the sender's identity or
//! replay a session key into a conversation with a third party without the tag failing.
//!
//! Section 11 also lists `conversation_id` and `message_id` among the metadata it would bind. The
//! ratchet in `migo-crypto` does not thread per-message associated data, and adding it here alone
//! would desync from a web or Android peer and make genuine messages undecryptable. Binding that
//! extra context is a coordinated change across all four implementations, so it is deferred here in
//! the same terms as in the TypeScript SDK rather than done half-way on one client.

use std::collections::HashMap;

use migo_core::{Id, OsRandom};
use migo_crypto::identity::{KeyPair, SignedPrekey, PUBLIC_KEY_LEN};
use migo_crypto::x3dh::{initiate, respond, InitialMessage, PrekeyBundle};
use migo_crypto::{IdentityPublic, IdentitySecret, RatchetSession};

use super::envelope::{Envelope, Preamble};
use super::CryptoError;

/// How many one-time prekeys this device publishes at a time.
///
/// Each one gives one incoming session forward secrecy against a later compromise of the signed
/// prekey. A hundred is enough that a device offline for a week still has unused ones when it
/// returns, and small enough that the published bundle stays a few kilobytes.
pub const ONE_TIME_PREKEY_COUNT: u32 = 100;

/// This device's own key material.
///
/// Generated on the device and never sent anywhere: `KEY_PUBLISH` publishes the *public* halves and
/// the signature over the signed prekey, and nothing else ever leaves (brief section 10).
pub struct DeviceKeys {
    /// The long-term identity: an Ed25519 signing key and an X25519 exchange key.
    pub identity: IdentitySecret,
    /// The id the signed prekey is published under.
    pub signed_prekey_id: u32,
    /// The signed prekey's private half.
    pub signed_prekey: KeyPair,
    /// The unused one-time prekeys, by id. An entry is removed the first time a session consumes it.
    pub one_time: HashMap<u32, KeyPair>,
    /// The saved sign-in, when the vault held one. Present here rather than in a separate file because
    /// it is sealed under the same passphrase and is useless without the keys beside it.
    pub session: Option<crate::vault::SavedSession>,
}

impl DeviceKeys {
    /// Generates a complete, fresh set: identity, one signed prekey, and [`ONE_TIME_PREKEY_COUNT`]
    /// one-time prekeys.
    pub fn generate() -> Self {
        let mut random = OsRandom;
        let identity = IdentitySecret::generate(&mut random);
        let signed_prekey = KeyPair::generate(&mut random);
        let one_time = (1..=ONE_TIME_PREKEY_COUNT)
            .map(|id| (id, KeyPair::generate(&mut random)))
            .collect();
        Self {
            identity,
            signed_prekey_id: 1,
            signed_prekey,
            one_time,
            session: None,
        }
    }

    /// This device's public identity, for the safety number and for `KEY_PUBLISH`.
    pub fn identity_public(&self) -> IdentityPublic {
        self.identity.public()
    }

    /// The signed prekey with the signature that binds it to this identity.
    pub fn signed_prekey_signed(&self) -> SignedPrekey {
        SignedPrekey::create(&self.identity, self.signed_prekey_id, &self.signed_prekey)
    }

    /// The public halves of the unused one-time prekeys, sorted by id so a republish is stable.
    pub fn one_time_public(&self) -> Vec<(u32, [u8; PUBLIC_KEY_LEN])> {
        let mut out: Vec<(u32, [u8; PUBLIC_KEY_LEN])> = self
            .one_time
            .iter()
            .map(|(id, pair)| (*id, pair.public()))
            .collect();
        out.sort_unstable_by_key(|(id, _)| *id);
        out
    }
}

/// One live session with one remote device.
struct Entry {
    session: RatchetSession,
    /// The preamble we send on every message until this device replies. `None` once it has, and for
    /// sessions we answered rather than started.
    outgoing_preamble: Option<Preamble>,
    /// The preamble that created this session when we were the responder.
    ///
    /// Kept so a *re-sent* first message — the sender repeats it until we reply, and our reply may
    /// still be in flight — is recognised as belonging to the session we already built rather than
    /// silently replacing it, which would throw away every key derived since.
    origin: Option<Preamble>,
}

/// Every session this device holds, keyed by remote device id.
pub struct SessionStore {
    keys: DeviceKeys,
    sessions: HashMap<Id, Entry>,
}

impl SessionStore {
    #[must_use]
    pub fn new(keys: DeviceKeys) -> Self {
        Self {
            keys,
            sessions: HashMap::new(),
        }
    }

    /// This device's own keys, for publishing and for the safety number.
    #[must_use]
    pub fn keys(&self) -> &DeviceKeys {
        &self.keys
    }

    /// Forgets every session.
    ///
    /// Called on sign-out. The ratchet state is the only thing that can decrypt already-received
    /// messages, so dropping it is what makes a sign-out mean something on this device.
    pub fn clear(&mut self) {
        self.sessions.clear();
    }

    /// Seals `plaintext` for one device, starting a session from `bundle` if there is none.
    ///
    /// `bundle` is required only for the first message; pass `None` once a session exists. A caller
    /// that has no bundle and no session gets [`CryptoError::NoBundle`] rather than a silent
    /// plaintext send.
    pub fn seal(
        &mut self,
        device: Id,
        bundle: Option<&PrekeyBundle>,
        plaintext: &[u8],
    ) -> Result<Envelope, CryptoError> {
        let mut random = OsRandom;

        if !self.sessions.contains_key(&device) {
            let bundle = bundle.ok_or(CryptoError::NoBundle)?;
            // `initiate` verifies the bundle's signed prekey against the claimed identity before it
            // does any Diffie-Hellman. That check is what makes the server untrusted: it chooses
            // which bundle to serve, and a substituted prekey fails here, on this device, before a
            // single byte of the message is composed.
            let (seed, initial, _ephemeral) = initiate(&self.keys.identity, bundle, &mut random)?;
            let session =
                RatchetSession::initiator(&seed, bundle.signed_prekey.public_key, &mut random)?;
            self.sessions.insert(
                device,
                Entry {
                    session,
                    outgoing_preamble: Some(preamble_of(&initial)),
                    origin: None,
                },
            );
        }

        let entry = self.sessions.get_mut(&device).expect("inserted above");
        let (header, ciphertext) = entry.session.encrypt_next(plaintext, &mut random)?;

        // The peer has replied, so it has the session; the preamble has done its job and every
        // further message saves the ~110 bytes it costs.
        if entry.session.received_count() > 0 {
            entry.outgoing_preamble = None;
        }

        Ok(match &entry.outgoing_preamble {
            Some(preamble) => Envelope::initial(preamble.clone(), header, ciphertext),
            None => Envelope::established(header, ciphertext),
        })
    }

    /// Opens an envelope from one device, answering X3DH first if it carries a preamble.
    pub fn open(&mut self, device: Id, envelope: &Envelope) -> Result<Vec<u8>, CryptoError> {
        if let Some(preamble) = &envelope.preamble {
            let already = self
                .sessions
                .get(&device)
                .is_some_and(|entry| entry.origin.as_ref() == Some(preamble));
            if !already {
                let session = self.answer(preamble)?;
                self.sessions.insert(
                    device,
                    Entry {
                        session,
                        outgoing_preamble: None,
                        origin: Some(preamble.clone()),
                    },
                );
            }
        }

        let entry = self
            .sessions
            .get_mut(&device)
            .ok_or(CryptoError::NoSession)?;
        let plaintext = entry
            .session
            .decrypt(&envelope.header, &envelope.ciphertext)?;
        // We have heard from them, so they have the session. Stop paying for the preamble.
        entry.outgoing_preamble = None;
        Ok(plaintext)
    }

    /// Runs X3DH as the responder for one preamble, consuming the one-time prekey it names.
    ///
    /// The one-time prekey is removed whether or not the session goes on to decrypt anything. That
    /// is the point of it being one-time: reusing it would give two sessions the same fourth DH
    /// input, and an attacker who recorded both would only have to break one.
    fn answer(&mut self, preamble: &Preamble) -> Result<RatchetSession, CryptoError> {
        if preamble.signed_prekey_id != self.keys.signed_prekey_id {
            return Err(CryptoError::UnknownPrekey);
        }
        let one_time = match preamble.one_time_prekey_id {
            Some(id) => Some(
                self.keys
                    .one_time
                    .remove(&id)
                    .ok_or(CryptoError::UnknownPrekey)?,
            ),
            None => None,
        };
        let initial = InitialMessage {
            identity: preamble.identity,
            ephemeral_key: preamble.ephemeral_key,
            signed_prekey_id: preamble.signed_prekey_id,
            one_time_prekey_id: preamble.one_time_prekey_id,
        };
        let seed = respond(
            &self.keys.identity,
            &self.keys.signed_prekey,
            one_time.as_ref(),
            &initial,
        )?;
        // The responder's first ratchet key is its signed prekey pair, which is what lets the
        // initiator's first message decrypt without a round trip. `KeyPair` is deliberately not
        // `Clone` — a key that copies itself silently is a key that ends up in two places — so the
        // pair is rebuilt from its seed, which is the same key by construction.
        let pair = KeyPair::from_seed(self.keys.signed_prekey.expose_seed());
        Ok(RatchetSession::responder(&seed, pair))
    }

    /// How many unused one-time prekeys remain.
    ///
    /// The pool only ever shrinks: a key is consumed and deleted when a peer opens a session against
    /// it, and minting more means persisting them, which needs the vault passphrase this process does
    /// not keep. The worker watches the number so it can say so before the pool is empty.
    #[must_use]
    pub fn one_time_remaining(&self) -> usize {
        self.keys.one_time.len()
    }
}

/// The envelope preamble for an X3DH initial message.
fn preamble_of(initial: &InitialMessage) -> Preamble {
    Preamble {
        identity: initial.identity,
        ephemeral_key: initial.ephemeral_key,
        signed_prekey_id: initial.signed_prekey_id,
        one_time_prekey_id: initial.one_time_prekey_id,
    }
}

/// Rebuilds a [`PrekeyBundle`] from the wire form the gateway serves.
///
/// Lengths are checked here rather than trusted, because these bytes came from the server and the
/// server is not trusted to be well-formed any more than it is trusted to be honest. The signature
/// check happens later, inside [`initiate`], which is the call that would otherwise use the key.
pub fn bundle_from_wire(
    identity_key: &[u8],
    signed_prekey_id: u32,
    signed_prekey: &[u8],
    signed_prekey_signature: &[u8],
    one_time: Option<(u32, &[u8])>,
) -> Result<PrekeyBundle, CryptoError> {
    let identity = IdentityPublic::parse(identity_key)?;
    let public_key = fixed32(signed_prekey).ok_or(CryptoError::Envelope("signed prekey length"))?;
    let signature: [u8; 64] = signed_prekey_signature
        .try_into()
        .map_err(|_| CryptoError::Envelope("signed prekey signature length"))?;
    let one_time_prekey = match one_time {
        Some((id, bytes)) => Some((
            id,
            fixed32(bytes).ok_or(CryptoError::Envelope("one-time prekey length"))?,
        )),
        None => None,
    };
    Ok(PrekeyBundle {
        identity,
        signed_prekey: SignedPrekey {
            key_id: signed_prekey_id,
            public_key,
            signature,
        },
        one_time_prekey,
    })
}

fn fixed32(bytes: &[u8]) -> Option<[u8; PUBLIC_KEY_LEN]> {
    bytes.try_into().ok()
}
