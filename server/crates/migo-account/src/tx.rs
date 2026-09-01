//! The transaction domain: EIP-1559 on Avalanche C-Chain, signed on the device.
//!
//! # What opens here and what does not
//!
//! Section 182 shipped the EVM wallet as an address and nothing more; this
//! module is the half it deliberately deferred (§184): a transaction can now
//! be *built* and *signed*, entirely locally, for broadcast on Avalanche
//! C-Chain. What does not change is the boundary: the private key still never
//! leaves the [`EvmWallet`] object, the only output that crosses it is a
//! signed raw transaction, and the server remains unaware that blockchain
//! exists. The RPC conversation — nonce, gas estimate, broadcast, receipt —
//! belongs to the clients, because it is a conversation with a public network
//! and needs no trust from this one.
//!
//! # One transaction type, from one standard
//!
//! EIP-1559 (type `0x02`) is the only type built here. Avalanche has supported
//! dynamic fees since Cortina, so a legacy fallback would be a second code
//! path with its own bugs and no user it serves. The fields are the EIP's
//! nine: `chain_id`, `nonce`, `max_priority_fee_per_gas`, `max_fee_per_gas`,
//! `gas_limit`, `to`, `value`, `data`, and an `access_list` this build always
//! writes empty (a native AVAX transfer has no state access to pre-pay for).
//!
//! The signing hash is `Keccak-256(0x02 || RLP(fields))` and the signature is
//! ECDSA-secp256k1 with low-s normalization, exactly as every EVM chain
//! verifies them — which is the point: a transaction this module signs is not
//! a "Migo-shaped" transaction, it is the transaction any Ethereum tool
//! would accept, over a key any BIP-44 wallet would derive.
//!
//! # RLP, in the open
//!
//! RLP is implemented here rather than imported because it is small (four
//! length-prefix rules and one recursion), it is the exact place a
//! subtle bug becomes a transaction that pays a different amount than the one
//! the user confirmed, and the decoder must be strict anyway: recovery parses
//! bytes that came from a network, so a non-minimal length prefix or a
//! trailing byte after a valid item is rejected rather than tolerated. The
//! encoder is pinned by the RLP specification's own examples and the decoder
//! by a real Avalanche C-Chain transaction, both in the conformance vectors.
//!
//! # Chain identity is checked, not assumed
//!
//! A transaction is bound to its chain by the `chain_id` inside the signed
//! body — that is the whole replay protection. [`Network`] carries the two
//! chains this build speaks (C-Chain mainnet 43114 and Fuji testnet 43113)
//! with their pinned RPC URLs, and [`Network::check_chain_id`] exists so an
//! RPC-observed `eth_chainId` can be verified against the configured network
//! *before* anything is built (§184, spec #44): a mismatch must close the
//! session, not produce a transaction for the wrong chain. The type is data,
//! not a closed enum — the same agility rule as ADR-0013's `algorithm`
//! column, so a second EVM network later is a constant and a vector case,
//! not a port.
//!
//! # EIP-712
//!
//! Typed-data signing (spec #42) is the other half a wallet eventually owes:
//! `domain_separator` and `hash_struct` implement the EIP-712 hash
//! construction over a small, explicit value model — the caller supplies the
//! type hash, the values arrive already typed, and nothing parses a type
//! string at runtime (a parser here would be a large attack surface with no
//! product need behind it yet). The one rule the EIP buries in a subclause —
//! and the one this module's conformance vectors pin against the EIP's own
//! worked example — is that a struct's *type hash* covers not just its own
//! declaration but every struct type it references, appended sorted by name;
//! [`eip712_encode_type`] builds that string so no port has to remember it.
//! The UI obligation from the brief — display the domain, chain, contract,
//! and message, never a bare "sign data?" — is a client rule; this module's
//! job is to make the bytes underneath it correct and identical across the
//! three ports.

use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, SecretKey};
use tiny_keccak::{Hasher, Keccak};

use crate::error::{AccountError, Result};
use crate::evm::EvmWallet;

/// The secp256k1 group order halved, `0x7fff…20a0`, for low-s
/// normalization. A signature with `s > n/2` is valid ECDSA but invalid on
/// every EVM chain, which rejects malleable signatures — so normalization is
/// part of signing, not a caller concern.
const SECP256K1_N_HALF: [u8; 32] = {
    // n = 0xFFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFE BAAEDCE6 AF48A03B BFD25E8C
    //        D0364141; this block is n shifted right one bit, computed here
    // rather than transcribed so the value and its source stay together.
    let mut half = [0u8; 32];
    let n = [
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFE, 0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36,
        0x41, 0x41,
    ];
    let mut carry = 0;
    let mut i = 32;
    while i > 0 {
        i -= 1;
        let byte = n[i];
        half[i] = (byte >> 1) | carry;
        carry = (byte & 1) << 7;
    }
    half
};

/// The transaction type byte for EIP-1559 — the first byte of both the
/// signing hash input and the raw transaction.
const EIP1559_TYPE: u8 = 0x02;

/// An EVM network this build can name: a chain id and a pinned RPC.
///
/// The RPC URL is a documented constant, not a configuration knob, because
/// the brief is explicit that the user picks a network and never a URL —
/// a self-supplied RPC is the classic way a wallet gets shown a fake chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    /// Human name for UI surfaces.
    pub name: &'static str,
    /// The EIP-155 chain id, signed inside every transaction.
    pub chain_id: u64,
    /// The pinned public RPC endpoint.
    pub rpc_url: &'static str,
}

/// Avalanche C-Chain mainnet: chain id 43114.
pub const AVALANCHE_MAINNET: Network = Network {
    name: "Avalanche C-Chain",
    chain_id: 43114,
    rpc_url: "https://api.avax.network/ext/bc/C/rpc",
};

/// Avalanche Fuji testnet: chain id 43113. Verification network — the
/// release checklist runs real transactions here, never on mainnet.
pub const FUJI_TESTNET: Network = Network {
    name: "Avalanche Fuji",
    chain_id: 43113,
    rpc_url: "https://api.avax-test.network/ext/bc/C/rpc",
};

impl Network {
    /// Verifies an RPC-observed chain id against this network. Called with
    /// the answer to `eth_chainId` before a transaction is built; a mismatch
    /// is the §44 chain-confusion case and must close the session.
    ///
    /// # Errors
    ///
    /// [`AccountError::ChainMismatch`] naming both ids.
    pub fn check_chain_id(&self, observed: u64) -> Result<()> {
        if observed == self.chain_id {
            Ok(())
        } else {
            Err(AccountError::ChainMismatch {
                configured: self.chain_id,
                observed,
            })
        }
    }
}

// --- RLP ----------------------------------------------------------------------

/// One RLP item: a byte string or a list of items.
///
/// RLP has exactly these two shapes; integers are byte strings an encoder
/// produced from a big-endian minimal representation, and the round trip is
/// the caller's to interpret. The type is deliberately simple — the danger
/// in RLP is never the tree shape, it is the length-prefix rules, which live
/// in one place below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rlp {
    /// A byte string (which may carry an integer's minimal encoding).
    String(Vec<u8>),
    /// A list of items.
    List(Vec<Rlp>),
}

impl Rlp {
    /// The RLP encoding of this item.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Rlp::String(bytes) => {
                // Canonical form: a one-byte string below 0x80 is the byte
                // itself — the rule the decoder enforces and the encoder
                // must honor, or their round trip is not the identity.
                if bytes.len() == 1 && bytes[0] < 0x80 {
                    out.push(bytes[0]);
                } else {
                    encode_length(out, bytes.len(), 0x80);
                    out.extend_from_slice(bytes);
                }
            }
            Rlp::List(items) => {
                // A list encodes its contents first, then prefixes the whole
                // payload — the one genuinely recursive rule in RLP.
                let mut payload = Vec::new();
                for item in items {
                    item.encode_into(&mut payload);
                }
                encode_length(out, payload.len(), 0xC0);
                out.extend_from_slice(&payload);
            }
        }
    }

    /// The bytes of a string item.
    ///
    /// # Errors
    ///
    /// [`AccountError::MalformedRlp`] if this item is a list.
    pub fn as_string(&self) -> Result<&[u8]> {
        match self {
            Rlp::String(bytes) => Ok(bytes),
            Rlp::List(_) => Err(AccountError::MalformedRlp {
                what: "expected an RLP string, found a list",
            }),
        }
    }

    /// The items of a list.
    ///
    /// # Errors
    ///
    /// [`AccountError::MalformedRlp`] if this item is a string.
    pub fn as_list(&self) -> Result<&[Rlp]> {
        match self {
            Rlp::List(items) => Ok(items),
            Rlp::String(_) => Err(AccountError::MalformedRlp {
                what: "expected an RLP list, found a string",
            }),
        }
    }

    /// Interprets a string item as a minimal big-endian integer, the way the
    /// EIP-1559 fields were encoded. Rejects the non-canonical forms a
    /// hostile raw transaction might carry: leading zeros (the encoder never
    /// writes them) and a multi-byte string that should have been one byte.
    ///
    /// # Errors
    ///
    /// [`AccountError::MalformedRlp`] for non-minimal encodings, or
    /// [`AccountError::BadLength`] when the integer exceeds [`u128`].
    pub fn as_uint(&self) -> Result<u128> {
        let bytes = self.as_string()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        // 0 encodes as an empty string; 0x00 as its own byte is written only
        // for the string "\x00", never for the integer zero.
        if bytes.len() > 1 && bytes[0] == 0 {
            return Err(AccountError::MalformedRlp {
                what: "integer has a non-minimal (zero-leading) encoding",
            });
        }
        if bytes.len() == 1 && bytes[0] == 0 {
            return Err(AccountError::MalformedRlp {
                what: "integer zero must encode as the empty string",
            });
        }
        let mut value: u128 = 0;
        for byte in bytes {
            value = value
                .checked_mul(256)
                .and_then(|v| v.checked_add(u128::from(*byte)))
                .ok_or(AccountError::BadLength {
                    what: "RLP integer",
                    expected: 15,
                    actual: bytes.len(),
                })?;
        }
        Ok(value)
    }
}

/// Encodes a payload length prefix. `offset` is 0x80 for strings, 0xC0 for
/// lists; the short form is `offset + len` for payloads up to 55 bytes, the
/// long form is `offset + 55 + (bytes of length)` followed by the length
/// itself in minimal big-endian.
fn encode_length(out: &mut Vec<u8>, len: usize, offset: u8) {
    if len <= 55 {
        out.push(offset + len as u8);
    } else {
        let mut length_bytes = [0u8; 8];
        let mut n = 0;
        let mut value = len as u64;
        while value > 0 {
            length_bytes[n] = (value & 0xFF) as u8;
            value >>= 8;
            n += 1;
        }
        out.push(offset + 55 + n as u8);
        for i in (0..n).rev() {
            out.push(length_bytes[i]);
        }
    }
}

/// Decodes exactly one RLP item from `bytes`, rejecting trailing data.
///
/// Strictness is the point: this parses raw transactions that arrived over a
/// network, where a tolerant decoder is a differential oracle at best and a
/// memory-exhaustion primitive at worst. Lengths must be minimal (a
/// one-byte payload inside a long-form prefix is rejected) and the input
/// must be consumed exactly.
///
/// # Errors
///
/// [`AccountError::MalformedRlp`] for any non-canonical or truncated input.
pub fn rlp_decode(bytes: &[u8]) -> Result<Rlp> {
    let (item, consumed) = rlp_decode_item(bytes)?;
    if consumed != bytes.len() {
        return Err(AccountError::MalformedRlp {
            what: "trailing bytes after a complete RLP item",
        });
    }
    Ok(item)
}

/// Decodes one item and returns it with the number of bytes it consumed.
fn rlp_decode_item(bytes: &[u8]) -> Result<(Rlp, usize)> {
    let Some(&first) = bytes.first() else {
        return Err(AccountError::MalformedRlp {
            what: "empty input where an RLP item was expected",
        });
    };

    // Single byte below 0x80: the string is the byte itself.
    if first < 0x80 {
        return Ok((Rlp::String(vec![first]), 1));
    }

    let (offset, is_list) = if first <= 0xB7 {
        (0x80, false)
    } else if first <= 0xBF {
        // Long-form string: the next (first - 0xB7) bytes are the length.
        let length_of_length = usize::from(first - 0xB7);
        let len = read_length(bytes, 1, length_of_length)?;
        let start = 1 + length_of_length;
        let payload = take(bytes, start, len)?;
        return Ok((Rlp::String(payload.to_vec()), start + len));
    } else if first <= 0xF7 {
        (0xC0, true)
    } else {
        let length_of_length = usize::from(first - 0xF7);
        let len = read_length(bytes, 1, length_of_length)?;
        let start = 1 + length_of_length;
        let payload = take(bytes, start, len)?;
        let mut items = Vec::new();
        let mut offset_in_payload = 0;
        while offset_in_payload < payload.len() {
            let (item, consumed) = rlp_decode_item(&payload[offset_in_payload..])?;
            items.push(item);
            offset_in_payload += consumed;
        }
        if offset_in_payload != payload.len() {
            return Err(AccountError::MalformedRlp {
                what: "list payload does not end on an item boundary",
            });
        }
        return Ok((Rlp::List(items), start + len));
    };

    // Short forms: payload length is in the prefix byte itself.
    let len = usize::from(first - offset);
    let payload = take(bytes, 1, len)?;
    if is_list {
        let mut items = Vec::new();
        let mut at = 0;
        while at < payload.len() {
            let (item, consumed) = rlp_decode_item(&payload[at..])?;
            items.push(item);
            at += consumed;
        }
        Ok((Rlp::List(items), 1 + len))
    } else {
        // Canonical form: a one-byte string with value below 0x80 is the
        // byte itself, so 0x81 xx with xx < 0x80 is a redundant encoding.
        if len == 1 && payload[0] < 0x80 {
            return Err(AccountError::MalformedRlp {
                what: "single byte below 0x80 must encode as itself",
            });
        }
        Ok((Rlp::String(payload.to_vec()), 1 + len))
    }
}

/// Reads a long-form length of `length_of_length` bytes starting at `at`,
/// rejecting non-minimal encodings (a leading zero, or a length that fits in
/// fewer bytes than were used).
fn read_length(bytes: &[u8], at: usize, length_of_length: usize) -> Result<usize> {
    if length_of_length > 8 {
        return Err(AccountError::MalformedRlp {
            what: "RLP length prefix longer than 8 bytes",
        });
    }
    let length_bytes = take(bytes, at, length_of_length)?;
    if length_bytes[0] == 0 {
        return Err(AccountError::MalformedRlp {
            what: "RLP length has a leading zero byte",
        });
    }
    // Minimality is enforced by two rules: a leading zero byte is rejected
    // above, and a length small enough for the short form (or, with a
    // leading zero, for fewer bytes) is rejected below. Together they leave
    // exactly one encoding per length.
    let mut len: usize = 0;
    for byte in length_bytes {
        len = len
            .checked_mul(256)
            .and_then(|v| v.checked_add(usize::from(*byte)))
            .ok_or(AccountError::MalformedRlp {
                what: "RLP length overflows the platform address space",
            })?;
    }
    if len <= 55 {
        return Err(AccountError::MalformedRlp {
            what: "RLP length written in long form for a payload that fits the short form",
        });
    }
    Ok(len)
}

/// Slices `len` bytes at `at`, or fails as truncated input.
fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    bytes.get(at..at + len).ok_or(AccountError::MalformedRlp {
        what: "RLP input ends inside an item",
    })
}

/// Encodes an integer as its minimal big-endian RLP string. Zero is the
/// empty string — the one integer rule most hand-rolled encoders get wrong.
#[must_use]
pub fn rlp_uint(value: u128) -> Rlp {
    if value == 0 {
        return Rlp::String(Vec::new());
    }
    let bytes = value.to_be_bytes();
    let first = bytes
        .iter()
        .position(|b| *b != 0)
        .expect("value is non-zero");
    Rlp::String(bytes[first..].to_vec())
}

// --- the transaction ----------------------------------------------------------

/// An EIP-1559 transaction body: everything the user confirmed, exactly as it
/// will be signed.
///
/// `data` is empty for a native AVAX transfer — the whole token-transfer
/// semantics of a native send live in `value`, and a non-empty `data` makes
/// the transaction a contract call, which the send UI does not offer. The
/// access list is always empty in this build and encoded as such; the field
/// exists in the encoding because the chain expects nine items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eip1559Tx {
    /// The chain this transaction is bound to by signature (EIP-155 replay
    /// protection). Must equal the network the client verified.
    pub chain_id: u64,
    /// The account's next nonce, from `eth_getTransactionCount` — never
    /// guessed, because a wrong nonce is a replay or a stuck transaction.
    pub nonce: u64,
    /// The priority fee ceiling, wei per gas.
    pub max_priority_fee_per_gas: u128,
    /// The total fee ceiling, wei per gas.
    pub max_fee_per_gas: u128,
    /// The gas limit, from `eth_estimateGas` plus headroom.
    pub gas_limit: u64,
    /// The recipient.
    pub to: [u8; 20],
    /// The amount, wei. AVAX has 18 decimals.
    pub value: u128,
    /// Call data — empty for a native transfer.
    pub data: Vec<u8>,
}

impl Eip1559Tx {
    /// The nine signed fields as an RLP list, with the empty access list.
    #[must_use]
    pub fn body_rlp(&self) -> Vec<u8> {
        Rlp::List(vec![
            rlp_uint(u128::from(self.chain_id)),
            rlp_uint(u128::from(self.nonce)),
            rlp_uint(self.max_priority_fee_per_gas),
            rlp_uint(self.max_fee_per_gas),
            rlp_uint(u128::from(self.gas_limit)),
            Rlp::String(self.to.to_vec()),
            rlp_uint(self.value),
            Rlp::String(self.data.clone()),
            Rlp::List(Vec::new()),
        ])
        .encode()
    }

    /// The hash signed: `Keccak-256(0x02 || RLP(fields))`. This — not the
    /// raw transaction, not the receipt — is what the user's confirmation
    /// and the signature must agree on, which is why it is a named method
    /// and not a private detail.
    #[must_use]
    pub fn signing_hash(&self) -> [u8; 32] {
        let body = self.body_rlp();
        let mut input = Vec::with_capacity(body.len() + 1);
        input.push(EIP1559_TYPE);
        input.extend_from_slice(&body);
        keccak256(&input)
    }

    /// Signs this transaction with `wallet`, returning the raw transaction
    /// ready for `eth_sendRawTransaction`.
    ///
    /// Signing is recoverable-signature based so the y-parity comes from the
    /// signature itself rather than a second key operation, and low-s
    /// normalization flips the parity with it — the two must move together
    /// or the recovery lands on the wrong point.
    ///
    /// # Errors
    ///
    /// Nothing in practice; the error type exists because the secp256k1 API
    /// is fallible and this signature is part of the crate's [`Result`].
    pub fn sign(&self, wallet: &EvmWallet) -> Result<SignedTx> {
        let digest = self.signing_hash();
        let message = Message::from_digest(digest);
        let secret =
            SecretKey::from_secret_bytes(wallet.private_key_bytes()).expect("wallet key parses");
        let recoverable = RecoverableSignature::sign_ecdsa_recoverable(message, &secret);
        let (recovery_id, signature) = recoverable.serialize_compact();
        let mut parity = recovery_id.to_u8() & 1;

        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&signature[..32]);
        s.copy_from_slice(&signature[32..]);

        // Low-s: if s is above n/2 the EVM rejects the signature as
        // malleable. Replacing s with n - s keeps it a valid signature over
        // the same digest but mirrors the recovered point, so the parity bit
        // flips with it.
        if s.as_slice() > SECP256K1_N_HALF.as_slice() {
            // n - s, over big-endian bytes. `s` is a valid signature half,
            // so s < n and the subtraction never borrows past the top byte.
            let n = [
                0xFFu8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0xFF, 0xFF, 0xFE, 0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E,
                0x8C, 0xD0, 0x36, 0x41, 0x41,
            ];
            let mut complement = [0u8; 32];
            let mut borrow = 0i16;
            for i in (0..32).rev() {
                let diff = i16::from(n[i]) - i16::from(s[i]) - borrow;
                if diff < 0 {
                    complement[i] = (diff + 256) as u8;
                    borrow = 1;
                } else {
                    complement[i] = diff as u8;
                    borrow = 0;
                }
            }
            s = complement;
            parity ^= 1;
        }

        // The serialized form is `0x02 || RLP([the nine fields, y_parity, r,
        // s])` — a *flat* list of twelve items, unlike legacy transactions
        // where the signature wraps an already-encoded body. The type byte
        // sits outside the RLP envelope, which is why a raw transaction
        // starts `0x02 f8…`.
        let mut items = self.body_items();
        items.push(rlp_uint(u128::from(parity)));
        items.push(Rlp::String(r.to_vec()));
        items.push(Rlp::String(s.to_vec()));
        let envelope = Rlp::List(items).encode();
        let mut raw = Vec::with_capacity(envelope.len() + 1);
        raw.push(EIP1559_TYPE);
        raw.extend_from_slice(&envelope);
        let tx_hash = keccak256(&raw);

        Ok(SignedTx { raw, tx_hash })
    }

    /// The nine signed fields as RLP items (the raw transaction reuses the
    /// same items and appends the signature).
    fn body_items(&self) -> Vec<Rlp> {
        vec![
            rlp_uint(u128::from(self.chain_id)),
            rlp_uint(u128::from(self.nonce)),
            rlp_uint(self.max_priority_fee_per_gas),
            rlp_uint(self.max_fee_per_gas),
            rlp_uint(u128::from(self.gas_limit)),
            Rlp::String(self.to.to_vec()),
            rlp_uint(self.value),
            Rlp::String(self.data.clone()),
            Rlp::List(Vec::new()),
        ]
    }
}

/// A signed transaction: the raw bytes and the hash the chain will know it
/// by. The hash is `Keccak-256(raw)` — it depends on the signature, so it is
/// computed once here rather than re-derived by every consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTx {
    raw: Vec<u8>,
    tx_hash: [u8; 32],
}

impl SignedTx {
    /// The raw transaction, hex-ready for `eth_sendRawTransaction`.
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The transaction hash — also the key it is polled by.
    #[must_use]
    pub fn tx_hash(&self) -> &[u8; 32] {
        &self.tx_hash
    }
}

/// Parses a raw type-2 transaction and recovers the sender's address.
///
/// This is the proof that a signature belongs to the wallet that claims it,
/// and it is deliberately public: the ports' conformance tests sign with a
/// vector wallet and recover, which checks the entire path — body encoding,
/// digest, signature validity, parity handling — without needing ECDSA to be
/// deterministic anywhere (the Rust, TypeScript, and Kotlin stacks each use
/// their own nonces by design, and any valid low-s signature is the same
/// transaction to the chain).
///
/// Strictness: the envelope must be the nine fields, `r` and `s` exactly 32
/// bytes, `s` at most n/2, the parity bit 0 or 1, and the chain id non-zero
/// (a type-2 transaction without replay protection is not one).
///
/// # Errors
///
/// [`AccountError::NotATransaction`] for a non-type-2 envelope,
/// [`AccountError::MalformedRlp`] for structural problems, and
/// [`AccountError::BadSignature`] when the signature is not a valid
/// recovery over the body's own digest.
pub fn recover_sender(raw: &[u8]) -> Result<[u8; 20]> {
    // The type byte is outside the RLP envelope, and the envelope itself is
    // a *flat* list of twelve items: the nine body fields, then y_parity,
    // r, s — unlike a legacy transaction, which nests an encoded body
    // inside the signature wrapper.
    let Some((&tx_type, envelope_bytes)) = raw.split_first() else {
        return Err(AccountError::NotATransaction);
    };
    if tx_type != EIP1559_TYPE {
        return Err(AccountError::NotATransaction);
    }
    let envelope = rlp_decode(envelope_bytes).map_err(|e| match e {
        AccountError::MalformedRlp { .. } => AccountError::NotATransaction,
        other => other,
    })?;
    let items = envelope
        .as_list()
        .map_err(|_| AccountError::NotATransaction)?;
    if items.len() != 12 {
        return Err(AccountError::NotATransaction);
    }
    let body = &items[..9];

    let chain_id = body[0]
        .as_uint()
        .map_err(|_| AccountError::NotATransaction)?;
    if chain_id == 0 || chain_id > u128::from(u64::MAX) {
        return Err(AccountError::NotATransaction);
    }

    // Re-encode the body exactly as it arrived and hash it — recovery must
    // run over the bytes the signature was made over, byte for byte, so a
    // non-canonical field encoding inside `raw` is preserved rather than
    // normalized away here (the decoder's minimality rules already refused
    // the ambiguous forms).
    let body_bytes = Rlp::List(body.to_vec()).encode();
    let mut signing_input = Vec::with_capacity(body_bytes.len() + 1);
    signing_input.push(EIP1559_TYPE);
    signing_input.extend_from_slice(&body_bytes);
    let digest = keccak256(&signing_input);

    let parity = items[9]
        .as_uint()
        .map_err(|_| AccountError::NotATransaction)?;
    if parity > 1 {
        return Err(AccountError::BadSignature);
    }
    let r_bytes = items[10]
        .as_string()
        .map_err(|_| AccountError::NotATransaction)?;
    let s_bytes = items[11]
        .as_string()
        .map_err(|_| AccountError::NotATransaction)?;
    if r_bytes.len() != 32 || s_bytes.len() != 32 {
        return Err(AccountError::BadSignature);
    }
    if s_bytes > SECP256K1_N_HALF.as_slice() {
        // High-s: not a signature any EVM chain accepts, and not one this
        // module produces — treating it as recoverable would certify bytes
        // the chain will reject.
        return Err(AccountError::BadSignature);
    }

    let recovery = RecoveryId::from_u8_masked(parity as u8);
    let mut signature_64 = [0u8; 64];
    signature_64[..32].copy_from_slice(r_bytes);
    signature_64[32..].copy_from_slice(s_bytes);
    let signature = RecoverableSignature::from_compact(&signature_64, recovery)
        .map_err(|_| AccountError::BadSignature)?;
    let message = Message::from_digest(digest);
    let public = signature
        .recover_ecdsa(message)
        .map_err(|_| AccountError::BadSignature)?;

    let uncompressed = public.serialize_uncompressed();
    let hash = keccak256(&uncompressed[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    Ok(address)
}

// --- EIP-712 ------------------------------------------------------------------

/// Builds an EIP-712 `encodeType` string: the primary struct's declaration,
/// followed by the declaration of every struct type it references —
/// transitively — sorted by name.
///
/// The appendix is the part of EIP-712 every hand-rolled implementation
/// gets wrong (this one did, first): a struct that references other structs
/// does not hash to `keccak("Name(type member,…)")` alone; the referenced
/// declarations ride along. `Transaction(Person from,Person to,Asset tx)`
/// encodes as
/// `"Transaction(Person from,Person to,Asset tx)Asset(address token,uint256 amount)Person(address wallet,string name)"`.
/// The sort is by struct name; because a declaration begins with its name
/// followed by `(` (0x28, below every character that can continue a name),
/// sorting the declaration strings themselves is equivalent.
///
/// Each referenced declaration must itself already carry *its* references —
/// the caller closes the transitive set; this function only sorts and
/// appends what it is given.
#[must_use]
pub fn eip712_encode_type(primary: &str, referenced: &[&str]) -> String {
    let mut declarations: Vec<&str> = referenced.to_vec();
    declarations.sort_unstable();
    let mut out = String::from(primary);
    for declaration in declarations {
        out.push_str(declaration);
    }
    out
}

/// `Keccak-256(encodeType)` for the primary struct being signed — the message
/// half's counterpart of [`Eip712Domain::type_hash`], which does the same one
/// computation for the domain.
#[must_use]
pub fn eip712_type_hash(primary: &str, referenced: &[&str]) -> [u8; 32] {
    keccak256(eip712_encode_type(primary, referenced).as_bytes())
}

/// A typed value in the EIP-712 model this module hashes.
///
/// Deliberately narrow: the types the account surface actually signs today,
/// with 256-bit integers carried as fixed 32-byte big-endian arrays (the
/// crate holds no bigint dependency and does not need one to hash bytes).
/// Structs compose by hash: a struct field's contribution to its parent's
/// encoding is the child's `hash_struct` output — computed by the caller
/// with the child's own type hash (built with [`eip712_encode_type`] if the
/// child itself references structs) and supplied here as
/// [`Eip712Value::Bytes32`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eip712Value {
    /// `address` — left-padded to 32 bytes.
    Address([u8; 20]),
    /// `bytes32` — used verbatim.
    Bytes32([u8; 32]),
    /// `uintN`/`intN` — supplied as the 32-byte big-endian encoding; a
    /// `u64`/`u128` helper covers the common cases.
    Uint256([u8; 32]),
    /// `string` — hashed as `Keccak-256(utf-8)`.
    String(String),
    /// `bytes` — hashed as `Keccak-256(contents)`.
    Bytes(Vec<u8>),
    /// A dynamic array of same-typed values — `Keccak-256(concat(encodings))`.
    Array(Vec<Eip712Value>),
}

impl Eip712Value {
    /// An `Eip712Value::Uint256` from a `u128`.
    #[must_use]
    pub fn uint256(value: u128) -> Self {
        let mut out = [0u8; 32];
        out[16..].copy_from_slice(&value.to_be_bytes());
        Eip712Value::Uint256(out)
    }

    /// The 32-byte abi encoding of this value inside a hashStruct: fixed
    /// types are padded/hashed per EIP-712's `encodeData`, dynamic types by
    /// hashing their contents.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Eip712Value::Address(address) => {
                let mut out = vec![0u8; 32];
                out[12..].copy_from_slice(address);
                out
            }
            Eip712Value::Bytes32(bytes) => bytes.to_vec(),
            Eip712Value::Uint256(bytes) => bytes.to_vec(),
            Eip712Value::String(text) => keccak256(text.as_bytes()).to_vec(),
            Eip712Value::Bytes(bytes) => keccak256(bytes).to_vec(),
            Eip712Value::Array(items) => {
                let mut concatenated = Vec::new();
                for item in items {
                    concatenated.extend_from_slice(&item.encode());
                }
                keccak256(&concatenated).to_vec()
            }
        }
    }
}

/// The EIP-712 domain of a signing request. Field presence matters: the
/// domain separator's type hash is built from exactly the fields that are
/// `Some`, in the EIP's fixed order (name, version, chainId,
/// verifyingContract, salt), because a separator computed over different
/// fields than the dApp displayed is the primary EIP-712 phishing shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Eip712Domain {
    /// A human-readable name.
    pub name: Option<String>,
    /// A version string.
    pub version: Option<String>,
    /// The EIP-155 chain id.
    pub chain_id: Option<u64>,
    /// The contract that will verify the signature.
    pub verifying_contract: Option<[u8; 20]>,
    /// A disambiguating salt.
    pub salt: Option<[u8; 32]>,
}

impl Eip712Domain {
    /// `Keccak-256("EIP712Domain(" + joined types + ")")` over exactly the
    /// present fields.
    #[must_use]
    pub fn type_hash(&self) -> [u8; 32] {
        let mut types: Vec<&str> = Vec::new();
        if self.name.is_some() {
            types.push("string name");
        }
        if self.version.is_some() {
            types.push("string version");
        }
        if self.chain_id.is_some() {
            types.push("uint256 chainId");
        }
        if self.verifying_contract.is_some() {
            types.push("address verifyingContract");
        }
        if self.salt.is_some() {
            types.push("bytes32 salt");
        }
        let declaration = format!("EIP712Domain({})", types.join(","));
        keccak256(declaration.as_bytes())
    }

    /// The domain separator: `Keccak-256(abi.encode(typeHash, values...))`.
    #[must_use]
    pub fn separator(&self) -> [u8; 32] {
        let mut encoding = self.type_hash().to_vec();
        if let Some(name) = &self.name {
            encoding.extend_from_slice(&Eip712Value::String(name.clone()).encode());
        }
        if let Some(version) = &self.version {
            encoding.extend_from_slice(&Eip712Value::String(version.clone()).encode());
        }
        if let Some(chain_id) = self.chain_id {
            encoding.extend_from_slice(&Eip712Value::uint256(u128::from(chain_id)).encode());
        }
        if let Some(contract) = self.verifying_contract {
            encoding.extend_from_slice(&Eip712Value::Address(contract).encode());
        }
        if let Some(salt) = self.salt {
            encoding.extend_from_slice(&Eip712Value::Bytes32(salt).encode());
        }
        keccak256(&encoding)
    }
}

/// `hashStruct`: `Keccak-256(typeHash || encodeData(values))`, the message
/// half of the EIP-712 digest.
#[must_use]
pub fn eip712_hash_struct(type_hash: &[u8; 32], values: &[Eip712Value]) -> [u8; 32] {
    let mut encoding = type_hash.to_vec();
    for value in values {
        encoding.extend_from_slice(&value.encode());
    }
    keccak256(&encoding)
}

/// The final digest a wallet signs:
/// `Keccak-256(0x1901 || domainSeparator || hashStruct)`.
#[must_use]
pub fn eip712_digest(domain_separator: &[u8; 32], struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut input = Vec::with_capacity(66);
    input.extend_from_slice(&[0x19, 0x01]);
    input.extend_from_slice(domain_separator);
    input.extend_from_slice(struct_hash);
    keccak256(&input)
}

// --- address input ------------------------------------------------------------

/// Parses an address string for the send flow: `0x` optional, exactly 40 hex
/// characters. All-lowercase and all-uppercase are accepted as unchecked;
/// mixed case is accepted only when its EIP-55 checksum matches — a typo in
/// a checksummed recipient is the last line of defense before funds move,
/// and it must fail here rather than on the chain (§184, spec #44).
///
/// # Errors
///
/// [`AccountError::BadAddress`] for anything that is not 40 hex characters,
/// [`AccountError::AddressChecksumFailed`] for a mixed-case string whose
/// EIP-55 checksum does not match.
pub fn parse_address(text: &str) -> Result<[u8; 20]> {
    let stripped = text.strip_prefix("0x").unwrap_or(text);
    if stripped.len() != 40 || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AccountError::BadAddress);
    }
    let has_lower = stripped.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = stripped.chars().any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper {
        let bytes = decode_hex(stripped);
        let checksummed = crate::evm::eip55(&bytes);
        if checksummed[2..] != *stripped {
            return Err(AccountError::AddressChecksumFailed);
        }
        return Ok(bytes);
    }
    Ok(decode_hex(stripped))
}

/// Lowercase hex decode for a known-even-length, known-hex string.
fn decode_hex(text: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("caller checked hex digits");
    }
    out
}

/// Keccak-256, the one hash the EVM world agrees on.
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut digest = [0u8; 32];
    hasher.update(data);
    hasher.finalize(&mut digest);
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root::MigoRoot;

    fn wallet() -> EvmWallet {
        let root = MigoRoot::from_bytes(&[0x77u8; 32]).expect("root");
        EvmWallet::from_root(&root, 0).expect("wallet")
    }

    #[test]
    fn rlp_matches_the_specification_examples() {
        // The RLP specification's own examples, which every implementation
        // on earth agrees on. "dog" and the cat/dog list are the canonical
        // string/list cases; 1024 is the two-byte integer; the empty string
        // and empty list are the degenerate encodings.
        assert_eq!(
            Rlp::String(b"dog".to_vec()).encode(),
            [0x83, b'd', b'o', b'g']
        );
        assert_eq!(
            Rlp::List(vec![
                Rlp::String(b"cat".to_vec()),
                Rlp::String(b"dog".to_vec())
            ])
            .encode(),
            [0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
        assert_eq!(rlp_uint(1024).encode(), [0x82, 0x04, 0x00]);
        assert_eq!(rlp_uint(0).encode(), [0x80]);
        assert_eq!(Rlp::String(Vec::new()).encode(), [0x80]);
        assert_eq!(Rlp::List(Vec::new()).encode(), [0xc0]);
        assert_eq!(Rlp::String(vec![0x00]).encode(), [0x00]);
    }

    #[test]
    fn rlp_length_boundaries_are_exact() {
        // 55 bytes is the last short-form payload; 56 needs the long form
        // with a one-byte length. A one-off here corrupts every large
        // transaction silently.
        let short = vec![0xABu8; 55];
        let mut encoded = Rlp::String(short.clone()).encode();
        assert_eq!(encoded[0], 0x80 + 55);
        let long = vec![0xABu8; 56];
        encoded = Rlp::String(long.clone()).encode();
        assert_eq!(&encoded[..3], &[0xB8, 0x38, 0xAB]);
        assert_eq!(encoded.len(), 2 + 56);

        // And the decoder refuses the long form where the short one applies.
        let bad = [0xB8u8, 0x01, 0xAB];
        assert!(rlp_decode(&bad).is_err());
    }

    #[test]
    fn rlp_round_trips_nested_structures() {
        let item = Rlp::List(vec![
            rlp_uint(43114),
            Rlp::String(vec![0x11; 300]),
            Rlp::List(vec![Rlp::String(vec![]), rlp_uint(1)]),
        ]);
        let decoded = rlp_decode(&item.encode()).expect("round trip");
        assert_eq!(decoded, item);
    }

    #[test]
    fn the_decoder_is_strict() {
        // Trailing bytes after a complete item.
        assert!(rlp_decode(&[0x80, 0x00]).is_err());
        // Truncated payload.
        assert!(rlp_decode(&[0x83, b'd', b'o']).is_err());
        // Leading-zero integer encoding: valid RLP, non-canonical integer —
        // the string decodes, its integer reading does not.
        assert!(rlp_decode(&[0x82, 0x00, 0x04])
            .and_then(|item| item.as_uint())
            .is_err());
        // Integer zero as a single zero byte instead of the empty string.
        assert!(rlp_decode(&[0x00]).and_then(|item| item.as_uint()).is_err());
    }

    #[test]
    fn a_signed_transaction_recovers_its_sender() {
        let wallet = wallet();
        let tx = Eip1559Tx {
            chain_id: FUJI_TESTNET.chain_id,
            nonce: 7,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 25_000_000_000,
            gas_limit: 21_000,
            to: [0x42u8; 20],
            value: 1_500_000_000_000_000_000, // 1.5 AVAX
            data: Vec::new(),
        };
        let signed = tx.sign(&wallet).expect("sign");
        assert_eq!(
            recover_sender(signed.raw()).expect("recover"),
            *wallet.address(),
            "the recovered sender must be the signing wallet"
        );
        // The raw transaction is a type-2 envelope: 0x02 then a list.
        assert_eq!(signed.raw()[0], 0x02);
        // And its hash is keccak of the raw bytes.
        assert_eq!(signed.tx_hash(), &keccak256(signed.raw()));
    }

    #[test]
    fn signing_is_bound_to_the_chain_and_the_fields() {
        let base = Eip1559Tx {
            chain_id: AVALANCHE_MAINNET.chain_id,
            nonce: 1,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 2,
            gas_limit: 21_000,
            to: [0x01; 20],
            value: 3,
            data: Vec::new(),
        };
        let base_hash = base.signing_hash();

        // A different chain id is a different digest — replay protection in
        // one line.
        let mut other = base.clone();
        other.chain_id = FUJI_TESTNET.chain_id;
        assert_ne!(other.signing_hash(), base_hash);

        // Every user-visible field is inside the digest: changing the value
        // or the recipient changes what is signed, which is the property the
        // confirmation screen depends on.
        let mut pricier = base.clone();
        pricier.value = 4;
        assert_ne!(pricier.signing_hash(), base_hash);

        let mut elsewhere = base.clone();
        elsewhere.to = [0x02; 20];
        assert_ne!(elsewhere.signing_hash(), base_hash);
    }

    #[test]
    fn a_tampered_transaction_loses_its_sender() {
        let wallet = wallet();
        let tx = Eip1559Tx {
            chain_id: AVALANCHE_MAINNET.chain_id,
            nonce: 0,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 2,
            gas_limit: 21_000,
            to: [0x03; 20],
            value: 5,
            data: Vec::new(),
        };
        let signed = tx.sign(&wallet).expect("sign");
        assert_eq!(recover_sender(signed.raw()), Ok(*wallet.address()));

        // Flip one bit of the trailing signature: recovery either fails
        // outright or lands on a different point entirely — never back on
        // the signing wallet. Either way the tampered bytes no longer
        // certify this wallet as sender, which is the property that makes
        // "what was signed" and "what was shown" unable to drift apart.
        let mut tampered = signed.raw().to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_ne!(recover_sender(&tampered), Ok(*wallet.address()));
    }

    #[test]
    fn chain_ids_are_checked_not_assumed() {
        assert!(AVALANCHE_MAINNET.check_chain_id(43114).is_ok());
        assert!(FUJI_TESTNET.check_chain_id(43113).is_ok());
        let err = AVALANCHE_MAINNET
            .check_chain_id(1)
            .expect_err("Ethereum is not C-Chain");
        assert_eq!(
            err,
            AccountError::ChainMismatch {
                configured: 43114,
                observed: 1
            }
        );
    }

    #[test]
    fn addresses_parse_with_eip55_discipline() {
        // All lowercase and all uppercase are unchecked forms and pass.
        let lower = "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed";
        let upper = lower.to_uppercase().replace("0X", "0x");
        assert_eq!(
            parse_address(lower).expect("lowercase"),
            parse_address(&upper).expect("uppercase")
        );

        // The canonical mixed-case form passes.
        let checksummed = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        assert!(parse_address(checksummed).is_ok());

        // A wrong-checksum mixed-case string is rejected — the typo never
        // reaches the chain.
        let mut corrupted = checksummed.to_string();
        let a = corrupted.find('a').expect("a lowercase letter exists");
        corrupted.replace_range(a..a + 1, "A");
        assert!(matches!(
            parse_address(&corrupted),
            Err(AccountError::AddressChecksumFailed)
        ));

        // Wrong length and non-hex never parse.
        assert!(matches!(
            parse_address("0x1234"),
            Err(AccountError::BadAddress)
        ));
        assert!(matches!(
            parse_address("0xzz00"),
            Err(AccountError::BadAddress)
        ));
    }

    #[test]
    fn eip712_domain_separator_depends_on_field_presence() {
        // Same values, different present fields: different separators. This
        // is the property that keeps a displayed domain and a signed domain
        // from being two different things.
        let full = Eip712Domain {
            name: Some("Ether Mail".into()),
            version: Some("1".into()),
            chain_id: Some(1),
            verifying_contract: Some([0xcc; 20]),
            salt: None,
        };
        let mut minimal = full.clone();
        minimal.name = None;
        minimal.version = None;
        assert_ne!(full.separator(), minimal.separator());
        // 32 bytes, like every EIP-712 hash.
        assert_eq!(full.separator().len(), 32);
    }

    #[test]
    fn eip712_values_encode_per_the_eip() {
        // address left-pads to 32; uint256 pads from the right of its own
        // encoding; string and bytes hash their contents.
        let mut padded = vec![0u8; 12];
        padded.extend_from_slice(&[0xAB; 20]);
        assert_eq!(Eip712Value::Address([0xAB; 20]).encode(), padded);
        assert_eq!(Eip712Value::uint256(1).encode()[31], 1);
        let hashed = Eip712Value::String("hello".into()).encode();
        assert_eq!(hashed.as_slice(), keccak256(b"hello"));
    }

    #[test]
    fn eip712_encode_type_appends_referenced_structs_sorted() {
        // The EIP's own example: Transaction references Asset and Person,
        // and the encoding appends both, sorted by name.
        assert_eq!(
            eip712_encode_type(
                "Transaction(Person from,Person to,Asset tx)",
                &[
                    "Person(address wallet,string name)",
                    "Asset(address token,uint256 amount)",
                ]
            ),
            "Transaction(Person from,Person to,Asset tx)Asset(address token,uint256 amount)Person(address wallet,string name)"
        );
    }

    /// The EIP-712 specification's worked example, end to end: the Ether
    /// Mail domain, a Mail message from Cow to Bob, and the digest the EIP
    /// publishes (and signs with keccak256("cow") as the private key). The
    /// type hash carrying the referenced Person declaration is the rule this
    /// test exists to keep — an implementation that hashes only
    /// "Mail(Person from,Person to,string contents)" produces a plausible,
    /// entirely different digest.
    #[test]
    fn eip712_matches_the_specifications_worked_example() {
        let person_type = keccak256(b"Person(string name,address wallet)");
        let mail_type = keccak256(
            eip712_encode_type(
                "Mail(Person from,Person to,string contents)",
                &["Person(string name,address wallet)"],
            )
            .as_bytes(),
        );

        // Cow's wallet address, as bytes (the EIP prints it EIP-55 checksummed;
        // the casing is display-only and the bytes are what the hash sees).
        let cow: [u8; 20] = [
            0xcd, 0x2a, 0x3d, 0x9f, 0x93, 0x8e, 0x13, 0xcd, 0x94, 0x7e, 0xc0, 0x5a, 0xbc, 0x7f,
            0xe7, 0x34, 0xdf, 0x8d, 0xd8, 0x26,
        ];
        let bob: [u8; 20] = [0xbb; 20];

        let from = eip712_hash_struct(
            &person_type,
            &[Eip712Value::String("Cow".into()), Eip712Value::Address(cow)],
        );
        let to = eip712_hash_struct(
            &person_type,
            &[Eip712Value::String("Bob".into()), Eip712Value::Address(bob)],
        );
        let mail = eip712_hash_struct(
            &mail_type,
            &[
                Eip712Value::Bytes32(from),
                Eip712Value::Bytes32(to),
                Eip712Value::String("Hello, Bob!".into()),
            ],
        );

        let domain = Eip712Domain {
            name: Some("Ether Mail".into()),
            version: Some("1".into()),
            chain_id: Some(1),
            verifying_contract: Some([0xcc; 20]),
            salt: None,
        };
        let digest = eip712_digest(&domain.separator(), &mail);

        assert_eq!(
            hex::encode(digest),
            "be609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2"
        );
    }
}
