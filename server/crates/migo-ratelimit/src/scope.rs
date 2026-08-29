//! What a bucket is keyed by.
//!
//! Brief section 120 names seven rate-limited surfaces and this module has exactly
//! seven variants, one per surface, in the same order. That is deliberate: the brief is
//! the specification and a limiter with an eighth surface nobody specified, or a
//! seventh missing, is a limiter whose coverage nobody can check by reading it.
//!
//! # Why the surfaces are separate buckets rather than one
//!
//! Each surface answers a different question, and a single bucket can only answer one.
//! An account bucket stops one user shouting; it does nothing about ten thousand
//! throwaway accounts behind one host, which is what the IP bucket is for. A device
//! bucket stops one runaway client without punishing the user's other sessions. A room
//! bucket protects the *readers* of a busy room from its writers, none of whom is
//! individually abusive. An endpoint bucket stops a caller from spending an otherwise
//! reasonable budget entirely on the one expensive operation.
//!
//! They are charged together and the first refusal wins, so the effective limit is the
//! tightest surface — which is the point. Adding a surface can only ever make the
//! system more restrictive, never less, so a new one cannot open a hole.

use std::fmt;
use std::net::IpAddr;

use migo_cache::key::{CacheKey, SCOPE_BUCKET};
use migo_core::Id;
use migo_protocol::Opcode;

/// Length of the token fingerprint [`BucketKey::token`] accepts.
///
/// Thirty-two bytes, which is the output of the MAC in `migo-crypto`. The signature
/// takes an array rather than a slice so that a caller who has the raw token in hand
/// cannot pass it: `&[u8]` would accept `token.as_bytes()` and put a bearer credential
/// into a cache key, where it would then appear in every `KEYS`-style operator dump.
pub const TOKEN_FINGERPRINT_BYTES: usize = 32;

/// How many bytes of the fingerprint go into the key.
///
/// Sixteen, hex-encoded to thirty-two characters. Half a MAC is 128 bits of collision
/// resistance, which for a key namespace is enormous overkill and still shorter than
/// the tail budget; the other half is left out because a key does not need to be a
/// verifier, only a distinguisher.
const TOKEN_KEY_BYTES: usize = 16;

/// A rate-limited surface.
///
/// `Copy` and eight bytes wide at most, so it is passed by value and stored in a
/// verdict without an allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The caller's network, truncated. Catches distributed abuse from one host.
    Ip,
    /// The authenticated account. The user's own budget.
    Account,
    /// One installation of one client. Contains a runaway loop to the session that
    /// wrote it.
    Device,
    /// A credential, by fingerprint. Bounds what a leaked token can do before it is
    /// revoked.
    Token,
    /// A room. Protects its readers from its writers.
    Room,
    /// A bot identity. Bots get a larger budget and a separate one, so a busy
    /// integration cannot spend a human's.
    Bot,
    /// One opcode for one subject. Stops a whole budget going on one expensive call.
    Endpoint,
}

impl Scope {
    /// Every surface, in brief section 120's order.
    ///
    /// Iterated by the metrics registration so that all seven rejection series exist
    /// from startup. A series that appears only once it fires is a series no alert can
    /// be written against before the incident it was meant to catch.
    pub const ALL: &'static [Self] = &[
        Self::Ip,
        Self::Account,
        Self::Device,
        Self::Token,
        Self::Room,
        Self::Bot,
        Self::Endpoint,
    ];

    /// The key fragment. Short, because it is a prefix on the largest keyspace in the
    /// system.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Account => "ac",
            Self::Device => "dv",
            Self::Token => "tk",
            Self::Room => "rm",
            Self::Bot => "bt",
            Self::Endpoint => "ep",
        }
    }

    /// The metric label. Spelled out, because a dashboard is read by people.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Account => "account",
            Self::Device => "device",
            Self::Token => "token",
            Self::Room => "room",
            Self::Bot => "bot",
            Self::Endpoint => "endpoint",
        }
    }

    /// Position in [`Scope::ALL`], for indexing a fixed-size array of counters.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Ip => 0,
            Self::Account => 1,
            Self::Device => 2,
            Self::Token => 3,
            Self::Room => 4,
            Self::Bot => 5,
            Self::Endpoint => 6,
        }
    }

    /// Whether one caller's standing may set this surface's limit.
    ///
    /// False for the surfaces strangers share. A room's bucket must not widen because
    /// a trusted member happened to send the message that created it — the next
    /// hundred messages could come from a hundred new accounts, and the room's readers
    /// would pay for a budget that was granted on somebody else's reputation. The same
    /// argument applies to an IP: a residential NAT and a botnet's exit node look
    /// identical from the server, and one long-standing user behind either must not
    /// raise the ceiling for everybody else behind it.
    ///
    /// True for the surfaces that name exactly one party, where the standing being
    /// scaled is that party's own.
    #[must_use]
    pub const fn scales_with_tier(self) -> bool {
        match self {
            Self::Ip | Self::Room => false,
            Self::Account | Self::Device | Self::Token | Self::Bot | Self::Endpoint => true,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A surface and the exact bucket on it.
///
/// Constructed through the named constructors and never by field, so that a key can
/// only be built the one way its surface allows: an IP is always truncated, a token is
/// always a fingerprint. The `CacheKey` is built once here and carried, rather than
/// rebuilt inside the limiter, because a request charges several keys and every one of
/// them would otherwise be formatted twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketKey {
    scope: Scope,
    key: CacheKey,
}

impl BucketKey {
    /// The caller's network.
    ///
    /// The address is truncated by [`network`] before it becomes a key, so the bucket
    /// is shared by everybody in the same /24 or /64. That is the privacy requirement
    /// (brief sections 162 and 174: address data is cut to a network class and kept at
    /// most seven days, and a full address never reaches a log) and it is also better
    /// limiting: an attacker with a /64 to spend would otherwise get a fresh budget per
    /// address, and there are more addresses in one /64 than there are grains of sand.
    #[must_use]
    pub fn ip(address: IpAddr) -> Self {
        Self::at(Scope::Ip, &network(address))
    }

    /// One account.
    #[must_use]
    pub fn account(account_id: Id) -> Self {
        Self::at(Scope::Account, &account_id.to_text())
    }

    /// One account, billed by the service that does the work.
    ///
    /// The companion of [`Self::endpoint_write_of_account`], and it exists for the same
    /// reason: the gateway edge bills every frame on [`Self::account`], and the owning
    /// domain then meters its own write. Sharing one bucket made a single expensive
    /// opcode cost twice the price the IDL puts on it — a `KEY_PUBLISH` at twenty took
    /// forty of a probationary account's fifty, so the first thing a new client does left
    /// it nothing to send a message with. Two buckets, each sized for the opcode once,
    /// keep both layers metering at the price the registry actually states.
    #[must_use]
    pub fn account_write(account_id: Id) -> Self {
        Self::at(Scope::Account, &format!("{}/write", account_id.to_text()))
    }

    /// One device.
    #[must_use]
    pub fn device(device_id: Id) -> Self {
        Self::at(Scope::Device, &device_id.to_text())
    }

    /// One credential, named by a MAC or hash of it.
    ///
    /// Never the credential itself. See [`TOKEN_FINGERPRINT_BYTES`] for why the
    /// parameter is an array.
    #[must_use]
    pub fn token(fingerprint: &[u8; TOKEN_FINGERPRINT_BYTES]) -> Self {
        let mut hex = String::with_capacity(TOKEN_KEY_BYTES * 2);
        for byte in &fingerprint[..TOKEN_KEY_BYTES] {
            // `write!` to a String cannot fail. Formatted by hand rather than through
            // a hex crate to keep this crate's dependency list at three.
            hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        Self::at(Scope::Token, &hex)
    }

    /// One room.
    #[must_use]
    pub fn room(room_id: Id) -> Self {
        Self::at(Scope::Room, &room_id.to_text())
    }

    /// One bot identity.
    #[must_use]
    pub fn bot(bot_id: Id) -> Self {
        Self::at(Scope::Bot, &bot_id.to_text())
    }

    /// One opcode for one account.
    #[must_use]
    pub fn endpoint_of_account(account_id: Id, opcode: Opcode) -> Self {
        Self::at(
            Scope::Endpoint,
            &format!("{}/{}", account_id.to_text(), opcode.name()),
        )
    }

    /// One opcode for one account, billed to the service that does the work.
    ///
    /// The gateway edge charges every frame on [`Self::endpoint_of_account`]; the owning
    /// domain then meters its own write on the way in. Both layers billing one bucket
    /// meant the second charge met a bucket the first had already emptied, and the most
    /// expensive opcodes refused forever for the newest accounts. The write surface is a
    /// separate bucket with the same policy shape, so each layer pays its own way.
    #[must_use]
    pub fn endpoint_write_of_account(account_id: Id, opcode: Opcode) -> Self {
        Self::at(
            Scope::Endpoint,
            &format!("{}/{}/write", account_id.to_text(), opcode.name()),
        )
    }

    /// One opcode for one network. The pre-authentication form: before a session has
    /// an account there is still something to limit per operation, and it is the only
    /// thing the server knows about the caller.
    #[must_use]
    pub fn endpoint_of_ip(address: IpAddr, opcode: Opcode) -> Self {
        Self::at(
            Scope::Endpoint,
            &format!("{}/{}", network(address), opcode.name()),
        )
    }

    /// Which surface this is. Read by the limiter to pick a policy and by the verdict
    /// to say which surface refused.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// The key the cache sees.
    #[must_use]
    pub const fn cache_key(&self) -> &CacheKey {
        &self.key
    }

    fn at(scope: Scope, tail: &str) -> Self {
        Self {
            scope,
            key: CacheKey::new(SCOPE_BUCKET, &format!("{}/{tail}", scope.code())),
        }
    }
}

impl fmt::Display for BucketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key)
    }
}

/// An address reduced to the network it sits in: /24 for IPv4, /64 for IPv6.
///
/// Public because this is also the form that may be logged. Two functions that both
/// truncate an address would eventually disagree, and the one that disagrees by being
/// more precise is a privacy incident, so there is one.
///
/// The output is standard notation, which means an IPv6 network's colons are
/// percent-escaped by [`CacheKey`]. That is accepted rather than worked around: a
/// second textual form for IPv6 would be one more thing to keep in step with the
/// first, and the escaping is documented and injective.
#[must_use]
pub fn network(address: IpAddr) -> String {
    match address {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            format!("{a}.{b}.{c}.0/24")
        }
        IpAddr::V6(v6) => {
            let [a, b, c, d, ..] = v6.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
    }
}
