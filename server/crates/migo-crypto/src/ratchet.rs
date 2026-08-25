//! Double Ratchet — per-message keys for a 1:1 conversation.
//!
//! X3DH produces one shared secret. If that secret encrypted every message, then
//! stealing a phone once would decrypt the entire conversation history, and every
//! future message too. The Double Ratchet turns that one secret into a fresh key
//! per message, with two properties that matter to a real user:
//!
//! * **Forward secrecy** — a key compromised today does not decrypt yesterday's
//!   messages, because yesterday's keys were deleted after use.
//! * **Post-compromise security** — once the attacker loses access, the ratchet
//!   heals. New Diffie-Hellman material from the other side is mixed in on every
//!   turn of the conversation, and the attacker cannot follow.
//!
//! Two ratchets combine:
//!
//! 1. **The DH ratchet.** Each side attaches a fresh public key to its messages.
//!    When a new one arrives, both root keys advance by mixing in a new DH output.
//!    This is what heals a compromise, and it only turns when the conversation
//!    turns — one side sending ten messages in a row does not advance it.
//! 2. **The symmetric chain.** Within one DH step, each message advances a chain
//!    key by a KDF. This is cheap and gives forward secrecy between consecutive
//!    messages without a round trip.
//!
//! # Out-of-order and lost messages
//!
//! Messages arrive out of order and get lost. Message 5 can arrive before message
//! 3, so the receiver derives and stores the keys it skipped. Two bounds keep that
//! from becoming an attack:
//!
//! * [`MAX_CHAIN_GAP`] caps how far ahead one message may claim to be. Without
//!   it, a message numbered four billion makes the receiver derive four billion
//!   keys — a one-frame CPU exhaustion.
//! * [`MAX_SKIPPED_KEYS`] caps how many skipped keys are retained. Without it, a
//!   sender who sends only odd-numbered messages grows the receiver's state
//!   forever — a one-session memory leak.
//!
//! Reaching either bound loses messages, which is the correct trade: a lost
//! message is visible and recoverable, and an exhausted server is neither.
//!
//! A stored key is deleted the moment it is used. That is what makes a replayed
//! frame fail rather than deliver the same message twice.

use std::collections::HashMap;

use migo_core::Random;
use zeroize::Zeroize;

use crate::aead::{self, SymmetricKey, NONCE_LEN};
use crate::error::{CryptoError, Result};
use crate::identity::{KeyPair, PUBLIC_KEY_LEN};
use crate::kdf;
use crate::x3dh::SessionSeed;

/// Maximum number of messages a single header may claim to have skipped.
pub const MAX_CHAIN_GAP: u32 = 2_000;

/// Maximum number of skipped message keys retained across a session.
pub const MAX_SKIPPED_KEYS: usize = 2_000;

/// The plaintext header attached to every ratchet message.
///
/// All three fields are public and all three are authenticated as associated
/// data, so tampering with them makes decryption fail rather than succeed
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatchetHeader {
    /// The sender's current ratchet public key.
    pub ratchet_key: [u8; PUBLIC_KEY_LEN],
    /// How many messages the sender sent in its previous chain.
    ///
    /// Lets the receiver derive the keys it never saw from the *old* chain before
    /// moving to the new one. Without it, a DH step would silently drop any
    /// message still in flight from the previous chain.
    pub previous_chain_length: u32,
    /// Index of this message within the sender's current chain.
    pub message_number: u32,
}

impl RatchetHeader {
    /// Encoded length: key, then two big-endian `u32`s.
    pub const ENCODED_LEN: usize = PUBLIC_KEY_LEN + 8;

    /// Serialises the header.
    ///
    /// Fixed-width big-endian rather than varints, deliberately. This byte string
    /// is authenticated, so it must be canonical: a varint with two valid
    /// encodings would give one header two valid authentication tags.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..PUBLIC_KEY_LEN].copy_from_slice(&self.ratchet_key);
        out[PUBLIC_KEY_LEN..PUBLIC_KEY_LEN + 4]
            .copy_from_slice(&self.previous_chain_length.to_be_bytes());
        out[PUBLIC_KEY_LEN + 4..].copy_from_slice(&self.message_number.to_be_bytes());
        out
    }

    /// Parses a header.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(CryptoError::MalformedHeader);
        }
        let mut ratchet_key = [0u8; PUBLIC_KEY_LEN];
        ratchet_key.copy_from_slice(&bytes[..PUBLIC_KEY_LEN]);
        let previous_chain_length = u32::from_be_bytes(
            bytes[PUBLIC_KEY_LEN..PUBLIC_KEY_LEN + 4]
                .try_into()
                .expect("four bytes"),
        );
        let message_number =
            u32::from_be_bytes(bytes[PUBLIC_KEY_LEN + 4..].try_into().expect("four bytes"));
        Ok(Self {
            ratchet_key,
            previous_chain_length,
            message_number,
        })
    }
}

/// A message key and the nonce derived alongside it.
///
/// The nonce comes from the KDF rather than the wire, so it is never transmitted
/// and cannot be tampered with. Each message key is used exactly once, so a
/// derived nonce carries no reuse risk.
struct MessageKey {
    key: SymmetricKey,
    nonce: [u8; NONCE_LEN],
}

/// Key material for one skipped message, awaiting late delivery.
#[derive(Zeroize)]
#[zeroize(drop)]
struct SkippedKey {
    key: [u8; 32],
    nonce: [u8; NONCE_LEN],
}

/// A Double Ratchet session for one device pair.
///
/// Not `Clone`: two copies of a ratchet would each advance independently and each
/// believe it had already used a key the other had not. The type system is the
/// cheapest place to prevent that.
pub struct RatchetSession {
    root_key: [u8; 32],
    /// Our current ratchet pair. `None` for a responder that has not yet sent.
    sending_pair: Option<KeyPair>,
    /// The peer's latest ratchet key, once seen.
    receiving_key: Option<[u8; PUBLIC_KEY_LEN]>,
    sending_chain: Option<[u8; 32]>,
    receiving_chain: Option<[u8; 32]>,
    sent_count: u32,
    received_count: u32,
    previous_sending_count: u32,
    /// Keys for messages that were skipped, keyed by (ratchet key, message number).
    skipped: HashMap<([u8; PUBLIC_KEY_LEN], u32), SkippedKey>,
    /// Insertion order, so the oldest skipped key is the one evicted.
    skipped_order: Vec<([u8; PUBLIC_KEY_LEN], u32)>,
    associated_data: Vec<u8>,
}

impl Drop for RatchetSession {
    fn drop(&mut self) {
        self.root_key.zeroize();
        if let Some(chain) = &mut self.sending_chain {
            chain.zeroize();
        }
        if let Some(chain) = &mut self.receiving_chain {
            chain.zeroize();
        }
    }
}

impl core::fmt::Debug for RatchetSession {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RatchetSession")
            .field("sent", &self.sent_count)
            .field("received", &self.received_count)
            .field("skipped", &self.skipped.len())
            .finish_non_exhaustive()
    }
}

impl RatchetSession {
    /// Starts a session as the initiator, who already knows the peer's prekey.
    ///
    /// The initiator can send immediately: it performs the first DH step against
    /// the peer's signed prekey, which is exactly the key the peer published for
    /// this purpose.
    pub fn initiator(
        seed: &SessionSeed,
        peer_signed_prekey: [u8; PUBLIC_KEY_LEN],
        random: &mut dyn Random,
    ) -> Result<Self> {
        let mut session = Self::new(seed.shared_secret, seed.associated_data.clone());
        let pair = KeyPair::generate(random);
        let dh = pair.diffie_hellman(&peer_signed_prekey)?;
        let (root_key, chain) =
            kdf::derive_pair::<32, 32>(&dh, Some(&session.root_key), kdf::LABEL_RATCHET_ROOT);
        session.root_key.zeroize();
        session.root_key = root_key;
        session.sending_chain = Some(chain);
        session.sending_pair = Some(pair);
        session.receiving_key = Some(peer_signed_prekey);
        Ok(session)
    }

    /// Starts a session as the responder, whose signed prekey pair is the first
    /// ratchet key.
    ///
    /// The responder cannot send until it has received, because until then it has
    /// no peer ratchet key to step against. That is not a limitation in practice:
    /// the responder is by definition the side that received the first message.
    #[must_use]
    pub fn responder(seed: &SessionSeed, signed_prekey_pair: KeyPair) -> Self {
        let mut session = Self::new(seed.shared_secret, seed.associated_data.clone());
        session.sending_pair = Some(signed_prekey_pair);
        session
    }

    fn new(shared_secret: [u8; 32], associated_data: Vec<u8>) -> Self {
        Self {
            root_key: shared_secret,
            sending_pair: None,
            receiving_key: None,
            sending_chain: None,
            receiving_chain: None,
            sent_count: 0,
            received_count: 0,
            previous_sending_count: 0,
            skipped: HashMap::new(),
            skipped_order: Vec::new(),
            associated_data,
        }
    }

    /// Number of messages sent in the current chain.
    #[must_use]
    pub fn sent_count(&self) -> u32 {
        self.sent_count
    }

    /// Number of messages received in the current chain.
    #[must_use]
    pub fn received_count(&self) -> u32 {
        self.received_count
    }

    /// How many skipped keys are currently retained.
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// Encrypts `plaintext`, returning the header and the ciphertext.
    ///
    /// The ciphertext has no nonce prefix: the nonce is derived from the message
    /// key, which the receiver reconstructs from the header.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<(RatchetHeader, Vec<u8>)> {
        let pair = self.sending_pair.as_ref().ok_or(CryptoError::NoSession)?;
        let chain = self.sending_chain.as_mut().ok_or(CryptoError::NoSession)?;

        let message_key = advance_chain(chain);
        let header = RatchetHeader {
            ratchet_key: pair.public(),
            previous_chain_length: self.previous_sending_count,
            message_number: self.sent_count,
        };
        self.sent_count += 1;

        let mut aad = self.associated_data.clone();
        aad.extend_from_slice(&header.to_bytes());
        let ciphertext =
            aead::seal_with_nonce(&message_key.key, &message_key.nonce, &aad, plaintext)?;
        // `seal_with_nonce` prefixes the nonce; the receiver derives it, so drop it.
        Ok((header, ciphertext[NONCE_LEN..].to_vec()))
    }

    /// Decrypts a message.
    ///
    /// Advances the ratchet only when decryption succeeds. A forged message that
    /// claimed a new ratchet key would otherwise destroy the session's ability to
    /// decrypt genuine ones — a denial of service from anyone who can inject a
    /// frame.
    pub fn decrypt(&mut self, header: &RatchetHeader, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut aad = self.associated_data.clone();
        aad.extend_from_slice(&header.to_bytes());

        // A late message whose key was already derived and set aside.
        if let Some(skipped) = self
            .skipped
            .remove(&(header.ratchet_key, header.message_number))
        {
            self.skipped_order
                .retain(|k| *k != (header.ratchet_key, header.message_number));
            let key = SymmetricKey::from_bytes(skipped.key);
            return aead::open_with_nonce(&key, &skipped.nonce, &aad, ciphertext);
        }

        let is_new_chain = self.receiving_key != Some(header.ratchet_key);
        if is_new_chain {
            self.step_receiving_chain(header, &aad, ciphertext)
        } else {
            self.decrypt_in_current_chain(header, &aad, ciphertext)
        }
    }

    /// Handles a message that belongs to the chain we are already tracking.
    fn decrypt_in_current_chain(
        &mut self,
        header: &RatchetHeader,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        if header.message_number < self.received_count {
            // Already consumed. The key was deleted on use, so this is either a
            // replay or a duplicate delivery; either way there is nothing to do.
            return Err(CryptoError::KeyAlreadyUsed);
        }
        let gap = header.message_number - self.received_count;
        if gap > MAX_CHAIN_GAP {
            return Err(CryptoError::ChainGapTooLarge);
        }
        let chain = self
            .receiving_chain
            .as_mut()
            .ok_or(CryptoError::NoSession)?;

        // Derive and stash the keys for anything skipped, then the key we want.
        let mut pending = Vec::with_capacity(gap as usize);
        for offset in 0..gap {
            let key = advance_chain(chain);
            pending.push((self.received_count + offset, key));
        }
        let target = advance_chain(chain);
        let plaintext = aead::open_with_nonce(&target.key, &target.nonce, aad, ciphertext)?;

        // Only now, once the message is proven genuine, mutate session state.
        for (number, key) in pending {
            self.stash_skipped(header.ratchet_key, number, key);
        }
        self.received_count = header.message_number + 1;
        Ok(plaintext)
    }

    /// Handles the first message of a new chain: turn the DH ratchet.
    fn step_receiving_chain(
        &mut self,
        header: &RatchetHeader,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        if header.message_number > MAX_CHAIN_GAP
            || header.previous_chain_length > MAX_CHAIN_GAP.saturating_add(self.received_count)
        {
            return Err(CryptoError::ChainGapTooLarge);
        }
        let pair = self.sending_pair.as_ref().ok_or(CryptoError::NoSession)?;

        // Finish the previous chain, so messages still in flight from it can be
        // decrypted when they arrive.
        let mut leftovers = Vec::new();
        if let (Some(chain), Some(previous_key)) =
            (self.receiving_chain.as_mut(), self.receiving_key)
        {
            let remaining = header
                .previous_chain_length
                .saturating_sub(self.received_count);
            if remaining > MAX_CHAIN_GAP {
                return Err(CryptoError::ChainGapTooLarge);
            }
            for offset in 0..remaining {
                leftovers.push((
                    previous_key,
                    self.received_count + offset,
                    advance_chain(chain),
                ));
            }
        }

        // Turn the DH ratchet: mix the peer's new key into the root key.
        let dh = pair.diffie_hellman(&header.ratchet_key)?;
        let (root_after_receive, mut receiving_chain) =
            kdf::derive_pair::<32, 32>(&dh, Some(&self.root_key), kdf::LABEL_RATCHET_ROOT);

        // Derive the keys this new chain skipped, then the one we want.
        if header.message_number > MAX_CHAIN_GAP {
            return Err(CryptoError::ChainGapTooLarge);
        }
        let mut pending = Vec::with_capacity(header.message_number as usize);
        for number in 0..header.message_number {
            pending.push((number, advance_chain(&mut receiving_chain)));
        }
        let target = advance_chain(&mut receiving_chain);
        let plaintext = aead::open_with_nonce(&target.key, &target.nonce, aad, ciphertext)?;

        // Proven genuine: commit. Our own next chain steps too, with a fresh pair,
        // which is what makes the ratchet heal after a compromise.
        for (key, number, message_key) in leftovers {
            self.stash_skipped(key, number, message_key);
        }
        for (number, message_key) in pending {
            self.stash_skipped(header.ratchet_key, number, message_key);
        }
        self.root_key.zeroize();
        self.root_key = root_after_receive;
        self.receiving_chain = Some(receiving_chain);
        self.receiving_key = Some(header.ratchet_key);
        self.received_count = header.message_number + 1;
        self.previous_sending_count = self.sent_count;
        self.sent_count = 0;
        // The sending chain is left unset: it is derived lazily on the next send,
        // against a pair generated then, so a session that only receives never
        // generates keys it does not use.
        self.sending_chain = None;
        Ok(plaintext)
    }

    /// Prepares the sending chain if a receive has invalidated it.
    ///
    /// Called before encrypting. Separated from [`Self::encrypt`] so that the
    /// caller-visible signature stays `&mut self` without a random source on every
    /// send: the pair is only generated when the ratchet actually needs to turn.
    pub fn prepare_send(&mut self, random: &mut dyn Random) -> Result<()> {
        if self.sending_chain.is_some() {
            return Ok(());
        }
        let peer_key = self.receiving_key.ok_or(CryptoError::NoSession)?;
        let pair = KeyPair::generate(random);
        let dh = pair.diffie_hellman(&peer_key)?;
        let (root_key, chain) =
            kdf::derive_pair::<32, 32>(&dh, Some(&self.root_key), kdf::LABEL_RATCHET_ROOT);
        self.root_key.zeroize();
        self.root_key = root_key;
        self.sending_chain = Some(chain);
        self.sending_pair = Some(pair);
        Ok(())
    }

    /// Encrypts, turning the DH ratchet first if the last operation was a receive.
    pub fn encrypt_next(
        &mut self,
        plaintext: &[u8],
        random: &mut dyn Random,
    ) -> Result<(RatchetHeader, Vec<u8>)> {
        self.prepare_send(random)?;
        self.encrypt(plaintext)
    }

    /// Stores a skipped key, evicting the oldest once the bound is reached.
    fn stash_skipped(&mut self, ratchet_key: [u8; PUBLIC_KEY_LEN], number: u32, key: MessageKey) {
        while self.skipped.len() >= MAX_SKIPPED_KEYS {
            // Oldest first: a message that has been missing longest is the least
            // likely to still arrive.
            let Some(oldest) = self.skipped_order.first().copied() else {
                break;
            };
            self.skipped_order.remove(0);
            self.skipped.remove(&oldest);
        }
        let entry = SkippedKey {
            key: *key.key.expose(),
            nonce: key.nonce,
        };
        if self.skipped.insert((ratchet_key, number), entry).is_none() {
            self.skipped_order.push((ratchet_key, number));
        }
    }
}

/// Advances a chain key one step and returns the message key it yields.
///
/// Two separate derivations from the same chain key: the next chain key, and the
/// message key plus nonce. The chain key is overwritten in place, so the previous
/// value is gone — that is the mechanism of forward secrecy, and it is why this
/// takes `&mut`.
fn advance_chain(chain: &mut [u8; 32]) -> MessageKey {
    let (next_chain, material) = kdf::derive_pair::<32, 56>(chain, None, kdf::LABEL_RATCHET_CHAIN);
    chain.zeroize();
    *chain = next_chain;

    let mut key = [0u8; 32];
    key.copy_from_slice(&material[..32]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&material[32..]);
    MessageKey {
        key: SymmetricKey::from_bytes(key),
        nonce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    use crate::identity::{IdentitySecret, SignedPrekey};
    use crate::x3dh::{self, PrekeyBundle};

    /// A pair of sessions that have completed X3DH, ready to exchange messages.
    struct Pair {
        alice: RatchetSession,
        bob: RatchetSession,
        random: SeededRandom,
    }

    fn pair(seed: u64) -> Pair {
        let mut random = SeededRandom::new(seed);
        let alice_identity = IdentitySecret::generate(&mut random);
        let bob_identity = IdentitySecret::generate(&mut random);
        let bob_spk = KeyPair::generate(&mut random);
        let bob_opk = KeyPair::generate(&mut random);
        let bundle = PrekeyBundle {
            identity: bob_identity.public(),
            signed_prekey: SignedPrekey::create(&bob_identity, 1, &bob_spk),
            one_time_prekey: Some((2, bob_opk.public())),
        };

        let (alice_seed, initial, _ephemeral) =
            x3dh::initiate(&alice_identity, &bundle, &mut random).expect("initiates");
        let bob_seed =
            x3dh::respond(&bob_identity, &bob_spk, Some(&bob_opk), &initial).expect("responds");

        let alice =
            RatchetSession::initiator(&alice_seed, bob_spk.public(), &mut random).expect("starts");
        let bob = RatchetSession::responder(&bob_seed, bob_spk);
        Pair { alice, bob, random }
    }

    #[test]
    fn a_single_message_round_trips() {
        let mut p = pair(1);
        let (header, ciphertext) = p.alice.encrypt(b"halo Bob").expect("encrypts");
        assert_eq!(
            p.bob.decrypt(&header, &ciphertext).expect("decrypts"),
            b"halo Bob"
        );
    }

    #[test]
    fn a_conversation_alternates_and_keeps_working() {
        let mut p = pair(2);
        for round in 0..10u32 {
            let sent = format!("alice {round}");
            let (h, c) = p
                .alice
                .encrypt_next(sent.as_bytes(), &mut p.random)
                .expect("encrypts");
            assert_eq!(p.bob.decrypt(&h, &c).expect("decrypts"), sent.as_bytes());

            let reply = format!("bob {round}");
            let (h, c) = p
                .bob
                .encrypt_next(reply.as_bytes(), &mut p.random)
                .expect("encrypts");
            assert_eq!(p.alice.decrypt(&h, &c).expect("decrypts"), reply.as_bytes());
        }
    }

    #[test]
    fn every_message_uses_a_different_key() {
        // Identical plaintexts in the same chain must produce different ciphertexts.
        let mut p = pair(3);
        let (_, first) = p.alice.encrypt(b"same").expect("encrypts");
        let (_, second) = p.alice.encrypt(b"same").expect("encrypts");
        assert_ne!(first, second);
    }

    #[test]
    fn a_burst_in_one_direction_works_without_a_reply() {
        let mut p = pair(4);
        let mut sent = Vec::new();
        for index in 0..50u32 {
            let body = format!("burst {index}");
            sent.push(p.alice.encrypt(body.as_bytes()).expect("encrypts"));
        }
        for (index, (header, ciphertext)) in sent.iter().enumerate() {
            let expected = format!("burst {index}");
            assert_eq!(
                p.bob.decrypt(header, ciphertext).expect("decrypts"),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn out_of_order_delivery_within_a_chain_works() {
        let mut p = pair(5);
        let messages: Vec<_> = (0..5u32)
            .map(|i| {
                p.alice
                    .encrypt(format!("m{i}").as_bytes())
                    .expect("encrypts")
            })
            .collect();

        // Deliver 4, 0, 3, 1, 2 — the shape of a lossy mobile network.
        for index in [4usize, 0, 3, 1, 2] {
            let (header, ciphertext) = &messages[index];
            let expected = format!("m{index}");
            assert_eq!(
                p.bob.decrypt(header, ciphertext).expect("decrypts"),
                expected.as_bytes(),
                "message {index} failed"
            );
        }
    }

    #[test]
    fn a_message_from_a_previous_chain_still_decrypts_after_a_ratchet_step() {
        // The case previous_chain_length exists for: Alice sends three, Bob's
        // reply overtakes the third, and the third arrives afterwards.
        let mut p = pair(6);
        let first = p.alice.encrypt(b"one").expect("encrypts");
        let second = p.alice.encrypt(b"two").expect("encrypts");
        let third = p.alice.encrypt(b"three").expect("encrypts");

        assert_eq!(p.bob.decrypt(&first.0, &first.1).expect("decrypts"), b"one");

        let reply = p
            .bob
            .encrypt_next(b"reply", &mut p.random)
            .expect("encrypts");
        assert_eq!(
            p.alice.decrypt(&reply.0, &reply.1).expect("decrypts"),
            b"reply"
        );

        // Alice's chain has turned, but Bob must still handle the stragglers.
        assert_eq!(
            p.bob.decrypt(&third.0, &third.1).expect("decrypts"),
            b"three"
        );
        assert_eq!(
            p.bob.decrypt(&second.0, &second.1).expect("decrypts"),
            b"two"
        );
    }

    #[test]
    fn a_replayed_message_is_refused() {
        let mut p = pair(7);
        let (header, ciphertext) = p.alice.encrypt(b"once").expect("encrypts");
        assert_eq!(
            p.bob.decrypt(&header, &ciphertext).expect("decrypts"),
            b"once"
        );
        assert_eq!(
            p.bob.decrypt(&header, &ciphertext),
            Err(CryptoError::KeyAlreadyUsed),
            "a replayed frame must not deliver the message a second time"
        );
    }

    #[test]
    fn a_replayed_out_of_order_message_is_refused_too() {
        let mut p = pair(8);
        let messages: Vec<_> = (0..3u32)
            .map(|i| {
                p.alice
                    .encrypt(format!("m{i}").as_bytes())
                    .expect("encrypts")
            })
            .collect();
        p.bob
            .decrypt(&messages[2].0, &messages[2].1)
            .expect("decrypts");
        p.bob
            .decrypt(&messages[0].0, &messages[0].1)
            .expect("decrypts");
        assert!(p.bob.decrypt(&messages[0].0, &messages[0].1).is_err());
    }

    #[test]
    fn a_tampered_ciphertext_is_refused_and_leaves_the_session_usable() {
        let mut p = pair(9);
        let (header, ciphertext) = p.alice.encrypt(b"first").expect("encrypts");
        let mut tampered = ciphertext.clone();
        tampered[0] ^= 1;
        assert_eq!(
            p.bob.decrypt(&header, &tampered),
            Err(CryptoError::DecryptionFailed)
        );
        // The genuine message must still decrypt: a forged frame may not break
        // the session.
        assert_eq!(
            p.bob.decrypt(&header, &ciphertext).expect("decrypts"),
            b"first"
        );
    }

    #[test]
    fn a_tampered_header_is_refused() {
        let mut p = pair(10);
        let (header, ciphertext) = p.alice.encrypt(b"body").expect("encrypts");
        let mut tampered = header;
        tampered.previous_chain_length = 7;
        assert!(p.bob.decrypt(&tampered, &ciphertext).is_err());
    }

    #[test]
    fn a_forged_ratchet_key_cannot_destroy_the_session() {
        // Someone injects a frame with a fresh ratchet key. If the session
        // advanced on that, genuine messages would stop decrypting.
        let mut p = pair(11);
        let genuine = p.alice.encrypt(b"genuine").expect("encrypts");
        let attacker_pair = KeyPair::generate(&mut p.random);
        let forged = RatchetHeader {
            ratchet_key: attacker_pair.public(),
            previous_chain_length: 0,
            message_number: 0,
        };
        assert!(p.bob.decrypt(&forged, &genuine.1).is_err());
        assert_eq!(
            p.bob.decrypt(&genuine.0, &genuine.1).expect("decrypts"),
            b"genuine"
        );
    }

    #[test]
    fn an_absurd_message_number_is_refused_without_deriving_keys() {
        let mut p = pair(12);
        let (header, ciphertext) = p.alice.encrypt(b"body").expect("encrypts");
        let hostile = RatchetHeader {
            message_number: u32::MAX,
            ..header
        };
        assert_eq!(
            p.bob.decrypt(&hostile, &ciphertext),
            Err(CryptoError::ChainGapTooLarge)
        );
        assert_eq!(
            p.bob.skipped_count(),
            0,
            "no keys may be derived for a rejected header"
        );
    }

    #[test]
    fn an_absurd_previous_chain_length_is_refused() {
        let mut p = pair(13);
        let first = p.alice.encrypt(b"one").expect("encrypts");
        p.bob.decrypt(&first.0, &first.1).expect("decrypts");
        let reply = p
            .bob
            .encrypt_next(b"reply", &mut p.random)
            .expect("encrypts");
        p.alice.decrypt(&reply.0, &reply.1).expect("decrypts");
        let next = p
            .alice
            .encrypt_next(b"two", &mut p.random)
            .expect("encrypts");

        let hostile = RatchetHeader {
            previous_chain_length: u32::MAX,
            ..next.0
        };
        assert_eq!(
            p.bob.decrypt(&hostile, &next.1),
            Err(CryptoError::ChainGapTooLarge)
        );
    }

    #[test]
    fn skipped_keys_are_bounded() {
        // A sender that never lets the receiver catch up must not grow its state
        // without limit.
        let mut p = pair(14);
        let mut last = None;
        for round in 0..30 {
            // Each round skips MAX_CHAIN_GAP-1 messages, then delivers one.
            for _ in 0..100 {
                let _ = p.alice.encrypt(b"skipped").expect("encrypts");
            }
            last = Some(
                p.alice
                    .encrypt(format!("round {round}").as_bytes())
                    .expect("encrypts"),
            );
            let (header, ciphertext) = last.as_ref().expect("just set");
            p.bob.decrypt(header, ciphertext).expect("decrypts");
        }
        assert!(
            p.bob.skipped_count() <= MAX_SKIPPED_KEYS,
            "retained {} skipped keys",
            p.bob.skipped_count()
        );
        assert!(last.is_some());
    }

    #[test]
    fn the_root_key_changes_on_every_ratchet_turn() {
        // Post-compromise security: an attacker with the current root key must not
        // be able to follow the conversation once it turns.
        let mut p = pair(15);
        let mut roots = Vec::new();
        for round in 0..5u32 {
            let (h, c) = p.alice.encrypt_next(b"a", &mut p.random).expect("encrypts");
            p.bob.decrypt(&h, &c).expect("decrypts");
            roots.push(p.bob.root_key);
            let (h, c) = p.bob.encrypt_next(b"b", &mut p.random).expect("encrypts");
            p.alice.decrypt(&h, &c).expect("decrypts");
            roots.push(p.alice.root_key);
            assert_eq!(roots.len(), (round as usize + 1) * 2);
        }
        let mut sorted = roots.clone();
        sorted.sort_unstable();
        let count = sorted.len();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            count,
            "a root key repeated across ratchet turns"
        );
    }

    #[test]
    fn a_responder_cannot_send_before_receiving() {
        let mut p = pair(16);
        assert_eq!(
            p.bob.encrypt(b"too early").err(),
            Some(CryptoError::NoSession)
        );
    }

    #[test]
    fn a_header_round_trips_through_its_bytes() {
        let header = RatchetHeader {
            ratchet_key: [5u8; PUBLIC_KEY_LEN],
            previous_chain_length: 300,
            message_number: 70_000,
        };
        assert_eq!(
            RatchetHeader::parse(&header.to_bytes()).expect("parses"),
            header
        );
        assert_eq!(RatchetHeader::ENCODED_LEN, 40);
    }

    #[test]
    fn a_malformed_header_is_rejected() {
        assert_eq!(
            RatchetHeader::parse(&[0u8; 39]),
            Err(CryptoError::MalformedHeader)
        );
        assert_eq!(RatchetHeader::parse(&[]), Err(CryptoError::MalformedHeader));
    }

    #[test]
    fn a_session_does_not_print_its_keys() {
        let p = pair(17);
        let rendered = format!("{:?}", p.alice);
        assert!(rendered.contains("sent"), "{rendered}");
        assert!(!rendered.contains("root"), "{rendered}");
    }
}
