#!/usr/bin/env python3
"""Generate the account-root conformance vectors in shared/protocol/vectors/crypto/.

An independent implementation of the account-root derivations Migo uses, built
from the specifications rather than from the Rust code — the same house rule as
`generate_crypto_vectors.py`: expected bytes must come from a *different* codebase
than the one under test, so a shared bug cannot hide behind a green build.

Two constructions are produced here, and two more that build on the wallet:

* Domain-separated seeds (``account-domains.json``): ``HKDF-SHA256`` (RFC 5869)
  splits one 32-byte account root into a seed per purpose — identity, EVM wallet,
  end-to-end encryption, backup, device. The E2EE domain is split once more, by a
  second HKDF round, into the founding device's signing and exchange sub-seeds;
  those two ride on the E2EE domain case as extra fields. HKDF is written out from
  the RFC here, not called from a library KDF, exactly as the crypto generator does.
* EVM wallets (``account-evm.json``): the EVM seed feeds a BIP-32 (BIP-0032)
  hierarchy, ``m/44'/60'/0'/0/i`` per BIP-44, whose leaf key becomes an Ethereum
  address via a secp256k1 public key, Keccak-256, and the EIP-55 checksum. secp256k1
  point arithmetic and Keccak-f[1600] have no home in `hashlib` or `cryptography`,
  so both are implemented here from their specifications.
* Transactions (``account-tx.json``): EIP-1559 bodies and signing hashes over the
  same wallets — RLP written from the Ethereum specification's appendix, ECDSA
  public-key recovery from the curve. Signature bytes are NOT pinned (the ports
  sign with their own libraries and prove validity by recovering the sender);
  one case is a real mainnet C-Chain transaction, pinned raw, so the parse and
  recovery paths are checked against the chain and not only against this file.
* Typed data (``account-eip712.json``): EIP-712 domain separators, hashStructs,
  and digests over a recursive value model, anchored on the EIP's own worked
  example — digest and signature both.

Everything written from scratch is checked against published test vectors before a
single output file is produced (see :func:`self_check`). That ordering is the whole
point: a generator that is wrong produces vectors that are wrong, and a wrong vector
is worse than none because the build stays green while every client agrees on the
wrong bytes.

The house rule limits the borrowed primitives to `hashlib`'s SHA-256 / SHA-512 (and
their HMACs) — the same ones the crypto generator leans on. secp256k1, Keccak-256,
EIP-55, and — for the BIP-32 self-check alone — Base58 and RIPEMD-160 are written
from their specs below. Note that two of those (Base58Check, RIPEMD-160) exist only
to read and reproduce the *published Bitcoin* BIP-32 test vector; no byte emitted
into an account vector ever passes through them.

The two sibling files in this directory — account-mldsa.json and
account-container.json — are rust-reference, written by the migo-account example
binary, and are neither produced nor checked here.

Usage:
    python3 tools/vectors/generate_account_vectors.py [--check] [--quiet]

Pure standard library: no third-party import at all.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import pathlib
import sys

OUT_DIR = (
    pathlib.Path(__file__).resolve().parents[2] / "shared" / "protocol" / "vectors" / "crypto"
)

HASH_LEN = 32  # SHA-256 output, and therefore HKDF's block size

# --- account inputs, fixed literals -----------------------------------------
#
# Three roots chosen to be visibly distinct at a glance, plus the all-zero root:
# a KDF that ignored its input would map every root to the same seeds, and ROOT_C
# sitting beside ROOT_A/ROOT_B in the file makes that failure obvious rather than
# something a reader has to compute to notice.

ROOT_A = bytes.fromhex("8f2a1c9d4e6b3a7f5d0c8e1b9a2f4d6c8e0a2b4d6f8c0e2a4b6d8f0a2c4e6d80")
ROOT_B = bytes.fromhex("1a3c5e7f9b1d3f5a7c9e1b3d5f7a9c1e3b5d7f9a1c3e5b7d9f1a3c5e7b9d1f3a")
ROOT_C = bytes.fromhex("00" * 32)
ROOTS = [("root-a", ROOT_A), ("root-b", ROOT_B), ("root-c", ROOT_C)]

# The five account domains. The label is the exact ASCII passed to HKDF as `info`
# (and stored in the case as `label`); the short word only names the case. Two
# labels that differ by one byte must produce unrelated seeds, which is the
# property the whole file exists to pin. These match the DOMAIN_* constants in
# server/crates/migo-account/src/root.rs.
DOMAINS = [
    ("identity", "MIGO/IDENTITY/V1"),
    ("evm", "MIGO/EVM/V1"),
    ("e2ee", "MIGO/E2EE/V1"),
    ("backup", "MIGO/BACKUP/V1"),
    ("device", "MIGO/DEVICE/V1"),
]

# The E2EE domain seed is split once more, into a signing seed and an exchange
# seed, by a second HKDF round whose `info` is one of these sub-labels. These
# match LABEL_E2EE_SIGNING / LABEL_E2EE_EXCHANGE in root.rs.
E2EE_DOMAIN = "MIGO/E2EE/V1"
E2EE_SIGNING_LABEL = "migo-e2ee-signing-v1"
E2EE_EXCHANGE_LABEL = "migo-e2ee-exchange-v1"

EVM_DOMAIN = "MIGO/EVM/V1"

# BIP-44 for Ethereum: m / 44' / 60' / 0' / 0 / index. The first three levels are
# hardened (high bit set); the change level (0) and the address index are not.
HARDENED = 0x80000000
EVM_PATH_PREFIX = [44 | HARDENED, 60 | HARDENED, 0 | HARDENED, 0]

# Which (root, address index) pairs to emit. index 7 on ROOT_B exercises a
# non-hardened child other than 0; the rest cover the first address of each root.
EVM_CASES = [
    ("root-a", ROOT_A, 0),
    ("root-a", ROOT_A, 1),
    ("root-b", ROOT_B, 0),
    ("root-b", ROOT_B, 7),
    ("root-c", ROOT_C, 0),
]


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


# --- Keccak-256 (Keccak-f[1600]), the pre-NIST padding Ethereum uses ---------
#
# This is original Keccak, not SHA-3: the only difference is the domain byte in
# the sponge padding — 0x01 here, where SHA-3 would use 0x06. Everything else,
# the permutation included, is identical.

_KECCAK_RC = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
]
_KECCAK_RATE = 136  # bytes; 1088-bit rate for the 256-bit capacity variant
_MASK64 = (1 << 64) - 1


def _rotl64(value: int, bits: int) -> int:
    bits %= 64
    return ((value << bits) | (value >> (64 - bits))) & _MASK64


def keccak_f1600(lanes: list[int]) -> None:
    """The 24-round Keccak-f[1600] permutation over 25 lanes, lane (x,y) at x+5y.

    Written straight from the Keccak reference pseudo-code: theta mixes columns,
    rho-and-pi walk the (x,y) lattice rotating by the triangular numbers, chi is
    the only non-linear step, and iota breaks the round symmetry.
    """
    for rnd in range(24):
        # theta: fold each column to a parity, then diffuse it across two columns.
        c = [lanes[x] ^ lanes[x + 5] ^ lanes[x + 10] ^ lanes[x + 15] ^ lanes[x + 20] for x in range(5)]
        d = [c[(x + 4) % 5] ^ _rotl64(c[(x + 1) % 5], 1) for x in range(5)]
        for x in range(5):
            for y in range(5):
                lanes[x + 5 * y] ^= d[x]
        # rho and pi as one walk: (x,y) -> (y, 2x+3y), rotating by (t+1)(t+2)/2.
        x, y = 1, 0
        current = lanes[1]
        for t in range(24):
            x, y = y, (2 * x + 3 * y) % 5
            offset = ((t + 1) * (t + 2) // 2) % 64
            current, lanes[x + 5 * y] = lanes[x + 5 * y], _rotl64(current, offset)
        # chi: the row-wise non-linearity. ~row[i] is negative in Python, but the
        # following AND with a 64-bit value discards the sign bits cleanly.
        for y in range(5):
            row = [lanes[x + 5 * y] for x in range(5)]
            for x in range(5):
                lanes[x + 5 * y] = row[x] ^ ((~row[(x + 1) % 5]) & row[(x + 2) % 5])
        # iota.
        lanes[0] ^= _KECCAK_RC[rnd]


def keccak256(data: bytes) -> bytes:
    """Keccak-256 of `data`. Sponge with rate 136, capacity 512, pad10*1 over 0x01."""
    lanes = [0] * 25
    msg = bytearray(data)
    msg.append(0x01)  # first pad bit AND the (empty) Keccak domain — 0x06 would be SHA-3
    while len(msg) % _KECCAK_RATE != 0:
        msg.append(0x00)
    msg[-1] ^= 0x80  # final pad bit; folds into the 0x01 byte as 0x81 when they coincide
    for offset in range(0, len(msg), _KECCAK_RATE):
        for i in range(_KECCAK_RATE // 8):  # 17 rate lanes
            lanes[i] ^= int.from_bytes(msg[offset + 8 * i : offset + 8 * i + 8], "little")
        keccak_f1600(lanes)
    # One squeeze is enough: 32 bytes fit inside the 136-byte rate.
    return b"".join(lanes[i].to_bytes(8, "little") for i in range(4))[:HASH_LEN]


# --- secp256k1 point arithmetic, SEC 2 / FIPS 186 short Weierstrass ----------
#
# y^2 = x^3 + 7 over F_p, a = 0. Affine coordinates with None as the point at
# infinity. Scalar multiplication is textbook double-and-add: this file is read
# by humans and run once, so constant-time is neither achievable nor wanted.

SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_GX = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
SECP256K1_GY = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8
SECP256K1_G = (SECP256K1_GX, SECP256K1_GY)

Point = tuple[int, int]  # or None for the identity


def _inv_mod_p(value: int) -> int:
    """Field inverse by Fermat, since p is prime: a^(p-2) == a^-1 (mod p)."""
    return pow(value % SECP256K1_P, SECP256K1_P - 2, SECP256K1_P)


def point_on_curve(point: Point | None) -> bool:
    if point is None:
        return True
    x, y = point
    return (y * y - (x * x * x + 7)) % SECP256K1_P == 0


def point_add(p: Point | None, q: Point | None) -> Point | None:
    """Group law. Handles identity, the P + (-P) = O case, and doubling."""
    if p is None:
        return q
    if q is None:
        return p
    x1, y1 = p
    x2, y2 = q
    if x1 == x2:
        if (y1 + y2) % SECP256K1_P == 0:
            return None  # inverses: their sum is the point at infinity
        # x equal and not inverse means p == q, so this is a doubling.
        slope = (3 * x1 * x1) * _inv_mod_p(2 * y1) % SECP256K1_P
    else:
        slope = (y2 - y1) * _inv_mod_p(x2 - x1) % SECP256K1_P
    x3 = (slope * slope - x1 - x2) % SECP256K1_P
    y3 = (slope * (x1 - x3) - y1) % SECP256K1_P
    return (x3, y3)


def point_mul(scalar: int, point: Point | None) -> Point | None:
    """`scalar` times `point`. The scalar is used as given, not reduced mod n, so
    the self-check can assert that exactly n*G lands on the identity."""
    result: Point | None = None
    addend = point
    while scalar:
        if scalar & 1:
            result = point_add(result, addend)
        addend = point_add(addend, addend)
        scalar >>= 1
    return result


def public_point(private_key: int) -> Point:
    """The public key point for a private scalar, checked to lie on the curve —
    the "validate the curve equation for every point you produce" invariant."""
    point = point_mul(private_key, SECP256K1_G)
    assert point is not None and point_on_curve(point), "derived public key is off-curve"
    return point


def ser_uncompressed(point: Point) -> bytes:
    """SEC1 uncompressed: 0x04 || X || Y, 65 bytes."""
    x, y = point
    return b"\x04" + x.to_bytes(32, "big") + y.to_bytes(32, "big")


def ser_compressed(point: Point) -> bytes:
    """SEC1 compressed: 0x02/0x03 by Y parity || X, 33 bytes. BIP-32 hashes this."""
    x, y = point
    return bytes([0x02 | (y & 1)]) + x.to_bytes(32, "big")


# --- BIP-32 hierarchical derivation, BIP-0032 --------------------------------


def _hmac_sha512(key: bytes, message: bytes) -> bytes:
    return hmac.new(key, message, hashlib.sha512).digest()


def bip32_master(seed: bytes) -> tuple[int, bytes]:
    """Master key from a seed: I = HMAC-SHA512("Bitcoin seed", seed); (IL, IR)."""
    i = _hmac_sha512(b"Bitcoin seed", seed)
    secret = int.from_bytes(i[:32], "big")
    # BIP-32: the master is invalid if IL is zero or >= n. It never is for these
    # fixed seeds, but the check is the spec's, so it is asserted rather than assumed.
    assert 0 < secret < SECP256K1_N, "BIP-32 master key is zero or >= n"
    return secret, i[32:]


def bip32_ckd_priv(parent_key: int, parent_chain: bytes, index: int) -> tuple[int, bytes]:
    """CKDpriv, BIP-0032 section on private child key derivation.

    Hardened children (high bit set) commit to the parent *private* key, which is
    what stops a leaked public key and chain code from unrolling the tree; normal
    children commit to the compressed public key so they can be derived publicly.
    """
    if index & HARDENED:
        data = b"\x00" + parent_key.to_bytes(32, "big") + index.to_bytes(4, "big")
    else:
        data = ser_compressed(public_point(parent_key)) + index.to_bytes(4, "big")
    i = _hmac_sha512(parent_chain, data)
    il = int.from_bytes(i[:32], "big")
    # Same spec validity gate as the master; a real deployment would advance to the
    # next index, but for these fixed roots the event must simply never occur.
    assert il < SECP256K1_N, "BIP-32 CKDpriv IL >= n (invalid child index)"
    child_key = (il + parent_key) % SECP256K1_N
    assert child_key != 0, "BIP-32 CKDpriv produced a zero child key"
    return child_key, i[32:]


def bip32_derive(seed: bytes, path: list[int]) -> tuple[int, bytes]:
    """Fold CKDpriv down `path` from the master, returning (private key, chain code)."""
    key, chain = bip32_master(seed)
    for index in path:
        key, chain = bip32_ckd_priv(key, chain, index)
    return key, chain


# --- Ethereum address and EIP-55 checksum ------------------------------------


def eth_address(point: Point) -> bytes:
    """The 20-byte address: low 20 bytes of keccak256(X || Y) over the 64-byte
    coordinate pair — the 0x04 SEC1 prefix is deliberately excluded."""
    x, y = point
    return keccak256(x.to_bytes(32, "big") + y.to_bytes(32, "big"))[-20:]


def eip55(address: bytes) -> str:
    """EIP-55: uppercase a hex nibble of the address where the matching nibble of
    keccak256(lowercase-hex-string) is >= 8. The hash is over the ASCII text, not
    the address bytes, which is the detail most look-alike implementations miss.
    Returns the 0x-prefixed mixed-case string."""
    lower = address.hex()
    digest = keccak256(lower.encode("ascii")).hex()
    out = "".join(
        ch.upper() if ch.isalpha() and int(digest[i], 16) >= 8 else ch
        for i, ch in enumerate(lower)
    )
    return "0x" + out


# --- RLP, from the specification ---------------------------------------------
#
# Four length-prefix rules and one recursion. The encoder writes the canonical
# form (a one-byte string below 0x80 is the byte itself; zero is the empty
# string), and the decoder refuses everything the encoder would not have
# written — the account transactions parse bytes that arrived over a network,
# where a tolerant decoder is a differential oracle at best.

RlpItem = bytes | list["RlpItem"]


def rlp_encode(item: RlpItem) -> bytes:
    if isinstance(item, bytes):
        if len(item) == 1 and item[0] < 0x80:
            return item
        return _rlp_prefix(len(item), 0x80) + item
    payload = b"".join(rlp_encode(child) for child in item)
    return _rlp_prefix(len(payload), 0xC0) + payload


def rlp_uint(value: int) -> bytes:
    """The minimal big-endian encoding; zero is the empty string — the one
    integer rule most hand-rolled encoders get wrong."""
    if value == 0:
        return b""
    length = (value.bit_length() + 7) // 8
    return value.to_bytes(length, "big")


def _rlp_prefix(length: int, offset: int) -> bytes:
    if length <= 55:
        return bytes([offset + length])
    length_bytes = length.to_bytes((length.bit_length() + 7) // 8, "big")
    return bytes([offset + 55 + len(length_bytes)]) + length_bytes


def rlp_decode(data: bytes) -> RlpItem:
    """Strict: consumes exactly the input, rejects non-minimal lengths."""
    item, consumed = _rlp_decode_item(data, 0)
    if consumed != len(data):
        raise ValueError("trailing bytes after a complete RLP item")
    return item


def _rlp_decode_item(data: bytes, offset: int) -> tuple[RlpItem, int]:
    if offset >= len(data):
        raise ValueError("RLP input ends where an item was expected")
    first = data[offset]
    if first < 0x80:
        return data[offset : offset + 1], offset + 1
    if first <= 0xB7:  # short string
        length, start = first - 0x80, offset + 1
    elif first <= 0xBF:  # long string
        lol = first - 0xB7
        length, start = _rlp_read_length(data, offset + 1, lol)
    elif first <= 0xF7:  # short list
        length, start = first - 0xC0, offset + 1
    else:  # long list
        lol = first - 0xF7
        length, start = _rlp_read_length(data, offset + 1, lol)
    end = start + length
    if end > len(data):
        raise ValueError("RLP input ends inside an item")
    payload = data[start:end]
    if first <= 0xBF and length == 1 and payload[0] < 0x80:
        raise ValueError("single byte below 0x80 must encode as itself")
    if first >= 0xC0:
        items: list[RlpItem] = []
        at = start
        while at < end:
            child, at = _rlp_decode_item(data, at)
            items.append(child)
        return items, end
    return payload, end


def _rlp_read_length(data: bytes, at: int, length_of_length: int) -> tuple[int, int]:
    if at + length_of_length > len(data):
        raise ValueError("RLP input ends inside a length prefix")
    length_bytes = data[at : at + length_of_length]
    if length_bytes[0] == 0:
        raise ValueError("RLP length has a leading zero byte")
    length = int.from_bytes(length_bytes, "big")
    if length <= 55:
        raise ValueError("RLP length written in long form for a short-form payload")
    return length, at + length_of_length


# --- EIP-1559 (type-2) transactions ------------------------------------------
#
# The serialized form is 0x02 || RLP([the nine fields, y_parity, r, s]) — a FLAT
# list of twelve items, unlike legacy transactions where the signature wraps an
# already-encoded body. The signing hash covers only the first nine items:
# keccak256(0x02 || RLP(fields)). The access list is always empty in this build.

EIP1559_TYPE = b"\x02"


def eip1559_body(
    chain_id: int,
    nonce: int,
    max_priority_fee_per_gas: int,
    max_fee_per_gas: int,
    gas_limit: int,
    to: bytes,
    value: int,
    data: bytes,
) -> list[RlpItem]:
    return [
        rlp_uint(chain_id),
        rlp_uint(nonce),
        rlp_uint(max_priority_fee_per_gas),
        rlp_uint(max_fee_per_gas),
        rlp_uint(gas_limit),
        to,
        rlp_uint(value),
        data,
        [],  # access list, always empty in this build
    ]


def eip1559_signing_hash(body: list[RlpItem]) -> bytes:
    return keccak256(EIP1559_TYPE + rlp_encode(body))


# --- ECDSA public-key recovery -----------------------------------------------


def _inv_mod_n(value: int) -> int:
    return pow(value % SECP256K1_N, SECP256K1_N - 2, SECP256K1_N)


def ecdsa_recover(digest: bytes, r: int, s: int, parity: int) -> Point:
    """Recover the public key from a signature over `digest`. `parity` is the
    y-parity bit (0/1); EIP-1559 carries it as the tenth field of the envelope."""
    if not 1 <= r < SECP256K1_N:
        raise ValueError("r out of range")
    x = r  # parity 2/3 (x + n) never occurs in a type-2 transaction
    y_squared = (pow(x, 3, SECP256K1_P) + 7) % SECP256K1_P
    y = pow(y_squared, (SECP256K1_P + 1) // 4, SECP256K1_P)
    if pow(y, 2, SECP256K1_P) != y_squared:
        raise ValueError("r is not an x-coordinate on the curve")
    if y % 2 != parity:
        y = SECP256K1_P - y
    z = int.from_bytes(digest, "big")
    r_inv = _inv_mod_n(r)
    u1 = (-z * r_inv) % SECP256K1_N
    u2 = (s * r_inv) % SECP256K1_N
    point = point_add(point_mul(u1, SECP256K1_G), point_mul(u2, (x, y)))
    if point is None:
        raise ValueError("recovery produced the point at infinity")
    return point


def eip1559_recover_sender(raw: bytes) -> bytes:
    """Decode a raw type-2 transaction and recover its sender. The full pipeline
    a conformance port must reproduce: strict RLP decode, body re-encode,
    signing hash, ECDSA recovery, address derivation."""
    if raw[:1] != EIP1559_TYPE:
        raise ValueError("not a type-2 transaction")
    items = rlp_decode(raw[1:])
    if not isinstance(items, list) or len(items) != 12:
        raise ValueError("not a 12-item EIP-1559 envelope")
    body = items[:9]
    parity = int.from_bytes(items[9], "big")
    r = int.from_bytes(items[10], "big")
    s = int.from_bytes(items[11], "big")
    if parity not in (0, 1):
        raise ValueError("y-parity is not a bit")
    if len(items[10]) != 32 or len(items[11]) != 32:
        raise ValueError("r or s is not 32 bytes")
    if s > SECP256K1_N // 2:
        raise ValueError("high-s signature")
    if int.from_bytes(body[0], "big") == 0:
        raise ValueError("chain id zero")
    digest = eip1559_signing_hash(body)
    return eth_address(ecdsa_recover(digest, r, s, parity))


# --- RFC 6979 deterministic ECDSA (self-check only) --------------------------
#
# Exists to check this file's EIP-712 implementation against the EIP's published
# worked example *signature*, not just its digest: signing the digest with
# the example's private key under RFC 6979 must reproduce the published
    # r/s byte for byte. No account vector is signed by this; the ports sign
    # with their own libraries and prove validity by recovery.


def rfc6979_k(private_key: int, digest: bytes) -> int:
    x_octets = private_key.to_bytes(32, "big")
    z = int.from_bytes(digest, "big")
    z2 = z - SECP256K1_N if z >= SECP256K1_N else z
    h1 = z2.to_bytes(32, "big")
    v = b"\x01" * 32
    k = b"\x00" * 32
    k = hmac.new(k, v + b"\x00" + x_octets + h1, hashlib.sha256).digest()
    v = hmac.new(k, v, hashlib.sha256).digest()
    k = hmac.new(k, v + b"\x01" + x_octets + h1, hashlib.sha256).digest()
    v = hmac.new(k, v, hashlib.sha256).digest()
    while True:
        v = hmac.new(k, v, hashlib.sha256).digest()
        candidate = int.from_bytes(v, "big")
        if 1 <= candidate < SECP256K1_N:
            return candidate
        k = hmac.new(k, v + b"\x00", hashlib.sha256).digest()
        v = hmac.new(k, v, hashlib.sha256).digest()


def ecdsa_sign_rfc6979(private_key: int, digest: bytes) -> tuple[int, int]:
    z = int.from_bytes(digest, "big") % SECP256K1_N
    nonce = rfc6979_k(private_key, digest)
    point = public_point(nonce)
    r = point[0] % SECP256K1_N
    s = (_inv_mod_n(nonce) * (z + r * private_key)) % SECP256K1_N
    return r, s


# --- EIP-712 typed-data hashing ----------------------------------------------


def eip712_encode_type(primary: str, referenced: list[str]) -> str:
    """The primary struct's declaration followed by every referenced struct's
    declaration, sorted by name. The appendix is the part of EIP-712 every
    hand-rolled implementation gets wrong: a struct that references other
    structs does not hash its bare declaration but the declaration with all
    referenced types riding along. Sorting the declaration strings is
    equivalent to sorting by name because `(` (0x28) sorts below every
    character that can continue an identifier."""
    return primary + "".join(sorted(referenced))


def eip712_type_hash(primary: str, referenced: list[str]) -> bytes:
    return keccak256(eip712_encode_type(primary, referenced).encode("ascii"))


def _unhex(value: str) -> bytes:
    """Hex from a vector file, with or without the 0x prefix."""
    if value.startswith(("0x", "0X")):
        value = value[2:]
    return bytes.fromhex(value)


def eip712_encode_value(value: dict) -> bytes:
    """The 32-byte encodeData contribution of one typed value. Structs arrive
    as {"struct": {...}} and contribute their own hashStruct, recursively."""
    if "struct" in value:
        struct = value["struct"]
        return eip712_hash_struct(
            eip712_type_hash(struct["primary_type"], struct["referenced_types"]),
            [eip712_encode_value(v) for v in struct["values"]],
        )
    kind = value["type"]
    if kind == "address":
        return b"\x00" * 12 + _unhex(value["value"])
    if kind in ("uint256", "bytes32"):
        out = _unhex(value["value"])
        if kind == "uint256" and len(out) < 32:  # JSON carries small integers short
            out = b"\x00" * (32 - len(out)) + out
        return out
    if kind == "string":
        return keccak256(value["value"].encode("utf-8"))
    if kind == "bytes":
        return keccak256(_unhex(value["value"]))
    if kind == "array":
        return keccak256(b"".join(eip712_encode_value(v) for v in value["values"]))
    raise ValueError(f"unknown EIP-712 value type {kind}")


def eip712_hash_struct(type_hash: bytes, encoded_values: list[bytes]) -> bytes:
    return keccak256(type_hash + b"".join(encoded_values))


def eip712_domain_separator(domain: dict) -> bytes:
    """hashStruct(EIP712Domain) over exactly the fields that are present, in
    the EIP's fixed order (name, version, chainId, verifyingContract, salt)."""
    members: list[bytes] = []
    if "name" in domain:
        members.append(keccak256(domain["name"].encode("utf-8")))
    if "version" in domain:
        members.append(keccak256(domain["version"].encode("utf-8")))
    if "chain_id" in domain:
        # encodeData left-pads every atomic value to 32 bytes — a small chainId
        # contributes 31 zero bytes, not its minimal integer encoding.
        members.append(domain["chain_id"].to_bytes(32, "big"))
    if "verifying_contract" in domain:
        members.append(b"\x00" * 12 + _unhex(domain["verifying_contract"]))
    if "salt" in domain:
        members.append(_unhex(domain["salt"]))
    present = []
    if "name" in domain:
        present.append("string name")
    if "version" in domain:
        present.append("string version")
    if "chain_id" in domain:
        present.append("uint256 chainId")
    if "verifying_contract" in domain:
        present.append("address verifyingContract")
    if "salt" in domain:
        present.append("bytes32 salt")
    type_hash = keccak256(("EIP712Domain(" + ",".join(present) + ")").encode("ascii"))
    return eip712_hash_struct(type_hash, members)


def eip712_digest(domain_separator: bytes, struct_hash: bytes) -> bytes:
    return keccak256(b"\x19\x01" + domain_separator + struct_hash)


def _mail_person(name: str, address: str, person_decl: str) -> dict:
    """One Person value of the EIP-712 worked example, as a vector-struct."""
    return {
        "struct": {
            "primary_type": person_decl,
            "referenced_types": [],
            "values": [
                {"type": "string", "value": name},
                {"type": "address", "value": address},
            ],
        }
    }


# --- self-check-only primitives: Base58Check and RIPEMD-160 ------------------
#
# These read and reproduce the *published Bitcoin* BIP-32 test vector, nothing
# else. No account vector emitted by this file is ever routed through them, so
# they stand apart from the account crypto above and are never called by it.

_B58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def base58check_decode(text: str) -> bytes:
    """Decode Base58, strip and verify the 4-byte double-SHA-256 checksum.

    The checksum is the point: it turns a mistyped published extended key into a
    loud failure at self-check time instead of a silently wrong "expected" value.
    Every string this decodes is ground truth transcribed by hand, and this is
    what proves the transcription.
    """
    number = 0
    for ch in text:
        number = number * 58 + _B58_ALPHABET.index(ch)
    body = number.to_bytes((number.bit_length() + 7) // 8, "big")
    body = b"\x00" * (len(text) - len(text.lstrip("1"))) + body  # leading '1' -> 0x00
    payload, checksum = body[:-4], body[-4:]
    recomputed = hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    if recomputed != checksum:
        raise ValueError(f"Base58Check checksum failed: a BIP-32 vector string is mistyped ({text[:12]}...)")
    return payload


# RIPEMD-160 (from the specification) so the fingerprint half of the BIP-32 check
# needs no OpenSSL legacy provider — `hashlib.new("ripemd160")` is not portable
# across build configurations, and a self-check that can vanish with the platform
# is not a check. Used only inside hash160().

_RMD_R = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
    7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5, 2, 14, 11, 8,
    3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12,
    1, 9, 11, 10, 0, 8, 12, 4, 13, 3, 7, 15, 14, 5, 6, 2,
    4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13,
]
_RMD_RP = [
    5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12,
    6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12, 4, 9, 1, 2,
    15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13,
    8, 6, 4, 1, 3, 11, 15, 0, 5, 12, 2, 13, 9, 7, 10, 14,
    12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11,
]
_RMD_S = [
    11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8,
    7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15, 9, 11, 7, 13, 12,
    11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5,
    11, 12, 14, 15, 14, 15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12,
    9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6,
]
_RMD_SP = [
    8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6,
    9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12, 7, 6, 15, 13, 11,
    9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5,
    15, 5, 8, 11, 14, 14, 6, 14, 6, 9, 12, 9, 12, 5, 15, 8,
    8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11,
]
_RMD_KL = [0x00000000, 0x5A827999, 0x6ED9EBA1, 0x8F1BBCDC, 0xA953FD4E]
_RMD_KR = [0x50A28BE6, 0x5C4DD124, 0x6D703EF3, 0x7A6D76E9, 0x00000000]
_MASK32 = 0xFFFFFFFF


def _rmd_f(j: int, x: int, y: int, z: int) -> int:
    if j < 16:
        return x ^ y ^ z
    if j < 32:
        return (x & y) | (~x & z)
    if j < 48:
        return (x | (~y & _MASK32)) ^ z
    if j < 64:
        return (x & z) | (y & ~z)
    return x ^ (y | (~z & _MASK32))


def _rotl32(value: int, bits: int) -> int:
    return ((value << bits) | (value >> (32 - bits))) & _MASK32


def ripemd160(data: bytes) -> bytes:
    """RIPEMD-160: two parallel 80-step lines over 512-bit blocks, little-endian."""
    h = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0]
    padded = bytearray(data)
    padded.append(0x80)
    while len(padded) % 64 != 56:
        padded.append(0x00)
    padded += (len(data) * 8).to_bytes(8, "little")  # length in bits, little-endian
    for base in range(0, len(padded), 64):
        block = padded[base : base + 64]
        x = [int.from_bytes(block[4 * i : 4 * i + 4], "little") for i in range(16)]
        al, bl, cl, dl, el = h
        ar, br, cr, dr, er = h
        for j in range(80):
            rnd = j // 16
            t = (al + _rmd_f(j, bl, cl, dl) + x[_RMD_R[j]] + _RMD_KL[rnd]) & _MASK32
            t = (_rotl32(t, _RMD_S[j]) + el) & _MASK32
            al, el, dl, cl, bl = el, dl, _rotl32(cl, 10), bl, t
            t = (ar + _rmd_f(79 - j, br, cr, dr) + x[_RMD_RP[j]] + _RMD_KR[rnd]) & _MASK32
            t = (_rotl32(t, _RMD_SP[j]) + er) & _MASK32
            ar, er, dr, cr, br = er, dr, _rotl32(cr, 10), br, t
        t = (h[1] + cl + dr) & _MASK32
        h[1] = (h[2] + dl + er) & _MASK32
        h[2] = (h[3] + el + ar) & _MASK32
        h[3] = (h[4] + al + br) & _MASK32
        h[4] = (h[0] + bl + cr) & _MASK32
        h[0] = t
    return b"".join(word.to_bytes(4, "little") for word in h)


def hash160(data: bytes) -> bytes:
    """Bitcoin's Hash160 = RIPEMD-160(SHA-256(data)); its first 4 bytes are the
    BIP-32 key fingerprint. Self-check only."""
    return ripemd160(hashlib.sha256(data).digest())


# BIP-32 Test Vector 1 (seed 000102030405060708090a0b0c0d0e0f), each level as its
# published Base58Check extended private and public keys. These are the ground
# truth; base58check_decode verifies each one's checksum before it is trusted, so
# a typo here fails the self-check loudly instead of certifying a wrong answer.
BIP32_TV1_SEED = bytes.fromhex("000102030405060708090a0b0c0d0e0f")
BIP32_TV1 = [
    (
        "m",
        [],
        "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi",
        "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8",
    ),
    (
        "m/0H",
        [HARDENED + 0],
        "xprv9uHRZZhk6KAJC1avXpDAp4MDc3sQKNxDiPvvkX8Br5ngLNv1TxvUxt4cV1rGL5hj6KCesnDYUhd7oWgT11eZG7XnxHrnYeSvkzY7d2bhkJ7",
        "xpub68Gmy5EdvgibQVfPdqkBBCHxA5htiqg55crXYuXoQRKfDBFA1WEjWgP6LHhwBZeNK1VTsfTFUHCdrfp1bgwQ9xv5ski8PX9rL2dZXvgGDnw",
    ),
    (
        "m/0H/1",
        [HARDENED + 0, 1],
        "xprv9wTYmMFdV23N2TdNG573QoEsfRrWKQgWeibmLntzniatZvR9BmLnvSxqu53Kw1UmYPxLgboyZQaXwTCg8MSY3H2EU4pWcQDnRnrVA1xe8fs",
        "xpub6ASuArnXKPbfEwhqN6e3mwBcDTgzisQN1wXN9BJcM47sSikHjJf3UFHKkNAWbWMiGj7Wf5uMash7SyYq527Hqck2AxYysAA7xmALppuCkwQ",
    ),
    (
        "m/0H/1/2H",
        [HARDENED + 0, 1, HARDENED + 2],
        "xprv9z4pot5VBttmtdRTWfWQmoH1taj2axGVzFqSb8C9xaxKymcFzXBDptWmT7FwuEzG3ryjH4ktypQSAewRiNMjANTtpgP4mLTj34bhnZX7UiM",
        "xpub6D4BDPcP2GT577Vvch3R8wDkScZWzQzMMUm3PWbmWvVJrZwQY4VUNgqFJPMM3No2dFDFGTsxxpG5uJh7n7epu4trkrX7x7DogT5Uv6fcLW5",
    ),
    (
        "m/0H/1/2H/2",
        [HARDENED + 0, 1, HARDENED + 2, 2],
        "xprvA2JDeKCSNNZky6uBCviVfJSKyQ1mDYahRjijr5idH2WwLsEd4Hsb2Tyh8RfQMuPh7f7RtyzTtdrbdqqsunu5Mm3wDvUAKRHSC34sJ7in334",
        "xpub6FHa3pjLCk84BayeJxFW2SP4XRrFd1JYnxeLeU8EqN3vDfZmbqBqaGJAyiLjTAwm6ZLRQUMv1ZACTj37sR62cfN7fe5JnJ7dh8zL4fiyLHV",
    ),
    (
        "m/0H/1/2H/2/1000000000",
        [HARDENED + 0, 1, HARDENED + 2, 2, 1000000000],
        "xprvA41z7zogVVwxVSgdKUHDy1SKmdb533PjDz7J6N6mV6uS3ze1ai8FHa8kmHScGpWmj4WggLyQjgPie1rFSruoUihUZREPSL39UNdE3BBDu76",
        "xpub6H1LXWLaKsWFhvm6RVpEL9P4KfRZSW7abD2ttkWP3SSQvnyA8FSVqNTEcYFgJS2UaFcxupHiYkro49S8yGasTvXEYBVPamhGW6cFJodrTHy",
    ),
]

# EIP-55 supplies eight worked example addresses (two all-caps, two all-lower,
# four mixed). All eight are already correctly checksummed, so each must be a
# fixed point of the checksum function applied to its own lowercase form.
EIP55_EXAMPLES = [
    "0x52908400098527886E0F7030069857D2E4169EE7",
    "0x8617E340B3D01FA5F11F306F4090FD50E238070D",
    "0xde709f2102306220921060314715629080e2fb77",
    "0x27b1fdb04752bbc536007a920d24acb045561c26",
    "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
    "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
    "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
]

# Published Keccak-256 digests. NOTE the empty-string digest ends in ...a470, not
# the frequently-miscited ...a456; this is confirmed against pycryptodome and is
# pinned further by the three longer digests below, which no wrong permutation
# could also reproduce.
KECCAK_VECTORS = [
    (b"", "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
    (b"abc", "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"),
    (
        b"The quick brown fox jumps over the lazy dog",
        "4d741b6f1eb29cb2a9b9911c82f56fa8d73b04959d3d9d222895df6c0b28aa15",
    ),
    (
        b"The quick brown fox jumps over the lazy dog.",
        "578951e24efd62a3d63a86f7cd19aaa53c898fe287d2552133220370240b572d",
    ),
]

# RLP examples from the specification's own appendix, plus the boundary that
# decides short form from long form (a 55-byte payload is the largest short
# string; one more byte needs a length-of-length). The expected side is the
# exact hex the spec prints.
RLP_VECTORS = [
    (b"dog", "83646f67"),
    ([b"cat", b"dog"], "c88363617483646f67"),
    (b"", "80"),
    (b"\x00", "00"),
    (b"\x0f", "0f"),
    (b"\x04\x00", "820400"),
    ([[], []], "c2c0c0"),
]
RLP_LOREM = b"Lorem ipsum dolor sit amet, consectetur adipisicing elit"  # 56 bytes
assert len(RLP_LOREM) == 56

# A real Avalanche C-Chain mainnet type-2 transaction, byte for byte as a
# public RPC endpoint served it. This is the only place the transaction formats
# are checked against the chain itself rather than against this file's own
# beliefs: decoding this raw must recover the sender the block explorer
# attributes it to, and its Keccak-256 digest must be its hash. A client that
# is merely self-consistent can still disagree with the chain; this case is
# how that disagreement is caught before funds depend on it.
AVAX_TX_RAW = bytes.fromhex(
    "02f89382a86a83032deb8416af0f5d84222d8d2382ea609428d9ccedf1b7ac9b3f090f4f029"
    "2837de87c1d3980a46a96ea181d6eb3586a6a5f18396c454587132cdd454500000000000000"
    "01a05d81fa9a00c080a045e2a143cf835f959944d884ee17fe82ddfaf4e838b19c31f97765e"
    "d1a71314aa039c8f0891a9148b6033cb040b64c6150b6bb210866ea8dbbcfa5b5da055f0ed9"
)
AVAX_TX_SENDER = "53e5024c1252e81ada67b61ab0ae64f5b9a7e0a3"
AVAX_TX_HASH = "681da1c7f76629f2b0ba57fedff17dabab57bf84ed9553919ba438098bfe9811"

# EIP-712's worked example ("Ether Mail"), exactly as the EIP prints it: the
# domain separator, the message's hashStruct, and the final digest are all
# published values, and so is the signature — taken with the example's private
# key (keccak256("cow"), the "Cow" sender's own key) under RFC 6979. The digest
# and the signature together pin the whole construction: a wrong encodeType, a
# wrong member ordering, or a wrong padding rule cannot reproduce both.
EIP712_MAIL_DOMAIN = {
    "name": "Ether Mail",
    "version": "1",
    "chain_id": 1,
    "verifying_contract": "cc" * 20,
}
EIP712_MAIL_DOMAIN_SEPARATOR = "f2cee375fa42b42143804025fc449deafd50cc031ca257e0b194a650a912090f"
EIP712_MAIL_STRUCT_HASH = "c52c0ee5d84264471806290a3f2c4cecfc5490626bf912d01f240d7a274b371e"
EIP712_MAIL_DIGEST = "be609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2"
EIP712_MAIL_FROM_ADDRESS = "cd2a3d9f938e13cd947ec05abc7fe734df8dd826"  # "Cow"
EIP712_MAIL_TO_ADDRESS = "bb" * 20  # "Bob"
EIP712_COW_KEY = 0xC85EF7D79691FE79573B1A7064C19C1A9819EBDBD1FAAAB1A8EC92344438AAF4
EIP712_MAIL_SIG_R = 0x4355C47D63924E8A72E509B65029052EB6C299D53A04E167C5775FD466751C9D
EIP712_MAIL_SIG_S = 0x07299936D304C153F6443DFA05F40FF007D72911B6F72307F996231605B91562
EIP712_MAIL_SIG_PARITY = 1  # the EIP prints v = 0x1c


# --- self checks against published vectors ----------------------------------


def self_check() -> list[str]:
    """Validate every from-scratch primitive against published vectors before any
    output file is written. A failure here means the generator is wrong, and the
    only safe response is to emit nothing."""
    checked = []

    # HKDF-SHA256 — RFC 5869 test cases 1, 2, 3 (appendices A.1-A.3). Case 1 is the
    # one the task requires; the other two add a three-round expansion and the
    # empty-salt/empty-info edge that a naive HKDF gets subtly wrong.
    assert hkdf(
        bytes.fromhex("0b" * 22),
        bytes.fromhex("000102030405060708090a0b0c"),
        bytes.fromhex("f0f1f2f3f4f5f6f7f8f9"),
        42,
    ).hex() == (
        "3cb25f25faacd57a90434f64d0362f2a"
        "2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        "34007208d5b887185865"
    ), "RFC 5869 A.1 (HKDF-SHA256) mismatch"
    assert hkdf(
        bytes(range(0x00, 0x50)), bytes(range(0x60, 0xB0)), bytes(range(0xB0, 0x100)), 82
    ).hex() == (
        "b11e398dc80327a1c8e7f78c596a4934"
        "4f012eda2d4efad8a050cc4c19afa97c"
        "59045a99cac7827271cb41c65e590e09"
        "da3275600c2f09b8367793a9aca3db71"
        "cc30c58179ec3e87c14c01d5c1f3434f"
        "1d87"
    ), "RFC 5869 A.2 (HKDF-SHA256) mismatch"
    assert hkdf(bytes.fromhex("0b" * 22), b"", b"", 42).hex() == (
        "8da4e775a563c18f715f802a063c5a31"
        "b8a11f5c5ee1879ec3454e5f3c738d2d"
        "9d201395faa4b61a96c8"
    ), "RFC 5869 A.3 (HKDF-SHA256) mismatch"
    checked.append("RFC 5869 A.1-A.3 (HKDF-SHA256)")

    # Keccak-256 — the pre-NIST 0x01 padding against the published Keccak-256
    # digests (empty, "abc", and both fox pangrams). Four independent digests pin
    # the permutation; the empty digest's true last byte is 0x70, not the widely
    # miscited 0x56, and the other three prove the implementation is right.
    for message, want in KECCAK_VECTORS:
        got = keccak256(message).hex()
        assert got == want, f"Keccak-256({message!r}) mismatch: {got}"
    checked.append("Keccak-256 (empty, abc, and two pangram digests)")

    # secp256k1 — the generator identity the task names, plus the two facts that
    # only hold if the group law is right: (n-1)*G is the reflection of G, and n*G
    # is the point at infinity. point_mul does not reduce mod n, so n*G == O is a
    # real test of the identity handling rather than of 0*G.
    assert point_on_curve(SECP256K1_G), "G is not on the curve"
    assert point_mul(1, SECP256K1_G) == SECP256K1_G, "1*G != G"
    assert point_mul(SECP256K1_N - 1, SECP256K1_G) == (
        SECP256K1_GX,
        (SECP256K1_P - SECP256K1_GY) % SECP256K1_P,
    ), "(n-1)*G != -G"
    assert point_mul(SECP256K1_N, SECP256K1_G) is None, "n*G != O"
    checked.append("secp256k1 (1*G, (n-1)*G = -G, n*G = O, G on curve)")

    # BIP-32 Test Vector 1, reproduced completely. This is the check that CKDpriv
    # is really BIP-32 and not a look-alike: a wrong derivation cannot match six
    # levels of published chain code, private key, public key, and fingerprint.
    # The parent fingerprint of each level equals Hash160 of the previous level's
    # public key, so verifying it against the (independently computed) parent key
    # exercises the identifier derivation too.
    prev_fingerprint = b"\x00\x00\x00\x00"  # the master's parent fingerprint is zero
    for label_path, path, xprv, xpub in BIP32_TV1:
        prv = base58check_decode(xprv)  # also verifies the checksum
        pub = base58check_decode(xpub)
        depth = prv[4]
        parent_fp = prv[5:9]
        child_number = int.from_bytes(prv[9:13], "big")
        want_chain = prv[13:45]
        want_priv = prv[45:78]  # 0x00 || 32-byte key
        want_pub = pub[45:78]  # 33-byte compressed public key
        assert depth == len(path), f"BIP-32 {label_path}: depth {depth} != {len(path)}"
        assert child_number == (path[-1] if path else 0), f"BIP-32 {label_path}: child number"
        assert parent_fp == prev_fingerprint, f"BIP-32 {label_path}: parent fingerprint mismatch"

        key, chain = bip32_derive(BIP32_TV1_SEED, path)
        assert chain == want_chain, f"BIP-32 {label_path}: chain code mismatch"
        assert b"\x00" + key.to_bytes(32, "big") == want_priv, f"BIP-32 {label_path}: private key mismatch"
        point = public_point(key)
        assert ser_compressed(point) == want_pub, f"BIP-32 {label_path}: public key mismatch"
        prev_fingerprint = hash160(ser_compressed(point))[:4]
    checked.append("BIP-32 test vector 1 (6 levels: chain code, private key, public key, fingerprint)")

    # EIP-55 — every worked example address is its own checksummed form.
    for address in EIP55_EXAMPLES:
        got = eip55(bytes.fromhex(address[2:].lower()))
        assert got == address, f"EIP-55 mismatch: {got} != {address}"
    checked.append("EIP-55 (eight example addresses round-trip)")

    # End-to-end address derivation against Ethereum's own ground truth: the
    # EIP-155 example secret key must produce its published address. This exercises
    # the whole public-key -> Keccak-256 -> EIP-55 pipeline at once, independently
    # of BIP-32, so a fault anywhere in it is caught even though the account roots
    # have no published address of their own.
    eip155_key = 0x4646464646464646464646464646464646464646464646464646464646464646
    assert eip55(eth_address(public_point(eip155_key))) == (
        "0x9d8A62f656a8d1615C1294fd71e9CFb3E4855A4F"
    ), "EIP-155 example address mismatch"
    checked.append("EIP-155 example key -> address (secp256k1 + Keccak-256 + EIP-55)")

    # RLP — the specification's appendix examples, the 55/56 boundary, and the
    # two strictness rules a tolerant decoder would quietly accept: a single
    # byte below 0x80 must encode as itself (so 0x81 0x05 is not RLP), and an
    # item must consume exactly the input it was given.
    for item, want in RLP_VECTORS:
        got = rlp_encode(item).hex()
        assert got == want, f"RLP encode mismatch: {got} != {want}"
        assert rlp_encode(rlp_decode(bytes.fromhex(want))) == bytes.fromhex(
            want
        ), f"RLP round trip broken for {want}"
    assert rlp_uint(0) == b"", "RLP zero is not the empty string"
    assert rlp_encode(rlp_uint(1024)).hex() == "820400", "RLP integer 1024 mismatch"
    assert rlp_encode(b"x" * 55).hex() == "b7" + "78" * 55, "55-byte string is short form"
    assert rlp_encode(RLP_LOREM).hex() == "b838" + RLP_LOREM.hex(), "56-byte string is long form"
    for bad, why in [
        (bytes.fromhex("8105"), "single byte below 0x80 in long form"),
        (bytes.fromhex("b8012a"), "length 1 in long form"),
        (bytes.fromhex("b9002a2a"), "length with a leading zero"),
        (bytes.fromhex("83") + b"dog" + b"\x00", "trailing bytes"),
    ]:
        try:
            rlp_decode(bad)
        except ValueError:
            pass
        else:
            raise AssertionError(f"strict RLP decoder accepted {why}")
    checked.append("RLP (spec appendix examples, 55/56 boundary, strict decoding)")

    # A real Avalanche C-Chain mainnet transaction. Keccak-256 of the raw bytes
    # is its hash — pinning the serialized form — and sender recovery through
    # this file's own RLP decoder, body re-encode, signing hash, and ECDSA
    # recovery lands on the address the chain attributes it to. This is the
    # single check that would have caught both transaction-format mistakes this
    # generator exists to prevent: a nested signature envelope, or a signing
    # hash computed over the wrong fields.
    assert keccak256(AVAX_TX_RAW).hex() == AVAX_TX_HASH, "C-Chain tx hash mismatch"
    assert (
        eip1559_recover_sender(AVAX_TX_RAW).hex() == AVAX_TX_SENDER
    ), "C-Chain tx sender recovery mismatch"
    checked.append("Avalanche C-Chain transaction (hash + sender recovery, chain-sourced)")

    # EIP-712 — the specification's "Ether Mail" worked example, digest first,
    # then the signature. The domain separator and hashStruct are checked so a
    # failure names which half of the construction is wrong; the digest and the
    # RFC 6979 signature (r and s, with the EIP's own private key) are the
    # published values, and recovery with the published parity returns the
    # example sender's public key, closing the loop on the parity bit too.
    assert eip55(eth_address(public_point(EIP712_COW_KEY))) == (
        "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826"
    ), "the EIP-712 example key does not own the example sender address"
    person_decl = "Person(string name,address wallet)"
    mail_decl = "Mail(Person from,Person to,string contents)"
    assert eip712_encode_type(mail_decl, [person_decl]) == (
        "Mail(Person from,Person to,string contents)Person(string name,address wallet)"
    ), "EIP-712 encodeType does not append the referenced struct"
    mail_values = [
        _mail_person("Cow", EIP712_MAIL_FROM_ADDRESS, person_decl),
        _mail_person("Bob", EIP712_MAIL_TO_ADDRESS, person_decl),
        {"type": "string", "value": "Hello, Bob!"},
    ]
    mail_type_hash = eip712_type_hash(mail_decl, [person_decl])
    mail_struct_hash = eip712_hash_struct(
        mail_type_hash, [eip712_encode_value(v) for v in mail_values]
    )
    mail_separator = eip712_domain_separator(EIP712_MAIL_DOMAIN)
    mail_digest = eip712_digest(mail_separator, mail_struct_hash)
    assert mail_separator.hex() == EIP712_MAIL_DOMAIN_SEPARATOR, "EIP-712 domain separator mismatch"
    assert mail_struct_hash.hex() == EIP712_MAIL_STRUCT_HASH, "EIP-712 hashStruct mismatch"
    assert mail_digest.hex() == EIP712_MAIL_DIGEST, "EIP-712 digest mismatch"
    sig_r, sig_s = ecdsa_sign_rfc6979(EIP712_COW_KEY, mail_digest)
    assert sig_r == EIP712_MAIL_SIG_R, "EIP-712 example signature r mismatch"
    assert sig_s == EIP712_MAIL_SIG_S, "EIP-712 example signature s mismatch"
    assert ecdsa_recover(mail_digest, sig_r, sig_s, EIP712_MAIL_SIG_PARITY) == (
        public_point(EIP712_COW_KEY)
    ), "EIP-712 example signature does not recover to the sender's key"
    checked.append("EIP-712 Ether Mail example (separator, hashStruct, digest, RFC 6979 signature)")

    return checked


# --- account derivations -----------------------------------------------------


def domain_seed(root: bytes, label: str) -> bytes:
    """A domain's 32-byte seed: one HKDF round, absent salt, `info` = the label.
    Mirrors MigoRoot::domain_seed = kdf::derive::<32>(root, None, label)."""
    return hkdf(root, None, label.encode("ascii"), 32)


def e2ee_sub_seed(root: bytes, sub_label: str) -> bytes:
    """A second HKDF round over the E2EE domain seed, `info` = the sub-label.
    Mirrors founding_device_e2ee_seeds' per-key expansion."""
    return hkdf(domain_seed(root, E2EE_DOMAIN), None, sub_label.encode("ascii"), 32)


# --- file builders -----------------------------------------------------------


def domains_file() -> dict:
    cases = []
    for root_name, root in ROOTS:
        for word, label in DOMAINS:
            case = {
                "name": f"{root_name}-{word}",
                "root": root.hex(),
                "label": label,
                "seed": domain_seed(root, label).hex(),
            }
            # The E2EE domain seed is not used raw: it is expanded once more into
            # the founding device's signing and exchange seeds, which ride on this
            # same case so the consumer checks both the domain seed and its split.
            if label == E2EE_DOMAIN:
                case["e2ee_signing_seed"] = e2ee_sub_seed(root, E2EE_SIGNING_LABEL).hex()
                case["e2ee_exchange_seed"] = e2ee_sub_seed(root, E2EE_EXCHANGE_LABEL).hex()
            case["provenance"] = "independent-python-rfc5869"
            cases.append(case)

    return {
        "$comment": "Domain-separated account-root seeds: seed = HKDF-SHA256(ikm=root, salt=absent, info=label). Five domains over three roots; the E2EE case also carries the founding device's two sub-seeds.",
        "provenance": "computed by tools/vectors/generate_account_vectors.py, an independent HKDF-SHA256 written from RFC 5869 (extract with an absent salt = 32 zero bytes, one 32-byte expand round), self-checked against the RFC 5869 test cases before emission",
        "note": "`label` is the ASCII `info` of a single HKDF round and `seed` is its 32-byte output. On a MIGO/E2EE/V1 case, `e2ee_signing_seed` and `e2ee_exchange_seed` are a SECOND HKDF round over `seed` (ikm = the E2EE domain seed, info = migo-e2ee-signing-v1 / migo-e2ee-exchange-v1) — the founding device's Ed25519 and X25519 seeds, not the domain seed itself.",
        "cases": cases,
    }


def evm_file() -> dict:
    cases = []
    for root_name, root, index in EVM_CASES:
        seed = domain_seed(root, EVM_DOMAIN)
        key, chain = bip32_derive(seed, EVM_PATH_PREFIX + [index])
        point = public_point(key)
        address = eth_address(point)
        cases.append(
            {
                "name": f"{root_name}-index-{index}",
                "root": root.hex(),
                "index": index,
                "private_key": key.to_bytes(32, "big").hex(),
                "chain_code": chain.hex(),
                "public_key_uncompressed": ser_uncompressed(point).hex(),
                "address": address.hex(),
                "address_checksummed": eip55(address),
                "provenance": "independent-python-bip32-keccak",
            }
        )

    return {
        "$comment": "EVM wallet derivation from the account root: HKDF EVM seed -> BIP-32 master -> m/44'/60'/0'/0/index -> secp256k1 uncompressed public key -> Keccak-256 address -> EIP-55 checksum.",
        "provenance": "computed by tools/vectors/generate_account_vectors.py: BIP-32 CKDpriv, secp256k1 point arithmetic and Keccak-f[1600] implemented in pure Python from their specifications, self-checked against BIP-32 test vector 1, the secp256k1 generator, the published Keccak-256 digests, the EIP-55 examples and the EIP-155 example address before emission",
        "note": "The path is m/44'/60'/0'/0/{index} (BIP-44 for coin type 60). `address` is the 20-byte address as lowercase hex WITHOUT a 0x prefix; `address_checksummed` is the same address in 0x-prefixed EIP-55 mixed case. `chain_code` is after the full path and `public_key_uncompressed` is 65 bytes (0x04 || X || Y); the consumer pins the address, and these intermediates are pinned for the ports and for debugging.",
        "cases": cases,
    }


def _evm_wallet(root: bytes, index: int) -> tuple[int, bytes]:
    """(private key, address) of the EVM wallet at an address index, from the
    account root — the same derivation `account-evm.json` pins."""
    seed = domain_seed(root, EVM_DOMAIN)
    key, _ = bip32_derive(seed, EVM_PATH_PREFIX + [index])
    return key, eth_address(public_point(key))


def tx_file() -> dict:
    cases = []
    # The recipient is a wallet of a *different* root, so no case can send to
    # its own sender and every case's `to` differs from its `sender` at a glance.
    _, recipient = _evm_wallet(ROOT_B, 0)
    scenarios = [
        # (name, root, index, chain_id, nonce, priority, max_fee, gas, value, data)
        (
            "native-mainnet",
            ROOT_A,
            0,
            43114,
            0,
            2_000_000_000,
            30_000_000_000,
            21_000,
            1_500_000_000_000_000_000,
            b"",
        ),
        (
            "native-second-address",
            ROOT_A,
            1,
            43114,
            3,
            1_000_000_000,
            25_000_000_000,
            21_000,
            250_000_000_000_000,
            b"",
        ),
        (
            "native-fuji",
            ROOT_B,
            7,
            43113,
            1,
            1_500_000_000,
            28_000_000_000,
            21_000,
            2_000_000_000_000_000_000,
            b"",
        ),
        (
            "one-wei-zero-priority",
            ROOT_C,
            0,
            43114,
            0,
            0,
            25_000_000_000,
            21_000,
            1,
            b"",
        ),
        (
            "high-nonce",
            ROOT_B,
            0,
            43113,
            4_294_967_295,
            2_500_000_000,
            50_000_000_000,
            21_000,
            123_456_789,
            b"",
        ),
        (
            "with-calldata",
            ROOT_A,
            0,
            43114,
            7,
            2_000_000_000,
            30_000_000_000,
            30_000,
            0,
            # an ERC-20 transfer(address,uint256) selector with the recipient and
            # a 4-decimal amount padded to 32 bytes — exercises `data` and a
            # zero `value` (both encode as RLP's empty string) in one case.
            bytes.fromhex("a9059cbb" + "00" * 12 + recipient.hex() + "00" * 31 + "04"),
        ),
    ]
    for name, root, index, chain_id, nonce, priority, max_fee, gas, value, data in scenarios:
        key, sender = _evm_wallet(root, index)
        body = eip1559_body(chain_id, nonce, priority, max_fee, gas, recipient, value, data)
        cases.append(
            {
                "name": name,
                "root": root.hex(),
                "index": index,
                "chain_id": str(chain_id),
                "nonce": str(nonce),
                "max_priority_fee_per_gas": str(priority),
                "max_fee_per_gas": str(max_fee),
                "gas_limit": str(gas),
                "recipient": recipient.hex(),
                "value_wei": str(value),
                "data": data.hex(),
                "sender": sender.hex(),
                "body_rlp": rlp_encode(body).hex(),
                "signing_hash": eip1559_signing_hash(body).hex(),
                "provenance": "independent-python-rlp-secp256k1",
            }
        )

    # The chain-sourced case: a real mainnet transaction, pinned byte for byte.
    # Its decoded fields are produced by this file's own decoder from the pinned
    # raw, and the self-check has already proved that the raw recovers to the
    # sender and hashes to the hash before any file is written.
    items = rlp_decode(AVAX_TX_RAW[1:])
    body = items[:9]
    cases.append(
        {
            "name": "avalanche-c-chain-observed",
            "provenance": "chain-sourced",
            "$comment": (
                "A real Avalanche C-Chain mainnet type-2 transaction, served by a "
                "public RPC endpoint. The consumer must decode `raw` (strictly), "
                "re-encode its body, and recover `sender` from the signature, and "
                "keccak256(`raw`) must equal `tx_hash`. A client that is merely "
                "self-consistent can still disagree with the chain; this case is "
                "how that is caught."
            ),
            "chain_id": str(int.from_bytes(body[0], "big")),
            "nonce": str(int.from_bytes(body[1], "big")),
            "max_priority_fee_per_gas": str(int.from_bytes(body[2], "big")),
            "max_fee_per_gas": str(int.from_bytes(body[3], "big")),
            "gas_limit": str(int.from_bytes(body[4], "big")),
            "recipient": body[5].hex(),
            "value_wei": str(int.from_bytes(body[6], "big")),
            "data": body[7].hex(),
            "sender": AVAX_TX_SENDER,
            "raw": AVAX_TX_RAW.hex(),
            "tx_hash": AVAX_TX_HASH,
        }
    )

    return {
        "$comment": (
            "EIP-1559 (type-2) transaction bodies and signing hashes from the "
            "account roots: body = RLP([chain_id, nonce, max_priority_fee_per_gas, "
            "max_fee_per_gas, gas_limit, to, value, data, access_list]) with the "
            "access list always empty, and signing_hash = keccak256(0x02 || body)."
        ),
        "provenance": "computed by tools/vectors/generate_account_vectors.py: RLP written from the specification and self-checked against its appendix examples and strictness rules, secp256k1 from the curve parameters, Keccak-256 from the published digests; the chain-sourced case is a real mainnet transaction whose hash and sender recovery are pinned",
        "note": (
            "SIGNATURES ARE DELIBERATELY NOT PINNED. The ports sign with their own "
            "library and their own nonce, so pinned signature bytes could only be "
            "met by copying them. A port proves its signing path instead by "
            "recovering the sender from its own raw transaction and getting "
            "`sender` back. The pinned bytes are the deterministic ones: `body_rlp` "
            "(which a port must produce before signing) and `signing_hash` (which "
            "it must hash before signing). Integer fields are DECIMAL STRINGS, "
            "not JSON numbers: `value_wei` and the fee fields exceed JavaScript's "
            "2^53 exact-integer range, and a JSON number would arrive in the "
            "TypeScript port already rounded. `value_wei` is wei, AVAX's 18 "
            "decimals. The last case is chain-sourced and carries `raw`/`tx_hash` "
            "instead of a body to re-encode."
        ),
        "cases": cases,
    }


def eip712_file() -> dict:
    person_decl = "Person(string name,address wallet)"
    mail_decl = "Mail(Person from,Person to,string contents)"
    mail_values = [
        _mail_person("Cow", EIP712_MAIL_FROM_ADDRESS, person_decl),
        _mail_person("Bob", EIP712_MAIL_TO_ADDRESS, person_decl),
        {"type": "string", "value": "Hello, Bob!"},
    ]

    send_decl = "Send(address wallet,address to,uint256 amount,uint256 nonce)"
    bag_decl = "Bag(address account,bytes32 id,uint256 count,string label,bytes memo)"
    batch_decl = "Batch(Transfer[] items)"
    transfer_decl = "Transfer(address from,address to,uint256 amount)"

    def struct(primary: str, referenced: list[str], values: list[dict]) -> dict:
        return {"struct": {"primary_type": primary, "referenced_types": referenced, "values": values}}

    cases = []

    def add_case(name: str, domain: dict, message: dict, provenance: str) -> None:
        sep = eip712_domain_separator(domain)
        ms = message["struct"]
        type_hash = eip712_type_hash(ms["primary_type"], ms["referenced_types"])
        struct_hash = eip712_hash_struct(
            type_hash, [eip712_encode_value(v) for v in ms["values"]]
        )
        cases.append(
            {
                "name": name,
                "domain": domain,
                "message": message,
                "encode_type": eip712_encode_type(ms["primary_type"], ms["referenced_types"]),
                "expected": {
                    "type_hash": type_hash.hex(),
                    "domain_separator": sep.hex(),
                    "struct_hash": struct_hash.hex(),
                    "digest": eip712_digest(sep, struct_hash).hex(),
                },
                "provenance": provenance,
            }
        )

    # The specification's own worked example, expected values and all.
    add_case(
        "eip712-spec-example",
        EIP712_MAIL_DOMAIN,
        struct(mail_decl, [person_decl], mail_values),
        "eip712-spec-example",
    )
    # Domain-field presence: only name+version.
    add_case(
        "domain-name-version",
        {"name": "Migo", "version": "1"},
        struct(send_decl, [], [
            {"type": "address", "value": "ab" * 20},
            {"type": "address", "value": "cd" * 20},
            {"type": "uint256", "value": "0de0b6b3a7640000"},  # 1e18 wei, short hex
            {"type": "uint256", "value": "01"},
        ]),
        "independent-python-keccak-eip712",
    )
    # All five domain fields, including the salt.
    add_case(
        "domain-all-five-fields",
        {
            "name": "Migo",
            "version": "1",
            "chain_id": 43114,
            "verifying_contract": "28d9ccedf1b7ac9b3f090f4f0292837de87c1d39",
            "salt": "00" * 32,
        },
        struct(send_decl, [], [
            {"type": "address", "value": "ab" * 20},
            {"type": "address", "value": "cd" * 20},
            {"type": "uint256", "value": "00" * 30 + "0102"},
            {"type": "uint256", "value": "00"},
        ]),
        "independent-python-keccak-eip712",
    )
    # Every atomic value type at least once, including bytes and bytes32.
    add_case(
        "atomic-types",
        {"name": "Migo", "version": "1", "chain_id": 43113},
        struct(bag_decl, [], [
            {"type": "address", "value": "ef" * 20},
            {"type": "bytes32", "value": "aa" * 32},
            {"type": "uint256", "value": "ff"},
            {"type": "string", "value": "activity: 1 AVAX from faucet"},
            {"type": "bytes", "value": "deadbeef"},
        ]),
        "independent-python-keccak-eip712",
    )
    # A struct array, and a struct referencing a struct: two referenced-type
    # declarations appended in name order, and an array hashed as the Keccak of
    # its elements' encodings — both rules are where hand-rolled ports break.
    add_case(
        "struct-array-and-references",
        {"name": "Migo", "version": "1", "chain_id": 43114, "verifying_contract": "ab" * 20},
        struct(batch_decl, [transfer_decl], [
            {
                "type": "array",
                "values": [
                    struct(transfer_decl, [], [
                        {"type": "address", "value": "11" * 20},
                        {"type": "address", "value": "22" * 20},
                        {"type": "uint256", "value": "05"},
                    ]),
                    struct(transfer_decl, [], [
                        {"type": "address", "value": "33" * 20},
                        {"type": "address", "value": "44" * 20},
                        {"type": "uint256", "value": "07"},
                    ]),
                ],
            }
        ]),
        "independent-python-keccak-eip712",
    )

    return {
        "$comment": (
            "EIP-712 typed-data hashing: domain separator, hashStruct, and final "
            "digest over the value tree each case carries. The value model is "
            "recursive: {\"type\", \"value\"} for atomic values, {\"struct\": "
            "{\"primary_type\", \"referenced_types\", \"values\"}} for structs, and "
            "{\"type\": \"array\", \"values\": [...]} for arrays."
        ),
        "provenance": "computed by tools/vectors/generate_account_vectors.py: Keccak-256 from the published digests, encodeType/encodeData written from EIP-712 and self-checked against the specification's worked example — domain separator, hashStruct, digest, and the RFC 6979 signature taken with the example's own private key — before emission",
        "note": (
            "uint256 and bytes32 values are hex WITHOUT a 0x prefix and may be "
            "shorter than 32 bytes (left-pad to 32); addresses are 20-byte hex; "
            "`domain` carries only the fields it uses, and the separator is built "
            "from exactly those, in the EIP's fixed order. `encode_type` is the "
            "full string hashed into the type hash, referenced declarations "
            "appended in name order — pinned for readability, since it is the rule "
            "most ports get wrong. The spec-example case's expected values are the "
            "EIP's published ones, not this file's own computation."
        ),
        "cases": cases,
    }


# --- driver ------------------------------------------------------------------

FILES = {
    "account-domains.json": domains_file,
    "account-evm.json": evm_file,
    "account-tx.json": tx_file,
    "account-eip712.json": eip712_file,
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
        print("stale account vectors: " + ", ".join(stale), file=sys.stderr)
        print("run: python3 tools/vectors/generate_account_vectors.py", file=sys.stderr)
        return 1
    if args.check and not args.quiet:
        print(f"up to date: {len(FILES)} account vector files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
