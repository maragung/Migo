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
//! A participant who joins mid-call receives the current key by their own path:
//! [`CallKeyState::sealed_join_distribution`] seals the current epoch and key
//! under a wrapping key derived from the *joiner's* pairwise session secret
//! (HKDF under [`kdf::LABEL_CALL_JOIN`], the call id as salt), and the joiner
//! opens it with [`CallKeyState::from_join_distribution`]. No server relay
//! ever sees more than the sealed blob, and the caller is expected to rotate
//! on the join so what the joiner receives is a key that did not exist while
//! they were outside the call — pre-join media stays sealed because the
//! frames before the rotation are bound to an older epoch. After that first
//! key, the joiner rides the same rotations as everyone else.
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

    /// Length of a join distribution's plaintext: the epoch, then the key.
    pub const JOIN_DISTRIBUTION_LEN: usize = 8 + CALL_KEY_LEN;

    /// Seals the current epoch and key for a participant joining mid-call.
    ///
    /// The wrapping key is derived from `session_secret` — the pairwise session
    /// this device shares *with the joiner*, not the one the call started from
    /// — under its own label (see [`kdf::LABEL_CALL_JOIN`]), so the call's own
    /// key and the key that wraps it for the joiner are never the same
    /// material. The call id is both the HKDF salt and the AEAD associated
    /// data, so the blob cannot be opened for another call. The joiner reads
    /// the epoch out of the sealed body itself, which means a blob that
    /// lied about its epoch does not parse.
    ///
    /// The caller should rotate *before* distributing: a joiner handed the key
    /// that was current while they were outside the call can open the media
    /// that key sealed. Rotation on join is what makes "sealed for them at
    /// join" also mean "sealed *against* them until join".
    pub fn sealed_join_distribution(
        &self,
        session_secret: &[u8],
        random: &mut dyn Random,
    ) -> Result<Vec<u8>> {
        let mut plaintext = Vec::with_capacity(Self::JOIN_DISTRIBUTION_LEN);
        plaintext.extend_from_slice(&self.epoch.to_be_bytes());
        plaintext.extend_from_slice(&self.key);
        aead::seal(
            &join_wrapping_key(session_secret, self.call_id),
            self.call_id.as_bytes(),
            &plaintext,
            random,
        )
    }

    /// Opens a join distribution into the state it carries: the joiner's first
    /// key of a call already in progress.
    ///
    /// This is a constructor, not an [`adopt`](Self::adopt): the joiner holds
    /// no earlier epoch to compare against, so the first distribution is the
    /// baseline, exactly as the first sender-key distribution is. The blob must
    /// open under the session secret this device shares with the sender of the
    /// distribution and be bound to `call_id`.
    pub fn from_join_distribution(
        session_secret: &[u8],
        call_id: Id,
        sealed: &[u8],
    ) -> Result<Self> {
        let mut plaintext = aead::open(
            &join_wrapping_key(session_secret, call_id),
            call_id.as_bytes(),
            sealed,
        )?;
        // Parse and clear: the Vec held the key material in the clear, on both
        // the success and the length-refusal path. The length is read before
        // zeroizing because a cleared Vec no longer knows it.
        let actual = plaintext.len();
        let parsed: Result<[u8; Self::JOIN_DISTRIBUTION_LEN], _> = plaintext.as_slice().try_into();
        plaintext.zeroize();
        let bytes = parsed.map_err(|_| CryptoError::BadLength {
            what: "call join distribution",
            expected: Self::JOIN_DISTRIBUTION_LEN,
            actual,
        })?;
        Ok(Self {
            call_id,
            epoch: u64::from_be_bytes(bytes[..8].try_into().expect("eight bytes")),
            key: bytes[8..].try_into().expect("thirty-two bytes"),
        })
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

/// The key that wraps a join distribution for one joiner.
///
/// Derived from the pairwise session secret the distributor shares with that
/// joiner, under its own label — the third purpose that secret serves, so it
/// must not share a label with the ratchet or the call key itself.
fn join_wrapping_key(session_secret: &[u8], call_id: Id) -> SymmetricKey {
    SymmetricKey::from_bytes(kdf::derive::<CALL_KEY_LEN>(
        session_secret,
        Some(call_id.as_bytes()),
        kdf::LABEL_CALL_JOIN,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    /// A session secret both call participants hold and no server ever saw.
    const SESSION: &[u8] = &[0x0a; 32];

    /// The pairwise secret the caller shares with a *third* device joining
    /// mid-call — different from the call's own session, because it belongs to
    /// a different pair of devices.
    const JOINER_SESSION: &[u8] = &[0x0b; 32];

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

    #[test]
    fn a_mid_call_joiner_receives_the_current_key_sealed_for_them() {
        // Finding F4: the joiner's first key travels sealed under their own
        // pairwise session with the caller, through whatever relay already
        // moves sealed blobs between devices. The rotation on join is what
        // keeps pre-join media sealed from them.
        let mut random = SeededRandom::new(13);
        let mut caller = CallKeyState::from_session(SESSION, call_id());
        caller.rotate(&mut random).expect("rotates");
        let pre_join = caller
            .seal_frame(b"while the joiner was outside", &mut random)
            .expect("seals");

        caller.rotate(&mut random).expect("rotates");
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        let joiner = CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &sealed)
            .expect("opens");
        assert_eq!(joiner.epoch(), 2);

        let frame = caller
            .seal_frame(b"welcome in", &mut random)
            .expect("seals");
        assert_eq!(joiner.open_frame(&frame).expect("opens"), b"welcome in");
        assert_eq!(
            joiner.open_frame(&pre_join),
            Err(CryptoError::DecryptionFailed),
            "the joiner read media from before they joined"
        );
    }

    #[test]
    fn a_call_that_never_rotated_hands_the_joiner_the_epoch_zero_key() {
        // The honest boundary of the mechanism: without a rotation there is no
        // older epoch for pre-join media to be stranded on, so the epoch-0 key
        // the joiner receives opens it. Pre-join secrecy comes from rotating
        // on join, not from the distribution itself — a test that pretended
        // otherwise would be pretending it in the model.
        let mut random = SeededRandom::new(14);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let earlier = caller
            .seal_frame(b"before anyone else arrived", &mut random)
            .expect("seals");
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        let joiner = CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &sealed)
            .expect("opens");
        assert_eq!(joiner.epoch(), 0);
        assert_eq!(
            joiner.open_frame(&earlier).expect("opens"),
            b"before anyone else arrived",
            "epoch-0 media opens under the epoch-0 key; this is why callers rotate on join"
        );
    }

    #[test]
    fn a_joiner_rides_the_rotations_after_their_first_key() {
        // The first key is a baseline, not a leash: the joiner applies the same
        // sealed updates as everyone else from the epoch they entered on.
        let mut random = SeededRandom::new(15);
        let mut caller = CallKeyState::from_session(SESSION, call_id());
        caller.rotate(&mut random).expect("rotates");
        caller.rotate(&mut random).expect("rotates");
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        let mut joiner = CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &sealed)
            .expect("opens");
        assert_eq!(joiner.epoch(), 2);

        let update = caller.rotate(&mut random).expect("rotates");
        joiner.adopt(3, &update).expect("adopts");
        assert_eq!(joiner.epoch(), 3);
        let frame = caller
            .seal_frame(b"third epoch", &mut random)
            .expect("seals");
        assert_eq!(joiner.open_frame(&frame).expect("opens"), b"third epoch");
    }

    #[test]
    fn a_join_distribution_needs_the_joiners_session() {
        // The blob is sealed to one pairwise session. The call's own other
        // seat — or the server, which holds none of these secrets — cannot
        // open the joiner's copy.
        let mut random = SeededRandom::new(16);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        // `CallKeyState` deliberately has neither `PartialEq` nor an all-seeing
        // `Debug` — it holds key material — so the assertion matches the error
        // instead of comparing whole states.
        assert!(matches!(
            CallKeyState::from_join_distribution(SESSION, call_id(), &sealed),
            Err(CryptoError::DecryptionFailed)
        ));
    }

    #[test]
    fn a_join_distribution_cannot_be_replayed_onto_another_call() {
        let mut random = SeededRandom::new(17);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        let other = CallKeyState::from_join_distribution(
            JOINER_SESSION,
            Id::from_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]),
            &sealed,
        );
        assert!(matches!(other, Err(CryptoError::DecryptionFailed)));
    }

    #[test]
    fn a_tampered_join_distribution_is_refused() {
        let mut random = SeededRandom::new(18);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let mut sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(matches!(
            CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &sealed),
            Err(CryptoError::DecryptionFailed)
        ));
    }

    #[test]
    fn a_truncated_join_distribution_is_refused() {
        let mut random = SeededRandom::new(19);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let sealed = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        assert!(CallKeyState::from_join_distribution(
            JOINER_SESSION,
            call_id(),
            &sealed[..sealed.len() - 1]
        )
        .is_err());
    }

    #[test]
    fn the_join_wrapping_key_is_not_the_call_key() {
        // Section 163: no key for two purposes. The wrapping key comes from
        // the same secret and the same salt but its own label; if the two
        // derivations ever agreed, the call key would be the wrapping key and
        // the separation would be gone. Checked at behaviour level: a genuine
        // blob opens, one wrapped under the call-key label does not.
        let mut random = SeededRandom::new(20);
        let caller = CallKeyState::from_session(SESSION, call_id());
        let genuine = caller
            .sealed_join_distribution(JOINER_SESSION, &mut random)
            .expect("seals");
        assert!(CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &genuine).is_ok());

        let mut body = Vec::with_capacity(CallKeyState::JOIN_DISTRIBUTION_LEN);
        body.extend_from_slice(&caller.epoch.to_be_bytes());
        body.extend_from_slice(&caller.key);
        let wrong = kdf::derive::<CALL_KEY_LEN>(
            JOINER_SESSION,
            Some(call_id().as_bytes()),
            kdf::LABEL_CALL_KEY,
        );
        let forged = aead::seal(
            &SymmetricKey::from_bytes(wrong),
            call_id().as_bytes(),
            &body,
            &mut random,
        )
        .expect("seals");
        assert!(matches!(
            CallKeyState::from_join_distribution(JOINER_SESSION, call_id(), &forged),
            Err(CryptoError::DecryptionFailed)
        ));
    }
}
