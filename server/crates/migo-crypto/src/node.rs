//! Server-to-server identity and the mesh handshake.
//!
//! Migo federates between servers, and a federated link is a far more
//! attractive target than a client connection: it carries every conversation
//! that crosses it. So the mesh has its own identity system, separate from user
//! identity, with three properties that TLS alone does not give:
//!
//! - **Mutual authentication by key, not by certificate authority.** Each node
//!   has an Ed25519 key pair and an allow-list of peer public keys. There is no
//!   CA to compromise and no "any valid certificate" failure mode, which is the
//!   one that has bitten every system that trusted the CA pool by default.
//! - **Freshness.** Both sides contribute a nonce and both sign a timestamp, so
//!   a recorded handshake cannot be replayed later. Skew is bounded by
//!   [`MAX_CLOCK_SKEW_MS`].
//! - **Binding.** Each signature covers the signer's id, the peer's id, both
//!   nonces, and the timestamp. A signature collected from a handshake with one
//!   peer therefore cannot be presented to another, which is the reflection
//!   attack that unbound challenge-response protocols keep rediscovering.
//!
//! # What this is not
//!
//! This is not a replacement for TLS; the mesh runs over TLS and this
//! authenticates *inside* it. Belt and braces is the correct posture for a link
//! whose compromise is invisible to users.
//!
//! It is also not client-to-client. Migo never opens a peer-to-peer link
//! between two users' devices for chat: it would leak IP addresses to anyone who
//! can get a conversation started with you, which for a social product is the
//! same as leaking them to everyone.
//!
//! # Handshake
//!
//! ```text
//! A -> B   NodeHello  { node_id: A, nonce: a, protocol_version }
//! B -> A   NodeHello  { node_id: B, nonce: b, protocol_version }
//! A -> B   NodeProof  { signature over ("migo-mesh-v1", A, B, a, b, t_a) }
//! B -> A   NodeProof  { signature over ("migo-mesh-v1", B, A, b, a, t_b) }
//! ```
//!
//! Note the field order in the transcript: each side signs *its own* id first.
//! The verifier reconstructs the transcript from the opposite point of view, so
//! a signature is only valid in the direction it was made.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use migo_core::{Id, Random, Timestamp};
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};
use crate::identity::SIGNATURE_LEN;

/// Domain separator for every mesh signature.
///
/// A node key signs nothing else, but the label is here anyway: the cost is 13
/// bytes and the alternative is discovering later that some other subsystem
/// reused the key and made a cross-protocol forgery possible.
pub const MESH_DOMAIN: &[u8] = b"migo-mesh-v1";

/// Length of a node public key.
pub const NODE_KEY_LEN: usize = 32;

/// Length of a handshake nonce.
pub const NONCE_LEN: usize = 32;

/// Accepted clock skew for a handshake, in milliseconds.
///
/// One minute either way. Wide enough that a node with sloppy NTP still
/// federates, narrow enough that a captured handshake is useless within the
/// minute. Nodes that drift further than this have an operational problem worth
/// surfacing rather than papering over.
pub const MAX_CLOCK_SKEW_MS: i64 = 60_000;

/// Mesh protocol version, sent in the clear and signed.
pub const MESH_PROTOCOL_VERSION: u32 = 1;

/// A node's public identity, as it appears in an allow-list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NodePublic {
    key: VerifyingKey,
}

impl core::fmt::Debug for NodePublic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NodePublic({})", self.fingerprint())
    }
}

impl NodePublic {
    /// Parses 32 raw bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let array: [u8; NODE_KEY_LEN] = bytes.try_into().map_err(|_| CryptoError::BadLength {
            what: "node public key",
            expected: NODE_KEY_LEN,
            actual: bytes.len(),
        })?;
        let key = VerifyingKey::from_bytes(&array).map_err(|_| CryptoError::InvalidPublicKey)?;
        Ok(Self { key })
    }

    /// Raw bytes, for storage and for config files.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; NODE_KEY_LEN] {
        self.key.to_bytes()
    }

    /// Short human-comparable form, for logs and for the operator who is reading
    /// two config files side by side at two in the morning.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let bytes = self.key.to_bytes();
        let mut out = String::with_capacity(23);
        for (index, byte) in bytes.iter().take(8).enumerate() {
            if index > 0 && index % 2 == 0 {
                out.push('-');
            }
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Verifies a mesh signature made by this node.
    pub fn verify(&self, transcript: &[u8], signature: &[u8]) -> Result<()> {
        let array: [u8; SIGNATURE_LEN] =
            signature.try_into().map_err(|_| CryptoError::BadLength {
                what: "mesh signature",
                expected: SIGNATURE_LEN,
                actual: signature.len(),
            })?;
        self.key
            .verify(transcript, &Signature::from_bytes(&array))
            .map_err(|_| CryptoError::BadSignature)
    }
}

/// A node's signing key. Lives only on that node, in a file the process reads at
/// startup and never transmits.
pub struct NodeSecret {
    key: SigningKey,
}

impl Drop for NodeSecret {
    fn drop(&mut self) {
        // `SigningKey` zeroizes its own scalar; this clears the copy of the seed
        // that `to_bytes` would otherwise leave reachable.
        let mut seed = self.key.to_bytes();
        seed.zeroize();
    }
}

impl core::fmt::Debug for NodeSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NodeSecret(public {})", self.public().fingerprint())
    }
}

impl NodeSecret {
    /// Generates a fresh node key. Used by `migod keygen`.
    pub fn generate(random: &mut dyn Random) -> Self {
        let mut seed = [0u8; 32];
        random.fill_bytes(&mut seed);
        let key = SigningKey::from_bytes(&seed);
        seed.zeroize();
        Self { key }
    }

    /// Loads a key from its 32-byte seed.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let array: [u8; 32] = seed.try_into().map_err(|_| CryptoError::BadLength {
            what: "node secret seed",
            expected: 32,
            actual: seed.len(),
        })?;
        Ok(Self {
            key: SigningKey::from_bytes(&array),
        })
    }

    /// The public half, for publishing to peers.
    #[must_use]
    pub fn public(&self) -> NodePublic {
        NodePublic {
            key: self.key.verifying_key(),
        }
    }

    /// The seed, for `migod keygen` to write to a file with mode 0600.
    ///
    /// Nothing else should call this. It exists because a key has to reach disk
    /// somehow, and hiding that behind a vaguer name would not make it safer.
    #[must_use]
    pub fn expose_seed(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// Signs a mesh transcript.
    #[must_use]
    pub fn sign(&self, transcript: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.key.sign(transcript).to_bytes()
    }
}

/// The first message each side sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeHello {
    /// The sender's node id, as registered in the peer's allow-list.
    pub node_id: Id,
    /// Fresh random bytes. Never reused, never derived from a clock.
    pub nonce: [u8; NONCE_LEN],
    /// Mesh protocol version the sender speaks.
    pub protocol_version: u32,
}

impl NodeHello {
    /// Builds a hello with a fresh nonce.
    pub fn new(node_id: Id, random: &mut dyn Random) -> Self {
        let mut nonce = [0u8; NONCE_LEN];
        random.fill_bytes(&mut nonce);
        Self {
            node_id,
            nonce,
            protocol_version: MESH_PROTOCOL_VERSION,
        }
    }
}

/// The second message each side sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeProof {
    /// When the signature was made, by the signer's clock.
    pub signed_at: Timestamp,
    /// Ed25519 signature over the transcript.
    pub signature: [u8; SIGNATURE_LEN],
}

/// Bytes both sides sign, from the signer's point of view.
///
/// Every field that could be swapped by a man in the middle is in here, each
/// fixed-width so the concatenation is unambiguous: the domain label, the mesh
/// version, who signed, who they think they are talking to, both nonces in
/// (signer, peer) order, and the timestamp.
#[must_use]
pub fn transcript(
    signer: Id,
    signer_nonce: &[u8; NONCE_LEN],
    peer: Id,
    peer_nonce: &[u8; NONCE_LEN],
    protocol_version: u32,
    signed_at: Timestamp,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(MESH_DOMAIN.len() + 4 + 16 + 16 + NONCE_LEN * 2 + 8);
    out.extend_from_slice(MESH_DOMAIN);
    out.extend_from_slice(&protocol_version.to_be_bytes());
    out.extend_from_slice(signer.as_bytes());
    out.extend_from_slice(peer.as_bytes());
    out.extend_from_slice(signer_nonce);
    out.extend_from_slice(peer_nonce);
    out.extend_from_slice(&signed_at.as_millis().to_be_bytes());
    out
}

/// Produces this node's proof for a completed hello exchange.
#[must_use]
pub fn prove(
    secret: &NodeSecret,
    local: &NodeHello,
    remote: &NodeHello,
    now: Timestamp,
) -> NodeProof {
    let transcript = transcript(
        local.node_id,
        &local.nonce,
        remote.node_id,
        &remote.nonce,
        MESH_PROTOCOL_VERSION,
        now,
    );
    NodeProof {
        signed_at: now,
        signature: secret.sign(&transcript),
    }
}

/// Verifies the peer's proof.
///
/// `expected` is the public key the allow-list holds for `remote.node_id`. The
/// caller looks it up by id *before* calling this, and a missing entry is a
/// refused connection, not an anonymous one — a node the operator has not named
/// does not get to federate.
pub fn verify_proof(
    expected: &NodePublic,
    local: &NodeHello,
    remote: &NodeHello,
    proof: &NodeProof,
    now: Timestamp,
) -> Result<()> {
    if remote.protocol_version != MESH_PROTOCOL_VERSION {
        return Err(CryptoError::MalformedHeader);
    }
    if remote.node_id == local.node_id {
        // A node cannot federate with itself, and a peer claiming our own id is
        // either a misconfiguration or someone reflecting our hello back at us.
        return Err(CryptoError::MalformedHeader);
    }
    if remote.nonce == local.nonce {
        // Reflection: the peer echoed our nonce instead of generating one. With a
        // 32-byte random nonce this cannot happen by chance.
        return Err(CryptoError::MalformedHeader);
    }
    let skew = proof.signed_at.as_millis() - now.as_millis();
    if skew.abs() > MAX_CLOCK_SKEW_MS {
        return Err(CryptoError::BadSignature);
    }
    // Reconstructed from the *peer's* point of view: their id and nonce first.
    let transcript = transcript(
        remote.node_id,
        &remote.nonce,
        local.node_id,
        &local.nonce,
        MESH_PROTOCOL_VERSION,
        proof.signed_at,
    );
    expected.verify(&transcript, &proof.signature)
}

#[cfg(test)]
mod tests {
    use migo_core::SeededRandom;

    use super::*;

    struct Node {
        secret: NodeSecret,
        hello: NodeHello,
    }

    fn node(seed: u64, id: u8) -> Node {
        let mut random = SeededRandom::new(seed);
        let secret = NodeSecret::generate(&mut random);
        let hello = NodeHello::new(Id::from_bytes([id; 16]), &mut random);
        Node { secret, hello }
    }

    fn pair() -> (Node, Node) {
        (node(1, 0xaa), node(2, 0xbb))
    }

    fn now() -> Timestamp {
        Timestamp::from_millis(1_700_000_000_000)
    }

    /// `verifier` checks a proof it believes `signer` produced.
    ///
    /// The helper exists because [`verify_proof`] takes the local hello before the
    /// remote one, and getting that pair backwards produces a test that fails for
    /// the wrong reason — or worse, one that passes for the wrong reason. Naming
    /// the two roles makes the mistake unwritable.
    fn verify_as(verifier: &Node, signer: &Node, proof: &NodeProof, now: Timestamp) -> Result<()> {
        verify_proof(
            &signer.secret.public(),
            &verifier.hello,
            &signer.hello,
            proof,
            now,
        )
    }

    #[test]
    fn a_handshake_completes_in_both_directions() {
        let (a, b) = pair();
        let t = now();
        let proof_a = prove(&a.secret, &a.hello, &b.hello, t);
        let proof_b = prove(&b.secret, &b.hello, &a.hello, t);
        assert!(verify_as(&b, &a, &proof_a, t).is_ok());
        assert!(verify_as(&a, &b, &proof_b, t).is_ok());
    }

    #[test]
    fn a_proof_cannot_be_reflected_back_at_its_own_author() {
        // A signs, the attacker sends A's own proof back to A claiming to be B.
        // Because each side signs its own id and nonce first, the transcript A
        // reconstructs for B is not the one A signed.
        let (a, b) = pair();
        let t = now();
        let proof_a = prove(&a.secret, &a.hello, &b.hello, t);
        assert_eq!(
            verify_as(&a, &b, &proof_a, t),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_proof_for_one_peer_does_not_work_for_another() {
        let (a, b) = pair();
        let c = node(3, 0xcc);
        let t = now();
        let proof_a = prove(&a.secret, &a.hello, &b.hello, t);
        // C is handed A's genuine signature, but C's nonce was never in it.
        assert_eq!(
            verify_as(&c, &a, &proof_a, t),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_recorded_handshake_expires() {
        let (a, b) = pair();
        let t = now();
        let proof = prove(&a.secret, &a.hello, &b.hello, t);
        let later = Timestamp::from_millis(t.as_millis() + MAX_CLOCK_SKEW_MS + 1);
        assert_eq!(
            verify_as(&b, &a, &proof, later),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_proof_from_the_future_is_refused() {
        let (a, b) = pair();
        let t = now();
        let ahead = Timestamp::from_millis(t.as_millis() + MAX_CLOCK_SKEW_MS + 1);
        let proof = prove(&a.secret, &a.hello, &b.hello, ahead);
        assert_eq!(verify_as(&b, &a, &proof, t), Err(CryptoError::BadSignature));
    }

    #[test]
    fn skew_inside_the_bound_is_tolerated() {
        let (a, b) = pair();
        let t = now();
        let proof = prove(&a.secret, &a.hello, &b.hello, t);
        for offset in [-MAX_CLOCK_SKEW_MS, 0, MAX_CLOCK_SKEW_MS] {
            let observed = Timestamp::from_millis(t.as_millis() + offset);
            assert!(
                verify_as(&b, &a, &proof, observed).is_ok(),
                "skew {offset} ms"
            );
        }
    }

    #[test]
    fn an_edited_timestamp_is_refused() {
        // The timestamp is inside the signature, so sliding it to stay inside the
        // skew window breaks the signature instead of extending the replay window.
        let (a, b) = pair();
        let t = now();
        let mut proof = prove(&a.secret, &a.hello, &b.hello, t);
        proof.signed_at = Timestamp::from_millis(t.as_millis() + 1);
        assert_eq!(verify_as(&b, &a, &proof, t), Err(CryptoError::BadSignature));
    }

    #[test]
    fn a_swapped_node_id_is_refused() {
        let (a, b) = pair();
        let t = now();
        let proof = prove(&a.secret, &a.hello, &b.hello, t);
        let mut lying = a.hello;
        lying.node_id = Id::from_bytes([0xcd; 16]);
        assert_eq!(
            verify_proof(&a.secret.public(), &b.hello, &lying, &proof, t),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn an_echoed_nonce_is_refused_before_any_verification() {
        let (a, b) = pair();
        let t = now();
        let mut echo = b.hello;
        echo.nonce = a.hello.nonce;
        let proof = prove(&b.secret, &echo, &a.hello, t);
        assert_eq!(
            verify_proof(&b.secret.public(), &a.hello, &echo, &proof, t),
            Err(CryptoError::MalformedHeader)
        );
    }

    #[test]
    fn a_peer_claiming_our_own_id_is_refused() {
        let (a, _b) = pair();
        let mut twin = node(4, 0x11);
        twin.hello.node_id = a.hello.node_id;
        let t = now();
        let proof = prove(&twin.secret, &twin.hello, &a.hello, t);
        assert_eq!(
            verify_proof(&twin.secret.public(), &a.hello, &twin.hello, &proof, t),
            Err(CryptoError::MalformedHeader)
        );
    }

    #[test]
    fn a_version_mismatch_is_refused() {
        let (a, b) = pair();
        let t = now();
        let mut ahead = a.hello;
        ahead.protocol_version = MESH_PROTOCOL_VERSION + 1;
        let proof = prove(&a.secret, &ahead, &b.hello, t);
        assert_eq!(
            verify_proof(&a.secret.public(), &b.hello, &ahead, &proof, t),
            Err(CryptoError::MalformedHeader)
        );
    }

    #[test]
    fn a_wrong_public_key_is_refused() {
        let (a, b) = pair();
        let c = node(5, 0xcc);
        let t = now();
        let proof = prove(&a.secret, &a.hello, &b.hello, t);
        assert_eq!(
            verify_proof(&c.secret.public(), &b.hello, &a.hello, &proof, t),
            Err(CryptoError::BadSignature)
        );
    }

    #[test]
    fn a_truncated_signature_is_refused_as_a_length_error() {
        let (a, b) = pair();
        let t = now();
        let proof = prove(&a.secret, &a.hello, &b.hello, t);
        assert!(matches!(
            a.secret
                .public()
                .verify(b"anything", &proof.signature[..32]),
            Err(CryptoError::BadLength {
                what: "mesh signature",
                ..
            })
        ));
    }

    #[test]
    fn a_public_key_round_trips() {
        let (a, _) = pair();
        let public = a.secret.public();
        assert_eq!(
            NodePublic::parse(&public.to_bytes()).expect("parses"),
            public
        );
        assert!(matches!(
            NodePublic::parse(&[0u8; 31]),
            Err(CryptoError::BadLength {
                what: "node public key",
                ..
            })
        ));
    }

    #[test]
    fn a_secret_round_trips_through_its_seed() {
        let (a, _) = pair();
        let reloaded = NodeSecret::from_seed(&a.secret.expose_seed()).expect("loads");
        assert_eq!(reloaded.public(), a.secret.public());
        assert!(matches!(
            NodeSecret::from_seed(&[0u8; 16]),
            Err(CryptoError::BadLength {
                what: "node secret seed",
                ..
            })
        ));
    }

    #[test]
    fn a_fingerprint_is_readable_and_short() {
        let (a, _) = pair();
        let fingerprint = a.secret.public().fingerprint();
        assert_eq!(fingerprint.len(), 19, "8 bytes as hex plus 3 separators");
        assert_eq!(fingerprint.matches('-').count(), 3);
    }

    #[test]
    fn a_secret_prints_only_its_public_fingerprint() {
        let (a, _) = pair();
        let public = a.secret.public();
        assert_eq!(
            format!("{:?}", a.secret),
            format!("NodeSecret(public {})", public.fingerprint())
        );
    }

    #[test]
    fn two_hellos_never_share_a_nonce() {
        let mut random = SeededRandom::new(9);
        let id = Id::from_bytes([1; 16]);
        let first = NodeHello::new(id, &mut random);
        let second = NodeHello::new(id, &mut random);
        assert_ne!(first.nonce, second.nonce);
    }

    #[test]
    fn the_transcript_binds_every_field() {
        // Each field is changed one at a time; every change must move the bytes.
        let base = transcript(
            Id::from_bytes([1; 16]),
            &[2; NONCE_LEN],
            Id::from_bytes([3; 16]),
            &[4; NONCE_LEN],
            1,
            now(),
        );
        let variants = [
            transcript(
                Id::from_bytes([9; 16]),
                &[2; NONCE_LEN],
                Id::from_bytes([3; 16]),
                &[4; NONCE_LEN],
                1,
                now(),
            ),
            transcript(
                Id::from_bytes([1; 16]),
                &[9; NONCE_LEN],
                Id::from_bytes([3; 16]),
                &[4; NONCE_LEN],
                1,
                now(),
            ),
            transcript(
                Id::from_bytes([1; 16]),
                &[2; NONCE_LEN],
                Id::from_bytes([9; 16]),
                &[4; NONCE_LEN],
                1,
                now(),
            ),
            transcript(
                Id::from_bytes([1; 16]),
                &[2; NONCE_LEN],
                Id::from_bytes([3; 16]),
                &[9; NONCE_LEN],
                1,
                now(),
            ),
            transcript(
                Id::from_bytes([1; 16]),
                &[2; NONCE_LEN],
                Id::from_bytes([3; 16]),
                &[4; NONCE_LEN],
                2,
                now(),
            ),
            transcript(
                Id::from_bytes([1; 16]),
                &[2; NONCE_LEN],
                Id::from_bytes([3; 16]),
                &[4; NONCE_LEN],
                1,
                Timestamp::from_millis(now().as_millis() + 1),
            ),
        ];
        for (index, variant) in variants.iter().enumerate() {
            assert_ne!(&base, variant, "field {index} is not bound");
            assert_eq!(base.len(), variant.len(), "the transcript is fixed width");
        }
        // Swapping the two roles must also change the bytes, or a proof would be
        // valid in both directions.
        let swapped = transcript(
            Id::from_bytes([3; 16]),
            &[4; NONCE_LEN],
            Id::from_bytes([1; 16]),
            &[2; NONCE_LEN],
            1,
            now(),
        );
        assert_ne!(base, swapped);
    }
}
