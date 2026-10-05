//! The ticket that admits one device to one call on the media plane.
//!
//! # Why a ticket rather than the session token
//!
//! The SFU is a separate process on purpose (section 92): its load profile is bandwidth, not
//! application logic, so it holds no database, no cache and no session store — which is exactly
//! why it cannot ask whether an access token is still valid. What it can do is verify a claim the
//! node signed. So the node mints a ticket when a device joins a group call over the signalling
//! plane, and the media plane verifies it with the same key, offline, in constant time, with
//! nothing to look up.
//!
//! The ticket names the call, the account, the device and an expiry. It names no bandwidth mode
//! (a session sets its own) and carries no key material of any kind: the media this plane forwards
//! is sealed under the call's own keys, agreed between participants, and those keys reach neither
//! process.
//!
//! # The encoding
//!
//! `base64url(payload || tag)`, where the payload is four fixed-width fields — call id, account
//! id, device id, expiry — and the tag is an HMAC-SHA256 over a domain-separated prefix and that
//! payload. Fixed widths mean a ticket's length is a constant, so a truncated ticket is refused by
//! arithmetic rather than by a length field a forger would also control. The domain separation is
//! what keeps this key from being usable for anything else the node signs with it.
//!
//! # What verification does not do
//!
//! It does not check that the call exists, that the account is a member of the conversation, or
//! that the account is not banned. Those are facts of the store, and the store is on the other
//! side of the ticket: the node that minted it had them in hand and would not have signed
//! otherwise. Trusting the signature is the whole point of a ticket, and re-deriving those facts
//! here would put the store back inside the process section 92 keeps it out of.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use migo_core::{Id, Timestamp};
use migo_sfu::Member;

/// The domain separator, so this key can sign tickets and nothing else.
const PREFIX: &[u8] = b"migo-sfu-ticket/v1";

/// A call id, an account id, a device id and an expiry: 16 + 16 + 16 + 8.
const PAYLOAD_LEN: usize = 56;

/// The full HMAC-SHA256 tag, not a truncation. The tag rides a connection QUIC already encrypts,
/// so bytes are not the scarce resource here, and a full tag leaves no room for an argument about
/// how short is still safe.
const TAG_LEN: usize = 32;

/// Every valid ticket is exactly this many bytes before base64url.
pub const TOKEN_LEN: usize = PAYLOAD_LEN + TAG_LEN;

/// What a ticket claims, once it has been verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SfuTicket {
    /// The group call the device may join.
    pub call_id: Id,
    /// The account and device the seat belongs to.
    pub member: Member,
    /// When the claim stops being true. A session is not torn down at this instant — the ticket is
    /// an admission, not a lease — but a reconnect after it must fetch a fresh one.
    pub expires_at: Timestamp,
}

/// Why a ticket was refused.
///
/// Every variant is one answer to one question a client can act on: mint a new one, or stop
/// trying. That is why this is an enum and not a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TicketError {
    /// The token is not valid base64url.
    #[error("the ticket is not valid base64url")]
    Encoding,
    /// The token decoded to a length no minted ticket can have.
    #[error("the ticket is not a whole ticket")]
    Length,
    /// The tag does not match the payload: the ticket was not minted by this node, or was edited
    /// after it was.
    #[error("the ticket's tag does not match its payload")]
    Forged,
    /// The claim has expired.
    #[error("the ticket expired")]
    Expired,
}

/// Why a signing key was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TicketKeyError {
    /// The key is shorter than the minimum an HMAC-SHA256 key is expected to have.
    #[error("the ticket key is {0} bytes; at least 32 are required")]
    TooShort(usize),
}

/// The key both halves of the node hold: the minting half in `migod`, the verifying half in the
/// media process.
///
/// The type exists so the key cannot be confused with any other string in a configuration file,
/// and so nothing can log it: its `Debug` prints the length of what it holds and never the bytes.
#[derive(Clone)]
pub struct TicketKey {
    key: Vec<u8>,
}

impl TicketKey {
    /// Wraps raw key material.
    ///
    /// The minimum length is the one an HMAC-SHA256 key is expected to have; a shorter key is a
    /// deployment mistake caught at startup rather than a weaker ticket discovered later.
    ///
    /// # Errors
    ///
    /// Refuses anything shorter than 32 bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, TicketKeyError> {
        if bytes.len() < 32 {
            return Err(TicketKeyError::TooShort(bytes.len()));
        }
        Ok(Self { key: bytes })
    }

    /// Decodes a key from the text a configuration file carries, by the same rules the
    /// configuration validator applies — one decoder, so validation cannot accept a key that
    /// signing then rejects.
    ///
    /// # Errors
    ///
    /// Refuses material that decodes to fewer than 32 bytes.
    pub fn from_config(value: &str) -> Result<Self, TicketKeyError> {
        Self::from_bytes(migo_core::config::decode_key_material(value))
    }

    /// How many bytes the key holds. Never the bytes themselves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.key.len()
    }

    /// True when the key holds no bytes, which construction forbids.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// Signs a claim, returning the token a client presents at the media plane.
    #[must_use]
    pub fn mint(&self, ticket: &SfuTicket) -> String {
        let payload = encode_payload(ticket);
        let tag = self.tag(&payload);
        let mut token = Vec::with_capacity(TOKEN_LEN);
        token.extend_from_slice(&payload);
        token.extend_from_slice(&tag);
        URL_SAFE_NO_PAD.encode(token)
    }

    /// Verifies a token and reads its claim.
    ///
    /// The tag is compared in constant time, so a caller learns only whether the ticket is valid —
    /// never how much of it was right.
    ///
    /// # Errors
    ///
    /// [`TicketError`], by the reason the claim was refused.
    pub fn verify(&self, token: &str, now: Timestamp) -> Result<SfuTicket, TicketError> {
        let raw = URL_SAFE_NO_PAD
            .decode(token.trim())
            .map_err(|_| TicketError::Encoding)?;
        if raw.len() != TOKEN_LEN {
            return Err(TicketError::Length);
        }
        let (payload, tag) = raw.split_at(PAYLOAD_LEN);
        let expected = self.tag(payload);
        // Constant time over the whole tag, and the lengths are equal by construction: everything
        // above this line is public bytes.
        if expected.ct_eq(tag).unwrap_u8() != 1 {
            return Err(TicketError::Forged);
        }
        let ticket = decode_payload(payload);
        if now.is_at_or_after(ticket.expires_at) {
            return Err(TicketError::Expired);
        }
        Ok(ticket)
    }

    /// The tag over a payload, domain-separated.
    fn tag(&self, payload: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC accepts a key of any length, and the field minimum is enforced");
        mac.update(PREFIX);
        mac.update(payload);
        let bytes = mac.finalize().into_bytes();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&bytes);
        tag
    }
}

impl std::fmt::Debug for TicketKey {
    /// Prints the length and nothing else: a key is not a log field.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketKey")
            .field("len", &self.key.len())
            .finish()
    }
}

/// Lays a claim out in its fixed-width form.
fn encode_payload(ticket: &SfuTicket) -> [u8; PAYLOAD_LEN] {
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[0..16].copy_from_slice(ticket.call_id.as_bytes());
    payload[16..32].copy_from_slice(ticket.member.account_id.as_bytes());
    payload[32..48].copy_from_slice(ticket.member.device_id.as_bytes());
    payload[48..56].copy_from_slice(&ticket.expires_at.as_millis().to_be_bytes());
    payload
}

/// Reads a claim back out of its fixed-width form.
fn decode_payload(payload: &[u8]) -> SfuTicket {
    let mut call = [0u8; 16];
    call.copy_from_slice(&payload[0..16]);
    let mut account = [0u8; 16];
    account.copy_from_slice(&payload[16..32]);
    let mut device = [0u8; 16];
    device.copy_from_slice(&payload[32..48]);
    let mut expiry = [0u8; 8];
    expiry.copy_from_slice(&payload[48..56]);
    SfuTicket {
        call_id: Id::from_bytes(call),
        member: Member {
            account_id: Id::from_bytes(account),
            device_id: Id::from_bytes(device),
        },
        expires_at: Timestamp::from_millis(i64::from_be_bytes(expiry)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic id for a test.
    fn id(seed: u8) -> Id {
        Id::from_bytes([seed; 16])
    }

    fn key() -> TicketKey {
        TicketKey::from_bytes(vec![7u8; 32]).expect("32 bytes is a key")
    }

    fn ticket() -> SfuTicket {
        SfuTicket {
            call_id: id(1),
            member: Member {
                account_id: id(2),
                device_id: id(3),
            },
            expires_at: Timestamp::from_millis(1_000_000),
        }
    }

    #[test]
    fn a_minted_ticket_verifies_back_to_its_claim() {
        let key = key();
        let token = key.mint(&ticket());
        let read = key
            .verify(&token, Timestamp::from_millis(999_999))
            .expect("valid");
        assert_eq!(read, ticket());
    }

    #[test]
    fn a_ticket_is_a_constant_length_whatever_it_names() {
        let key = key();
        let mut other = ticket();
        other.call_id = id(9);
        assert_eq!(key.mint(&ticket()).len(), key.mint(&other).len());
    }

    #[test]
    fn another_key_cannot_verify_a_ticket() {
        let token = key().mint(&ticket());
        let other = TicketKey::from_bytes(vec![8u8; 32]).expect("32 bytes is a key");
        assert_eq!(
            other.verify(&token, Timestamp::ZERO),
            Err(TicketError::Forged)
        );
    }

    #[test]
    fn a_flipped_byte_in_the_payload_is_refused() {
        let key = key();
        let token = key.mint(&ticket());
        let mut raw = URL_SAFE_NO_PAD.decode(&token).expect("decodes");
        raw[20] ^= 0x01;
        let edited = URL_SAFE_NO_PAD.encode(&raw);
        assert_eq!(
            key.verify(&edited, Timestamp::ZERO),
            Err(TicketError::Forged)
        );
    }

    #[test]
    fn an_expired_ticket_is_refused_at_its_instant_and_after() {
        let key = key();
        let token = key.mint(&ticket());
        let expiry = ticket().expires_at;
        assert!(
            key.verify(&token, expiry.saturating_add_millis(-1)).is_ok(),
            "the instant before the expiry is still inside the claim"
        );
        assert_eq!(
            key.verify(&token, expiry),
            Err(TicketError::Expired),
            "the expiry is the first instant the claim is not true"
        );
    }

    #[test]
    fn a_truncated_or_stretched_token_is_refused_by_length() {
        let key = key();
        let token = key.mint(&ticket());
        let raw = URL_SAFE_NO_PAD.decode(&token).expect("decodes");
        assert_eq!(
            key.verify(
                &URL_SAFE_NO_PAD.encode(&raw[..TOKEN_LEN - 1]),
                Timestamp::ZERO
            ),
            Err(TicketError::Length)
        );
        let mut longer = raw.clone();
        longer.push(0);
        assert_eq!(
            key.verify(&URL_SAFE_NO_PAD.encode(&longer), Timestamp::ZERO),
            Err(TicketError::Length)
        );
        assert_eq!(
            key.verify("not base64url at all!", Timestamp::ZERO),
            Err(TicketError::Encoding)
        );
    }

    #[test]
    fn a_key_shorter_than_the_minimum_is_refused() {
        assert_eq!(
            TicketKey::from_bytes(vec![0u8; 31]).err(),
            Some(TicketKeyError::TooShort(31))
        );
        assert!(TicketKey::from_bytes(vec![0u8; 32]).is_ok());
    }

    #[test]
    fn a_key_decodes_from_every_encoding_an_operator_pastes() {
        let standard = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
        let hex = "03".repeat(32);
        for text in [standard.as_str(), hex.as_str()] {
            let key = TicketKey::from_config(text).expect("decodes");
            assert_eq!(key.len(), 32);
        }
    }

    #[test]
    fn the_debug_of_a_key_is_its_length_and_never_its_bytes() {
        let key = TicketKey::from_bytes(vec![0xABu8; 32]).expect("32 bytes is a key");
        let printed = format!("{key:?}");
        assert!(printed.contains("len: 32"), "{printed}");
        assert!(!printed.contains("ab"), "{printed}");
    }
}
