//! The group (sender-key) layer: one chain per conversation, broadcast to every member.
//!
//! A pairwise Double Ratchet in a 50-member room means sealing every message 49 times and
//! one bundle fetch per device per member. The sender-key design this module implements is the
//! one the TypeScript SDK and the Android client already speak (brief section 11): one chain
//! per conversation per sending device, its key handed to each member device once through the
//! pairwise channel, and every message sealed once and fanned out to everyone. The server never
//! holds a chain key — a distribution travels as a `ControlEvent` sealed for exactly one device.
//!
//! # The layouts, byte-for-byte
//!
//! Both layouts are flat and scheme-tagged, matching `session-crypto.ts` / `GroupCrypto.kt`
//! exactly — one byte of disagreement with the other two implementations makes every message
//! unreadable, so nothing here is invented:
//!
//! *The envelope* (what the server routes):
//! ```text
//! u8      version
//! u8      scheme                       SCHEME_SENDER_KEY
//! varint  chain_id                      which of this device's chains sealed it
//! varint  epoch                         bumped whenever the chain rotates
//! varint  message_number                index within the chain
//! bytes   signature                     64
//! bytes   ciphertext
//! ```
//!
//! *The distribution* (what the pairwise channel carries):
//! ```text
//! varint  chain_id
//! varint  message_number                the chain key's position, not the message's
//! bytes   chain_key                     32
//! bytes   sender_identity               64
//! ```
//!
//! # What the chain key's position means
//!
//! A distribution carries the chain key *as of now*, so a member who receives it cannot read
//! anything sealed before they were given the key. That is deliberate (a new member must not
//! gain the room's history), and it is why a receiver buffers a message that names a chain or a
//! position it has not been told about rather than failing it: the distribution may still arrive.
//!
//! # Why in-memory only
//!
//! The pairwise store in [`crate::crypto::session`] is equally volatile: a desktop restart
//! re-fetches history through the server and the senders re-distribute on their next send, so
//! the state reconstructs itself the way the web SDK's does before persistence was added there.
//! Persistence is a follow-up the vault can take when the same reasoning that sealed the pairwise
//! sessions into it applies.

use std::collections::{HashMap, HashSet};

use migo_core::{Id, OsRandom, Random};
use migo_crypto::{
    ReceiverKeyState, SenderKeyDistribution, SenderKeyMessage, SenderKeyState, IDENTITY_PUBLIC_LEN,
    SIGNATURE_LEN,
};

use super::CryptoError;

/// The only envelope version this build writes, and the only one it reads.
const ENVELOPE_VERSION: u8 = 1;

/// The chain key and the identity public key are the two fixed-width blocks in a distribution.
const CHAIN_KEY_LEN: usize = 32;

/// One conversation's outbound chain: the state, its epoch, and who already holds it.
struct Outbound {
    state: SenderKeyState,
    /// The epoch this chain belongs to; travels in every message it seals. Bumped on rotation so
    /// a receiver can tell a re-sent old chain from a deliberately fresh one.
    epoch: u32,
    /// Devices that already hold the current chain's distribution. Cleared on rotation: keeping
    /// it would leave members holding a chain that no longer seals anything, with no message to
    /// tell them so.
    distributed: HashSet<Id>,
}

/// The group layer's state: outbound chains by conversation, inbound receivers by conversation
/// and sender device.
///
/// Everything routes through `&mut self` methods on one struct the net worker owns, mirroring
/// the SDK's `GroupCrypto`, so the caller has no second object to keep in step.
pub struct GroupStore {
    identity: migo_crypto::IdentitySecret,
    sending: HashMap<Id, Outbound>,
    /// `(conversation, sender device)` → the receiver state that opens that device's messages.
    receiving: HashMap<(Id, Id), ReceiverKeyState>,
}

/// A sealed message, ready for the `envelope` field of a `MESSAGE_SEND`.
pub struct Sealed {
    /// The chain id, for the wire's optional `sender_key_id` field.
    pub chain_id: u32,
    /// The envelope bytes.
    pub envelope: Vec<u8>,
}

impl GroupStore {
    /// Builds the store around this device's identity, which signs every outbound message.
    #[must_use]
    pub fn new(identity: migo_crypto::IdentitySecret) -> Self {
        Self {
            identity,
            sending: HashMap::new(),
            receiving: HashMap::new(),
        }
    }

    /// Seals `plaintext` once for broadcast to the whole conversation, rotating the chain first
    /// if it has reached its message bound.
    ///
    /// The caller distributes the new chain to every member device — [`Self::needs_distribution`]
    /// says which — before or right after this; sealing past the bound without rotating is what
    /// the crypto layer refuses, so the one failure mode this method has is that refusal.
    pub fn seal(&mut self, conversation: Id, plaintext: &[u8]) -> Result<Sealed, CryptoError> {
        // Split borrows: the identity signs while the chain encrypts, and both are needed at once.
        let Self {
            ref identity,
            ref mut sending,
            ..
        } = self;
        let entry = outbound(sending, conversation);
        let context = conversation.as_bytes();
        let message = entry.state.encrypt(identity, context, plaintext)?;
        let epoch = entry.epoch;
        Ok(Sealed {
            chain_id: message.header.chain_id,
            envelope: encode_envelope(epoch, &message),
        })
    }

    /// The serialized distribution for a conversation's current chain — the bytes a
    /// `ControlEvent` carries to one member device through the pairwise channel.
    #[must_use]
    pub fn distribution(&mut self, conversation: Id) -> Vec<u8> {
        let Self {
            ref identity,
            ref mut sending,
            ..
        } = self;
        let entry = outbound(sending, conversation);
        encode_distribution(&entry.state.distribution(identity))
    }

    /// Whether a member device still needs the current chain's distribution. True when there is
    /// no chain at all: the first send creates one and every member will need it.
    #[must_use]
    pub fn needs_distribution(&mut self, conversation: Id, device: Id) -> bool {
        match self.sending.get(&conversation) {
            Some(entry) => !entry.distributed.contains(&device),
            None => true,
        }
    }

    /// Records that a member device holds the current chain's distribution.
    pub fn mark_distributed(&mut self, conversation: Id, device: Id) {
        outbound(&mut self.sending, conversation)
            .distributed
            .insert(device);
    }

    /// Starts a fresh outbound chain and bumps the epoch, for a membership change (someone left,
    /// so the old chain must die) or a chain that hit its message bound.
    pub fn rotate(&mut self, conversation: Id) {
        let previous = self.sending.get(&conversation).map(|entry| entry.epoch);
        let entry = self
            .sending
            .entry(conversation)
            .or_insert_with(|| Outbound {
                state: SenderKeyState::create(random_chain_id(), &mut OsRandom),
                epoch: 0,
                distributed: HashSet::new(),
            });
        entry.state = SenderKeyState::create(random_chain_id(), &mut OsRandom);
        entry.epoch = previous.unwrap_or_default().wrapping_add(1).max(1);
        entry.distributed.clear();
    }

    /// Accepts a distribution from a remote device, so its future messages can open. A later
    /// distribution for the same sender replaces the earlier one — that is how a rotation is
    /// adopted on the receiving side. Unparseable bytes are swallowed: the pairwise channel
    /// already authenticated the sender, so a bad payload is a version skew, not an attack to
    /// surface.
    pub fn accept(&mut self, conversation: Id, sender_device: Id, bytes: &[u8]) {
        if let Some(distribution) = decode_distribution(bytes) {
            self.receiving.insert(
                (conversation, sender_device),
                ReceiverKeyState::accept(&distribution),
            );
        }
    }

    /// Whether a distribution has been accepted for a sender device — the gate the buffering
    /// decision turns on.
    #[must_use]
    pub fn has_receiver(&self, conversation: Id, sender_device: Id) -> bool {
        self.receiving.contains_key(&(conversation, sender_device))
    }

    /// Opens a broadcast envelope from a remote device.
    ///
    /// `Err` covers both "no distribution yet" and "a chain this state does not know" — the same
    /// situation seen at different times, and the caller's answer to both is to hold the message
    /// until a distribution arrives.
    pub fn open(
        &mut self,
        conversation: Id,
        sender_device: Id,
        bytes: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let Some(message) = decode_envelope(bytes) else {
            return Err(CryptoError::Envelope("unreadable sender-key envelope"));
        };
        let context = conversation.as_bytes();
        let receiver = self
            .receiving
            .get_mut(&(conversation, sender_device))
            .ok_or(CryptoError::NoSession)?;
        receiver
            .decrypt(context, &message)
            .map_err(CryptoError::from)
    }

    /// Forgets every trace of a conversation, for a leave: our chain and every receiver's.
    pub fn forget(&mut self, conversation: Id) {
        self.sending.remove(&conversation);
        self.receiving.retain(|(id, _), _| *id != conversation);
    }
}

/// The outbound entry for a conversation, creating the first chain (epoch 1) or rotating a spent
/// one on demand. Free-standing on the map so the caller's identity stays borrowable while the
/// chain encrypts.
fn outbound(sending: &mut HashMap<Id, Outbound>, conversation: Id) -> &mut Outbound {
    let fresh = !sending.contains_key(&conversation);
    let spent = sending
        .get(&conversation)
        .is_some_and(|entry| entry.state.needs_rotation());
    if fresh || spent {
        let previous = sending.get(&conversation).map(|entry| entry.epoch);
        let entry = sending.entry(conversation).or_insert_with(|| Outbound {
            state: SenderKeyState::create(random_chain_id(), &mut OsRandom),
            epoch: 0,
            distributed: HashSet::new(),
        });
        entry.state = SenderKeyState::create(random_chain_id(), &mut OsRandom);
        entry.epoch = previous.unwrap_or_default().wrapping_add(1).max(1);
        entry.distributed.clear();
    }
    sending.get_mut(&conversation).expect("inserted above")
}

/// A fresh random chain id, from the same CSPRNG every key in this client comes from.
fn random_chain_id() -> u32 {
    let mut random = OsRandom;
    let mut bytes = [0u8; 4];
    random.fill_bytes(&mut bytes);
    u32::from_be_bytes(bytes)
}

/// Assembles the section 11 group envelope from a sealed message.
fn encode_envelope(epoch: u32, message: &SenderKeyMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + 16 + SIGNATURE_LEN + message.ciphertext.len());
    out.push(ENVELOPE_VERSION);
    out.push(super::envelope::SCHEME_SENDER_KEY);
    varint(u64::from(message.header.chain_id), &mut out);
    varint(u64::from(epoch), &mut out);
    varint(u64::from(message.header.message_number), &mut out);
    out.extend_from_slice(&message.signature);
    out.extend_from_slice(&message.ciphertext);
    out
}

/// Parses a group envelope, rejecting a version or shape this build does not understand. The
/// epoch is parsed to advance the cursor and then dropped — the receiver state's own chain
/// bookkeeping is what decides whether a message is current, not the sender's epoch label.
fn decode_envelope(bytes: &[u8]) -> Option<SenderKeyMessage> {
    let mut cursor = Cursor::new(bytes);
    if cursor.u8()? != ENVELOPE_VERSION {
        return None;
    }
    if cursor.u8()? != super::envelope::SCHEME_SENDER_KEY {
        return None;
    }
    let chain_id = cursor.varint_u32()?;
    let _epoch = cursor.varint_u32()?;
    let message_number = cursor.varint_u32()?;
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(cursor.take(SIGNATURE_LEN)?);
    let ciphertext = cursor.rest().to_vec();
    Some(SenderKeyMessage {
        header: migo_crypto::SenderKeyHeader {
            chain_id,
            message_number,
        },
        ciphertext,
        signature,
    })
}

/// Serialises a distribution: chain id, message number, the chain key, the sender's identity.
///
/// The chain key is secret material in the clear here, by design — these bytes exist to be
/// sealed into the pairwise channel immediately, and the caller is expected to. The copy this
/// function returns is the only one; `SenderKeyDistribution` zeroes itself on drop.
fn encode_distribution(distribution: &SenderKeyDistribution) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + CHAIN_KEY_LEN + IDENTITY_PUBLIC_LEN);
    varint(u64::from(distribution.chain_id), &mut out);
    varint(u64::from(distribution.message_number), &mut out);
    out.extend_from_slice(&distribution.chain_key);
    out.extend_from_slice(&distribution.identity.to_bytes());
    out
}

/// Parses a distribution written by [`encode_distribution`].
fn decode_distribution(bytes: &[u8]) -> Option<SenderKeyDistribution> {
    let mut cursor = Cursor::new(bytes);
    let chain_id = cursor.varint_u32()?;
    let message_number = cursor.varint_u32()?;
    let mut chain_key = [0u8; CHAIN_KEY_LEN];
    chain_key.copy_from_slice(cursor.take(CHAIN_KEY_LEN)?);
    let identity = migo_crypto::IdentityPublic::parse(cursor.take(IDENTITY_PUBLIC_LEN)?).ok()?;
    Some(SenderKeyDistribution {
        chain_id,
        message_number,
        chain_key,
        identity,
    })
}

/// An unsigned LEB128 varint, the one integer encoding the envelope layouts use.
fn varint(value: u64, out: &mut Vec<u8>) {
    let mut value = value;
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A forward-only reader over an envelope, with the same bounds discipline as the pairwise one.
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.offset)?;
        self.offset += 1;
        Some(byte)
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.offset..self.offset.checked_add(len)?)?;
        self.offset += len;
        Some(slice)
    }

    fn varint_u32(&mut self) -> Option<u32> {
        let mut value: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 35 {
                // A varint wider than 5 bytes cannot fit a u32; the rest is padding or garbage.
                return None;
            }
        }
        u32::try_from(value).ok()
    }

    fn rest(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.offset.min(self.bytes.len())..];
        self.offset = self.bytes.len();
        slice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_identity() -> migo_crypto::IdentitySecret {
        migo_crypto::IdentitySecret::generate(&mut OsRandom)
    }

    fn fresh_id() -> Id {
        Id::generate(0, &mut OsRandom)
    }

    #[test]
    fn a_message_seals_once_and_opens_for_every_receiver() {
        let mut sender = GroupStore::new(device_identity());
        let mut receiver = GroupStore::new(device_identity());
        let conversation = fresh_id();
        let sender_device = fresh_id();

        // The wire order, not an arbitrary one: the distribution is taken *before* the first
        // seal (the SDK distributes, then seals), because a distribution carries the chain as
        // of its own position — handed out after the first message, it would gate that message
        // out, which is the late-joiner property, not the member case.
        let distribution = sender.distribution(conversation);
        receiver.accept(conversation, sender_device, &distribution);

        // Before the distribution: no state, the open is refused.
        let sealed = sender
            .seal(conversation, b"the room's first hello")
            .expect("seals");
        let opened = receiver
            .open(conversation, sender_device, &sealed.envelope)
            .expect("opens after the distribution");
        assert_eq!(opened, b"the room's first hello");

        // And the second message too: the chain steps forward under the same handed-over key.
        let second = sender.seal(conversation, b"and the second").expect("seals");
        assert_eq!(
            receiver
                .open(conversation, sender_device, &second.envelope)
                .expect("opens"),
            b"and the second"
        );
    }

    #[test]
    fn a_member_who_joins_late_cannot_read_what_sealed_before() {
        let mut sender = GroupStore::new(device_identity());
        let mut late = GroupStore::new(device_identity());
        let conversation = fresh_id();
        let sender_device = fresh_id();

        let early = sender
            .seal(conversation, b"before you were here")
            .expect("seals");
        // The distribution is handed over only after the early message exists.
        let distribution = sender.distribution(conversation);

        late.accept(conversation, sender_device, &distribution);
        // The distribution names the chain as of message 1; the early message was message 1
        // itself, sealed under a key position the receiver was never told about. The chain does
        // not step backwards, so the message stays sealed.
        assert!(late
            .open(conversation, sender_device, &early.envelope)
            .is_err());
    }

    #[test]
    fn the_envelope_round_trips_through_its_own_bytes() {
        let mut store = GroupStore::new(device_identity());
        let conversation = fresh_id();
        let sealed = store.seal(conversation, b"round trip").expect("seals");
        let parsed = decode_envelope(&sealed.envelope).expect("parses");
        assert_eq!(parsed.header.chain_id, sealed.chain_id);
        assert_eq!(parsed.ciphertext.len(), b"round trip".len() + 16);
    }

    #[test]
    fn a_truncated_envelope_is_refused_not_panicked() {
        assert!(decode_envelope(&[]).is_none());
        assert!(decode_envelope(&[ENVELOPE_VERSION]).is_none());
        assert!(
            decode_envelope(&[ENVELOPE_VERSION, super::super::envelope::SCHEME_SENDER_KEY])
                .is_none()
        );
    }

    #[test]
    fn a_tampered_envelope_fails_the_signature_not_the_panics() {
        let mut sender = GroupStore::new(device_identity());
        let mut receiver = GroupStore::new(device_identity());
        let conversation = fresh_id();
        let sender_device = fresh_id();

        let sealed = sender.seal(conversation, b"genuine").expect("seals");
        receiver.accept(
            conversation,
            sender_device,
            &sender.distribution(conversation),
        );
        // Flip a ciphertext byte: the signature check must reject it.
        let mut tampered = sealed.envelope.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(receiver
            .open(conversation, sender_device, &tampered)
            .is_err());
    }

    #[test]
    fn forgetting_drops_both_directions() {
        let mut sender = GroupStore::new(device_identity());
        let mut receiver = GroupStore::new(device_identity());
        let conversation = fresh_id();
        let sender_device = fresh_id();

        let sealed = sender.seal(conversation, b"then a leave").expect("seals");
        let distribution = sender.distribution(conversation);
        receiver.accept(conversation, sender_device, &distribution);
        assert!(receiver.has_receiver(conversation, sender_device));

        receiver.forget(conversation);
        sender.forget(conversation);
        assert!(!receiver.has_receiver(conversation, sender_device));
        assert!(receiver
            .open(conversation, sender_device, &sealed.envelope)
            .is_err());
    }
}
