//! The access token: a signed, opaque, fixed-layout bearer credential.
//!
//! # Why not a JWT
//!
//! A JWT would be the conventional choice and it is the wrong one here.
//!
//! A JWT carries its own algorithm identifier, which means the verifier is told by
//! the attacker which algorithm to verify with. Twenty years of that field have
//! produced `alg: none` acceptance, RS256-verified-as-HS256 key confusion, and a long
//! tail of libraries that got one of the two right. The mitigation is always the same
//! — ignore the header and pin the algorithm — at which point the header is dead
//! weight that still has to be parsed. This format has no algorithm field, so there is
//! no algorithm to confuse.
//!
//! A JWT also puts a JSON parser on the hot path of every single request, before
//! authentication. That parser is reachable by anyone who can open a socket. A fixed
//! byte layout has no parser: it has a length check and eight slice indexes, all of
//! them bounded by constants known at compile time.
//!
//! Finally, a JWT is roughly twice the size for the same claims, and this token rides
//! on every frame the gateway receives.
//!
//! The cost of going custom is that third-party tooling cannot read our tokens. That
//! is not a cost. Nothing third-party should be reading our session tokens.
//!
//! Note that "custom format" is not "custom crypto" (ADR-0003): the only primitive
//! involved is [`MacKey`], which is HMAC-SHA-256 from an audited crate.
//!
//! # Layout
//!
//! All integers big-endian. Offsets are absolute and every one of them is a constant.
//!
//! ```text
//!   0..1     version           1  always TOKEN_VERSION
//!   1..17    account id       16
//!  17..33    device id        16
//!  33..49    session id       16
//!  49..57    capabilities      8  bitmask, see the capability module
//!  57..65    issued at         8  Migo-epoch millis
//!  65..73    expires at        8  Migo-epoch millis
//!  73..81    authenticated at  8  Migo-epoch millis, carried across refreshes
//!  81..82    region length     1  0..=REGION_MAX_BYTES
//!  82..98    region           16  ASCII, zero padded
//!  98..130   tag              32  HMAC-SHA-256 over bytes 0..98
//! ```
//!
//! The tag covers the version byte, so an attacker cannot re-label a v1 token as a
//! future v2 with different field meanings.
//!
//! # Why `authenticated_at` is in the token
//!
//! Error code `REAUTHENTICATION_REQUIRED` (1108) exists for operations that need proof
//! the human is still present — changing a passphrase, removing the last device,
//! deleting the account (brief section 125). Answering "how long ago did they type
//! their passphrase" needs a timestamp, not a bit: a freshness *bit* minted at sign-in
//! would still read fresh fourteen minutes later, which is most of the token's life.
//!
//! It is carried forward across refreshes rather than reset, because a refresh is not
//! a proof of presence — a stolen refresh token would otherwise reset the freshness
//! clock forever, which is exactly backwards.
//!
//! # Revocation
//!
//! A signed token is valid until it expires; that is what "stateless" means. Immediate
//! revocation happens on the paths that read the session row anyway, and
//! [`Signer::verify`] deliberately does not read anything. So the exposure after a
//! revocation is bounded by `auth.access_ttl_seconds` — fifteen minutes in the shipped
//! configuration — and callers that cannot accept fifteen minutes must use the
//! checked path instead. This is written down here because it is the kind of property
//! that gets quietly assumed away.

use std::fmt;

use migo_core::{config::decode_key_material, Id, Result, Secret, Timestamp};
use migo_crypto::{mac::LABEL_SESSION_TOKEN, MacKey};
use migo_protocol::{codes, fault};

use crate::capability::Capabilities;

/// Format version. Bumped when a field moves, never when a value changes meaning.
pub const TOKEN_VERSION: u8 = 1;

/// Longest region label a token can carry.
///
/// Sixteen bytes fits every plausible region name (`us-east-1`, `eu-central-1`,
/// `ap-southeast-2`) with room left over. It is a fixed field rather than
/// length-prefixed-and-variable because a fixed layout has no parser, and a region
/// label is not the place to introduce one.
pub const REGION_MAX_BYTES: usize = 16;

const OFF_VERSION: usize = 0;
const OFF_ACCOUNT: usize = 1;
const OFF_DEVICE: usize = 17;
const OFF_SESSION: usize = 33;
const OFF_CAPABILITIES: usize = 49;
const OFF_ISSUED: usize = 57;
const OFF_EXPIRES: usize = 65;
const OFF_AUTHENTICATED: usize = 73;
const OFF_REGION_LEN: usize = 81;
const OFF_REGION: usize = 82;

/// Bytes covered by the tag.
pub const SIGNED_BYTES: usize = OFF_REGION + REGION_MAX_BYTES;

/// Total decoded token length.
pub const TOKEN_BYTES: usize = SIGNED_BYTES + migo_crypto::mac::TAG_LEN;

/// Length of the base64url text form. Constant, because the byte length is.
pub const TOKEN_TEXT_LEN: usize = (TOKEN_BYTES * 4).div_ceil(3);

/// How long a refresh token is before encoding.
///
/// Thirty-two bytes of randomness. It carries no claims — it is a lookup key and
/// nothing else, so there is nothing to encode and no reason for structure. Structure
/// in a refresh token is structure an attacker can study.
pub const REFRESH_BYTES: usize = 32;

/// What a verified access token says.
///
/// Everything here was signed. Nothing here was looked up, which is the point: a
/// gateway that has to hit the database to learn who is speaking cannot survive a
/// reconnect storm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claims {
    /// Who the caller is.
    pub account_id: Id,
    /// Which of their devices. Compared against the device the client claims, so a
    /// token lifted from one device cannot be replayed from another that has its own
    /// key material.
    pub device_id: Id,
    /// Which login. This is the revocation handle: revoking a session invalidates
    /// every token minted from it on the paths that check.
    pub session_id: Id,
    /// What authentication state permits, as a bitmask.
    pub capabilities: Capabilities,
    /// When this token was minted.
    pub issued_at: Timestamp,
    /// When it stops being accepted.
    pub expires_at: Timestamp,
    /// When the human last proved presence. See the module docs.
    pub authenticated_at: Timestamp,
}

impl Claims {
    /// Whether the token is still inside its window at `now`.
    #[must_use]
    pub fn is_live(&self, now: Timestamp) -> bool {
        !now.is_at_or_after(self.expires_at)
    }

    /// Milliseconds since the human last authenticated.
    ///
    /// Saturating, so a clock that has gone backwards reads as zero elapsed rather than
    /// as an enormous age — the conservative direction, since a large age is what
    /// *satisfies* a freshness check.
    #[must_use]
    pub fn presence_age_ms(&self, now: Timestamp) -> u64 {
        now.saturating_since(self.authenticated_at)
    }

    /// Whether the human authenticated recently enough for a sensitive operation.
    #[must_use]
    pub fn presence_is_fresh(&self, now: Timestamp, within_ms: u64) -> bool {
        self.presence_age_ms(now) <= within_ms
    }
}

/// Mints and verifies access tokens, and hashes refresh tokens.
///
/// Holds two derived keys rather than the root, so the root can be dropped by the
/// caller after construction. The two labels are separate on purpose: a stored refresh
/// hash must not also be a valid signature over the same bytes.
pub struct Signer {
    access: MacKey,
    refresh: MacKey,
    region: Box<[u8]>,
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No key material, not even a length. A `Debug` that prints a key length is a
        // `Debug` that will eventually print a key.
        f.debug_struct("Signer")
            .field("region", &String::from_utf8_lossy(&self.region))
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// Builds a signer from configured key material.
    ///
    /// The key is decoded with [`decode_key_material`], the same function
    /// [`migo_core::Config::validate`] uses to measure it. Two decoders would
    /// eventually disagree, and the disagreement would present as tokens that
    /// validation accepted and signing rejected.
    ///
    /// Refuses a region label longer than [`REGION_MAX_BYTES`] rather than truncating
    /// it: a silently shortened region would place rooms in the wrong place, and it
    /// would do so only in the deployment whose name happened to be long.
    pub fn new(key: &Secret, region: &str) -> Result<Self> {
        let root = decode_key_material(key.expose());
        if root.len() < migo_core::config::MIN_TOKEN_KEY_BYTES {
            return Err(fault::internal(format!(
                "token key decodes to {} bytes, need at least {}",
                root.len(),
                migo_core::config::MIN_TOKEN_KEY_BYTES
            )));
        }
        if region.len() > REGION_MAX_BYTES {
            return Err(fault::internal(format!(
                "region label is {} bytes, limit is {REGION_MAX_BYTES}",
                region.len()
            )));
        }
        if !region.is_ascii() {
            return Err(fault::internal(
                "region label must be ASCII: it travels in a fixed-width binary field",
            ));
        }
        Ok(Self {
            access: MacKey::derive(&root, LABEL_SESSION_TOKEN),
            refresh: MacKey::derive(&root, migo_crypto::mac::LABEL_REFRESH_TOKEN),
            region: region.as_bytes().to_vec().into_boxed_slice(),
        })
    }

    /// The region this signer stamps into tokens.
    #[must_use]
    pub fn region(&self) -> &str {
        // Checked ASCII at construction, so this cannot fail; the lossy form avoids an
        // unwrap on a path that has no business panicking.
        std::str::from_utf8(&self.region).unwrap_or("")
    }

    /// Encodes and signs a set of claims.
    #[must_use]
    pub fn mint(&self, claims: &Claims) -> String {
        let mut buf = [0u8; TOKEN_BYTES];
        buf[OFF_VERSION] = TOKEN_VERSION;
        buf[OFF_ACCOUNT..OFF_DEVICE].copy_from_slice(claims.account_id.as_bytes());
        buf[OFF_DEVICE..OFF_SESSION].copy_from_slice(claims.device_id.as_bytes());
        buf[OFF_SESSION..OFF_CAPABILITIES].copy_from_slice(claims.session_id.as_bytes());
        buf[OFF_CAPABILITIES..OFF_ISSUED]
            .copy_from_slice(&claims.capabilities.bits().to_be_bytes());
        buf[OFF_ISSUED..OFF_EXPIRES].copy_from_slice(&claims.issued_at.to_wire().to_be_bytes());
        buf[OFF_EXPIRES..OFF_AUTHENTICATED]
            .copy_from_slice(&claims.expires_at.to_wire().to_be_bytes());
        buf[OFF_AUTHENTICATED..OFF_REGION_LEN]
            .copy_from_slice(&claims.authenticated_at.to_wire().to_be_bytes());
        // Length fits by the check in `new`.
        buf[OFF_REGION_LEN] = self.region.len() as u8;
        buf[OFF_REGION..OFF_REGION + self.region.len()].copy_from_slice(&self.region);

        let tag = self.access.tag(&buf[..SIGNED_BYTES]);
        buf[SIGNED_BYTES..].copy_from_slice(&tag);
        encode(&buf)
    }

    /// Verifies a token and returns its claims.
    ///
    /// # Order of checks
    ///
    /// Length, then tag, then version, then fields, then expiry. Verifying the tag
    /// before reading any field means a forged token with an unknown version fails
    /// exactly the same way as a forged token with a known one, so the response
    /// carries no information about which parts of a guess were right.
    ///
    /// Expiry is checked last so that an expired-but-genuine token yields
    /// `TOKEN_EXPIRED` — a client that gets that code knows to refresh, whereas
    /// `TOKEN_INVALID` tells it to throw everything away and make the user sign in
    /// again. Getting this backwards produces a product that logs people out roughly
    /// every fifteen minutes.
    pub fn verify(&self, token: &str, now: Timestamp) -> Result<Claims> {
        let raw = decode(token)?;
        self.access
            .verify(&raw[..SIGNED_BYTES], &raw[SIGNED_BYTES..])
            .map_err(|_| fault::error(codes::TOKEN_INVALID, "access token signature mismatch"))?;

        if raw[OFF_VERSION] != TOKEN_VERSION {
            // Authentic but from a version this build does not know. Not
            // `TOKEN_INVALID`: the client should refresh into the current format, not
            // conclude its credentials are forged.
            return Err(fault::error(
                codes::TOKEN_EXPIRED,
                format!(
                    "access token version {} is not {TOKEN_VERSION}",
                    raw[OFF_VERSION]
                ),
            ));
        }

        let region_len = usize::from(raw[OFF_REGION_LEN]);
        if region_len > REGION_MAX_BYTES {
            // Signed, so this is our own bug rather than an attack.
            return Err(fault::internal(
                "signed access token declares an impossible region length",
            ));
        }

        let claims = Claims {
            account_id: Id::from_bytes(fixed16(&raw[OFF_ACCOUNT..OFF_DEVICE])),
            device_id: Id::from_bytes(fixed16(&raw[OFF_DEVICE..OFF_SESSION])),
            session_id: Id::from_bytes(fixed16(&raw[OFF_SESSION..OFF_CAPABILITIES])),
            capabilities: Capabilities::from_bits(u64::from_be_bytes(fixed8(
                &raw[OFF_CAPABILITIES..OFF_ISSUED],
            ))),
            issued_at: Timestamp::from_wire(u64::from_be_bytes(fixed8(
                &raw[OFF_ISSUED..OFF_EXPIRES],
            ))),
            expires_at: Timestamp::from_wire(u64::from_be_bytes(fixed8(
                &raw[OFF_EXPIRES..OFF_AUTHENTICATED],
            ))),
            authenticated_at: Timestamp::from_wire(u64::from_be_bytes(fixed8(
                &raw[OFF_AUTHENTICATED..OFF_REGION_LEN],
            ))),
        };

        if !claims.is_live(now) {
            return Err(fault::error(codes::TOKEN_EXPIRED, "access token expired"));
        }
        Ok(claims)
    }

    /// Reads the region a token was minted in, without trusting it.
    ///
    /// Separate from [`Signer::verify`] because the region is the one field a node may
    /// want from a token it cannot verify: a token minted in another region is signed
    /// with that region's key, so verification fails and the useful answer is "send
    /// this to `eu-central-1`" rather than "invalid".
    ///
    /// Returns `None` for anything malformed. The value is unauthenticated — it may be
    /// used to route a retry and for nothing else.
    #[must_use]
    pub fn peek_region(token: &str) -> Option<String> {
        let raw = decode(token).ok()?;
        if raw[OFF_VERSION] != TOKEN_VERSION {
            return None;
        }
        let len = usize::from(raw[OFF_REGION_LEN]);
        if len > REGION_MAX_BYTES {
            return None;
        }
        std::str::from_utf8(&raw[OFF_REGION..OFF_REGION + len])
            .ok()
            .map(str::to_owned)
    }

    /// Generates a refresh token and the tag to store for it.
    ///
    /// Returns `(token, stored_tag)`. Only the tag is persisted, so a database dump
    /// yields no working credentials — and because the tag is keyed, an attacker who
    /// has the dump but not the token key cannot even confirm a guess offline.
    pub fn mint_refresh(&self, random: &mut dyn migo_core::Random) -> (Secret, [u8; 32]) {
        let mut bytes = [0u8; REFRESH_BYTES];
        random.fill_bytes(&mut bytes);
        let tag = self.refresh.tag(&bytes);
        (Secret::new(encode(&bytes)), tag)
    }

    /// Recomputes the stored tag for a refresh token the client presented.
    ///
    /// Any input length is accepted and produces a tag; a garbage token simply fails
    /// to match a row. This is deliberate — refusing early on a length check would let
    /// a caller distinguish "wrong shape" from "wrong value", and that distinction is
    /// worth nothing to a legitimate client.
    #[must_use]
    pub fn refresh_tag(&self, token: &str) -> [u8; 32] {
        match decode_refresh(token) {
            Some(bytes) => self.refresh.tag(&bytes),
            // Tag the text as given. Cannot collide with a real tag, since a real one
            // is over exactly `REFRESH_BYTES` of decoded input.
            None => self.refresh.tag_parts(&[b"raw:", token.as_bytes()]),
        }
    }
}

/// Base64url without padding. No padding because these values end up in URLs, headers,
/// and JSON, and `=` is escaped differently by each of the three.
fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes a token to exactly [`TOKEN_BYTES`].
///
/// The length check comes first and is the reason every slice index in this module is
/// infallible.
fn decode(token: &str) -> Result<[u8; TOKEN_BYTES]> {
    use base64::Engine as _;
    if token.len() != TOKEN_TEXT_LEN {
        return Err(fault::error(
            codes::TOKEN_INVALID,
            "access token has the wrong length",
        ));
    }
    let mut out = [0u8; TOKEN_BYTES];
    let written = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(token.as_bytes(), &mut out)
        .map_err(|_| fault::error(codes::TOKEN_INVALID, "access token is not base64url"))?;
    if written != TOKEN_BYTES {
        return Err(fault::error(
            codes::TOKEN_INVALID,
            "access token decoded to the wrong size",
        ));
    }
    Ok(out)
}

/// Decodes a refresh token, or `None` if it is not one.
fn decode_refresh(token: &str) -> Option<[u8; REFRESH_BYTES]> {
    use base64::Engine as _;
    let mut out = [0u8; REFRESH_BYTES];
    let written = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(token.as_bytes(), &mut out)
        .ok()?;
    (written == REFRESH_BYTES).then_some(out)
}

/// Copies a 16-byte window into an array.
///
/// The callers all pass constant-width windows, so the `unwrap_or` branch is
/// unreachable; it exists so that a future offset typo becomes a nil id rather than a
/// panic in the authentication path.
fn fixed16(window: &[u8]) -> [u8; 16] {
    window.try_into().unwrap_or([0u8; 16])
}

/// Copies an 8-byte window into an array. See [`fixed16`].
fn fixed8(window: &[u8]) -> [u8; 8] {
    window.try_into().unwrap_or([0u8; 8])
}
