//! One Double Ratchet per conversation and remote device, and the X3DH policy that starts them.
//!
//! A conversation is not a session, and neither is a device. Every *device* a peer signs in on has
//! its own long-term identity, and the web and Android clients key the pairwise layer by
//! `(conversation, device)` — the session that carries a sender-key distribution for one room is
//! not the session that carries a direct chat with the same person. This store matches that
//! keying exactly: one ratchet per pair, so a two-person chat where each side has a laptop and a
//! phone is four ratchets, and a message is sealed once per recipient device. That is what makes
//! a compromised phone unable to read what the laptop received.
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
//! Section 11 also lists `conversation_id` and `message_id` among the metadata the tag binds. The
//! conversation and the sending device are bound now — envelope version 2, with the context
//! `migo-crypto`'s `aad` module builds and this store hands the ratchet on both sides. What that
//! buys is narrower than it sounds, and worth stating exactly, because this layer's traffic is not
//! what its name suggests: content never travels here. Every message, one-to-one or group, is sealed
//! once under a sender key, and what travels pairwise is the *distribution* of that key to one
//! device. Relocating a distribution is the case section 11 calls worse than a denial of service —
//! a chain installed in a conversation the sender never authorised, under which the recipient then
//! reads whatever the relocator sends — and binding the conversation id is what stops it. Binding
//! the sending device is what stops a distribution being replayed as if it came from a different
//! device of the same account.
//!
//! `message_id` is deliberately still unbound. A distribution rides inside the message that carries
//! it and has no id of its own: [`SessionStore::seal`] produces one envelope per recipient device,
//! while the message id is minted by whoever builds the frame, so the flags byte stays clear until
//! there is a binding on this path that genuinely has an id.
//!
//! The version travels on the parsed [`Envelope`] rather than being read as a constant here: a
//! receiver has to reproduce the *sender's* bytes, and for a message written before the flip that
//! means no context at all.

use std::collections::HashMap;

use migo_account::{DeviceCredential, IdentityKey, MigoRoot};
use migo_core::{Id, OsRandom, Random};
use migo_crypto::aad;
use migo_crypto::identity::{KeyPair, SignedPrekey, PUBLIC_KEY_LEN};
use migo_crypto::x3dh::{initiate, respond, InitialMessage, PrekeyBundle};
use migo_crypto::{EnvelopeVersion, IdentityPublic, IdentitySecret, RatchetSession};

use super::envelope::{Envelope, Preamble};
use super::CryptoError;

/// How many one-time prekeys this device publishes at a time.
///
/// Each one gives one incoming session forward secrecy against a later compromise of the signed
/// prekey. A hundred is enough that a device offline for a week still has unused ones when it
/// returns, and small enough that the published bundle stays a few kilobytes.
pub const ONE_TIME_PREKEY_COUNT: u32 = 100;

/// The context an envelope of `version` binds, for the metadata the frame carries.
///
/// One function for both directions, so a writer and a reader of the same version cannot disagree
/// about the bytes. `aad::context` returns empty bytes for version 1 whatever it is handed, which is
/// what lets a single call site be correct for both: the version, not the caller, decides whether
/// there is a context to bind. `message_id` is always `None` here — see the module docs on why this
/// layer has no id to bind.
fn context_for(
    version: EnvelopeVersion,
    scheme: u8,
    sender_device: &Id,
    conversation: &Id,
) -> Vec<u8> {
    aad::context(version, scheme, sender_device, conversation, None)
}

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
    /// The unified account root, when this device holds one.
    ///
    /// `None` on a device that signed in with a passphrase before the account had a root and never
    /// restored a container — such a device is a passenger, not a founder: it cannot sign the
    /// identity half of a challenge, and only a `.migo` container or the founding device can change
    /// that. Stored as the raw 32 bytes so the vault format never depends on the reference crate's
    /// types, and rebuilt through [`MigoRoot::from_bytes`] at every use.
    pub root: Option<[u8; 32]>,
    /// The ML-DSA device credential's seed, when this device has one.
    ///
    /// Random, not root-derived — that is the whole two-signature design: a root that leaks from a
    /// backup alone holds the account half of the login ceremony and none of the device half.
    pub device_credential_seed: Option<[u8; 32]>,
    /// This client's tracked AVAX transactions (§184's Activity list), sealed into the vault as
    /// FIELD_TXS. Present here rather than in a separate file for the same reason the saved
    /// sign-in is: it is account history, useless without the account and safe beside it.
    ///
    /// Mid-session updates stay in the worker's memory and are re-sealed the next time the
    /// passphrase is available — the same trade the one-time prekey pool makes, for the same
    /// reason: this process deliberately does not hold the passphrase after unlock.
    pub txs: Vec<crate::vault::TxRecord>,
    /// When this device last sealed a `.migo` container, in unix seconds, sealed into the vault
    /// as FIELD_LAST_BACKUP_AT.
    ///
    /// The security checkup's backup row is drawn from it. A rotation clears it — a container
    /// sealed before the rotation cannot vouch an account whose identity half it no longer
    /// holds — and the worker's memory of the date is cleared with it, at the same ceremony.
    /// Mid-session updates live in the worker's memory and are re-sealed at the next passphrase
    /// moment, the same trade the Activity list makes.
    pub last_backup_at: Option<u64>,
    /// The successor identity key's seed, set once this device has rotated the account's
    /// ML-DSA identity key (or pre-committed the rotation).
    ///
    /// The root's identity derivation has no version, so a successor cannot be derived from it:
    /// the reference crate would have to grow a `V2` domain, and every other client with it. A
    /// rotation therefore mints a fresh random seed and stores it here, beside the root it
    /// supersedes. [`Self::identity_key`] prefers it, which is what keeps every ceremony after
    /// a rotation — the unlock fallback's login, the next rotation, a re-publish on the next
    /// sign-in — signing with the key the server actually knows. The root stays: the wallets are
    /// still derived from it, and `.migo` backups still seal it — and, since the container format
    /// grew a `rotated_identity` field, a backup sealed by this device carries the successor's
    /// seed beside it, which is what lets a fresh container vouch for the account after a
    /// rotation where one sealed before it cannot.
    pub rotated_identity_seed: Option<[u8; 32]>,
    /// The last-seen E2EE identity fingerprint of each peer device this account has spoken to,
    /// sealed into the vault as FIELD_PEER_FINGERPRINTS.
    ///
    /// Keyed by device, not by conversation: the identity key a bundle carries belongs to a
    /// device, and the same person on a second device is a second fingerprint — pinning it to
    /// the conversation would store the same fact once per thread and warn per thread, while
    /// keying it to the account would hide a device change behind a conversation that never
    /// mentioned it. Mid-session updates live in the worker's memory and are re-sealed at the
    /// next passphrase moment, the same trade the Activity list makes.
    pub peer_fingerprints: HashMap<Id, [u8; 32]>,
}

impl DeviceKeys {
    /// Generates the founding device of a new account: the E2EE identity is *derived* from the
    /// root's E2EE domain, so a `.migo` container that carries the root also carries the ability
    /// to recover this device's E2EE history. Only the founding device gets this — additional
    /// devices generate their own, which is what keeps a container restore from silently becoming
    /// a second copy of one device's ratchets.
    pub fn founding(root: &MigoRoot) -> Self {
        let mut random = OsRandom;
        let (signing, exchange) = migo_account::founding_device_e2ee_seeds(root);
        let identity = IdentitySecret::from_seeds(signing, exchange);
        let signed_prekey = KeyPair::generate(&mut random);
        let one_time = (1..=ONE_TIME_PREKEY_COUNT)
            .map(|id| (id, KeyPair::generate(&mut random)))
            .collect();
        let mut credential = [0u8; 32];
        random.fill_bytes(&mut credential);
        Self {
            identity,
            signed_prekey_id: 1,
            signed_prekey,
            one_time,
            session: None,
            root: Some(root.as_bytes().try_into().expect("the root is 32 bytes")),
            device_credential_seed: Some(credential),
            txs: Vec::new(),
            rotated_identity_seed: None,
            peer_fingerprints: HashMap::new(),
            last_backup_at: None,
        }
    }

    /// Generates an additional device of an existing account: fresh random E2EE identity, fresh
    /// device credential, and no root. This is the passphrase sign-in shape — the device can take
    /// part in future ML-DSA logins as *itself*, but it is not the account.
    pub fn additional() -> Self {
        let mut random = OsRandom;
        let identity = IdentitySecret::generate(&mut random);
        let signed_prekey = KeyPair::generate(&mut random);
        let one_time = (1..=ONE_TIME_PREKEY_COUNT)
            .map(|id| (id, KeyPair::generate(&mut random)))
            .collect();
        let mut credential = [0u8; 32];
        random.fill_bytes(&mut credential);
        Self {
            identity,
            signed_prekey_id: 1,
            signed_prekey,
            one_time,
            session: None,
            root: None,
            device_credential_seed: Some(credential),
            txs: Vec::new(),
            rotated_identity_seed: None,
            peer_fingerprints: HashMap::new(),
            last_backup_at: None,
        }
    }

    /// The account root, when this device holds one.
    #[must_use]
    pub fn root(&self) -> Option<MigoRoot> {
        self.root
            .as_ref()
            .and_then(|bytes| MigoRoot::from_bytes(bytes).ok())
    }

    /// The account's ML-DSA identity key: the rotated successor's when the vault holds one, else
    /// the root's derivation.
    ///
    /// Every ceremony — login, add-device, rotation — signs with this key, so a device with
    /// neither a successor nor a root has no `identity_key` and the worker refuses the ceremony
    /// locally rather than sending the server a signature it cannot make. The successor wins
    /// over the root whenever both are present, because the root's derivation stopped being the
    /// account's key the moment a rotation succeeded.
    #[must_use]
    pub fn identity_key(&self) -> Option<IdentityKey> {
        if let Some(seed) = &self.rotated_identity_seed {
            return IdentityKey::from_seed(seed).ok();
        }
        self.root().map(|root| IdentityKey::from_root(&root))
    }

    /// This device's ML-DSA credential, when it has one.
    #[must_use]
    pub fn device_credential(&self) -> Option<DeviceCredential> {
        self.device_credential_seed
            .as_ref()
            .and_then(|seed| DeviceCredential::from_seed(seed).ok())
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
    /// The X3DH shared secret that started the session, so a later protocol that derives a
    /// second key from the *same* handshake — the group-call join wrapper of section 163, whose
    /// HKDF label "migo-call-join-v1" salts the pairwise secret with the call id — can do so
    /// without re-running the handshake. Both devices hold these identical bytes, whichever of
    /// them initiated, because X3DH's `respond` derives the same secret from the other three
    /// inputs. Zeroed when the entry is dropped: a secret that outlives its session is a copy
    /// nothing audits.
    secret: [u8; 32],
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

impl Drop for Entry {
    fn drop(&mut self) {
        self.secret.fill(0);
    }
}

/// Every session this device holds, keyed by conversation and remote device id.
pub struct SessionStore {
    keys: DeviceKeys,
    sessions: HashMap<(Id, Id), Entry>,
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

    /// Adopts the successor identity seed a completed rotation installed.
    ///
    /// The live store moves with the vault or not at all: every ceremony after a rotation signs
    /// with the key the server now knows, and a store that went on deriving from the root would
    /// sign each of them — the unlock fallback's login, the next rotation — with the retired key
    /// and be refused as invalid credentials, with nothing in the message saying why.
    pub fn adopt_rotated_identity_seed(&mut self, seed: [u8; 32]) {
        self.keys.rotated_identity_seed = Some(seed);
    }

    /// Forgets every session.
    ///
    /// Called on sign-out. The ratchet state is the only thing that can decrypt already-received
    /// messages, so dropping it is what makes a sign-out mean something on this device.
    pub fn clear(&mut self) {
        self.sessions.clear();
    }

    /// Forgets the sessions of one conversation, or of one device within it.
    ///
    /// Called when leaving a conversation: the membership that authorised those ratchets is gone,
    /// and keeping them would let a re-join re-use a chain the departed member may still hold.
    /// With `device`, only that device's sessions are dropped — the shape a peer's identity key
    /// change wants (section 155).
    pub fn forget(&mut self, conversation: Id, device: Option<Id>) {
        match device {
            Some(device) => {
                self.sessions.remove(&(conversation, device));
            }
            None => {
                self.sessions.retain(|(id, _), _| *id != conversation);
            }
        }
    }

    /// This device's E2EE identity, for the group layer that signs every broadcast it seals.
    #[must_use]
    pub fn identity(&self) -> &IdentitySecret {
        &self.keys.identity
    }

    /// The X3DH shared secret of the session with one device in one conversation, if any.
    ///
    /// The caller is a protocol that derives a second key from the same handshake — the
    /// group-call join wrapper, whose HKDF label "migo-call-join-v1" salts these bytes with the
    /// call id. The secret is returned by value, not by reference, so no borrow of it can outlive
    /// the session it belongs to; the store's own copy is zeroed with the entry.
    #[must_use]
    pub fn pairwise_secret(&self, conversation: Id, device: Id) -> Option<[u8; 32]> {
        self.sessions
            .get(&(conversation, device))
            .map(|entry| entry.secret)
    }

    /// Seals `plaintext` for one device in one conversation, starting a session from `bundle` if
    /// there is none.
    ///
    /// `bundle` is required only for the first message; pass `None` once a session exists. A caller
    /// that has no bundle and no session gets [`CryptoError::NoBundle`] rather than a silent
    /// plaintext send.
    ///
    /// `sending_device` is this device's own id — the one the frame this envelope rides in stamps as
    /// its sender. It is a parameter rather than state on the store for the same reason
    /// [`SessionStore::open`] takes the sender's from the frame: the id arrives with the grant at
    /// sign-in and changes with the session, while this store is built from the vault before either
    /// is known, so a field would freeze whichever id happened to be current when it was
    /// constructed. Every caller already holds the id it is about to put on the frame, which is also
    /// the only value that keeps the receiver's rebuilt context in step.
    pub fn seal(
        &mut self,
        conversation: Id,
        sending_device: Id,
        device: Id,
        bundle: Option<&PrekeyBundle>,
        plaintext: &[u8],
    ) -> Result<Envelope, CryptoError> {
        let mut random = OsRandom;
        let key = (conversation, device);

        if !self.sessions.contains_key(&key) {
            let bundle = bundle.ok_or(CryptoError::NoBundle)?;
            // `initiate` verifies the bundle's signed prekey against the claimed identity before it
            // does any Diffie-Hellman. That check is what makes the server untrusted: it chooses
            // which bundle to serve, and a substituted prekey fails here, on this device, before a
            // single byte of the message is composed.
            let (seed, initial, _ephemeral) = initiate(&self.keys.identity, bundle, &mut random)?;
            let session =
                RatchetSession::initiator(&seed, bundle.signed_prekey.public_key, &mut random)?;
            self.sessions.insert(
                key,
                Entry {
                    session,
                    secret: seed.shared_secret,
                    outgoing_preamble: Some(preamble_of(&initial)),
                    origin: None,
                },
            );
        }

        let entry = self.sessions.get_mut(&key).expect("inserted above");

        // The peer has replied, so it has the session; the preamble has done its job and every
        // further message saves the ~110 bytes it costs. Decided before the envelope is built rather
        // than after, because the scheme it yields is what the context binds: the two must be read
        // off one value or a message can go out declaring one layout and sealed under another.
        if entry.session.received_count() > 0 {
            entry.outgoing_preamble = None;
        }
        let preamble = entry.outgoing_preamble.clone();
        let scheme = if preamble.is_some() {
            super::envelope::SCHEME_DOUBLE_RATCHET_PREKEY
        } else {
            super::envelope::SCHEME_DOUBLE_RATCHET
        };

        // The scheme travels inside the context as well as in the envelope, so a distribution
        // relabelled from the plain form to the prekey form fails the tag rather than being read
        // under the wrong layout.
        let context = context_for(
            EnvelopeVersion::WRITTEN,
            scheme,
            &sending_device,
            &conversation,
        );
        let (header, ciphertext) = entry
            .session
            .encrypt_next(plaintext, &mut random, &context)?;

        Ok(match preamble {
            Some(preamble) => Envelope::initial(preamble, header, ciphertext),
            None => Envelope::established(header, ciphertext),
        })
    }

    /// Opens an envelope from one device in one conversation, answering X3DH first if it carries
    /// a preamble.
    ///
    /// `device` is the *sender's*, because that is what the context binds and what the session is
    /// keyed by; this device's own id is not part of opening anything.
    ///
    /// # Commit only on success
    ///
    /// A first message carries X3DH material, so a receiver with no session for the sender derives
    /// one rather than refusing outright. That derivation is committed here only after the tag has
    /// verified, because a distribution goes out to every device in a conversation: a first message
    /// pairwise-sealed for a *different* device also arrives here, decodes as a well-formed prekey
    /// envelope, and would — if committed eagerly — plant a bogus session in this slot so the real
    /// distribution could never open, and spend a one-time prekey doing it. The responder session is
    /// therefore derived locally, the decrypt attempted, and only then do the session and the
    /// consumed prekey become state.
    pub fn open(
        &mut self,
        conversation: Id,
        device: Id,
        envelope: &Envelope,
    ) -> Result<Vec<u8>, CryptoError> {
        let key = (conversation, device);
        let context = context_for(envelope.version, envelope.scheme, &device, &conversation);

        if let Some(preamble) = &envelope.preamble {
            let already = self
                .sessions
                .get(&key)
                .is_some_and(|entry| entry.origin.as_ref() == Some(preamble));
            if !already {
                let (mut session, secret, one_time_prekey) = self.derive_responder(preamble)?;
                let plaintext =
                    session.decrypt(&envelope.header, &envelope.ciphertext, &context)?;
                if let Some(id) = one_time_prekey {
                    self.keys.one_time.remove(&id);
                }
                self.sessions.insert(
                    key,
                    Entry {
                        session,
                        secret,
                        outgoing_preamble: None,
                        origin: Some(preamble.clone()),
                    },
                );
                return Ok(plaintext);
            }
        }

        let entry = self.sessions.get_mut(&key).ok_or(CryptoError::NoSession)?;
        let plaintext = entry
            .session
            .decrypt(&envelope.header, &envelope.ciphertext, &context)?;
        // We have heard from them, so they have the session. Stop paying for the preamble.
        entry.outgoing_preamble = None;
        Ok(plaintext)
    }

    /// Runs X3DH as the responder for one preamble, naming the one-time prekey that a *successful*
    /// open must then consume.
    ///
    /// Consuming is the caller's step, and it happens only once the tag has verified — see
    /// [`SessionStore::open`]. The key is still one-time in the sense that matters: it is removed
    /// before the session that used it can be used again, so two sessions never share the fourth DH
    /// input, and an attacker who recorded both would still only have to break one.
    fn derive_responder(
        &self,
        preamble: &Preamble,
    ) -> Result<(RatchetSession, [u8; 32], Option<u32>), CryptoError> {
        if preamble.signed_prekey_id != self.keys.signed_prekey_id {
            return Err(CryptoError::UnknownPrekey);
        }
        let one_time = match preamble.one_time_prekey_id {
            Some(id) => Some(
                self.keys
                    .one_time
                    .get(&id)
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
            // Already a reference: looked up rather than removed, because consuming the prekey is
            // the caller's step and waits for the tag (see `open`).
            one_time,
            &initial,
        )?;
        // The responder's first ratchet key is its signed prekey pair, which is what lets the
        // initiator's first message decrypt without a round trip. `KeyPair` is deliberately not
        // `Clone` — a key that copies itself silently is a key that ends up in two places — so the
        // pair is rebuilt from its seed, which is the same key by construction.
        let pair = KeyPair::from_seed(self.keys.signed_prekey.expose_seed());
        let secret = seed.shared_secret;
        Ok((
            RatchetSession::responder(&seed, pair),
            secret,
            preamble.one_time_prekey_id,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The founding device's E2EE identity is a function of the root, not a fresh draw: two
    /// founding devices of the same root agree on the identity seeds, which is what makes the
    /// account's E2EE history recoverable from a `.migo` container that carries the root.
    #[test]
    fn the_founding_identity_is_a_function_of_the_root() {
        let root = MigoRoot::from_bytes(&[9u8; 32]).expect("32 bytes is a root");
        let first = DeviceKeys::founding(&root);
        let second = DeviceKeys::founding(&root);

        assert_eq!(
            first.identity.expose_signing_seed(),
            second.identity.expose_signing_seed()
        );
        assert_eq!(
            first.identity.expose_exchange_seed(),
            second.identity.expose_exchange_seed()
        );

        // And it is the E2EE domain's derivation exactly — the reference crate's answer, not a
        // parallel implementation of the same idea.
        let (signing, exchange) = migo_account::founding_device_e2ee_seeds(&root);
        assert_eq!(first.identity.expose_signing_seed(), signing);
        assert_eq!(first.identity.expose_exchange_seed(), exchange);

        // The founding device is the one shape that carries the root.
        assert_eq!(
            first.root,
            Some(root.as_bytes().try_into().expect("32 bytes"))
        );
        assert!(first.device_credential_seed.is_some());
        // The prekeys stay random: forward secrecy must not be a function of the account.
        assert_ne!(
            first.signed_prekey.expose_seed(),
            second.signed_prekey.expose_seed()
        );
    }

    /// An additional device has no root and a fresh identity: two passphrase sign-ins on the same
    /// account are two devices, and neither inherits the founding device's ratchets.
    #[test]
    fn an_additional_device_is_its_own_device() {
        let first = DeviceKeys::additional();
        let second = DeviceKeys::additional();

        assert!(first.root.is_none());
        assert_ne!(
            first.identity.expose_signing_seed(),
            second.identity.expose_signing_seed()
        );
        assert!(first.device_credential_seed.is_some());
        assert_ne!(first.device_credential_seed, second.device_credential_seed);
        // Without a root there is no identity key: the worker refuses the ceremony locally rather
        // than asking the server whether it can sign.
        assert!(first.identity_key().is_none());
        assert!(first.device_credential().is_some());
    }

    /// A founding device's identity key is the root's identity domain, so the login signature it
    /// makes is the signature the server's challenge verifies.
    #[test]
    fn the_identity_key_comes_from_the_root() {
        let root = MigoRoot::from_bytes(&[11u8; 32]).expect("32 bytes is a root");
        let keys = DeviceKeys::founding(&root);
        let identity = keys.identity_key().expect("a root means an identity key");
        assert_eq!(
            identity.public_key(),
            migo_account::IdentityKey::from_root(&root).public_key()
        );
    }

    /// A rotated seed supersedes the root's derivation the moment it is set, and the root stays
    /// beside it. The order is the whole ceremony's consistency: sign with the successor after
    /// the server accepted it, never before, and keep the root for the wallets and the container.
    #[test]
    fn a_rotated_seed_supersedes_the_root_derivation() {
        let root = MigoRoot::from_bytes(&[12u8; 32]).expect("32 bytes is a root");
        let mut keys = DeviceKeys::founding(&root);
        let seed = [0x5au8; 32];

        keys.rotated_identity_seed = Some(seed);
        let identity = keys
            .identity_key()
            .expect("a successor means an identity key");
        assert_eq!(
            identity.public_key(),
            IdentityKey::from_seed(&seed)
                .expect("a seed is a key")
                .public_key()
        );
        assert_ne!(
            identity.public_key(),
            IdentityKey::from_root(&root).public_key()
        );
        // The root is not consumed by the rotation: the wallet derivations still read it, and
        // the crash-window heal of the next rotation attempt still signs with it.
        assert!(keys.root().is_some());
    }

    /// Adoption moves the live store with the vault: after `adopt_rotated_identity_seed`, the
    /// store hands out the successor exactly as a fresh unlock of the rotated vault would.
    #[test]
    fn adoption_moves_the_live_store_with_the_vault() {
        let root = MigoRoot::from_bytes(&[13u8; 32]).expect("32 bytes is a root");
        let keys = DeviceKeys::founding(&root);
        let mut store = SessionStore::new(keys);
        let seed = [0xa5u8; 32];

        store.adopt_rotated_identity_seed(seed);
        assert_eq!(
            store
                .keys()
                .identity_key()
                .expect("an identity key")
                .public_key(),
            IdentityKey::from_seed(&seed)
                .expect("a seed is a key")
                .public_key()
        );
    }

    /// The bundle a peer would fetch from the gateway, in the store's own shape: identity, signed
    /// prekey with its signature, and the first one-time prekey.
    fn published_bundle(keys: &DeviceKeys) -> PrekeyBundle {
        let signed = keys.signed_prekey_signed();
        let one_time = keys.one_time_public().into_iter().next();
        bundle_from_wire(
            &keys.identity_public().to_bytes(),
            signed.key_id,
            &signed.public_key,
            &signed.signature,
            one_time.as_ref().map(|(id, key)| (*id, key.as_slice())),
        )
        .expect("the store's own halves parse")
    }

    /// Both sides of a session hold the same X3DH shared secret, whichever of them initiated —
    /// the property the group-call join wrapper of section 163 turns on, because its HKDF label
    /// "migo-call-join-v1" must derive the same wrapping key on the joiner and on the seated
    /// participant answering it. A session whose two ends disagreed would seal a key the other
    /// end could never open, and the disagreement would be silent.
    #[test]
    fn both_ends_of_a_session_hold_the_same_secret() {
        let alice_keys = DeviceKeys::additional();
        let bob_keys = DeviceKeys::additional();
        let alice_device = Id::generate(0, &mut OsRandom);
        let bob_device = Id::generate(0, &mut OsRandom);
        let conversation = Id::generate(0, &mut OsRandom);
        let mut alice = SessionStore::new(alice_keys);
        let mut bob = SessionStore::new(bob_keys);

        // Alice initiates: her first seal runs X3DH against Bob's bundle.
        let envelope = alice
            .seal(
                conversation,
                bob_device,
                Some(&published_bundle(bob.keys())),
                b"the first word",
            )
            .expect("seals against the bundle");
        bob.open(conversation, alice_device, &envelope)
            .expect("Bob answers the session");

        let alice_secret = alice
            .pairwise_secret(conversation, bob_device)
            .expect("Alice recorded the handshake's secret");
        assert_eq!(
            bob.pairwise_secret(conversation, alice_device),
            Some(alice_secret)
        );

        // And a session Bob initiates lands on the same property: the secret is a function of the
        // handshake's inputs, not of who ran `initiate`.
        let reply = bob
            .seal(conversation, bob_device, alice_device, None, b"the answer")
            .expect("the established session seals without a bundle");
        assert!(reply.preamble.is_none());
        let other = Id::generate(0, &mut OsRandom);
        let first = bob
            .seal(
                other,
                bob_device,
                alice_device,
                Some(&published_bundle(alice.keys())),
                b"bob speaks first here",
            )
            .expect("Bob initiates elsewhere");
        alice
            .open(other, bob_device, &first)
            .expect("Alice answers");
        let bob_secret = bob
            .pairwise_secret(other, alice_device)
            .expect("Bob recorded the handshake's secret");
        assert_eq!(alice.pairwise_secret(other, bob_device), Some(bob_secret));

        // No session, no secret: the accessor reports absence rather than guessing.
        assert!(alice.pairwise_secret(other, bob_device).is_some());
        let stranger = Id::generate(0, &mut OsRandom);
        assert!(alice.pairwise_secret(other, stranger).is_none());
    }

    /// A distribution sealed for one conversation must not open in another.
    ///
    /// Section 11 states the stake exactly. Content never travels through this layer — every message
    /// is sealed once under a sender key and what travels pairwise is the *distribution* of that key
    /// to one device. Relocating a distribution is therefore worse than a denial of service: it
    /// installs a chain in a conversation the sender never authorised, and the recipient then reads
    /// whatever the relocator sends under it.
    ///
    /// This is the attack version 2 exists to stop, and the case that would have opened on the old
    /// code: the first message to a device carries X3DH material, and nothing in that derivation
    /// mentions the conversation — the identities and the prekeys are the same in every conversation
    /// the two devices share.
    #[test]
    fn a_distribution_relocated_to_another_conversation_does_not_open() {
        let alice_keys = DeviceKeys::additional();
        let bob_keys = DeviceKeys::additional();
        let alice_device = Id::generate(0, &mut OsRandom);
        let bob_device = Id::generate(0, &mut OsRandom);
        let conversation = Id::generate(0, &mut OsRandom);
        let elsewhere = Id::generate(0, &mut OsRandom);
        let mut alice = SessionStore::new(alice_keys);
        let mut bob = SessionStore::new(bob_keys);

        let bundle = published_bundle(bob.keys());
        let sealed = alice
            .seal(
                conversation,
                alice_device,
                bob_device,
                Some(&bundle),
                b"the chain we distribute",
            )
            .expect("seals against the bundle");
        let prekeys = bob.one_time_remaining();

        // The attack first, while the prekey is still unspent, so the assertion below is about a
        // prekey that must survive rather than one the genuine open has already taken.
        assert!(
            bob.open(elsewhere, alice_device, &sealed).is_err(),
            "a distribution for one conversation must not open in another"
        );
        assert_eq!(
            bob.one_time_remaining(),
            prekeys,
            "a message that failed to authenticate must not spend the prekey it named"
        );

        // And the control: in the conversation it was sealed for, the same bytes open.
        let opened = bob
            .open(conversation, alice_device, &sealed)
            .expect("a distribution opens in its own conversation");
        assert_eq!(opened, b"the chain we distribute");
        assert_eq!(
            bob.one_time_remaining(),
            prekeys - 1,
            "and once it has genuinely opened, the prekey is spent"
        );
    }

    /// The version byte selects the associated data, at the layer that has to build it.
    ///
    /// `migo-crypto`'s own tests pin that the ratchet feeds its context to the AEAD, and the vectors
    /// pin the context's bytes. Neither pins the thing that protects a conversation: that *this*
    /// store builds the context out of the frame's claimed metadata and hands it to the ratchet on
    /// both sides. A store that accepted a context parameter and passed an empty one would leave
    /// every test above green.
    #[test]
    fn the_version_byte_selects_the_associated_data_this_store_builds() {
        let alice_keys = DeviceKeys::additional();
        let bob_keys = DeviceKeys::additional();
        let alice_device = Id::generate(0, &mut OsRandom);
        let bob_device = Id::generate(0, &mut OsRandom);
        let conversation = Id::generate(0, &mut OsRandom);
        let mut alice = SessionStore::new(alice_keys);
        let mut bob = SessionStore::new(bob_keys);

        let bundle = published_bundle(bob.keys());
        let sealed = alice
            .seal(
                conversation,
                alice_device,
                bob_device,
                Some(&bundle),
                b"the chain we distribute",
            )
            .expect("seals against the bundle");
        assert_eq!(
            sealed.version,
            EnvelopeVersion::WRITTEN,
            "the pairwise layer writes the version this build declares"
        );

        // Relabelling it version 1 must break it. The ratchet would then seam an empty context where
        // the sender sealed a real one, so a tag that verifies can only mean the context never
        // reached the tag — which is precisely the failure this whole step exists to rule out.
        let mut relabelled =
            Envelope::decode(&sealed.encode().expect("encodes")).expect("its own encoding decodes");
        relabelled.version = EnvelopeVersion::V1;
        let prekeys = bob.one_time_remaining();
        assert!(
            bob.open(conversation, alice_device, &relabelled).is_err(),
            "a message relabelled to version 1 must not open under the version-2 associated data"
        );

        // The genuine envelope still opens, so the failure above is the relabelling and not a session
        // the failed attempt damaged: nothing is committed on a message that does not authenticate,
        // which for a first message means neither the session nor the one-time prekey it named.
        assert_eq!(bob.one_time_remaining(), prekeys);
        let opened = bob
            .open(conversation, alice_device, &sealed)
            .expect("the genuine envelope opens");
        assert_eq!(opened, b"the chain we distribute");
    }
}
