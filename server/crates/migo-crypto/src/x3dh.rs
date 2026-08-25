//! X3DH — asynchronous session setup.
//!
//! The problem X3DH solves: Alice wants to send Bob an encrypted message, and
//! Bob's phone is off. A plain Diffie-Hellman handshake needs both parties
//! online. X3DH lets Bob publish key material in advance so Alice can establish a
//! session with nobody but a server in the loop.
//!
//! Bob publishes, once per device:
//!
//! * **IK_B** — long-term identity key (see [`crate::identity`]).
//! * **SPK_B** — a medium-term signed prekey, rotated on the order of days,
//!   signed by IK_B so Alice can tell it really came from Bob.
//! * **OPK_B** — a batch of one-time prekeys, each used at most once.
//!
//! Alice generates an ephemeral **EK_A** and computes up to four DH outputs:
//!
//! ```text
//! DH1 = DH(IK_A, SPK_B)     authenticates Alice to Bob
//! DH2 = DH(EK_A, IK_B)      authenticates Bob to Alice
//! DH3 = DH(EK_A, SPK_B)     forward secrecy from the medium-term key
//! DH4 = DH(EK_A, OPK_B)     forward secrecy from a key used exactly once
//! SK  = HKDF(0xFF×32 || DH1 || DH2 || DH3 || DH4)
//! ```
//!
//! Each output is there for a reason. Drop DH1 and Bob cannot tell who is talking
//! to him. Drop DH2 and Alice cannot tell she is talking to Bob. Drop DH3 and
//! compromising the identity key retroactively decrypts everything. DH4 is what
//! gives the *first* message forward secrecy even before the ratchet starts — and
//! it is optional because a device that has been offline long enough may have run
//! out of one-time prekeys. Running out degrades that one property; it does not
//! break the session, which is why the protocol keeps working rather than
//! refusing to deliver.
//!
//! The `0xFF × 32` prefix is from the X3DH specification. It exists so the HKDF
//! input can never be confused with a raw curve point, which matters on curves
//! where 32 bytes of DH output would otherwise be a valid input to something else.
//!
//! # Associated data
//!
//! The identities of both parties are bound into the session's associated data:
//! `IK_A || IK_B`. Without that binding, a session key negotiated between Alice
//! and Bob could be replayed into a conversation with Carol — an unknown key-share
//! attack. With it, a message only authenticates in the conversation it was made
//! for.

use migo_core::Random;
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};
use crate::identity::{IdentityPublic, IdentitySecret, KeyPair, SignedPrekey, PUBLIC_KEY_LEN};
use crate::kdf;

/// The X3DH specification's domain-separation prefix.
const F_PREFIX: [u8; 32] = [0xFF; 32];

/// A prekey bundle as fetched from the server.
///
/// This arrives over an untrusted channel: the server picks which bundle to
/// serve. [`PrekeyBundle::verify`] is therefore not optional, and
/// [`initiate`] calls it before doing any Diffie-Hellman.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrekeyBundle {
    /// The device's long-term identity.
    pub identity: IdentityPublic,
    /// The signed medium-term prekey.
    pub signed_prekey: SignedPrekey,
    /// A one-time prekey, if the device still has unused ones.
    pub one_time_prekey: Option<(u32, [u8; PUBLIC_KEY_LEN])>,
}

impl PrekeyBundle {
    /// Checks that the signed prekey really came from the claimed identity.
    pub fn verify(&self) -> Result<()> {
        self.signed_prekey.verify(&self.identity)
    }
}

/// The material Alice must send Bob so he can derive the same secret.
///
/// Travels in the first message's header. None of it is secret — it is public keys
/// and key ids — but all of it is required, and a message that arrives without it
/// cannot be decrypted, which is why it rides with the message rather than being
/// fetched separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialMessage {
    /// Alice's long-term identity.
    pub identity: IdentityPublic,
    /// Alice's ephemeral public key for this session.
    pub ephemeral_key: [u8; PUBLIC_KEY_LEN],
    /// Which of Bob's signed prekeys was used.
    pub signed_prekey_id: u32,
    /// Which one-time prekey was used, if any.
    pub one_time_prekey_id: Option<u32>,
}

/// The output of a successful X3DH exchange.
///
/// `shared_secret` seeds the Double Ratchet root key; `associated_data` is
/// authenticated in every message of the session.
pub struct SessionSeed {
    /// 32 bytes of shared secret.
    pub shared_secret: [u8; 32],
    /// `IK_initiator || IK_responder`, 128 bytes.
    pub associated_data: Vec<u8>,
}

impl Drop for SessionSeed {
    fn drop(&mut self) {
        self.shared_secret.zeroize();
    }
}

impl core::fmt::Debug for SessionSeed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionSeed")
            .field("shared_secret", &"***")
            .field("associated_data_len", &self.associated_data.len())
            .finish()
    }
}

/// Runs X3DH as the initiator.
///
/// Verifies the bundle first: a bundle that does not verify means the server
/// served something the claimed device did not sign, and the correct response is
/// to send nothing at all.
pub fn initiate(
    identity: &IdentitySecret,
    bundle: &PrekeyBundle,
    random: &mut dyn Random,
) -> Result<(SessionSeed, InitialMessage, KeyPair)> {
    bundle.verify()?;

    let ephemeral = KeyPair::generate(random);
    let spk = &bundle.signed_prekey.public_key;

    let mut material = Vec::with_capacity(32 * 5);
    material.extend_from_slice(&F_PREFIX);
    // DH1: our identity to their signed prekey — proves who is speaking.
    material.extend_from_slice(&identity.diffie_hellman(spk)?);
    // DH2: our ephemeral to their identity — proves who is listening.
    material.extend_from_slice(&ephemeral.diffie_hellman(&bundle.identity.exchange)?);
    // DH3: our ephemeral to their signed prekey — forward secrecy.
    material.extend_from_slice(&ephemeral.diffie_hellman(spk)?);
    // DH4: our ephemeral to a key they will never reuse — forward secrecy for
    // the very first message.
    if let Some((_, opk)) = &bundle.one_time_prekey {
        material.extend_from_slice(&ephemeral.diffie_hellman(opk)?);
    }

    let shared_secret: [u8; 32] = kdf::derive(&material, None, kdf::LABEL_X3DH);
    material.zeroize();

    let initiator_identity = identity.public();
    let seed = SessionSeed {
        shared_secret,
        associated_data: associated_data(&initiator_identity, &bundle.identity),
    };
    let message = InitialMessage {
        identity: initiator_identity,
        ephemeral_key: ephemeral.public(),
        signed_prekey_id: bundle.signed_prekey.key_id,
        one_time_prekey_id: bundle.one_time_prekey.as_ref().map(|(id, _)| *id),
    };
    Ok((seed, message, ephemeral))
}

/// Runs X3DH as the responder.
///
/// `one_time_prekey` must be the pair whose id the initial message names, and the
/// caller must delete it before returning — reusing a one-time prekey costs the
/// forward secrecy it exists to provide. Enforcing single use is the storage
/// layer's job because only it knows what has already been consumed; this function
/// cannot tell a first use from a replay.
pub fn respond(
    identity: &IdentitySecret,
    signed_prekey: &KeyPair,
    one_time_prekey: Option<&KeyPair>,
    message: &InitialMessage,
) -> Result<SessionSeed> {
    if message.one_time_prekey_id.is_some() != one_time_prekey.is_some() {
        // The initiator says it used a one-time prekey and we have not supplied
        // one, or the reverse. Deriving anyway would produce a key the other side
        // does not have, and the failure would surface as an undecryptable
        // message with no explanation.
        return Err(CryptoError::NoSession);
    }

    let mut material = Vec::with_capacity(32 * 5);
    material.extend_from_slice(&F_PREFIX);
    material.extend_from_slice(&signed_prekey.diffie_hellman(&message.identity.exchange)?);
    material.extend_from_slice(&identity.diffie_hellman(&message.ephemeral_key)?);
    material.extend_from_slice(&signed_prekey.diffie_hellman(&message.ephemeral_key)?);
    if let Some(opk) = one_time_prekey {
        material.extend_from_slice(&opk.diffie_hellman(&message.ephemeral_key)?);
    }

    let shared_secret: [u8; 32] = kdf::derive(&material, None, kdf::LABEL_X3DH);
    material.zeroize();

    Ok(SessionSeed {
        shared_secret,
        associated_data: associated_data(&message.identity, &identity.public()),
    })
}

/// Builds the session's associated data: initiator identity, then responder.
///
/// The order is fixed by role rather than by who is computing it, so both sides
/// produce identical bytes.
fn associated_data(initiator: &IdentityPublic, responder: &IdentityPublic) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&initiator.to_bytes());
    out.extend_from_slice(&responder.to_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_core::SeededRandom;

    struct Bob {
        identity: IdentitySecret,
        signed_prekey: KeyPair,
        one_time_prekey: KeyPair,
        bundle: PrekeyBundle,
    }

    fn bob(random: &mut dyn Random, with_one_time: bool) -> Bob {
        let identity = IdentitySecret::generate(random);
        let signed_prekey = KeyPair::generate(random);
        let one_time_prekey = KeyPair::generate(random);
        let signed = SignedPrekey::create(&identity, 7, &signed_prekey);
        let bundle = PrekeyBundle {
            identity: identity.public(),
            signed_prekey: signed,
            one_time_prekey: with_one_time.then(|| (99, one_time_prekey.public())),
        };
        Bob {
            identity,
            signed_prekey,
            one_time_prekey,
            bundle,
        }
    }

    #[test]
    fn both_sides_derive_the_same_secret() {
        let mut random = SeededRandom::new(1);
        let alice = IdentitySecret::generate(&mut random);
        let bob = bob(&mut random, true);

        let (alice_seed, message, _ephemeral) =
            initiate(&alice, &bob.bundle, &mut random).expect("initiates");
        let bob_seed = respond(
            &bob.identity,
            &bob.signed_prekey,
            Some(&bob.one_time_prekey),
            &message,
        )
        .expect("responds");

        assert_eq!(alice_seed.shared_secret, bob_seed.shared_secret);
        assert_eq!(alice_seed.associated_data, bob_seed.associated_data);
    }

    #[test]
    fn a_session_works_without_a_one_time_prekey() {
        // The exhausted-prekeys case. Forward secrecy for the first message is
        // reduced, delivery is not.
        let mut random = SeededRandom::new(2);
        let alice = IdentitySecret::generate(&mut random);
        let bob = bob(&mut random, false);

        let (alice_seed, message, _) =
            initiate(&alice, &bob.bundle, &mut random).expect("initiates");
        assert!(message.one_time_prekey_id.is_none());
        let bob_seed =
            respond(&bob.identity, &bob.signed_prekey, None, &message).expect("responds");
        assert_eq!(alice_seed.shared_secret, bob_seed.shared_secret);
    }

    #[test]
    fn the_one_time_prekey_changes_the_secret() {
        let mut random = SeededRandom::new(3);
        let alice = IdentitySecret::generate(&mut random);
        let mut bob_state = bob(&mut random, true);

        let (with_opk, _, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        bob_state.bundle.one_time_prekey = None;
        let (without_opk, _, _) =
            initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        assert_ne!(with_opk.shared_secret, without_opk.shared_secret);
    }

    #[test]
    fn a_bundle_with_a_forged_prekey_is_refused_before_any_dh() {
        // The server substitutes a prekey it controls. This must fail, or the
        // server reads the conversation.
        let mut random = SeededRandom::new(4);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let attacker = IdentitySecret::generate(&mut random);
        let attacker_pair = KeyPair::generate(&mut random);

        let mut tampered = bob_state.bundle.clone();
        tampered.signed_prekey = SignedPrekey::create(&attacker, 7, &attacker_pair);

        assert_eq!(
            initiate(&alice, &tampered, &mut random).err(),
            Some(CryptoError::InvalidPrekeyBundle)
        );
    }

    #[test]
    fn a_bundle_whose_prekey_bytes_were_swapped_is_refused() {
        let mut random = SeededRandom::new(5);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let other = KeyPair::generate(&mut random);

        let mut tampered = bob_state.bundle.clone();
        tampered.signed_prekey.public_key = other.public();
        assert!(initiate(&alice, &tampered, &mut random).is_err());
    }

    #[test]
    fn a_mismatch_about_the_one_time_prekey_is_an_error_not_a_wrong_key() {
        let mut random = SeededRandom::new(6);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let (_, message, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");

        // Bob has lost the one-time prekey the message names.
        // Not `assert_eq!`: `SessionSeed` deliberately has no `PartialEq`, so
        // nobody can compare secrets with a non-constant-time `==`.
        assert!(matches!(
            respond(
                &bob_state.identity,
                &bob_state.signed_prekey,
                None,
                &message
            ),
            Err(CryptoError::NoSession)
        ));
    }

    #[test]
    fn the_wrong_responder_derives_a_different_secret() {
        let mut random = SeededRandom::new(7);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let carol = bob(&mut random, true);

        let (alice_seed, message, _) =
            initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        let carol_seed = respond(
            &carol.identity,
            &carol.signed_prekey,
            Some(&carol.one_time_prekey),
            &message,
        )
        .expect("derives something");
        assert_ne!(alice_seed.shared_secret, carol_seed.shared_secret);
    }

    #[test]
    fn associated_data_binds_both_identities_in_a_fixed_order() {
        let mut random = SeededRandom::new(8);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let (seed, _, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");

        assert_eq!(seed.associated_data.len(), 128);
        assert_eq!(&seed.associated_data[..64], &alice.public().to_bytes());
        assert_eq!(
            &seed.associated_data[64..],
            &bob_state.identity.public().to_bytes()
        );
    }

    #[test]
    fn two_sessions_with_the_same_bundle_differ() {
        // Alice's ephemeral key is fresh each time, so a repeated bundle must not
        // produce a repeated session key.
        let mut random = SeededRandom::new(9);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let (first, _, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        let (second, _, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        assert_ne!(first.shared_secret, second.shared_secret);
    }

    #[test]
    fn a_seed_does_not_print_its_secret() {
        let mut random = SeededRandom::new(10);
        let alice = IdentitySecret::generate(&mut random);
        let bob_state = bob(&mut random, true);
        let (seed, _, _) = initiate(&alice, &bob_state.bundle, &mut random).expect("initiates");
        let rendered = format!("{seed:?}");
        assert!(rendered.contains("***"), "{rendered}");
    }
}
