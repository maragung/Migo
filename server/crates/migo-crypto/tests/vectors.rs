//! Runs the cross-language conformance vectors in `shared/protocol/vectors/crypto`.
//!
//! Cryptographic code has a failure mode that ordinary code does not: it can be
//! wrong and still work. A KDF called with the salt and the secret transposed
//! produces perfectly good-looking random bytes, round-trips through its own
//! `open`, and passes every property test — and then the Kotlin client derives a
//! different key and nobody can read anything. Worse, the mistake is unfixable
//! once shipped, because the wrong bytes are now the format that a million
//! stored messages were sealed under.
//!
//! So the expected values here come from outside this crate. The HKDF, HMAC, and
//! HChaCha20 in `tools/vectors/generate_crypto_vectors.py` were written from RFC
//! 5869, RFC 2104, and draft-irtf-cfrg-xchacha, and that generator refuses to
//! emit anything until it reproduces those documents' own published vectors.
//! Where the RFC vectors are carried through into these files, they are labelled
//! with a `provenance` of `rfc-5869` or `rfc-4231-construction`, and a failure
//! there means the composition is wrong in a way that no amount of internal
//! agreement would have revealed.
//!
//! The rejection cases are not decoration. `open` returning a plaintext for a
//! message whose tag was flipped by one bit is not a bug in a corner case; it is
//! the absence of authentication, which is the only property the AEAD was chosen
//! for.

use std::path::PathBuf;

use migo_crypto::error::CryptoError;
use migo_crypto::{aead, kdf, mac, MacKey, SymmetricKey};
use serde_json::Value;

// --- loading ----------------------------------------------------------------

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../shared/protocol/vectors/crypto")
        .canonicalize()
        .expect(
            "shared/protocol/vectors/crypto must exist; run tools/vectors/generate_crypto_vectors.py",
        )
}

fn load(file: &str) -> Value {
    let path = vectors_dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Returns a section as a non-empty array. Empty is a failure: a crypto suite
/// that runs zero cases reports the strongest guarantee in the codebase and
/// checks nothing.
fn section<'a>(file: &'a Value, name: &str, path: &str) -> &'a Vec<Value> {
    let array = file
        .get(name)
        .unwrap_or_else(|| panic!("{path} has no `{name}` section"))
        .as_array()
        .unwrap_or_else(|| panic!("{path} `{name}` is not an array"));
    assert!(!array.is_empty(), "{path} `{name}` is empty");
    array
}

fn text<'a>(case: &'a Value, key: &str) -> &'a str {
    case.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case is missing string field `{key}`: {case}"))
}

fn name(case: &Value) -> &str {
    text(case, "name")
}

fn raw(case: &Value, key: &str) -> Vec<u8> {
    hex::decode(text(case, key)).unwrap_or_else(|e| panic!("field `{key}` is not hex: {e}"))
}

fn length(case: &Value, key: &str) -> usize {
    usize::try_from(
        case.get(key)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("case is missing integer field `{key}`: {case}")),
    )
    .expect("a length fits usize")
}

/// An absent `salt` field, or an explicit `null`, means RFC 5869's absent salt.
///
/// Kept distinct from `Some(&[])` on purpose. The two produce the same PRK —
/// HMAC pads a zero-length key and a 32-zero-byte key to the same block — and
/// `kdf.json` carries both so that a re-implementation which conflates them can
/// be shown to be accidentally right rather than assumed to be.
fn salt_of(case: &Value) -> Option<Vec<u8>> {
    match case.get("salt") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            Some(hex::decode(text).unwrap_or_else(|e| panic!("salt is not hex: {e}")))
        }
        Some(other) => panic!("salt has type {other:?}"),
    }
}

fn key_of(case: &Value, field: &str) -> SymmetricKey {
    let bytes = raw(case, field);
    SymmetricKey::parse(&bytes).unwrap_or_else(|e| panic!("field `{field}` is not a key: {e}"))
}

fn mac_key_bytes(case: &Value, field: &str) -> [u8; 32] {
    raw(case, field)
        .try_into()
        .expect("a mac key is exactly 32 bytes")
}

/// The variant name of an error, which is what a vector file names. `Display`
/// text is written for operators and gets reworded; the variant is the identity.
fn kind(error: &CryptoError) -> &'static str {
    match error {
        CryptoError::BadLength { .. } => "BadLength",
        CryptoError::InvalidPublicKey => "InvalidPublicKey",
        CryptoError::BadSignature => "BadSignature",
        CryptoError::DecryptionFailed => "DecryptionFailed",
        CryptoError::NoSession => "NoSession",
        CryptoError::ChainGapTooLarge => "ChainGapTooLarge",
        CryptoError::KeyAlreadyUsed => "KeyAlreadyUsed",
        CryptoError::MalformedHeader => "MalformedHeader",
        CryptoError::PassphraseHash => "PassphraseHash",
        CryptoError::InvalidPrekeyBundle => "InvalidPrekeyBundle",
    }
}

// --- label tables -----------------------------------------------------------
//
// A vector file names a label as text, but the runner resolves it through the
// crate's own constants and panics on an unknown name. Feeding `label.as_bytes()`
// straight to the KDF would make the test pass for a file that says
// `migo-x3dh-v2`, which is precisely the change that must not go unnoticed:
// renaming a label silently re-keys every session in the deployment.

fn kdf_label(label: &str) -> &'static [u8] {
    match label {
        "migo-x3dh-v1" => kdf::LABEL_X3DH,
        "migo-ratchet-root-v1" => kdf::LABEL_RATCHET_ROOT,
        "migo-ratchet-chain-v1" => kdf::LABEL_RATCHET_CHAIN,
        "migo-message-key-v1" => kdf::LABEL_MESSAGE_KEY,
        "migo-sender-chain-v1" => kdf::LABEL_SENDER_CHAIN,
        "migo-sender-message-v1" => kdf::LABEL_SENDER_MESSAGE,
        "migo-backup-v1" => kdf::LABEL_BACKUP,
        "migo-recovery-v1" => kdf::LABEL_RECOVERY,
        other => panic!("`{other}` is not a KDF label this build defines"),
    }
}

fn mac_label(label: &str) -> &'static [u8] {
    match label {
        "migo-session-token-v1" => mac::LABEL_SESSION_TOKEN,
        "migo-refresh-token-v1" => mac::LABEL_REFRESH_TOKEN,
        "migo-resume-cursor-v1" => mac::LABEL_RESUME_CURSOR,
        "migo-media-url-v1" => mac::LABEL_MEDIA_URL,
        "migo-pagination-v1" => mac::LABEL_PAGINATION,
        "migo-verification-v1" => mac::LABEL_VERIFICATION,
        "migo-webhook-v1" => mac::LABEL_WEBHOOK,
        other => panic!("`{other}` is not a MAC label this build defines"),
    }
}

/// Calls the const-generic [`kdf::derive`] for a length read at runtime.
///
/// The arm list is the set of output widths the vectors exercise; a new width
/// fails here loudly rather than being coerced into the nearest existing one.
fn derive_dyn(secret: &[u8], salt: Option<&[u8]>, label: &[u8], out: usize) -> Vec<u8> {
    match out {
        16 => kdf::derive::<16>(secret, salt, label).to_vec(),
        32 => kdf::derive::<32>(secret, salt, label).to_vec(),
        42 => kdf::derive::<42>(secret, salt, label).to_vec(),
        48 => kdf::derive::<48>(secret, salt, label).to_vec(),
        64 => kdf::derive::<64>(secret, salt, label).to_vec(),
        82 => kdf::derive::<82>(secret, salt, label).to_vec(),
        other => panic!("no derive arm for {other} bytes; add one"),
    }
}

// --- kdf --------------------------------------------------------------------

#[test]
fn hkdf_derivations_match_the_vectors() {
    let file = load("kdf.json");
    for case in section(&file, "cases", "kdf.json") {
        let secret = raw(case, "secret");
        let salt = salt_of(case);
        let label = kdf_label(text(case, "label"));
        let okm = derive_dyn(&secret, salt.as_deref(), label, length(case, "length"));
        assert_eq!(
            hex::encode(&okm),
            text(case, "okm"),
            "derivation for case `{}`",
            name(case)
        );
    }
}

#[test]
fn the_rfc_5869_vectors_pass_through_this_kdf() {
    // These are the specification's own numbers. They are the difference between
    // "this crate agrees with itself" and "this crate implements HKDF-SHA256".
    let file = load("kdf.json");
    for case in section(&file, "rfc", "kdf.json") {
        let secret = raw(case, "secret");
        let salt = salt_of(case);
        let info = raw(case, "label_hex");
        let okm = derive_dyn(&secret, salt.as_deref(), &info, length(case, "length"));
        assert_eq!(
            hex::encode(&okm),
            text(case, "okm"),
            "RFC 5869 case `{}`",
            name(case)
        );
    }
}

#[test]
fn an_absent_salt_and_an_empty_salt_agree() {
    // Not a coincidence to be relied on blindly, but a documented consequence of
    // RFC 5869 plus HMAC's key padding. It is asserted because the two spellings
    // appear in different call sites, and a future refactor that "fixes" one of
    // them into the other must not be able to change any derived key.
    let file = load("kdf.json");
    let cases = section(&file, "cases", "kdf.json");
    let find = |wanted: &str| {
        cases
            .iter()
            .find(|case| name(case) == wanted)
            .unwrap_or_else(|| panic!("kdf.json must carry the `{wanted}` case"))
    };
    let absent = find("ratchet_root_with_absent_salt");
    let empty = find("ratchet_root_with_empty_salt");
    assert!(absent.get("salt").is_none() || absent["salt"].is_null());
    assert_eq!(text(empty, "salt"), "", "the empty-salt case must be empty");
    assert_eq!(
        text(absent, "okm"),
        text(empty, "okm"),
        "an absent salt is HashLen zero bytes, which HMAC pads identically to a zero-length key"
    );
}

#[test]
fn paired_derivations_split_one_expansion() {
    let file = load("kdf.json");
    for case in section(&file, "pairs", "kdf.json") {
        let secret = raw(case, "secret");
        let salt = salt_of(case);
        let label = kdf_label(text(case, "label"));
        let first_len = length(case, "first_length");
        let second_len = length(case, "second_length");

        let (first, second): (Vec<u8>, Vec<u8>) = match (first_len, second_len) {
            (32, 32) => {
                let (a, b) = kdf::derive_pair::<32, 32>(&secret, salt.as_deref(), label);
                (a.to_vec(), b.to_vec())
            }
            (32, 16) => {
                let (a, b) = kdf::derive_pair::<32, 16>(&secret, salt.as_deref(), label);
                (a.to_vec(), b.to_vec())
            }
            other => panic!("no derive_pair arm for {other:?}; add one"),
        };
        assert_eq!(
            hex::encode(&first),
            text(case, "first"),
            "first half of pair `{}`",
            name(case)
        );
        assert_eq!(
            hex::encode(&second),
            text(case, "second"),
            "second half of pair `{}`",
            name(case)
        );

        // The pair is one expansion of A+B bytes split at A, not two expansions.
        // Asserting it here means a "simplification" into two `derive` calls
        // cannot pass, because two calls would produce two copies of the same
        // prefix instead of a contiguous stream.
        let combined = derive_dyn(&secret, salt.as_deref(), label, first_len + second_len);
        assert_eq!(
            hex::encode(&combined),
            format!("{}{}", text(case, "first"), text(case, "second")),
            "pair `{}` must be one expansion split in two",
            name(case)
        );
    }
}

// --- aead -------------------------------------------------------------------

#[test]
fn sealed_envelopes_match_the_vectors() {
    let file = load("aead.json");
    for case in section(&file, "cases", "aead.json") {
        let key = key_of(case, "key");
        let nonce: [u8; aead::NONCE_LEN] = raw(case, "nonce")
            .try_into()
            .expect("an XChaCha nonce is 24 bytes");
        let aad = raw(case, "aad");
        let plaintext = raw(case, "plaintext");
        let expected = text(case, "sealed");

        let sealed = aead::seal_with_nonce(&key, &nonce, &aad, &plaintext)
            .unwrap_or_else(|e| panic!("case `{}` must seal: {e}", name(case)));
        assert_eq!(
            hex::encode(&sealed),
            expected,
            "sealing case `{}`",
            name(case)
        );
        assert_eq!(
            sealed.len(),
            aead::NONCE_LEN + plaintext.len() + aead::TAG_LEN,
            "case `{}` layout is nonce || ciphertext || tag",
            name(case)
        );

        let opened = aead::open(&key, &aad, &sealed)
            .unwrap_or_else(|e| panic!("case `{}` must open: {e}", name(case)));
        assert_eq!(
            hex::encode(&opened),
            hex::encode(&plaintext),
            "opening case `{}`",
            name(case)
        );
    }
}

#[test]
fn tampered_envelopes_are_refused() {
    let file = load("aead.json");
    for case in section(&file, "invalid", "aead.json") {
        let key = key_of(case, "key");
        let aad = raw(case, "aad");
        let sealed = raw(case, "sealed");
        let expected = text(case, "error");
        let why = case.get("why").and_then(Value::as_str).unwrap_or("");

        match aead::open(&key, &aad, &sealed) {
            Ok(plaintext) => panic!(
                "case `{}` was accepted but must fail with {expected}: {why}\n  \
                 recovered {} bytes, which means this build has no authentication",
                name(case),
                plaintext.len()
            ),
            Err(error) => assert_eq!(
                kind(&error),
                expected,
                "case `{}` failed with the wrong error: {why}",
                name(case)
            ),
        }
    }
}

// --- mac --------------------------------------------------------------------

#[test]
fn token_macs_match_the_vectors() {
    let file = load("mac.json");
    for case in section(&file, "cases", "mac.json") {
        let root = raw(case, "root");
        let label = mac_label(text(case, "label"));
        let message = raw(case, "message");

        // The subkey is checked separately from the tag so a failure names the
        // half that broke rather than leaving both suspect.
        let subkey = kdf::derive::<32>(&root, None, label);
        assert_eq!(
            hex::encode(subkey),
            text(case, "key"),
            "subkey for case `{}`",
            name(case)
        );

        let key = MacKey::derive(&root, label);
        let tag = key.tag(&message);
        assert_eq!(
            hex::encode(tag),
            text(case, "tag"),
            "tag for case `{}`",
            name(case)
        );
        key.verify(&message, &tag)
            .unwrap_or_else(|e| panic!("case `{}` must verify its own tag: {e}", name(case)));

        // A tag that verifies over the wrong message is not a MAC.
        let mut other = message.clone();
        other.push(0x00);
        assert_eq!(
            key.verify(&other, &tag).map_err(|e| kind(&e)),
            Err("BadSignature"),
            "case `{}` must not verify over a different message",
            name(case)
        );
    }
}

#[test]
fn multi_part_macs_are_length_prefixed() {
    let file = load("mac.json");
    for case in section(&file, "parts", "mac.json") {
        let root = raw(case, "root");
        let label = mac_label(text(case, "label"));
        let parts: Vec<Vec<u8>> = case["parts"]
            .as_array()
            .unwrap_or_else(|| panic!("case `{}` has no parts", name(case)))
            .iter()
            .map(|part| hex::decode(part.as_str().expect("a part is hex")).expect("a part is hex"))
            .collect();
        let borrowed: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();

        let key = MacKey::derive(&root, label);
        assert_eq!(
            hex::encode(kdf::derive::<32>(&root, None, label)),
            text(case, "key"),
            "subkey for case `{}`",
            name(case)
        );
        let tag = key.tag_parts(&borrowed);
        assert_eq!(
            hex::encode(tag),
            text(case, "tag"),
            "multi-part tag for case `{}`",
            name(case)
        );
        key.verify_parts(&borrowed, &tag)
            .unwrap_or_else(|e| panic!("case `{}` must verify: {e}", name(case)));
    }
}

#[test]
fn a_different_split_of_the_same_bytes_gets_a_different_tag() {
    // The canonical HMAC footgun: without length prefixes, a token for user `1`
    // device `23` is also a valid token for user `12` device `3`. The named pairs
    // in the file are exactly the collisions a naive concatenation would produce.
    let file = load("mac.json");
    let parts = section(&file, "parts", "mac.json");
    let tag_named = |wanted: &str| -> &str {
        parts
            .iter()
            .find(|case| name(case) == wanted)
            .map(|case| text(case, "tag"))
            .unwrap_or_else(|| panic!("mac.json `parts` must carry `{wanted}`"))
    };
    for pair in section(&file, "distinct_pairs", "mac.json") {
        let left = text(pair, "left");
        let right = text(pair, "right");
        assert_ne!(
            tag_named(left),
            tag_named(right),
            "`{left}` and `{right}` must not share a tag: {}",
            pair.get("why").and_then(Value::as_str).unwrap_or("")
        );
    }
}

#[test]
fn the_rfc_4231_vectors_pass_through_this_hmac() {
    let file = load("mac.json");
    for case in section(&file, "rfc", "mac.json") {
        let key = MacKey::from_bytes(mac_key_bytes(case, "key"));
        let tag = key.tag(&raw(case, "message"));
        assert_eq!(
            hex::encode(tag),
            text(case, "tag"),
            "RFC 4231 case `{}`",
            name(case)
        );
    }
}

#[test]
fn tag_truncation_follows_the_documented_floor() {
    let file = load("mac.json");
    for case in section(&file, "truncation", "mac.json") {
        let key = MacKey::derive(&raw(case, "root"), mac_label(text(case, "label")));
        let message = raw(case, "message");
        let full = key.tag(&message);
        let take = length(case, "tag_len");
        let accepted = case["accepted"].as_bool().expect("accepted is a bool");
        let why = case.get("why").and_then(Value::as_str).unwrap_or("");

        let result = key.verify(&message, &full[..take.min(full.len())]);
        if accepted {
            result.unwrap_or_else(|e| {
                panic!(
                    "case `{}` must accept a {take}-byte tag: {e} ({why})",
                    name(case)
                )
            });
        } else {
            assert_eq!(
                result.map_err(|e| kind(&e)),
                Err("BadLength"),
                "case `{}` must refuse a {take}-byte tag: {why}",
                name(case)
            );
        }
    }
}

// --- the suite is present at all --------------------------------------------

#[test]
fn every_vector_file_is_present_and_populated() {
    let expected: [(&str, &[&str]); 3] = [
        ("kdf.json", &["cases", "rfc", "pairs"]),
        ("aead.json", &["cases", "invalid"]),
        (
            "mac.json",
            &["cases", "parts", "rfc", "truncation", "distinct_pairs"],
        ),
    ];
    let mut total = 0;
    for (file, sections) in expected {
        let loaded = load(file);
        assert!(
            loaded.get("provenance").and_then(Value::as_str).is_some(),
            "{file} must record where its expected bytes came from"
        );
        for name in sections {
            total += section(&loaded, name, file).len();
        }
    }
    assert!(
        total >= 40,
        "only {total} crypto vector cases, expected at least 40"
    );
}
