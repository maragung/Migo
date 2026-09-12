//! Sender keys — group messaging without quadratic cost.
//!
//! A pairwise Double Ratchet in a 200-member group means encrypting every message
//! 199 times. At messenger-era group sizes that is the difference between a message
//! that sends and a message that times out on a 2G connection.
//!
//! Instead, each sender keeps one symmetric chain per group. The chain key is
//! distributed once to each member *over the pairwise E2E channels*, so the
//! server never sees it, and after that a message is encrypted once and fanned out
//! to everyone. Cost per message becomes O(1) in the sender's work and O(1) in
//! bandwidth, with the O(n) cost paid only when the key distribution changes.
//!
//! # What this gives up, and what replaces it
//!
//! A sender key has forward secrecy — chain keys advance and old ones are deleted
//! — but no post-compromise security. Stealing a member's current chain key lets
//! the thief read that sender's future messages until the key is replaced. The
//! ratchet cannot heal on its own here, because there is no pairwise DH exchange
//! to mix in.
//!
//! The replacement is rotation, and it is not optional:
//!
//! * When a member **leaves or is removed**, every remaining sender distributes a
//!   fresh chain. Otherwise the departed member keeps reading the group.
//! * After [`MAX_MESSAGES_PER_CHAIN`] messages, so a compromise has a bounded
//!   window even in a group where nobody ever leaves.
//!
//! Each rotation is labeled by a
//! [`group_key_epoch`](SenderKeyState::group_key_epoch) that rises
//! monotonically with it, so a receiver can tell a deliberate re-key from a
//! resent old chain even before it holds the new distribution. Section 163 makes
//! the two one requirement: every membership change raises the epoch *and*
//! distributes a new sender key, because the member who left must not read what
//! follows — the fresh chain key is what enforces that, and the epoch is the
//! number the envelope carries so the change itself is visible on the wire.
//! The number travels in the distribution too, and the receiver's half of
//! "the epoch never regresses" is [`ReceiverKeyState::adopt`]: a distribution
//! that does not advance the generation the receiver already holds is refused
//! rather than installed, so a member who re-sends an old distribution cannot
//! strand a peer on a dead chain.
//!
//! Rotation on removal is a correctness requirement, not a policy knob. A group
//! implementation that skips it has a member who left in March still reading
//! messages in August.
//!
//! # Signing
//!
//! Symmetric keys prove only that *somebody in the group* wrote the message —
//! every member holds the chain key, so any member could forge another's message.
//! Each message therefore carries an Ed25519 signature from the sender's identity
//! key. Without it, group authorship is unverifiable, which in a moderation
//! context means a member can fabricate a message attributed to someone else.

use migo_core::Random;
use zeroize::Zeroize;

use crate::aead::{self, SymmetricKey, NONCE_LEN};
use crate::error::{CryptoError, Result};
use crate::identity::{IdentityPublic, IdentitySecret, SIGNATURE_LEN};
use crate::kdf;

/// Messages a single chain may produce before it must be rotated.
pub const MAX_MESSAGES_PER_CHAIN: u32 = 2_000;

/// How far ahead of the receiver a message may claim to be.
pub const MAX_CHAIN_GAP: u32 = 1_000;

/// Domain separator for a group message signature.
const GROUP_DOMAIN: &[u8] = b"migo-sender-key-v1";

/// The distribution message a sender hands to each group member.
///
/// Travels inside the pairwise E2E channel, never in the clear, and never through
/// a code path that could log it. `chain_key` is secret material; everything else
/// in this struct is not.
pub struct SenderKeyDistribution {
    /// The membership generation this chain belongs to. Rises on every
    /// rotation, never repeats, never regresses — and travels *in* the
    /// distribution, because the receiver's whole defence against a stale one
    /// is comparing this number against the generation it already holds.
    pub group_key_epoch: u32,
    /// Which chain this is, so a rotation can be distinguished from a resend.
    pub chain_id: u32,
    /// The message number the chain key corresponds to.
    ///
    /// A member who joins mid-conversation receives the chain key as of *now*, not
    /// from the beginning. That is deliberate: a new member must not be able to
    /// decrypt history they were not present for.
    pub message_number: u32,
    /// The chain key itself.
    pub chain_key: [u8; 32],
    /// The sender's identity, for verifying its signatures.
    pub identity: IdentityPublic,
}

impl Drop for SenderKeyDistribution {
    fn drop(&mut self) {
        self.chain_key.zeroize();
    }
}

impl core::fmt::Debug for SenderKeyDistribution {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SenderKeyDistribution")
            .field("group_key_epoch", &self.group_key_epoch)
            .field("chain_id", &self.chain_id)
            .field("message_number", &self.message_number)
            .field("chain_key", &"***")
            .finish()
    }
}

/// The header on a group message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderKeyHeader {
    /// Which chain of the sender's this message belongs to.
    pub chain_id: u32,
    /// Index within that chain.
    pub message_number: u32,
}

impl SenderKeyHeader {
    /// Encoded length: two big-endian `u32`s.
    pub const ENCODED_LEN: usize = 8;

    /// Serialises the header. Fixed-width, because it is authenticated.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..4].copy_from_slice(&self.chain_id.to_be_bytes());
        out[4..].copy_from_slice(&self.message_number.to_be_bytes());
        out
    }

    /// Parses a header.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(CryptoError::MalformedHeader);
        }
        Ok(Self {
            chain_id: u32::from_be_bytes(bytes[..4].try_into().expect("four bytes")),
            message_number: u32::from_be_bytes(bytes[4..].try_into().expect("four bytes")),
        })
    }
}

/// A sealed group message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKeyMessage {
    /// Chain and position.
    pub header: SenderKeyHeader,
    /// AEAD output, without a nonce prefix — the nonce is derived.
    pub ciphertext: Vec<u8>,
    /// The sender's signature over the header and the ciphertext.
    pub signature: [u8; SIGNATURE_LEN],
}

/// The sending half: one per group, held by the sender.
pub struct SenderKeyState {
    /// The membership generation this chain belongs to. Rises on every
    /// [`rotate`](SenderKeyState::rotate), never repeats, never regresses.
    group_key_epoch: u32,
    chain_id: u32,
    chain_key: [u8; 32],
    message_number: u32,
}

impl Drop for SenderKeyState {
    fn drop(&mut self) {
        self.chain_key.zeroize();
    }
}

impl SenderKeyState {
    /// Starts a fresh chain under `group_key_epoch`.
    ///
    /// The caller owns the numbering: the epoch counts the group's membership
    /// generations, so it starts at 1 on the first chain and rises by one per
    /// membership change. The value travels in the caller's envelope, not in the
    /// chain's own bytes.
    pub fn create(group_key_epoch: u32, chain_id: u32, random: &mut dyn Random) -> Self {
        let mut chain_key = [0u8; 32];
        random.fill_bytes(&mut chain_key);
        Self {
            group_key_epoch,
            chain_id,
            chain_key,
            message_number: 0,
        }
    }

    /// The membership generation this chain belongs to.
    #[must_use]
    pub fn group_key_epoch(&self) -> u32 {
        self.group_key_epoch
    }

    /// Rotates: a fresh chain key under the next epoch, for a membership change
    /// (section 163) or a chain that reached its message bound.
    ///
    /// The new chain starts at message 0 under `chain_id`, which the caller
    /// mints fresh so a receiver still holding the old chain can tell the two
    /// apart. The epoch rises by one and saturates rather than wrapping — an
    /// epoch that rolled over to a smaller number would make an old chain look
    /// current again, which is precisely the confusion the number exists to
    /// prevent. After rotating, the caller distributes the new chain with
    /// [`distribution`](Self::distribution).
    pub fn rotate(&mut self, chain_id: u32, random: &mut dyn Random) -> u32 {
        let mut chain_key = [0u8; 32];
        random.fill_bytes(&mut chain_key);
        self.chain_key.zeroize();
        self.chain_key = chain_key;
        self.chain_id = chain_id;
        self.message_number = 0;
        self.group_key_epoch = self.group_key_epoch.saturating_add(1).max(1);
        self.group_key_epoch
    }

    /// Which chain this state represents.
    #[must_use]
    pub fn chain_id(&self) -> u32 {
        self.chain_id
    }

    /// How many messages this chain has produced.
    #[must_use]
    pub fn message_number(&self) -> u32 {
        self.message_number
    }

    /// True once the chain has reached its rotation bound.
    ///
    /// Callers check this and rotate. It is a bound on the blast radius of a
    /// compromise, so ignoring it means the window is the lifetime of the group.
    #[must_use]
    pub fn needs_rotation(&self) -> bool {
        self.message_number >= MAX_MESSAGES_PER_CHAIN
    }

    /// Builds the distribution message for the chain's current position.
    ///
    /// The distribution names the generation the chain belongs to, so the
    /// receiver can refuse a stale one before any key material moves — see
    /// [`ReceiverKeyState::adopt`].
    #[must_use]
    pub fn distribution(&self, identity: &IdentitySecret) -> SenderKeyDistribution {
        SenderKeyDistribution {
            group_key_epoch: self.group_key_epoch,
            chain_id: self.chain_id,
            message_number: self.message_number,
            chain_key: self.chain_key,
            identity: identity.public(),
        }
    }

    /// Encrypts and signs a group message.
    pub fn encrypt(
        &mut self,
        identity: &IdentitySecret,
        group_context: &[u8],
        plaintext: &[u8],
    ) -> Result<SenderKeyMessage> {
        if self.needs_rotation() {
            // Refusing rather than silently continuing: the caller has a rotation
            // path, and a chain that runs past its bound is exactly the state the
            // bound exists to prevent.
            return Err(CryptoError::KeyAlreadyUsed);
        }
        let header = SenderKeyHeader {
            chain_id: self.chain_id,
            message_number: self.message_number,
        };
        let (key, nonce) = advance_chain(&mut self.chain_key);
        self.message_number += 1;

        let aad = associated_data(group_context, &header);
        let sealed = aead::seal_with_nonce(&key, &nonce, &aad, plaintext)?;
        let ciphertext = sealed[NONCE_LEN..].to_vec();

        // Sign header and ciphertext together, so neither can be moved onto the
        // other. Group authorship depends on this signature and nothing else.
        let mut signed = Vec::with_capacity(aad.len() + ciphertext.len());
        signed.extend_from_slice(&aad);
        signed.extend_from_slice(&ciphertext);
        Ok(SenderKeyMessage {
            header,
            ciphertext,
            signature: identity.sign(GROUP_DOMAIN, &signed),
        })
    }
}

/// The receiving half: one per (group, sender) pair.
pub struct ReceiverKeyState {
    /// The membership generation this state's chain belongs to. The baseline
    /// every later distribution from this sender is measured against; only
    /// [`adopt`](Self::adopt) moves it, and only forward.
    group_key_epoch: u32,
    chain_id: u32,
    chain_key: [u8; 32],
    next_message_number: u32,
    identity: IdentityPublic,
    /// Keys derived for messages that have not arrived yet, oldest first.
    skipped: Vec<(u32, [u8; 32], [u8; NONCE_LEN])>,
}

impl Drop for ReceiverKeyState {
    fn drop(&mut self) {
        self.chain_key.zeroize();
        for entry in &mut self.skipped {
            entry.1.zeroize();
        }
    }
}

impl ReceiverKeyState {
    /// Accepts the *first* distribution a receiver sees from this sender.
    ///
    /// The first distribution is the baseline: there is no earlier generation
    /// to compare against, so any epoch is accepted here and every later one
    /// is measured from it. Freshness of the baseline is vouched for by the
    /// pairwise channel it arrived through, which is the same channel that
    /// carries every later distribution. Once state exists, use
    /// [`adopt`](Self::adopt) — this constructor never refuses anything and
    /// must not be used to replace held state.
    #[must_use]
    pub fn accept(distribution: &SenderKeyDistribution) -> Self {
        Self {
            group_key_epoch: distribution.group_key_epoch,
            chain_id: distribution.chain_id,
            chain_key: distribution.chain_key,
            next_message_number: distribution.message_number,
            identity: distribution.identity,
            skipped: Vec::new(),
        }
    }

    /// Adopts a distribution into state that already tracks this sender.
    ///
    /// This is the receiver's half of "the epoch never regresses" (section
    /// 163). Four cases, in the order they are decided:
    ///
    /// * **Older epoch** — refused with [`CryptoError::KeyAlreadyUsed`]. This
    ///   is the finding-F2 attack itself: a member re-sends a distribution
    ///   from before the last membership change, and if it were installed the
    ///   receiver would sit on a chain nothing further is sealed under. The
    ///   held state is untouched, so the receiver keeps reading on the
    ///   current chain and can still re-sync forward.
    /// * **Same epoch, same chain** — a re-send of the distribution the
    ///   receiver already holds. Not a regression, and not a reason to
    ///   re-install either: re-installing would rewind the receiver's
    ///   position inside the live chain. Succeeds as a no-op.
    /// * **Same epoch, different chain** — refused. A rotation always raises
    ///   the epoch, so a second chain at one generation is a swap, not a
    ///   rotation, and installing it would be exactly the displacement the
    ///   epoch comparison exists to prevent.
    /// * **Newer epoch** — installed. The old chain key and every skipped key
    ///   are zeroized before the new state lands, and the skipped window is
    ///   emptied because message numbers restart with the chain.
    ///
    /// The boundary of this guarantee: refusing a stale distribution protects
    /// *liveness and correctness* of the receiver's chain, not
    /// confidentiality. A stale distribution contains only key material the
    /// holder was legitimately given at some point; the harm being refused is
    /// the stranded receiver, which is a denial of message delivery until
    /// re-sync — not a disclosure.
    pub fn adopt(&mut self, distribution: &SenderKeyDistribution) -> Result<()> {
        if distribution.group_key_epoch < self.group_key_epoch {
            return Err(CryptoError::KeyAlreadyUsed);
        }
        if distribution.group_key_epoch == self.group_key_epoch {
            if distribution.chain_id == self.chain_id {
                return Ok(());
            }
            return Err(CryptoError::KeyAlreadyUsed);
        }
        self.chain_key.zeroize();
        for entry in &mut self.skipped {
            entry.1.zeroize();
        }
        self.skipped.clear();
        self.group_key_epoch = distribution.group_key_epoch;
        self.chain_id = distribution.chain_id;
        self.chain_key = distribution.chain_key;
        self.next_message_number = distribution.message_number;
        self.identity = distribution.identity;
        Ok(())
    }

    /// The membership generation this state holds.
    ///
    /// The number a later distribution is compared against. Not secret, but
    /// not currently on the wire either — the comparison happens at adoption
    /// time, inside the receiver.
    #[must_use]
    pub fn group_key_epoch(&self) -> u32 {
        self.group_key_epoch
    }

    /// Which chain this state tracks.
    #[must_use]
    pub fn chain_id(&self) -> u32 {
        self.chain_id
    }

    /// How many out-of-order keys are retained.
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// Verifies and decrypts a group message.
    ///
    /// The signature is checked *before* any key derivation. A forged message
    /// should cost the receiver one signature verification, not a thousand KDF
    /// steps, and checking the cheap authentication first is what makes that true.
    pub fn decrypt(&mut self, group_context: &[u8], message: &SenderKeyMessage) -> Result<Vec<u8>> {
        if message.header.chain_id != self.chain_id {
            // A different chain means a rotation this receiver has not been told
            // about. The caller fetches the new distribution message and retries.
            return Err(CryptoError::NoSession);
        }
        let aad = associated_data(group_context, &message.header);
        let mut signed = Vec::with_capacity(aad.len() + message.ciphertext.len());
        signed.extend_from_slice(&aad);
        signed.extend_from_slice(&message.ciphertext);
        self.identity
            .verify(GROUP_DOMAIN, &signed, &message.signature)?;

        let number = message.header.message_number;
        if let Some(index) = self.skipped.iter().position(|(n, _, _)| *n == number) {
            let (_, key, nonce) = self.skipped.remove(index);
            let key = SymmetricKey::from_bytes(key);
            return aead::open_with_nonce(&key, &nonce, &aad, &message.ciphertext);
        }
        if number < self.next_message_number {
            return Err(CryptoError::KeyAlreadyUsed);
        }
        let gap = number - self.next_message_number;
        if gap > MAX_CHAIN_GAP {
            return Err(CryptoError::ChainGapTooLarge);
        }

        let mut pending = Vec::with_capacity(gap as usize);
        for offset in 0..gap {
            let (key, nonce) = advance_chain(&mut self.chain_key);
            pending.push((self.next_message_number + offset, *key.expose(), nonce));
        }
        let (key, nonce) = advance_chain(&mut self.chain_key);
        let plaintext = aead::open_with_nonce(&key, &nonce, &aad, &message.ciphertext)?;

        self.skipped.extend(pending);
        // Bounded, oldest evicted first, for the same reason as the pairwise
        // ratchet: a sender who never fills the gaps must not grow this forever.
        while self.skipped.len() > MAX_CHAIN_GAP as usize {
            let mut evicted = self.skipped.remove(0);
            evicted.1.zeroize();
        }
        self.next_message_number = number + 1;
        Ok(plaintext)
    }
}

/// Group context and header, authenticated on every message.
///
/// The group id is in here so a ciphertext cannot be lifted from one group and
/// replayed into another where the same sender is also a member.
fn associated_data(group_context: &[u8], header: &SenderKeyHeader) -> Vec<u8> {
    let mut out = Vec::with_capacity(group_context.len() + SenderKeyHeader::ENCODED_LEN);
    out.extend_from_slice(group_context);
    out.extend_from_slice(&header.to_bytes());
    out
}

/// Advances the chain and yields the message key and nonce.
fn advance_chain(chain: &mut [u8; 32]) -> (SymmetricKey, [u8; NONCE_LEN]) {
    let (next, material) = kdf::derive_pair::<32, 56>(chain, None, kdf::LABEL_SENDER_CHAIN);
    chain.zeroize();
    *chain = next;
    let mut key = [0u8; 32];
    key.copy_from_slice(&material[..32]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&material[32..]);
    (SymmetricKey::from_bytes(key), nonce)
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    const GROUP: &[u8] = b"group-01H0000000000000000000000";

    struct Group {
        sender_identity: IdentitySecret,
        sender: SenderKeyState,
        receiver: ReceiverKeyState,
        random: SeededRandom,
    }

    fn group(seed: u64) -> Group {
        let mut random = SeededRandom::new(seed);
        let sender_identity = IdentitySecret::generate(&mut random);
        let sender = SenderKeyState::create(1, 1, &mut random);
        let receiver = ReceiverKeyState::accept(&sender.distribution(&sender_identity));
        Group {
            sender_identity,
            sender,
            receiver,
            random,
        }
    }

    #[test]
    fn a_message_round_trips() {
        let mut g = group(1);
        let message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"halo semua")
            .expect("encrypts");
        assert_eq!(
            g.receiver.decrypt(GROUP, &message).expect("decrypts"),
            b"halo semua"
        );
    }

    #[test]
    fn one_ciphertext_serves_every_member() {
        // The whole point: encrypt once, and every member's receiver state opens
        // the same bytes.
        let mut g = group(2);
        let distribution = g.sender.distribution(&g.sender_identity);
        let mut members: Vec<_> = (0..20)
            .map(|_| ReceiverKeyState::accept(&distribution))
            .collect();
        let message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"satu pesan")
            .expect("encrypts");
        for member in &mut members {
            assert_eq!(
                member.decrypt(GROUP, &message).expect("decrypts"),
                b"satu pesan"
            );
        }
    }

    #[test]
    fn a_sequence_of_messages_round_trips() {
        let mut g = group(3);
        for index in 0..100u32 {
            let body = format!("pesan {index}");
            let message = g
                .sender
                .encrypt(&g.sender_identity, GROUP, body.as_bytes())
                .expect("encrypts");
            assert_eq!(
                g.receiver.decrypt(GROUP, &message).expect("decrypts"),
                body.as_bytes()
            );
        }
    }

    #[test]
    fn out_of_order_delivery_works() {
        let mut g = group(4);
        let messages: Vec<_> = (0..5u32)
            .map(|i| {
                g.sender
                    .encrypt(&g.sender_identity, GROUP, format!("m{i}").as_bytes())
                    .expect("encrypts")
            })
            .collect();
        for index in [3usize, 0, 4, 2, 1] {
            let expected = format!("m{index}");
            assert_eq!(
                g.receiver
                    .decrypt(GROUP, &messages[index])
                    .expect("decrypts"),
                expected.as_bytes(),
                "message {index}"
            );
        }
    }

    #[test]
    fn a_replayed_message_is_refused() {
        let mut g = group(5);
        let message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"once")
            .expect("encrypts");
        g.receiver.decrypt(GROUP, &message).expect("decrypts");
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::KeyAlreadyUsed)
        );
    }

    #[test]
    fn another_member_cannot_forge_a_message_as_this_sender() {
        // Every member has the chain key, so without the signature any member
        // could write as any other. This is the test that says they cannot.
        let mut g = group(6);
        let impostor = IdentitySecret::generate(&mut g.random);
        let forged = g
            .sender
            .encrypt(&impostor, GROUP, b"attributed to someone else");
        let forged = forged.expect("encrypts, since the chain key is shared");
        assert_eq!(
            g.receiver.decrypt(GROUP, &forged),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_tampered_ciphertext_is_refused() {
        let mut g = group(7);
        let mut message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"body")
            .expect("encrypts");
        message.ciphertext[0] ^= 1;
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_tampered_header_is_refused() {
        let mut g = group(8);
        let mut message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"body")
            .expect("encrypts");
        message.header.message_number = 5;
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_message_cannot_be_replayed_into_another_group() {
        let mut g = group(9);
        let message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"private to group")
            .expect("encrypts");
        assert!(g.receiver.decrypt(b"a-different-group", &message).is_err());
    }

    #[test]
    fn a_rotated_chain_is_reported_rather_than_mis_decrypted() {
        let mut g = group(10);
        let rotated = SenderKeyState::create(2, 2, &mut g.random);
        let mut rotated = rotated;
        let message = rotated
            .encrypt(&g.sender_identity, GROUP, b"new chain")
            .expect("encrypts");
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::NoSession)
        );
    }

    #[test]
    fn a_new_member_cannot_read_history() {
        // The distribution message carries the chain key as of now. A member who
        // joins at message 5 must not be able to open messages 0 through 4.
        let mut g = group(11);
        let old: Vec<_> = (0..5u32)
            .map(|i| {
                g.sender
                    .encrypt(&g.sender_identity, GROUP, format!("old {i}").as_bytes())
                    .expect("encrypts")
            })
            .collect();
        let mut newcomer = ReceiverKeyState::accept(&g.sender.distribution(&g.sender_identity));
        for message in &old {
            assert!(
                newcomer.decrypt(GROUP, message).is_err(),
                "a new member decrypted message {} from before they joined",
                message.header.message_number
            );
        }
        let fresh = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"after joining")
            .expect("encrypts");
        assert_eq!(
            newcomer.decrypt(GROUP, &fresh).expect("decrypts"),
            b"after joining"
        );
    }

    #[test]
    fn an_absurd_message_number_is_refused() {
        let mut g = group(12);
        let mut message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"body")
            .expect("encrypts");
        // Re-sign so the failure is the gap bound rather than the signature.
        message.header.message_number = u32::MAX;
        let aad = associated_data(GROUP, &message.header);
        let mut signed = aad.clone();
        signed.extend_from_slice(&message.ciphertext);
        message.signature = g.sender_identity.sign(GROUP_DOMAIN, &signed);
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::ChainGapTooLarge)
        );
    }

    #[test]
    fn a_chain_refuses_to_run_past_its_rotation_bound() {
        let mut g = group(13);
        // Fast-forward to the bound without encrypting two thousand messages.
        for _ in 0..MAX_MESSAGES_PER_CHAIN {
            let (_, _) = advance_chain(&mut g.sender.chain_key);
            g.sender.message_number += 1;
        }
        assert!(g.sender.needs_rotation());
        assert_eq!(
            g.sender
                .encrypt(&g.sender_identity, GROUP, b"one too many")
                .err(),
            Some(CryptoError::KeyAlreadyUsed)
        );
    }

    #[test]
    fn skipped_keys_are_bounded() {
        let mut g = group(14);
        // Three rounds of 501 messages: 1500 keys get stashed, which is past the
        // retention bound, while the chain itself stays under its rotation bound
        // so the sender is not the thing that stops first.
        for _ in 0..3 {
            for _ in 0..500 {
                let _ = g
                    .sender
                    .encrypt(&g.sender_identity, GROUP, b"skipped")
                    .expect("encrypts");
            }
            let delivered = g
                .sender
                .encrypt(&g.sender_identity, GROUP, b"delivered")
                .expect("encrypts");
            g.receiver.decrypt(GROUP, &delivered).expect("decrypts");
        }
        // Exactly at the bound, not merely below it: an assertion that only says
        // `<=` would still pass if eviction had never run at all.
        assert_eq!(g.receiver.skipped_count(), MAX_CHAIN_GAP as usize);
    }

    #[test]
    fn a_header_round_trips() {
        let header = SenderKeyHeader {
            chain_id: 7,
            message_number: 1_000_000,
        };
        assert_eq!(
            SenderKeyHeader::parse(&header.to_bytes()).expect("parses"),
            header
        );
        assert_eq!(
            SenderKeyHeader::parse(&[0u8; 7]),
            Err(CryptoError::MalformedHeader)
        );
    }

    #[test]
    fn a_distribution_message_does_not_print_its_key() {
        let g = group(15);
        let rendered = format!("{:?}", g.sender.distribution(&g.sender_identity));
        assert!(rendered.contains("***"), "{rendered}");
    }

    #[test]
    fn rotation_raises_the_epoch_and_mints_a_fresh_chain() {
        let mut g = group(16);
        assert_eq!(g.sender.group_key_epoch(), 1);
        let new_epoch = g.sender.rotate(2, &mut g.random);
        assert_eq!(new_epoch, 2);
        assert_eq!(g.sender.chain_id(), 2);
        // A member who joins the new chain receives its distribution at the
        // position it starts from, which is before the first message is sealed.
        // Once that distribution lands, the messages flow again.
        let mut caught_up = ReceiverKeyState::accept(&g.sender.distribution(&g.sender_identity));
        let message = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"after the change")
            .expect("encrypts");
        assert_eq!(
            g.receiver.decrypt(GROUP, &message),
            Err(CryptoError::NoSession)
        );
        assert_eq!(
            caught_up.decrypt(GROUP, &message).expect("decrypts"),
            b"after the change"
        );
    }

    #[test]
    fn a_member_who_left_cannot_read_after_the_rekey() {
        // Section 163's own property: a departed member holds the old chain and
        // never receives the new distribution, so nothing sealed after the
        // membership change opens for them.
        let mut g = group(17);
        let mut departed = ReceiverKeyState::accept(&g.sender.distribution(&g.sender_identity));
        g.sender.rotate(9, &mut g.random);
        let after = g
            .sender
            .encrypt(&g.sender_identity, GROUP, b"without them")
            .expect("encrypts");
        assert_eq!(departed.decrypt(GROUP, &after), Err(CryptoError::NoSession));
    }

    #[test]
    fn the_epoch_saturates_instead_of_rolling_over() {
        let mut random = SeededRandom::new(18);
        let mut sender = SenderKeyState::create(u32::MAX, 1, &mut random);
        let epoch = sender.rotate(2, &mut random);
        // Saturated, not wrapped: an epoch that rolled over to a smaller number
        // would make an old chain look current again.
        assert_eq!(epoch, u32::MAX);
    }

    #[test]
    fn a_stale_distribution_does_not_displace_a_current_one() {
        // Finding F2: a member re-sends a distribution from before the last
        // membership change. The receiver must refuse it, keep the chain it
        // holds, and keep reading on it — the refusal protects delivery, not
        // secrecy, and the test must not pretend otherwise.
        let mut random = SeededRandom::new(19);
        let identity = IdentitySecret::generate(&mut random);
        let mut sender = SenderKeyState::create(1, 1, &mut random);
        let stale = sender.distribution(&identity);
        let mut receiver = ReceiverKeyState::accept(&stale);

        sender.rotate(2, &mut random);
        receiver
            .adopt(&sender.distribution(&identity))
            .expect("the current distribution is adopted");
        let current_epoch = receiver.group_key_epoch();

        assert_eq!(
            receiver.adopt(&stale),
            Err(CryptoError::KeyAlreadyUsed),
            "a stale distribution was installed over a current one"
        );
        assert_eq!(
            receiver.group_key_epoch(),
            current_epoch,
            "a refused distribution must not move the epoch either"
        );
        let message = sender
            .encrypt(&identity, GROUP, b"still readable")
            .expect("encrypts");
        assert_eq!(
            receiver.decrypt(GROUP, &message).expect("decrypts"),
            b"still readable",
            "the receiver was stranded on the dead chain"
        );
    }

    #[test]
    fn an_adopted_distribution_re_syncs_the_receiver_forward() {
        // The other half of the guarantee: refusal must not become a trap. A
        // receiver who missed a rotation re-syncs by adopting the new
        // distribution, and the epoch only ever moves forward.
        let mut random = SeededRandom::new(20);
        let identity = IdentitySecret::generate(&mut random);
        let mut sender = SenderKeyState::create(1, 1, &mut random);
        let mut receiver = ReceiverKeyState::accept(&sender.distribution(&identity));
        assert_eq!(receiver.group_key_epoch(), 1);

        sender.rotate(2, &mut random);
        // The distribution is taken at rotation time, before anything is
        // sealed under the new chain: it names the position the chain starts
        // at, and adopting it afterwards lets the receiver derive the keys for
        // the messages it missed. Taking it after the missed message would
        // name one position further on, and the receiver would rightly refuse
        // the missed message as a replay — a frame numbered behind the chain
        // head is indistinguishable from one, which is decrypt's own guard.
        let resync = sender.distribution(&identity);
        let undelivered = sender
            .encrypt(&identity, GROUP, b"missed rotation")
            .expect("encrypts");
        assert_eq!(
            receiver.decrypt(GROUP, &undelivered),
            Err(CryptoError::NoSession),
            "a message from the new chain must not mis-decrypt on the old one"
        );

        receiver
            .adopt(&resync)
            .expect("a newer distribution is adopted");
        assert_eq!(receiver.group_key_epoch(), 2);
        assert_eq!(
            receiver.decrypt(GROUP, &undelivered).expect("decrypts"),
            b"missed rotation"
        );
    }

    #[test]
    fn a_resent_distribution_is_not_a_regression() {
        // Same epoch, same chain: a re-send of what the receiver already
        // holds succeeds as a no-op, and the no-op must not rewind the
        // receiver's position inside the live chain. The re-send carries the
        // position the chain started at, so an implementation that
        // re-installed instead of no-opping would set the receiver back to
        // message 0 and un-refuse the replay below.
        let mut random = SeededRandom::new(21);
        let identity = IdentitySecret::generate(&mut random);
        let mut sender = SenderKeyState::create(3, 1, &mut random);
        let resent = sender.distribution(&identity);
        let mut receiver = ReceiverKeyState::accept(&sender.distribution(&identity));

        let delivered = sender
            .encrypt(&identity, GROUP, b"the one message")
            .expect("encrypts");
        receiver.decrypt(GROUP, &delivered).expect("decrypts");

        receiver
            .adopt(&resent)
            .expect("a re-send of the current distribution is not a regression");

        assert_eq!(
            receiver.decrypt(GROUP, &delivered),
            Err(CryptoError::KeyAlreadyUsed),
            "the no-op re-adoption rewound the chain position and un-refused a replay"
        );
        let fresh = sender
            .encrypt(&identity, GROUP, b"still flowing")
            .expect("encrypts");
        assert_eq!(
            receiver.decrypt(GROUP, &fresh).expect("decrypts"),
            b"still flowing"
        );
    }

    #[test]
    fn a_second_chain_at_the_same_epoch_is_refused() {
        // A rotation always raises the epoch, so two chains at one generation
        // cannot both be legitimate. The second one is a swap wearing a
        // distribution's clothes, and must be refused.
        let mut random = SeededRandom::new(22);
        let identity = IdentitySecret::generate(&mut random);
        let mut sender = SenderKeyState::create(1, 1, &mut random);
        let mut receiver = ReceiverKeyState::accept(&sender.distribution(&identity));

        let impostor = SenderKeyState::create(1, 2, &mut random);
        assert_eq!(
            receiver.adopt(&impostor.distribution(&identity)),
            Err(CryptoError::KeyAlreadyUsed),
            "a second chain at the same epoch displaced the current one"
        );
        let message = sender
            .encrypt(&identity, GROUP, b"the real chain")
            .expect("encrypts");
        assert_eq!(
            receiver.decrypt(GROUP, &message).expect("decrypts"),
            b"the real chain"
        );
    }

    #[test]
    fn the_first_distribution_is_the_baseline_whatever_epoch_it_names() {
        // A receiver who joins a group mid-life first hears from a sender at
        // whatever generation the group is on. That first distribution is the
        // baseline — accepted without comparison — and everything after it is
        // measured from there, including refusals.
        let mut random = SeededRandom::new(23);
        let identity = IdentitySecret::generate(&mut random);
        let mut sender = SenderKeyState::create(5, 7, &mut random);
        let mut latecomer = ReceiverKeyState::accept(&sender.distribution(&identity));
        assert_eq!(latecomer.group_key_epoch(), 5);

        let message = sender
            .encrypt(&identity, GROUP, b"joined mid-life")
            .expect("encrypts");
        assert_eq!(
            latecomer.decrypt(GROUP, &message).expect("decrypts"),
            b"joined mid-life"
        );

        // And measured from that baseline: an epoch-4 distribution, which a
        // founding member would rightly refuse, is refused here too.
        let older = SenderKeyState::create(4, 6, &mut random);
        assert_eq!(
            latecomer.adopt(&older.distribution(&identity)),
            Err(CryptoError::KeyAlreadyUsed)
        );
        assert_eq!(latecomer.group_key_epoch(), 5);
    }
}
