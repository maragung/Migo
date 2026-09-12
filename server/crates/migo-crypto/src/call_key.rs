//! Call keys — the media key of an encrypted call.
//!
//! Section 163: a call's media key is *derived from the pairwise E2E session*
//! between the devices, not minted in the clear and not chosen by the server.
//! Both devices already hold a secret no server has ever seen, so HKDF under
//! [`kdf::LABEL_CALL_KEY`] turns it into a media key neither party had to send.
//! Deriving instead of distributing is what keeps the call's confidentiality
//! anchored to the session's: an attacker who cannot read the messages cannot
//! read the call either, because there is nothing else to read.
//!
//! # Rotation
//!
//! A group call's membership changes, and section 163 requires that a
//! participant who leaves cannot decrypt what follows and one who joins cannot
//! decrypt what came before. So the key rotates: the rotating side mints fresh
//! material for the next epoch and returns it *sealed under the current key* —
//! those bytes are the `sealed_key_material` of a `CallKeyUpdate`, and only a
//! device holding the current epoch can open them. [`CallKeyState::adopt`]
//! accepts such an update and refuses any epoch that does not advance, which is
//! the same replay-and-rollback refusal the message ratchets live by.
//!
//! A participant who joins mid-call receives the current key by their own path —
//! sealed for them at join, the way a sender-key distribution is — and then
//! rides the same rotations as everyone else. Handing them that first key is the
//! caller's distribution wiring, not this state's.
//!
//! # What this module is not
//!
//! It is the key, its derivation, and its rotation. It does not touch media
//! itself: which frames exist, how they are packetised, and when rotation is
//! triggered by roster events are the media plane's business (sections 166-168).

use migo_core::{Id, Random};
use zeroize::Zeroize;

use crate::aead::{self, SymmetricKey};
use crate::error::{CryptoError, Result};
use crate::kdf;

/// Length of a call's media key material.
pub const CALL_KEY_LEN: usize = 32;

/// The sending and receiving half of one call's key, one per call per device.
///
/// Both sides of a 1:1 call hold the same state by construction — same session
/// secret, same call id, same derivation — so there is no handshake to run and
/// nothing to agree on beyond the call id itself. In a group call every
/// participant converges on the same epoch by applying the same updates in
/// order.
pub struct CallKeyState {
    call_id: Id,
    epoch: u64,
    key: [u8; CALL_KEY_LEN],
}

impl Drop for CallKeyState {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl core::fmt::Debug for CallKeyState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CallKeyState")
            .field("call_id", &self.call_id)
            .field("epoch", &self.epoch)
            .field("key", &"***")
            .finish()
    }
}

impl CallKeyState {
    /// Derives the call's first key (epoch 0) from the pairwise session secret.
    ///
    /// The call id is the HKDF salt, so one session cannot produce the same
    /// media key for two different calls — a key that outlived its call would
    /// be a second purpose for a key that already had one.
    #[must_use]
    pub fn from_session(session_secret: &[u8], call_id: Id) -> Self {
        let key = kdf::derive::<CALL_KEY_LEN>(
            session_secret,
            Some(call_id.as_bytes()),
            kdf::LABEL_CALL_KEY,
        );
        Self {
            call_id,
            epoch: 0,
            key,
        }
    }

    /// The epoch this state's key belongs to. Zero until the first rotation.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Rotates: mints the next epoch's key and returns it sealed under the
    /// current one.
    ///
    /// The sealed bytes are what a `CallKeyUpdate` carries as
    /// `sealed_key_material`; its `epoch` field is the value this state reports
    /// after the call. State advances only after the sealing succeeds, so a
    /// failure leaves the call on the old key rather than between keys.
    pub fn rotate(&mut self, random: &mut dyn Random) -> Result<Vec<u8>> {
        let mut next = [0u8; CALL_KEY_LEN];
        random.fill_bytes(&mut next);
        let sealed = aead::seal(
            &SymmetricKey::from_bytes(self.key),
            &self.binding(self.epoch + 1),
            &next,
            random,
        )?;
        self.key.zeroize();
        self.key = next;
        self.epoch += 1;
        Ok(sealed)
    }

    /// Adopts a distributed update, moving to `epoch`.
    ///
    /// The update must open under the *current* key and be bound to exactly
    /// `epoch`, and the epoch must advance: an update that repeats or rolls back
    /// the epoch is a replay of an old key and is refused. As everywhere in this
    /// crate, the state moves only after the new material is verified, so a bad
    /// update cannot destroy a working key.
    pub fn adopt(&mut self, epoch: u64, sealed: &[u8]) -> Result<()> {
        if epoch <= self.epoch {
            // Reuse would let a replayed update re-install an old key and
            // re-open the media it sealed, which is the call-side face of the
            // rule the message ratchets enforce with the same error.
            return Err(CryptoError::KeyAlreadyUsed);
        }
        let material = aead::open(
            &SymmetricKey::from_bytes(self.key),
            &self.binding(epoch),
            sealed,
        )?;
        let next: [u8; CALL_KEY_LEN] =
            material
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::BadLength {
                    what: "call key material",
                    expected: CALL_KEY_LEN,
                    actual: material.len(),
                })?;
        self.key.zeroize();
        self.key = next;
        self.epoch = epoch;
        Ok(())
    }

    /// Seals one media frame under the current key.
    ///
    /// The associated data binds the call and the epoch, so a frame cannot be
    /// lifted into another call, and a frame from before a rotation cannot be
    /// presented as one from after it even to a device that kept the old key.
    pub fn seal_frame(&self, frame: &[u8], random: &mut dyn Random) -> Result<Vec<u8>> {
        aead::seal(
            &SymmetricKey::from_bytes(self.key),
            &self.binding(self.epoch),
            frame,
            random,
        )
    }

    /// Opens a media frame sealed under the current key.
    pub fn open_frame(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        aead::open(
            &SymmetricKey::from_bytes(self.key),
            &self.binding(self.epoch),
            sealed,
        )
    }

    /// The bytes every cryptographic operation here binds: which call, which
    /// epoch. Fixed-width so the pair cannot be re-split.
    fn binding(&self, epoch: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + 8);
        out.extend_from_slice(self.call_id.as_bytes());
        out.extend_from_slice(&epoch.to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    /// A session secret both call participants hold and no server ever saw.
    const SESSION: &[u8] = &[0x0a; 32];

    fn call_id() -> Id {
        Id::from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
    }

    #[test]
    fn both_sides_derive_the_same_key_from_one_session() {
        let caller = CallKeyState::from_session(SESSION, call_id());
        let callee = CallKeyState::from_session(SESSION, call_id());
        let mut random = SeededRandom::new(1);
        let frame = caller
            .seal_frame(b"media bytes", &mut random)
            .expect("seals");
        assert_eq!(callee.open_frame(&frame).expect("opens"), b"media bytes");
    }

    #[test]
    fn the_derivation_is_pinned_to_an_independent_vector() {
        // Not this implementation's own output copied back in: the expected
        // bytes were computed from RFC 5869 directly (HMAC-SHA256 extract with
        // the call id as salt, one expand round over the label), so a change to
        // the construction — not just the dependency — fails here.
        let state = CallKeyState::from_session(SESSION, call_id());
        let expected: [u8; CALL_KEY_LEN] = [
            0x3e, 0xc0, 0xb5, 0xb2, 0x95, 0x15, 0xed, 0xc6, 0xb4, 0xb5, 0x1d, 0x92, 0xc1, 0x31,
            0xc7, 0x56, 0xd1, 0xef, 0x49, 0x66, 0xdc, 0xa0, 0x54, 0x29, 0xed, 0x92, 0x6d, 0xc0,
            0x12, 0xa9, 0xcf, 0xa0,
        ];
        // The key is never exposed, so prove the pin through behaviour: the
        // derived key must seal a frame the expected bytes can open.
        let mut random = SeededRandom::new(2);
        let frame = state.seal_frame(b"pin", &mut random).expect("seals");
        let pinned = CallKeyState {
            call_id: call_id(),
            epoch: 0,
            key: expected,
        };
        assert_eq!(pinned.open_frame(&frame).expect("opens"), b"pin");
    }

    #[test]
    fn a_call_key_is_bound_to_its_call() {
        let mut random = SeededRandom::new(3);
        let this_call = CallKeyState::from_session(SESSION, call_id());
        let other_call = CallKeyState::from_session(
            SESSION,
            Id::from_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]),
        );
        let frame = this_call
            .seal_frame(b"this call only", &mut random)
            .expect("seals");
        assert_eq!(
            other_call.open_frame(&frame),
            Err(CryptoError::DecryptionFailed)
        );
    }

    #[test]
    fn a_call_key_is_not_a_message_key() {
        // The label is the separation (section 163: no key for two purposes).
        // Same secret, same salt, different label — the derivations must not
        // agree, which is checked here at the behaviour level: a frame sealed
        // under the call key does not open under an X3DH-labelled derivation.
        let mut random = SeededRandom::new(4);
        let state = CallKeyState::from_session(SESSION, call_id());
        let frame = state.seal_frame(b"media", &mut random).expect("seals");
        let wrong =
            kdf::derive::<CALL_KEY_LEN>(SESSION, Some(call_id().as_bytes()), kdf::LABEL_X3DH);
        let forgery = CallKeyState {
            call_id: call_id(),
            epoch: 0,
            key: wrong,
        };
        assert_eq!(
            forgery.open_frame(&frame),
            Err(CryptoError::DecryptionFailed)
        );
    }

    #[test]
    fn rotation_seals_and_adoption_opens() {
        let mut random = SeededRandom::new(5);
        let mut caller = CallKeyState::from_session(SESSION, call_id());
        let mut callee = CallKeyState::from_session(SESSION, call_id());
        let sealed = caller.rotate(&mut random).expect("rotates");
        assert_eq!(caller.epoch(), 1);
        callee.adopt(1, &sealed).expect("adopts");
        assert_eq!(callee.epoch(), 1);
        let frame = caller.seal_frame(b"new epoch", &mut random).expect("seals");
        assert_eq!(callee.open_frame(&frame).expect("opens"), b"new epoch");
    }

    #[test]
    fn media_before_a_rotation_is_not_readable_after_it() {
        // Section 163: a participant who joins cannot read media from before.
        let mut random = SeededRandom::new(6);
        let mut caller = CallKeyState::from_session(SESSION, call_id());
        let old = caller
            .seal_frame(b"before the join", &mut random)
            .expect("seals");
        let sealed = caller.rotate(&mut random).expect("rotates");
        let mut joiner = CallKeyState::from_session(SESSION, call_id());
        joiner.adopt(1, &sealed).expect("adopts");
        assert_eq!(joiner.open_frame(&old), Err(CryptoError::DecryptionFailed));
    }

    #[test]
    fn an_update_that_does_not_advance_the_epoch_is_refused() {
        let mut random = SeededRandom::new(7);
        let mut state = CallKeyState::from_session(SESSION, call_id());
        let sealed = state.rotate(&mut random).expect("rotates");
        assert_eq!(state.adopt(1, &sealed), Err(CryptoError::KeyAlreadyUsed));
        assert_eq!(state.adopt(0, &sealed), Err(CryptoError::KeyAlreadyUsed));
        // A refused update leaves the working key intact: the peer that applied
        // the rotation for real still opens this state's frames.
        let mut peer = CallKeyState::from_session(SESSION, call_id());
        peer.adopt(1, &sealed).expect("adopts");
        let mut random = SeededRandom::new(8);
        let frame = state
            .seal_frame(b"still the same key", &mut random)
            .expect("seals");
        assert_eq!(
            peer.open_frame(&frame).expect("opens"),
            b"still the same key"
        );
    }

    #[test]
    fn a_tampered_update_is_refused() {
        let mut random = SeededRandom::new(9);
        let mut state = CallKeyState::from_session(SESSION, call_id());
        let mut sealed = state.rotate(&mut random).expect("rotates");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        let mut peer = CallKeyState::from_session(SESSION, call_id());
        assert_eq!(peer.adopt(1, &sealed), Err(CryptoError::DecryptionFailed));
    }

    #[test]
    fn a_truncated_update_is_refused() {
        let mut random = SeededRandom::new(10);
        let mut state = CallKeyState::from_session(SESSION, call_id());
        let sealed = state.rotate(&mut random).expect("rotates");
        let mut peer = CallKeyState::from_session(SESSION, call_id());
        assert!(peer.adopt(1, &sealed[..sealed.len() - 1]).is_err());
    }

    #[test]
    fn an_update_cannot_be_replayed_onto_another_call() {
        let mut random = SeededRandom::new(11);
        let mut state = CallKeyState::from_session(SESSION, call_id());
        let sealed = state.rotate(&mut random).expect("rotates");
        let mut other = CallKeyState::from_session(
            SESSION,
            Id::from_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]),
        );
        assert_eq!(other.adopt(1, &sealed), Err(CryptoError::DecryptionFailed));
    }

    #[test]
    fn an_update_bound_to_a_different_epoch_is_refused() {
        // The frame says epoch 2 but the bytes are bound to another epoch: the
        // binding, not the frame's claim, decides.
        let mut random = SeededRandom::new(12);
        let mut state = CallKeyState::from_session(SESSION, call_id());
        let sealed = state.rotate(&mut random).expect("rotates");
        let mut peer = CallKeyState::from_session(SESSION, call_id());
        assert_eq!(peer.adopt(2, &sealed), Err(CryptoError::DecryptionFailed));
    }

    #[test]
    fn a_state_does_not_print_its_key() {
        let state = CallKeyState::from_session(SESSION, call_id());
        let rendered = format!("{state:?}");
        assert!(rendered.contains("***"), "{rendered}");
    }
}
