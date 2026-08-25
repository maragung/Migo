#!/usr/bin/env python3
"""Generate the crypto conformance vectors in shared/protocol/vectors/crypto/.

An independent implementation of the three constructions migo-crypto uses, built
from the specifications rather than from the Rust code:

* HKDF-SHA256 (RFC 5869) on `hmac` and `hashlib` — extract then expand, written
  out rather than called from a library, so the vectors do not inherit a
  library's interpretation of the RFC.
* HChaCha20 (draft-irtf-cfrg-xchacha section 2.2) and therefore
  XChaCha20-Poly1305, layered on `cryptography`'s ChaCha20Poly1305 for the inner
  AEAD.
* HMAC-SHA256 (RFC 2104) for the token MACs.

Both of the constructions written from scratch here are checked against the
published test vectors before a single output file is produced (see SELF_CHECKS).
That ordering matters: a generator that is wrong produces vectors that are wrong,
and a wrong vector is worse than no vector because the build stays green.

Usage:
    python3 tools/vectors/generate_crypto_vectors.py [--check]

Requires the `cryptography` package for the inner AEAD only.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import pathlib
import struct
import sys

OUT_DIR = (
    pathlib.Path(__file__).resolve().parents[2] / "shared" / "protocol" / "vectors" / "crypto"
)

# --- labels, transcribed from server/crates/migo-crypto/src/kdf.rs and mac.rs --

KDF_LABELS = [
    "migo-x3dh-v1",
    "migo-ratchet-root-v1",
    "migo-ratchet-chain-v1",
    "migo-message-key-v1",
    "migo-sender-chain-v1",
    "migo-sender-message-v1",
    "migo-backup-v1",
    "migo-recovery-v1",
]

MAC_LABELS = [
    "migo-session-token-v1",
    "migo-refresh-token-v1",
    "migo-resume-cursor-v1",
    "migo-media-url-v1",
    "migo-pagination-v1",
    "migo-verification-v1",
    "migo-webhook-v1",
]

HASH_LEN = 32
NONCE_LEN = 24
TAG_LEN = 16
MAC_TAG_LEN = 32
MIN_MAC_TAG_LEN = 16


# --- HKDF-SHA256, RFC 5869 ---------------------------------------------------


def hkdf_extract(salt: bytes | None, ikm: bytes) -> bytes:
    """RFC 5869 section 2.2. An absent salt is HashLen zero bytes, not no salt."""
    if salt is None:
        salt = b"\x00" * HASH_LEN
    return hmac.new(salt, ikm, hashlib.sha256).digest()


def hkdf_expand(prk: bytes, info: bytes, length: int) -> bytes:
    """RFC 5869 section 2.3: T(i) = HMAC(prk, T(i-1) || info || i)."""
    if length > 255 * HASH_LEN:
        raise ValueError("HKDF cannot expand that far")
    out, block, counter = b"", b"", 1
    while len(out) < length:
        block = hmac.new(prk, block + info + bytes([counter]), hashlib.sha256).digest()
        out += block
        counter += 1
    return out[:length]


def hkdf(ikm: bytes, salt: bytes | None, info: bytes, length: int) -> bytes:
    return hkdf_expand(hkdf_extract(salt, ikm), info, length)


# --- HChaCha20 and XChaCha20-Poly1305, draft-irtf-cfrg-xchacha ---------------


def _rotl32(value: int, bits: int) -> int:
    return ((value << bits) | (value >> (32 - bits))) & 0xFFFFFFFF


def _quarter_round(s: list[int], a: int, b: int, c: int, d: int) -> None:
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl32(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl32(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl32(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl32(s[b] ^ s[c], 7)


def hchacha20(key: bytes, nonce16: bytes) -> bytes:
    """The ChaCha20 permutation with no feed-forward, output words 0-3 and 12-15.

    Omitting the final addition of the input state is what makes this a PRF
    suitable for subkey derivation rather than a stream cipher block.
    """
    assert len(key) == 32 and len(nonce16) == 16
    state = [0x61707865, 0x3320646E, 0x79622D32, 0x6B206574]
    state += list(struct.unpack("<8I", key))
    state += list(struct.unpack("<4I", nonce16))
    for _ in range(10):  # 20 rounds = 10 double rounds
        _quarter_round(state, 0, 4, 8, 12)
        _quarter_round(state, 1, 5, 9, 13)
        _quarter_round(state, 2, 6, 10, 14)
        _quarter_round(state, 3, 7, 11, 15)
        _quarter_round(state, 0, 5, 10, 15)
        _quarter_round(state, 1, 6, 11, 12)
        _quarter_round(state, 2, 7, 8, 13)
        _quarter_round(state, 3, 4, 9, 14)
    return struct.pack("<4I", *state[0:4]) + struct.pack("<4I", *state[12:16])


def xchacha20poly1305_seal(key: bytes, nonce24: bytes, aad: bytes, plaintext: bytes) -> bytes:
    """Returns `nonce || ciphertext || tag`, the layout migo-crypto uses."""
    from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305

    assert len(nonce24) == NONCE_LEN
    subkey = hchacha20(key, nonce24[:16])
    inner_nonce = b"\x00\x00\x00\x00" + nonce24[16:]
    body = ChaCha20Poly1305(subkey).encrypt(inner_nonce, plaintext, aad or None)
    return nonce24 + body


# --- HMAC-SHA256 token MACs, RFC 2104 ---------------------------------------


def mac_key(root: bytes, label: bytes) -> bytes:
    return hkdf(root, None, label, 32)


def mac_tag(key: bytes, message: bytes) -> bytes:
    return hmac.new(key, message, hashlib.sha256).digest()


def mac_tag_parts(key: bytes, parts: list[bytes]) -> bytes:
    """Length-prefixed so ("ab", "c") and ("a", "bc") cannot collide."""
    mac = hmac.new(key, digestmod=hashlib.sha256)
    for part in parts:
        mac.update(struct.pack(">Q", len(part)))
        mac.update(part)
    return mac.digest()


# --- self checks against published vectors ----------------------------------


def self_check() -> list[str]:
    """Validates this file's own primitives before it is allowed to emit anything."""
    checked = []

    # RFC 5869 appendix A.1, SHA-256 with salt and info.
    okm = hkdf(
        bytes.fromhex("0b" * 22),
        bytes.fromhex("000102030405060708090a0b0c"),
        bytes.fromhex("f0f1f2f3f4f5f6f7f8f9"),
        42,
    )
    assert okm.hex() == (
        "3cb25f25faacd57a90434f64d0362f2a"
        "2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        "34007208d5b887185865"
    ), f"RFC 5869 A.1 mismatch: {okm.hex()}"
    checked.append("RFC 5869 A.1 (HKDF-SHA256, 42 bytes)")

    # RFC 5869 appendix A.2, the long inputs and a two-round expansion.
    okm = hkdf(bytes(range(0x00, 0x50)), bytes(range(0x60, 0xB0)), bytes(range(0xB0, 0x100)), 82)
    assert okm.hex() == (
        "b11e398dc80327a1c8e7f78c596a4934"
        "4f012eda2d4efad8a050cc4c19afa97c"
        "59045a99cac7827271cb41c65e590e09"
        "da3275600c2f09b8367793a9aca3db71"
        "cc30c58179ec3e87c14c01d5c1f3434f"
        "1d87"
    ), f"RFC 5869 A.2 mismatch: {okm.hex()}"
    checked.append("RFC 5869 A.2 (HKDF-SHA256, 82 bytes, three expand rounds)")

    # RFC 5869 appendix A.3, zero-length salt and info.
    okm = hkdf(bytes.fromhex("0b" * 22), b"", b"", 42)
    assert okm.hex() == (
        "8da4e775a563c18f715f802a063c5a31"
        "b8a11f5c5ee1879ec3454e5f3c738d2d"
        "9d201395faa4b61a96c8"
    ), f"RFC 5869 A.3 mismatch: {okm.hex()}"
    checked.append("RFC 5869 A.3 (HKDF-SHA256, empty salt and info)")

    # draft-irtf-cfrg-xchacha section 2.2.1.
    out = hchacha20(
        bytes.fromhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        bytes.fromhex("000000090000004a0000000031415927"),
    )
    assert out.hex() == (
        "82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc"
    ), f"HChaCha20 draft vector mismatch: {out.hex()}"
    checked.append("draft-irtf-cfrg-xchacha 2.2.1 (HChaCha20 subkey)")

    # RFC 4231 section 4.2, plain HMAC-SHA256, which the token MAC is built on.
    tag = mac_tag(bytes.fromhex("0b" * 20), b"Hi There")
    assert tag.hex() == (
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    ), f"RFC 4231 4.1 mismatch: {tag.hex()}"
    checked.append("RFC 4231 4.1 (HMAC-SHA256)")

    return checked


# --- case lists (hand-chosen) ----------------------------------------------

SECRET_A = bytes.fromhex("01" * 32)
SALT_A = bytes.fromhex("02" * 32)
DH_OUTPUT = bytes.fromhex("03" * 32)
ROOT = b"root"
ROOT_LONG = bytes.fromhex("a1" * 64)


def _hex_or_none(value: bytes | None) -> str | None:
    return None if value is None else value.hex()


def kdf_file() -> dict:
    cases = []

    # The construction pinned by migo-crypto's own known-vector test. If this and
    # that constant ever disagree, one of two independent derivations is wrong and
    # every existing session is at stake, so it is the first case in the file.
    cases.append(
        {
            "name": "x3dh_thirty_two_bytes",
            "secret": SECRET_A.hex(),
            "salt": SALT_A.hex(),
            "label": "migo-x3dh-v1",
            "length": 32,
            "okm": hkdf(SECRET_A, SALT_A, b"migo-x3dh-v1", 32).hex(),
            "provenance": "independent-python",
        }
    )

    # Every label, same inputs: the file is then also a proof of domain
    # separation, because two equal outputs here would be visible at a glance.
    for label in KDF_LABELS:
        cases.append(
            {
                "name": "label_" + label.replace("-", "_"),
                "secret": SECRET_A.hex(),
                "salt": None,
                "label": label,
                "length": 32,
                "okm": hkdf(SECRET_A, None, label.encode(), 32).hex(),
                "provenance": "independent-python",
            }
        )

    # Output lengths other than 32, including one that needs a second HMAC round.
    for length in (16, 64):
        cases.append(
            {
                "name": f"backup_{length}_bytes",
                "secret": SECRET_A.hex(),
                "salt": SALT_A.hex(),
                "label": "migo-backup-v1",
                "length": length,
                "okm": hkdf(SECRET_A, SALT_A, b"migo-backup-v1", length).hex(),
                "provenance": "independent-python",
            }
        )

    # An absent salt and a zero-length salt must agree: HMAC pads either one to
    # the same block. Two cases rather than a note, so a build can prove it.
    for name, salt in (("absent_salt", None), ("empty_salt", b"")):
        cases.append(
            {
                "name": f"ratchet_root_with_{name}",
                "secret": DH_OUTPUT.hex(),
                "salt": _hex_or_none(salt),
                "label": "migo-ratchet-root-v1",
                "length": 32,
                "okm": hkdf(DH_OUTPUT, salt, b"migo-ratchet-root-v1", 32).hex(),
                "provenance": "independent-python",
            }
        )

    # RFC 5869's own vectors, through this function's parameter shape: `info` is
    # the label. These are the cases that can catch a real implementation bug
    # rather than only a divergence between two of our own languages.
    rfc = [
        (
            "rfc_5869_a1",
            bytes.fromhex("0b" * 22),
            bytes.fromhex("000102030405060708090a0b0c"),
            bytes.fromhex("f0f1f2f3f4f5f6f7f8f9"),
            42,
        ),
        (
            "rfc_5869_a2",
            bytes(range(0x00, 0x50)),
            bytes(range(0x60, 0xB0)),
            bytes(range(0xB0, 0x100)),
            82,
        ),
        ("rfc_5869_a3", bytes.fromhex("0b" * 22), b"", b"", 42),
    ]
    rfc_cases = [
        {
            "name": name,
            "secret": ikm.hex(),
            "salt": salt.hex(),
            "label_hex": info.hex(),
            "length": length,
            "okm": hkdf(ikm, salt, info, length).hex(),
            "provenance": "rfc-5869",
        }
        for name, ikm, salt, info, length in rfc
    ]

    pairs = []
    for name, a, b, label in (
        ("ratchet_root_and_chain", 32, 32, "migo-ratchet-root-v1"),
        ("root_and_short_second", 32, 16, "migo-ratchet-chain-v1"),
    ):
        combined = hkdf(DH_OUTPUT, SALT_A, label.encode(), a + b)
        pairs.append(
            {
                "name": name,
                "secret": DH_OUTPUT.hex(),
                "salt": SALT_A.hex(),
                "label": label,
                "first_length": a,
                "second_length": b,
                "first": combined[:a].hex(),
                "second": combined[a:].hex(),
                "provenance": "independent-python",
            }
        )

    return {
        "$comment": "HKDF-SHA256 derivations: every Migo label, several output lengths, and RFC 5869's own vectors.",
        "provenance": "computed by tools/vectors/generate_crypto_vectors.py, an independent HKDF written from RFC 5869",
        "note": "`salt: null` means an absent salt, which RFC 5869 defines as HashLen zero bytes. `label_hex` appears instead of `label` when the info string is not printable text. A pair is one extract and one expand, split at `first_length` — not two derivations.",
        "cases": cases,
        "rfc": rfc_cases,
        "pairs": pairs,
    }


KEY_A = bytes.fromhex("0f" * 32)
KEY_ZERO = bytes(32)
NONCE_A = bytes.fromhex("101112131415161718191a1b1c1d1e1f2021222324252627")
NONCE_ZERO = bytes(NONCE_LEN)


def aead_file() -> dict:
    plans = [
        ("empty_plaintext_empty_aad", KEY_A, NONCE_A, b"", b""),
        ("empty_plaintext_with_aad", KEY_A, NONCE_A, b"migo-envelope-v1", b""),
        ("short_plaintext_no_aad", KEY_A, NONCE_A, b"", b"hello"),
        ("short_plaintext_with_aad", KEY_A, NONCE_A, b"conversation:42", b"hello"),
        (
            "exactly_one_chacha_block",
            KEY_A,
            NONCE_A,
            b"aad",
            bytes(range(64)),
        ),
        (
            "one_block_plus_one_byte",
            KEY_A,
            NONCE_A,
            b"aad",
            bytes(range(65)),
        ),
        ("all_zero_key_and_nonce", KEY_ZERO, NONCE_ZERO, b"", b"zero"),
        (
            "utf8_plaintext",
            KEY_A,
            NONCE_A,
            b"",
            "halo dunia — 世界".encode("utf-8"),
        ),
    ]

    cases = []
    for name, key, nonce, aad, plaintext in plans:
        sealed = xchacha20poly1305_seal(key, nonce, aad, plaintext)
        assert sealed[:NONCE_LEN] == nonce
        assert len(sealed) == NONCE_LEN + len(plaintext) + TAG_LEN, "layout is nonce||ct||tag"
        cases.append(
            {
                "name": name,
                "key": key.hex(),
                "nonce": nonce.hex(),
                "aad": aad.hex(),
                "plaintext": plaintext.hex(),
                "sealed": sealed.hex(),
                "provenance": "independent-python",
            }
        )

    # Rejection cases. Each is derived from a valid sealed message so that the
    # only difference is the one being tested.
    base_key = KEY_A
    base_aad = b"conversation:42"
    base_sealed = bytes.fromhex(
        next(c["sealed"] for c in cases if c["name"] == "short_plaintext_with_aad")
    )

    def flip(data: bytes, index: int) -> bytes:
        out = bytearray(data)
        out[index] ^= 0x01
        return bytes(out)

    invalid = [
        {
            "name": "flipped_tag_bit",
            "key": base_key.hex(),
            "aad": base_aad.hex(),
            "sealed": flip(base_sealed, len(base_sealed) - 1).hex(),
            "error": "DecryptionFailed",
            "why": "the tag is what makes the ciphertext unforgeable",
        },
        {
            "name": "flipped_ciphertext_bit",
            "key": base_key.hex(),
            "aad": base_aad.hex(),
            "sealed": flip(base_sealed, NONCE_LEN).hex(),
            "error": "DecryptionFailed",
            "why": "AEAD is not malleable: one bit changes the tag",
        },
        {
            "name": "flipped_nonce_bit",
            "key": base_key.hex(),
            "aad": base_aad.hex(),
            "sealed": flip(base_sealed, 0).hex(),
            "error": "DecryptionFailed",
            "why": "the nonce is authenticated by being an input to the subkey",
        },
        {
            "name": "wrong_associated_data",
            "key": base_key.hex(),
            "aad": b"conversation:43".hex(),
            "sealed": base_sealed.hex(),
            "error": "DecryptionFailed",
            "why": "an envelope must not open in a conversation it was not sealed for",
        },
        {
            "name": "wrong_key",
            "key": bytes.fromhex("f0" * 32).hex(),
            "aad": base_aad.hex(),
            "sealed": base_sealed.hex(),
            "error": "DecryptionFailed",
            "why": "the obvious case, stated so the file cannot be read as only testing subtle ones",
        },
        {
            "name": "truncated_below_nonce_and_tag",
            "key": base_key.hex(),
            "aad": base_aad.hex(),
            "sealed": base_sealed[: NONCE_LEN + TAG_LEN - 1].hex(),
            "error": "BadLength",
            "why": "rejected by length before any slicing, not by a panic",
        },
        {
            "name": "empty_input",
            "key": base_key.hex(),
            "aad": base_aad.hex(),
            "sealed": "",
            "error": "BadLength",
            "why": "zero bytes cannot contain a nonce",
        },
    ]

    return {
        "$comment": "XChaCha20-Poly1305 sealed envelopes: nonce || ciphertext || tag, plus the tampering a receiver must reject.",
        "provenance": "computed by tools/vectors/generate_crypto_vectors.py: HChaCha20 written from draft-irtf-cfrg-xchacha and checked against its test vector, over cryptography's ChaCha20Poly1305",
        "note": "`sealed` is the whole output of seal_with_nonce, so a runner can compare it to one value instead of reassembling three. Every rejection case is a one-bit or one-field edit of a valid message from `cases`.",
        "cases": cases,
        "invalid": invalid,
    }


def mac_file() -> dict:
    cases = []

    # The construction pinned by migo-crypto's own known-vector test.
    key = mac_key(ROOT, b"migo-session-token-v1")
    cases.append(
        {
            "name": "session_token_over_migo",
            "root": ROOT.hex(),
            "label": "migo-session-token-v1",
            "key": key.hex(),
            "message": b"migo".hex(),
            "tag": mac_tag(key, b"migo").hex(),
            "provenance": "independent-python",
        }
    )

    for label in MAC_LABELS:
        k = mac_key(ROOT_LONG, label.encode())
        cases.append(
            {
                "name": "label_" + label.replace("-", "_"),
                "root": ROOT_LONG.hex(),
                "label": label,
                "key": k.hex(),
                "message": b"subject-1".hex(),
                "tag": mac_tag(k, b"subject-1").hex(),
                "provenance": "independent-python",
            }
        )

    cases.append(
        {
            "name": "empty_message",
            "root": ROOT_LONG.hex(),
            "label": "migo-media-url-v1",
            "key": mac_key(ROOT_LONG, b"migo-media-url-v1").hex(),
            "message": "",
            "tag": mac_tag(mac_key(ROOT_LONG, b"migo-media-url-v1"), b"").hex(),
            "provenance": "independent-python",
        }
    )

    # The whole reason tag_parts exists: concatenation is ambiguous and the
    # length prefix removes the ambiguity. Both splits of "abc" are here so the
    # runner can assert the two tags differ, which is the property that stops a
    # token for user 1 device 23 from being valid for user 12 device 3.
    parts_key = mac_key(ROOT_LONG, b"migo-pagination-v1")
    parts_plans = [
        ("parts_ab_then_c", [b"ab", b"c"]),
        ("parts_a_then_bc", [b"a", b"bc"]),
        ("parts_single", [b"abc"]),
        ("parts_empty_list", []),
        ("parts_with_an_empty_member", [b"a", b"", b"bc"]),
        ("parts_user_and_device", [b"user-1", b"device-23"]),
    ]
    parts = [
        {
            "name": name,
            "root": ROOT_LONG.hex(),
            "label": "migo-pagination-v1",
            "key": parts_key.hex(),
            "parts": [p.hex() for p in members],
            "tag": mac_tag_parts(parts_key, members).hex(),
            "provenance": "independent-python",
        }
        for name, members in parts_plans
    ]

    distinct_pairs = [
        {
            "left": "parts_ab_then_c",
            "right": "parts_a_then_bc",
            "why": "the same concatenation with a different split must not share a tag",
        },
        {
            "left": "parts_ab_then_c",
            "right": "parts_single",
            "why": "two parts and one part are different messages",
        },
        {
            "left": "parts_single",
            "right": "parts_with_an_empty_member",
            "why": "an empty member is a member, and its length prefix says so",
        },
    ]

    # RFC 4231's plain HMAC-SHA256 cases, run through `MacKey::from_bytes` so the
    # HMAC itself is checked against a published value and not only against us.
    rfc = []
    for name, k, message in (
        ("rfc_4231_case_1", bytes.fromhex("0b" * 20) + bytes(12), b"Hi There"),
        ("rfc_4231_case_2", b"Jefe" + bytes(28), b"what do ya want for nothing?"),
        ("rfc_4231_case_3", bytes.fromhex("aa" * 32), bytes.fromhex("dd" * 50)),
    ):
        rfc.append(
            {
                "name": name,
                "key": k.hex(),
                "message": message.hex(),
                "tag": mac_tag(k, message).hex(),
                "provenance": "rfc-4231-construction",
            }
        )

    truncation = [
        {
            "name": "sixteen_byte_prefix_verifies",
            "root": ROOT.hex(),
            "label": "migo-session-token-v1",
            "message": b"migo".hex(),
            "tag_len": MIN_MAC_TAG_LEN,
            "accepted": True,
            "why": f"MIN_TAG_LEN is {MIN_MAC_TAG_LEN}: the same 128-bit margin as the AEAD tag",
        },
        {
            "name": "full_width_verifies",
            "root": ROOT.hex(),
            "label": "migo-session-token-v1",
            "message": b"migo".hex(),
            "tag_len": MAC_TAG_LEN,
            "accepted": True,
            "why": "the default",
        },
        {
            "name": "fifteen_byte_prefix_is_refused",
            "root": ROOT.hex(),
            "label": "migo-session-token-v1",
            "message": b"migo".hex(),
            "tag_len": MIN_MAC_TAG_LEN - 1,
            "accepted": False,
            "why": "a shorter tag is refused rather than quietly weakened",
        },
    ]

    return {
        "$comment": "HMAC-SHA256 token MACs: the per-purpose subkey, the tag, the length-prefixed multi-part tag, and truncation policy.",
        "provenance": "computed by tools/vectors/generate_crypto_vectors.py, an independent HKDF and HMAC written from RFC 5869 and RFC 2104",
        "note": "`key` is included as well as `tag` so a failure says which half is wrong: the derivation or the MAC. `rfc` cases bypass the derivation via MacKey::from_bytes, with keys padded to 32 bytes because that is the width this type takes.",
        "cases": cases,
        "parts": parts,
        "distinct_pairs": distinct_pairs,
        "rfc": rfc,
        "truncation": truncation,
    }


# --- driver -----------------------------------------------------------------

FILES = {
    "kdf.json": kdf_file,
    "aead.json": aead_file,
    "mac.json": mac_file,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if the committed files differ")
    parser.add_argument("--quiet", action="store_true", help="only report problems")
    args = parser.parse_args()

    for line in self_check():
        if not args.quiet:
            print(f"self-check ok: {line}")

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    stale = []
    for name, build in FILES.items():
        rendered = json.dumps(build(), indent=2, ensure_ascii=False) + "\n"
        path = OUT_DIR / name
        if args.check:
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current != rendered:
                stale.append(name)
        else:
            path.write_text(rendered, encoding="utf-8")
            print(f"wrote {path.relative_to(OUT_DIR.parents[3])}")

    if stale:
        print("stale crypto vectors: " + ", ".join(stale), file=sys.stderr)
        print("run: python3 tools/vectors/generate_crypto_vectors.py", file=sys.stderr)
        return 1
    if args.check and not args.quiet:
        print(f"up to date: {len(FILES)} crypto vector files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
