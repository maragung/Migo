//! What a session is allowed to be, as one machine word.
//!
//! The wire carries a `u64` in `Authenticated::capabilities`, and this module is the
//! meaning of those bits. Every bit here follows from **authentication state alone**:
//! whether there is an account behind the socket, what standing that account has, and
//! whether the account can be reached out-of-band.
//!
//! # What is deliberately not in here
//!
//! Product permissions — brief section 48's `CHAT_SEND`, `USER_BAN`, `ROOM_MANAGE` —
//! are *not* capabilities. They are scoped to a room or a conversation, they are
//! resolved from `(actor, scope, target)` at the moment of the action, and they change
//! while a session is open. A bitmask minted at sign-in and carried for fifteen
//! minutes inside a token cannot represent any of that, and a client that believed it
//! could would show a Ban button to somebody who lost the role ten minutes ago.
//!
//! So the rule is: a capability tells the client what kind of session it has. A
//! permission tells the server whether one action is allowed, and the server is the
//! only one that decides it.
//!
//! # Why a bitmask and not a set of strings
//!
//! Because it goes in the token, and the token is presented on every reconnect by
//! every device. Eight bytes is eight bytes; a string set is a parser plus an
//! allocation plus a length limit plus an escaping question.

use std::fmt;

use migo_ratelimit::TrustTier;
use migo_store::model::Account;

/// There is an authenticated account behind this session.
///
/// Bit zero rather than an implicit "the mask is non-zero", so that a client can tell
/// "the server sent no capabilities" from "the server sent capabilities and none are
/// set".
pub const AUTHENTICATED: u64 = 1 << 0;

/// The session belongs to a bot rather than a person.
///
/// Clients render bot messages differently and must not offer a bot a friend request.
pub const BOT: u64 = 1 << 1;

/// The account is new enough that limits are tightened.
///
/// Sent so the client can *explain* a refusal instead of showing a generic error. An
/// account that is being throttled for being one hour old and an account that is
/// being throttled for flooding deserve different wording.
pub const PROBATION: u64 = 1 << 2;

/// The account has earned raised limits.
pub const TRUSTED: u64 = 1 << 3;

/// The account has a verified-or-pending address on file.
///
/// Not "has an email": whether recovery and security notifications have anywhere to
/// go. A client that knows this can prompt for an address before the user relies on
/// account recovery, which is the warning brief section 106 requires to arrive
/// *before* the loss rather than after it.
pub const CONTACTABLE: u64 = 1 << 4;

/// Every bit this build defines. Anything outside it is reserved.
pub const KNOWN: u64 = AUTHENTICATED | BOT | PROBATION | TRUSTED | CONTACTABLE;

/// A session's capability set.
///
/// Unknown bits survive a round trip untouched. A newer node may mint a token with a
/// bit this build has never heard of, and an older node that strips it would silently
/// downgrade a session on every refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(u64);

impl Capabilities {
    /// An empty set. What an unauthenticated socket has.
    pub const NONE: Self = Self(0);

    /// Wraps a raw mask, including bits this build does not define.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The raw mask, for the wire and for the token.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether every bit in `mask` is set.
    ///
    /// `mask` rather than a single bit so that a caller can ask one question about a
    /// combination — an empty mask is vacuously present, which is the mathematically
    /// right answer and also the one that keeps `has(KNOWN & other)` from surprising
    /// anybody.
    #[must_use]
    pub const fn has(self, mask: u64) -> bool {
        self.0 & mask == mask
    }

    /// The same set with `mask` added.
    #[must_use]
    pub const fn with(self, mask: u64) -> Self {
        Self(self.0 | mask)
    }

    /// Bits this build does not define.
    #[must_use]
    pub const fn unknown(self) -> u64 {
        self.0 & !KNOWN
    }

    /// The capabilities of a human session, from the account and its standing.
    ///
    /// Standing comes in as an argument rather than being computed here: this
    /// function is about turning facts into bits, and [`crate::tier`] is about
    /// deciding the facts. Keeping them apart means the tier rules can change without
    /// touching the wire format.
    #[must_use]
    pub fn for_account(account: &Account, tier: TrustTier) -> Self {
        let mut bits = AUTHENTICATED;
        if account.email.is_some() || account.phone.is_some() {
            bits |= CONTACTABLE;
        }
        match tier {
            TrustTier::New => bits |= PROBATION,
            TrustTier::Trusted => bits |= TRUSTED,
            TrustTier::Bot => bits |= BOT,
            TrustTier::Anonymous | TrustTier::Established => {}
        }
        Self(bits)
    }
}

impl fmt::Display for Capabilities {
    /// Names the bits that are set, for a log line and for an operator.
    ///
    /// A hex mask in a support ticket is one lookup table away from being useful; a
    /// name is useful immediately.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named = [
            (AUTHENTICATED, "authenticated"),
            (BOT, "bot"),
            (PROBATION, "probation"),
            (TRUSTED, "trusted"),
            (CONTACTABLE, "contactable"),
        ];
        let mut first = true;
        for (bit, name) in named {
            if self.has(bit) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        let unknown = self.unknown();
        if unknown != 0 {
            if !first {
                f.write_str("|")?;
            }
            write!(f, "reserved:{unknown:#x}")?;
            first = false;
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}
