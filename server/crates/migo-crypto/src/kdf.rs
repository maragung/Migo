//! Key derivation.
//!
//! HKDF-SHA256 ([RFC 5869]) everywhere, and never a bare hash. The distinction
//! matters: `SHA256(secret || label)` is a length-extension hazard and gives no
//! domain separation, while HKDF's extract-then-expand structure is designed for
//! exactly this job and is what every reviewed protocol uses.
//!
//! Every derivation in Migo passes a distinct `info` label. Two different keys
//! must never come from the same input material with the same label, because
//! that is how a key that protects one thing ends up protecting another. The
//! labels are constants in this module rather than string literals at call sites,
//! so the full set is greppable in one place.
//!
//! [RFC 5869]: https://www.rfc-editor.org/rfc/rfc5869

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

/// Label for the X3DH shared secret.
pub const LABEL_X3DH: &[u8] = b"migo-x3dh-v1";
/// Label for a Double Ratchet root-key step.
pub const LABEL_RATCHET_ROOT: &[u8] = b"migo-ratchet-root-v1";
/// Label for a Double Ratchet chain-key step.
pub const LABEL_RATCHET_CHAIN: &[u8] = b"migo-ratchet-chain-v1";
/// Label for a per-message key derived from a chain key.
pub const LABEL_MESSAGE_KEY: &[u8] = b"migo-message-key-v1";
/// Label for a group sender-key chain step.
pub const LABEL_SENDER_CHAIN: &[u8] = b"migo-sender-chain-v1";
/// Label for a group sender-key per-message key.
pub const LABEL_SENDER_MESSAGE: &[u8] = b"migo-sender-message-v1";
/// Label for the key that encrypts a client-side backup.
pub const LABEL_BACKUP: &[u8] = b"migo-backup-v1";
/// Label for deriving a device-storage key from a recovery key.
pub const LABEL_RECOVERY: &[u8] = b"migo-recovery-v1";

/// Derives `N` bytes from `secret` under `label`.
///
/// `salt` is optional because HKDF is defined that way, and because the ratchet
/// steps use the previous root key as salt rather than a random value.
#[must_use]
pub fn derive<const N: usize>(secret: &[u8], salt: Option<&[u8]>, label: &[u8]) -> [u8; N] {
    let hk = Hkdf::<Sha256>::new(salt, secret);
    let mut out = [0u8; N];
    // `expand` fails only when the output length exceeds 255 × 32 bytes, which a
    // const generic of this size cannot reach.
    hk.expand(label, &mut out)
        .expect("HKDF output length is within one round");
    out
}

/// Derives two keys at once from a single extract step.
///
/// The ratchet needs a new root key and a new chain key from the same DH output.
/// Deriving them from one expansion, at different offsets, is the standard
/// construction; running HKDF twice with different labels would also work but
/// costs an extra extract for no benefit.
#[must_use]
pub fn derive_pair<const A: usize, const B: usize>(
    secret: &[u8],
    salt: Option<&[u8]>,
    label: &[u8],
) -> ([u8; A], [u8; B]) {
    let hk = Hkdf::<Sha256>::new(salt, secret);
    let mut buf = vec![0u8; A + B];
    hk.expand(label, &mut buf)
        .expect("HKDF output length is within one round");
    let mut first = [0u8; A];
    let mut second = [0u8; B];
    first.copy_from_slice(&buf[..A]);
    second.copy_from_slice(&buf[A..]);
    buf.zeroize();
    (first, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_separate_the_output() {
        let a: [u8; 32] = derive(b"same secret", None, LABEL_X3DH);
        let b: [u8; 32] = derive(b"same secret", None, LABEL_RATCHET_ROOT);
        assert_ne!(a, b, "different labels must give different keys");
    }

    #[test]
    fn salt_separates_the_output() {
        let a: [u8; 32] = derive(b"secret", Some(b"salt-1"), LABEL_X3DH);
        let b: [u8; 32] = derive(b"secret", Some(b"salt-2"), LABEL_X3DH);
        assert_ne!(a, b);
    }

    #[test]
    fn derivation_is_deterministic() {
        let a: [u8; 32] = derive(b"secret", Some(b"salt"), LABEL_X3DH);
        let b: [u8; 32] = derive(b"secret", Some(b"salt"), LABEL_X3DH);
        assert_eq!(
            a, b,
            "the other side of the conversation must get the same key"
        );
    }

    #[test]
    fn a_pair_matches_a_single_expansion_split_in_two() {
        let (root, chain) = derive_pair::<32, 32>(b"dh output", Some(b"root"), LABEL_RATCHET_ROOT);
        let combined: [u8; 64] = derive(b"dh output", Some(b"root"), LABEL_RATCHET_ROOT);
        assert_eq!(root, combined[..32]);
        assert_eq!(chain, combined[32..]);
        assert_ne!(root, chain);
    }

    #[test]
    fn every_label_is_distinct() {
        let labels = [
            LABEL_X3DH,
            LABEL_RATCHET_ROOT,
            LABEL_RATCHET_CHAIN,
            LABEL_MESSAGE_KEY,
            LABEL_SENDER_CHAIN,
            LABEL_SENDER_MESSAGE,
            LABEL_BACKUP,
            LABEL_RECOVERY,
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        let count = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), count, "two derivations share a label");
    }

    #[test]
    fn known_vector_is_stable() {
        // Pins the construction. If this changes, existing sessions break, so a
        // change here must come with a protocol version bump.
        //
        // The expected bytes are not this implementation's own output copied back
        // in: they were computed independently from RFC 5869 (HMAC-SHA256 extract
        // with the salt as key, then one expand round over `info || 0x01`). That
        // is what makes this a cross-check of the construction rather than a
        // record of whatever the current dependency happens to do.
        let out: [u8; 32] = derive(&[0x01; 32], Some(&[0x02; 32]), LABEL_X3DH);
        assert_eq!(
            out,
            [
                0xb2, 0x11, 0x6a, 0xdd, 0x57, 0xfe, 0x58, 0x0d, 0x1d, 0x0c, 0x1c, 0xd7, 0x93, 0x6f,
                0x58, 0x8c, 0x31, 0xae, 0x84, 0x85, 0x3b, 0xda, 0x1f, 0x70, 0xf5, 0x4f, 0x05, 0xaa,
                0x3a, 0x09, 0x0a, 0x0d,
            ]
        );
    }
}
