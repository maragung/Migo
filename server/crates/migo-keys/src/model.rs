//! Types the key service takes and returns.
//!
//! # Why these are not the protocol structs
//!
//! `migo_protocol::KeyPublish` and `migo_protocol::KeyBundle` exist and are generated,
//! so unlike the social frames there is a wire type to map to. These types exist anyway,
//! for the same reason every other domain crate has its own: this crate must be usable
//! and testable without a `Writer`, and a service whose signature is a wire struct is a
//! service whose tests need the wire format to be right before they can say anything
//! about the rule they are testing.
//!
//! The mapping is one function in the composition root, and it is where the two
//! deliberate mismatches between the brief and the IDL are handled. See
//! [`PublishRequest::signed_prekey_expires_at`] for the first and [`Bundle`] for the
//! second.

use migo_core::{Id, Timestamp};
use migo_ratelimit::TrustTier;

/// One-time prekeys a single publication may carry.
///
/// A hundred, which is what the client libraries generate in one batch. It is a bound on
/// a `Vec` that arrives from the network before anything is written, so it exists mostly
/// to keep a hostile publication from turning one charged request into an unbounded
/// insert.
pub const MAX_ONE_TIME_PREKEYS: usize = 100;

/// Devices one bundle fetch may return.
///
/// Twenty. A fetch with no `device_id` asks for every live device of an account, and each
/// one costs a consumed one-time prekey, so an account with a hundred devices would make
/// one charged request consume a hundred prekeys across the fleet. Twenty is far above
/// any real device count and keeps the worst case bounded.
pub const MAX_BUNDLES_PER_FETCH: usize = 20;

/// How long a published signed prekey stays acceptable.
///
/// Thirty days. Section 163 requires the client to rotate the signed prekey periodically
/// and does not name the period, so the server names the window it will honour and the
/// client rotates inside it. Long enough that a device offline for a fortnight still has
/// a valid prekey when it comes back; short enough that a compromised signed prekey stops
/// being useful within a month even if the device never rotates again.
pub const SIGNED_PREKEY_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// Below this many remaining one-time prekeys, a fetch tells the owner to top up.
///
/// Twenty, which is a fifth of [`MAX_ONE_TIME_PREKEYS`]. The point of a low-water mark
/// rather than a zero check is that a device has to be *told* while it can still be
/// helped: warning at zero warns after the guarantee has already been weakened for
/// however many conversations started in the meantime.
pub const ONE_TIME_PREKEY_LOW_WATER: u32 = 20;

/// Who is asking.
///
/// The same five fields as every other domain's caller, and its own type for the same
/// reason: a shared one would be a dependency between two layer-3 crates, and a
/// `migo_auth::RequestContext` here would make this crate depend on the crate that issues
/// tokens in order to publish a public key.
///
/// No `reauthenticated` flag. Publishing key material is not a step-up action: the device
/// doing it already holds the private halves, so a second passphrase prompt would protect
/// nothing that the session token does not already protect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on, and the device the keys belong to.
    ///
    /// Load-bearing for [`crate::Keyring::publish`]: key material is published *for the
    /// device that is asking*, never for a device named in the request. A field a client
    /// could fill would let one device replace another device's identity key, which is
    /// the whole attack that publishing exists to prevent.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

impl Caller {
    /// A caller at `now`.
    #[must_use]
    pub fn new(account_id: Id, device_id: Id, tier: TrustTier, now: Timestamp) -> Self {
        Self {
            account_id,
            device_id,
            tier,
            now,
            request_id: None,
        }
    }

    /// Sets the correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// What this deployment does when a device has run out of one-time prekeys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeysConfig {
    /// Refuse a bundle fetch that cannot include a one-time prekey.
    ///
    /// `false`, which is what section 163 asks for by default: when the one-time prekeys
    /// run out the session still forms from the signed prekey alone, the server flags it,
    /// and the owner is told to refill. `PREKEYS_EXHAUSTED` is only returned "bila
    /// kebijakan menuntut" — when a deployment would rather a conversation fail to start
    /// than start with a first message that lacks per-message forward secrecy.
    ///
    /// Turning it on is a real trade and not a hardening freebie: an offline device that
    /// exhausted its prekeys becomes unreachable, and unreachable is a state a user reads
    /// as the app being broken.
    pub refuse_when_exhausted: bool,
}

/// A device's public key material, as published.
///
/// Every byte here is public. There is no field on this struct, and no field on anything
/// this crate touches, that a private key could be put in — which is the structural half
/// of section 163's "server stores no private key in any form". The other half is that
/// nothing here ever derives, and so never has a reason to want one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRequest {
    /// Ed25519 signing key followed by X25519 exchange key, exactly
    /// [`migo_crypto::IDENTITY_PUBLIC_LEN`] bytes.
    pub identity_key: Vec<u8>,
    /// The id the publishing device assigned to this signed prekey.
    pub signed_prekey_id: u32,
    /// X25519 public key, exactly [`migo_crypto::PUBLIC_KEY_LEN`] bytes.
    pub signed_prekey: Vec<u8>,
    /// Ed25519 signature over the prekey, exactly [`migo_crypto::SIGNATURE_LEN`] bytes.
    pub signed_prekey_signature: Vec<u8>,
    /// When the signed prekey stops being acceptable.
    ///
    /// Section 163 lists this as a field of `KEY_PUBLISH`, and the generated
    /// `migo_protocol::KeyPublish` does not have it: the IDL and its golden vectors are
    /// frozen, and a domain crate does not get to extend the wire format on the way past.
    ///
    /// So it is a parameter here and the composition root fills it with
    /// `now + SIGNED_PREKEY_LIFETIME_MS`. The rule that an already-expired prekey is
    /// refused is still enforced against whatever arrives in this field, which today
    /// always passes; when the field reaches the IDL, the client's value flows into it
    /// and the check starts mattering without a line of this crate changing.
    pub signed_prekey_expires_at: Timestamp,
    /// One-time prekeys, `(key_id, public_key)`, at most [`MAX_ONE_TIME_PREKEYS`] of them.
    pub one_time_prekeys: Vec<(u32, Vec<u8>)>,
}

/// What a publication did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOutcome {
    /// How many one-time prekeys were stored.
    ///
    /// May be fewer than were sent: a key id this device has already published is
    /// skipped rather than replacing the stored key, because the stored one may already
    /// have been handed to a peer and overwriting it would hand two peers the same id
    /// with different bytes.
    pub accepted_prekeys: u32,
    /// The identity fingerprint, hex, lower case.
    ///
    /// Returned so that the publishing device can show the user the same safety number a
    /// contact will see, without asking the server what its own identity is. It is
    /// derived from the bytes that were just verified, so a client that renders it is
    /// rendering a fingerprint of the key the server actually stored.
    pub identity_fingerprint: String,
    /// One-time prekeys the device now has unconsumed.
    pub one_time_prekeys_remaining: u32,
}

/// One device's bundle, as a sender receives it.
///
/// # Why the expiry is here and not on the wire
///
/// Section 163 says the fetched bundle carries the signed prekey's expiry, and the
/// generated `migo_protocol::KeyBundle` has no field for it. Same frozen-IDL reason as
/// [`PublishRequest::signed_prekey_expires_at`], and the same resolution: the domain type
/// carries it, the mapping to the wire struct drops it, and nothing is lost today because
/// the server already refuses to serve a bundle whose signed prekey has expired. When the
/// field lands in the IDL the sender gets to see the nudge for itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    /// The account the device belongs to.
    pub account_id: Id,
    /// The device.
    pub device_id: Id,
    /// Identity key, [`migo_crypto::IDENTITY_PUBLIC_LEN`] bytes.
    pub identity_key: Vec<u8>,
    /// Signed prekey id.
    pub signed_prekey_id: u32,
    /// Signed prekey public bytes.
    pub signed_prekey: Vec<u8>,
    /// Signature over the signed prekey.
    pub signed_prekey_signature: Vec<u8>,
    /// When the signed prekey stops being acceptable.
    pub signed_prekey_expires_at: Timestamp,
    /// The one-time prekey this fetch consumed, if the device had one left.
    ///
    /// `None` means the device is out. The session still forms from the signed prekey
    /// alone; see [`KeysConfig::refuse_when_exhausted`].
    pub one_time_prekey: Option<(u32, Vec<u8>)>,
}

impl Bundle {
    /// Whether this bundle came without a one-time prekey.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.one_time_prekey.is_none()
    }
}

/// What a fetch returned, and what the caller should be told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// One bundle per live device, at most [`MAX_BUNDLES_PER_FETCH`].
    ///
    /// Empty is a possible answer and is not an error here: the caller decides whether an
    /// account with no published keys is `NOT_FOUND` or an empty list, because that
    /// choice is about what the *opcode* promises and not about key material.
    pub bundles: Vec<Bundle>,
    /// At least one bundle came without a one-time prekey.
    ///
    /// The flag section 163 asks the server to set. The caller passes it to the owning
    /// account as a top-up nudge; it is a fact about the *subject's* devices and not
    /// about the fetcher, so it is reported rather than turned into an error.
    pub any_exhausted: bool,
}
