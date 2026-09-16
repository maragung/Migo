//! The threat model, pinned (docs/03-security-threat-model.md §12).
//!
//! migo.md section 162 marks the *full* threat model and its tests as its
//! remaining SPEC item. This file is the test half of that deliverable: each
//! test here stands for one claim the model makes, and is named after the
//! adversary or the boundary it exercises rather than after a function, so the
//! mapping between the document and this suite is one-to-one.
//!
//! The unit tests inside `migo-crypto`'s modules pin the behaviour of one
//! construction at a time. What only an integration suite can pin is the
//! *composition*: what an adversary holds when they stand at a trust boundary
//! and everything the system put on the other side of it. That is the shape of
//! every test here — a pile of captured material, an attacker with their own
//! keys, and the assertion that the pile does not open.
//!
//! Two of these tests deliberately assert that something *succeeds* for the
//! attacker: a wholly substituted prekey bundle is cryptographically valid
//! (the safety number, not X3DH, is what detects substitution), and a stolen
//! group chain key does read messages sealed after the theft (sender keys have
//! no post-compromise security within a chain; rotation is the bound on that
//! window, and pretending otherwise in a test would be pretending it in the
//! model). Pinning the boundary is as much this suite's job as pinning the
//! wall.

use migo_core::{Id, SeededRandom, Timestamp};
use migo_crypto::error::CryptoError;
use migo_crypto::node::{self, NodeHello};
use migo_crypto::{
    aead, x3dh, IdentitySecret, KeyPair, NodeSecret, PrekeyBundle, RatchetSession,
    ReceiverKeyState, SenderKeyState, SignedPrekey, SymmetricKey,
};

const GROUP: &[u8] = b"group-01H0000000000000000000000";

#[test]
fn a_wholly_substituted_bundle_is_cryptographically_valid() {
    // §12.4, the substitution boundary. The server picks which bundle to
    // serve. When it substitutes only the prekey, the signature check refuses
    // it (x3dh.rs, `a_bundle_with_a_forged_prekey_is_refused_before_any_dh`).
    // When it substitutes *everything* — identity, prekey, one-time prekey,
    // all consistently signed — the bundle is internally consistent and X3DH
    // has no basis to refuse: nothing in the handshake names the identity the
    // caller expected, because the caller learned that identity from the same
    // server. This test pins that boundary honestly: `initiate` succeeds, and
    // the detection that actually works is the human comparing safety numbers
    // out of band (`IdentityPublic::fingerprint`, pinned in identity.rs).
    let mut random = SeededRandom::new(1);
    let alice = IdentitySecret::generate(&mut random);
    let attacker = IdentitySecret::generate(&mut random);
    let attacker_spk = KeyPair::generate(&mut random);
    let attacker_opk = KeyPair::generate(&mut random);

    let substituted = PrekeyBundle {
        identity: attacker.public(),
        signed_prekey: SignedPrekey::create(&attacker, 7, &attacker_spk),
        one_time_prekey: Some((9, attacker_opk.public())),
    };

    // Succeeds. This is not a bug being blessed: it is the exact line where
    // cryptography stops and out-of-band verification starts, written down as
    // a test so the model cannot drift away from the code.
    let result = x3dh::initiate(&alice, &substituted, &mut random);
    assert!(
        result.is_ok(),
        "a wholly substituted bundle must still initiate; if this ever fails, \
         §12.4 of the threat model is stale and must be rewritten"
    );
}

#[test]
fn everything_the_server_records_of_a_conversation_decrypts_nothing() {
    // §12.3, claim C25. The server's entire record of a 1:1 conversation is:
    // both published bundles (public keys and signatures), the initial
    // message (public keys and key ids), and every frame (headers and
    // ciphertext). An attacker holding that whole record, plus their own
    // freshly generated private keys, must not open a single frame.
    let mut random = SeededRandom::new(2);
    let alice_identity = IdentitySecret::generate(&mut random);
    let bob_identity = IdentitySecret::generate(&mut random);
    let bob_spk = KeyPair::generate(&mut random);
    let bob_opk = KeyPair::generate(&mut random);
    let bundle = PrekeyBundle {
        identity: bob_identity.public(),
        signed_prekey: SignedPrekey::create(&bob_identity, 7, &bob_spk),
        one_time_prekey: Some((9, bob_opk.public())),
    };

    let (alice_seed, initial, _) =
        x3dh::initiate(&alice_identity, &bundle, &mut random).expect("an honest bundle initiates");
    let bob_seed = x3dh::respond(&bob_identity, &bob_spk, Some(&bob_opk), &initial)
        .expect("the real responder derives the session");

    let mut alice = RatchetSession::initiator(&alice_seed, bob_spk.public(), &mut random)
        .expect("the initiator starts");
    let mut bob = RatchetSession::responder(&bob_seed, bob_spk);

    // A genuine conversation, every frame of which the "server" captures.
    //
    // Every call here passes an empty context, which is the version-1 associated data: this test is
    // about whether a captured transcript can be replayed against keys the attacker chose, and the
    // bound context of section 11 is orthogonal to that. A version-2 context would have to be
    // *not* empty and identical on both sides for the frames to open at all, which would add a
    // second thing the test depends on without testing it.
    let mut frames = Vec::new();
    for round in 0..5u32 {
        let sent = format!("alice {round}");
        let frame = alice
            .encrypt_next(sent.as_bytes(), &mut random, &[])
            .expect("encrypts");
        assert_eq!(
            bob.decrypt(&frame.0, &frame.1, &[]).expect("bob decrypts"),
            sent.as_bytes(),
            "the transcript must be genuine or the test proves nothing"
        );
        frames.push(frame);
        let reply = format!("bob {round}");
        let frame = bob
            .encrypt_next(reply.as_bytes(), &mut random, &[])
            .expect("encrypts");
        assert!(alice.decrypt(&frame.0, &frame.1, &[]).is_ok());
        frames.push(frame);
    }

    // The attacker: the full server record, and private keys of their own.
    // Replaying the initial message against their own keys derives a session
    // — X3DH cannot tell a responder with the wrong private keys from one
    // with the right ones, it just derives a different secret — and that
    // session must not open any frame of the captured transcript.
    let attacker_identity = IdentitySecret::generate(&mut random);
    let attacker_spk = KeyPair::generate(&mut random);
    let attacker_opk = KeyPair::generate(&mut random);
    let attacker_seed = x3dh::respond(
        &attacker_identity,
        &attacker_spk,
        Some(&attacker_opk),
        &initial,
    )
    .expect("a replayed initial message still derives *a* session");
    assert_ne!(
        attacker_seed.shared_secret, alice_seed.shared_secret,
        "the attacker derived the real secret from public material alone"
    );
    let mut attacker = RatchetSession::responder(&attacker_seed, attacker_spk);
    for (index, (header, ciphertext)) in frames.iter().enumerate() {
        assert!(
            attacker.decrypt(header, ciphertext, &[]).is_err(),
            "the attacker opened frame {index} of the captured transcript"
        );
    }
}

#[test]
fn a_stolen_group_chain_key_reads_forward_only_until_the_rotation() {
    // §12.3, claims C13 and C16 together. A sender key has forward secrecy —
    // the theft does not open the past — but no post-compromise security: the
    // thief does read the sender's future messages until the chain rotates.
    // Both halves are the claim, because a model that only stated the first
    // half would be overstating the guarantee.
    let mut random = SeededRandom::new(3);
    let identity = IdentitySecret::generate(&mut random);
    let mut sender = SenderKeyState::create(1, 1, &mut random);

    // Messages sealed before the theft. The thief must not open these.
    let before: Vec<_> = (0..5u32)
        .map(|i| {
            sender
                .encrypt(&identity, GROUP, format!("before {i}").as_bytes())
                .expect("encrypts")
        })
        .collect();

    // The theft: the chain key in its stealable form is the distribution
    // message, which is what a compromised member device or a ransacked
    // encrypted store yields.
    let mut thief = ReceiverKeyState::accept(&sender.distribution(&identity));

    // Messages sealed after the theft. The thief DOES open these — pinned
    // here on purpose — until the membership change rotates the chain.
    let after = sender
        .encrypt(&identity, GROUP, b"after the theft")
        .expect("encrypts");
    assert_eq!(
        thief
            .decrypt(GROUP, &after)
            .expect("the thief reads forward"),
        b"after the theft",
        "sender keys have no post-compromise security within a chain; \
         if this starts failing, §12.3 claim C16 is stale and must be rewritten"
    );

    // Forward secrecy of the chain: the theft does not open the past.
    for message in &before {
        assert!(
            thief.decrypt(GROUP, message).is_err(),
            "a thief with the current chain key opened message {} from before the theft",
            message.header.message_number
        );
    }

    // The rotation that every membership change requires (migo.md §163): a
    // fresh chain under a fresh epoch, distributed to the remaining members
    // over their pairwise channels. The thief is not a remaining member.
    sender.rotate(2, &mut random);
    let mut remaining = ReceiverKeyState::accept(&sender.distribution(&identity));
    let post_rotation = sender
        .encrypt(&identity, GROUP, b"after the rekey")
        .expect("encrypts");
    assert_eq!(
        remaining
            .decrypt(GROUP, &post_rotation)
            .expect("remaining members read on"),
        b"after the rekey"
    );
    assert_eq!(
        thief.decrypt(GROUP, &post_rotation),
        Err(CryptoError::NoSession),
        "the thief read past the rotation"
    );
}

#[test]
fn signatures_never_transfer_between_the_mesh_prekey_and_group_domains() {
    // §12.3, claim C23. One Ed25519 key can technically serve every signing
    // purpose in the system — a node operator's seed and a device identity
    // seed are the same kind of bytes — so the only thing that keeps a mesh
    // proof from being replayed as a prekey signature, or a group message
    // signature as either, is the domain label inside the signed bytes. This
    // test drives all three domains with the *same* underlying signing key
    // and asserts the signatures stay in their lanes.
    let seed = [7u8; 32];
    let exchange_seed = [8u8; 32];
    let identity = IdentitySecret::from_seeds(seed, exchange_seed);
    let node = NodeSecret::from_seed(&seed).expect("loads");

    // --- the prekey domain ---
    let mut random = SeededRandom::new(4);
    let pair = KeyPair::generate(&mut random);
    let prekey = SignedPrekey::create(&identity, 7, &pair);
    let prekey_signature = prekey.signature;
    // Positive control: the genuine signature verifies, or the rest of this
    // test would pass vacuously.
    prekey
        .verify(&identity.public())
        .expect("the genuine prekey signature verifies");

    // --- the mesh domain, signed by the very same key ---
    let hello_a = NodeHello::new(Id::from_bytes([0xaa; 16]), &mut random);
    let hello_b = NodeHello::new(Id::from_bytes([0xbb; 16]), &mut random);
    let now = Timestamp::from_millis(1_700_000_000_000);
    let proof = node::prove(&node, &hello_a, &hello_b, now);
    // Positive control again.
    node::verify_proof(&node.public(), &hello_b, &hello_a, &proof, now)
        .expect("the genuine mesh proof verifies");

    // --- the group-message domain, also signed by the same key ---
    let mut sender = SenderKeyState::create(1, 1, &mut random);
    let group_message = sender
        .encrypt(&identity, GROUP, b"signed by the same key")
        .expect("encrypts");

    // A mesh proof offered as a prekey signature: refused.
    let mut forged = prekey.clone();
    forged.signature = proof.signature;
    assert_eq!(
        forged.verify(&identity.public()),
        Err(CryptoError::InvalidPrekeyBundle),
        "a mesh proof verified as a prekey signature"
    );

    // A group-message signature offered as a prekey signature: refused.
    let mut forged = prekey;
    forged.signature = group_message.signature;
    assert_eq!(
        forged.verify(&identity.public()),
        Err(CryptoError::InvalidPrekeyBundle),
        "a group message signature verified as a prekey signature"
    );

    // A prekey signature offered as a mesh transcript signature: refused.
    // The transcript is the one the genuine proof covered, so the only thing
    // that can fail it is the domain inside the signed bytes.
    let transcript = node::transcript(
        hello_a.node_id,
        &hello_a.nonce,
        hello_b.node_id,
        &hello_b.nonce,
        node::MESH_PROTOCOL_VERSION,
        now,
    );
    assert_eq!(
        node.public().verify(&transcript, &prekey_signature),
        Err(CryptoError::BadSignature),
        "a prekey signature verified over a mesh transcript"
    );
}

#[test]
fn aead_failures_are_indistinguishable() {
    // §12.3, claim C21. `CryptoError::DecryptionFailed` deliberately does not
    // say which part failed, because a decryption routine that distinguishes
    // "wrong key" from "tampered tag" from "wrong context" hands an attacker
    // an oracle to walk. This pins the uniformity across the three causes an
    // attacker can actually provoke on one sealed message.
    let mut random = SeededRandom::new(5);
    let key = SymmetricKey::from_bytes([1u8; 32]);
    let sealed = aead::seal(&key, b"conversation-a", b"body", &mut random).expect("seals");

    let wrong_key = aead::open(
        &SymmetricKey::from_bytes([2u8; 32]),
        b"conversation-a",
        &sealed,
    );
    let wrong_context = aead::open(&key, b"conversation-b", &sealed);
    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    let tampered = aead::open(&key, b"conversation-a", &tampered);

    assert_eq!(wrong_key, Err(CryptoError::DecryptionFailed));
    assert_eq!(wrong_context, Err(CryptoError::DecryptionFailed));
    assert_eq!(tampered, Err(CryptoError::DecryptionFailed));
}
