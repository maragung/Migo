//! The types the bot service speaks in: who is calling, the permission bitmask, the
//! registration request, and the views handed back.
//!
//! # Scopes are a bitmask here, a raw integer in the store
//!
//! The `bot` row keeps `scopes` as a raw `i64`, opaque to the storage layer, exactly as a
//! game's `kind` or a ledger entry's `source` is a raw integer there. What those bits *mean*
//! — which one is "send messages", which is "moderate" — is a domain fact, and domain facts
//! live in this crate. [`Scopes`] is that mapping, in one place, with the conversion to and
//! from the stored integer beside it.

use migo_core::{Id, Secret, Timestamp};
use migo_ratelimit::TrustTier;

/// The most bots one owner may hold, the default cap.
///
/// A generous ceiling for a real integrator and a low one for an abuser scripting account
/// creation. It is a soft limit — the check races a concurrent registration and a small
/// overshoot is harmless — not a security invariant; the account and token uniqueness that
/// *is* an invariant is enforced atomically by the store.
pub const DEFAULT_MAX_BOTS_PER_OWNER: u16 = 25;

/// Longest display name a bot may carry, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 48;

/// Longest webhook URL this crate will store, in bytes.
pub const MAX_WEBHOOK_URL_BYTES: usize = 512;

/// Everything the service needs to know about the caller of a management request.
///
/// Identical in shape to every other layer-3 crate's caller. This is always the **owner** —
/// a human account managing bots it owns — never the bot itself: a bot has no method here it
/// could call, only a token the gateway authenticates.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The authenticated owner account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
    /// Correlation id, for joining a trace to a log line.
    pub request_id: Option<String>,
}

/// What a bot is permitted to do, as a bitmask over the six permissions of brief section 41.
///
/// The default a bot is registered with is [`Scopes::NONE`]: section 41 requires the minimum,
/// and the minimum is nothing. Each capability is granted deliberately by the owner. The bits
/// are part of the stored format and must not be renumbered; a new permission takes the next
/// free bit and widens [`Scopes::ALL`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scopes(u32);

impl Scopes {
    /// No permissions. What a freshly registered bot has, and the minimum section 41 mandates.
    pub const NONE: Self = Self(0);

    /// Read messages in conversations the bot is a member of.
    pub const READ_MESSAGES: Self = Self(1 << 0);

    /// Send messages as the bot.
    pub const SEND_MESSAGES: Self = Self(1 << 1);

    /// Take moderation actions the bot's account is otherwise entitled to.
    pub const MODERATE: Self = Self(1 << 2);

    /// Start, play, and manage mini-games.
    pub const MANAGE_GAMES: Self = Self(1 << 3);

    /// Read a conversation's member list.
    pub const READ_MEMBERS: Self = Self(1 << 4);

    /// Send announcements to a room.
    pub const SEND_ANNOUNCEMENTS: Self = Self(1 << 5);

    /// Every permission. For a fully trusted integration and for tests.
    pub const ALL: Self = Self(0b11_1111);

    /// Each scope paired with its stable slug, in bit order.
    ///
    /// The slugs are the wire names the API layer serialises to and parses from, and the
    /// closed set a client builds a permission picker out of. Never reworded once shipped:
    /// like an error code, an operator and an export both depend on the exact string.
    pub const NAMED: [(Self, &'static str); 6] = [
        (Self::READ_MESSAGES, "read_messages"),
        (Self::SEND_MESSAGES, "send_messages"),
        (Self::MODERATE, "moderate"),
        (Self::MANAGE_GAMES, "manage_games"),
        (Self::READ_MEMBERS, "read_members"),
        (Self::SEND_ANNOUNCEMENTS, "send_announcements"),
    ];

    /// Builds from a raw bitmask.
    ///
    /// Bits above the defined ones are dropped rather than kept, so a value from a newer
    /// build — or a hostile one written straight to the column — cannot grant a permission
    /// this build has never heard of.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// The raw bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The bitmask as the `i64` the store column holds.
    #[must_use]
    pub fn to_i64(self) -> i64 {
        i64::from(self.0)
    }

    /// The scopes for a stored integer.
    ///
    /// A negative or out-of-range value is a corrupt or hostile row and decodes to
    /// [`Scopes::NONE`] — the safe direction, granting nothing — rather than an error, so a
    /// single bad row cannot fail every authentication that reads it.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        u32::try_from(value).map_or(Self::NONE, Self::from_bits)
    }

    /// Whether every scope in `other` is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether there are no scopes at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The slug for a single scope bit, or `None` if `self` is not exactly one named scope.
    #[must_use]
    pub fn slug(self) -> Option<&'static str> {
        Self::NAMED
            .iter()
            .find_map(|&(scope, slug)| (scope == self).then_some(slug))
    }

    /// The scope a slug names, or `None` if the slug is not one of the six.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::NAMED
            .iter()
            .find_map(|&(scope, name)| (name == slug).then_some(scope))
    }

    /// The slugs of the scopes that are set, in bit order.
    ///
    /// What a management view lists, and what the API serialises. Only granted scopes appear,
    /// so the length is the count of permissions the bot actually holds.
    #[must_use]
    pub fn slugs(self) -> Vec<&'static str> {
        Self::NAMED
            .iter()
            .filter(|&&(scope, _)| self.contains(scope))
            .map(|&(_, slug)| slug)
            .collect()
    }
}

/// The request to register a new bot.
///
/// The service validates every field the way a human registration would — the username
/// through [`migo_auth::credential::username`], the display name and webhook URL locally —
/// before a single row is written.
#[derive(Clone, Debug)]
pub struct NewBotSpec {
    /// The backing account's handle, validated exactly as a person's would be.
    pub username: String,
    /// The bot's display name, shown beside its messages.
    pub display_name: String,
    /// The permissions to grant at creation. Pass [`Scopes::NONE`] for the section 41
    /// minimum; the owner can widen them later.
    pub scopes: Scopes,
    /// Where the deployment should POST updates for this bot, if the owner wants a webhook.
    /// Must be `https`.
    pub webhook_url: Option<String>,
    /// BCP-47 locale for the backing account. `None` takes the deployment default.
    pub locale: Option<String>,
}

/// A bot as its owner is allowed to see it.
///
/// Everything an owner needs to manage the bot, and nothing that would compromise it: the
/// token is absent because only its keyed tag is stored and even that never leaves the store
/// layer. `disabled` is the derived convenience; `disabled_at` is when, for a UI that shows
/// it.
#[derive(Clone, Debug)]
pub struct BotView {
    /// The bot row's id.
    pub bot_id: Id,
    /// The human who owns it.
    pub owner_id: Id,
    /// The account the bot posts under.
    pub account_id: Id,
    /// The bot's display name.
    pub name: String,
    /// The permissions it currently holds.
    pub scopes: Scopes,
    /// The owner's webhook endpoint, if set.
    pub webhook_url: Option<String>,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was disabled, if it is.
    pub disabled_at: Option<Timestamp>,
    /// Whether it is currently disabled — the derived form of `disabled_at.is_some()`.
    pub disabled: bool,
}

/// The result of registering a bot: the view, and the token shown exactly once.
///
/// The token is a [`Secret`]; it is returned here and never again, because the store keeps
/// only its keyed tag. An owner who loses it rotates rather than recovers it.
#[derive(Clone, Debug)]
pub struct Registered {
    /// The bot that was created.
    pub bot: BotView,
    /// Its bearer token, in plain text, for the one and only time it is available.
    pub token: Secret,
}

/// What a verified bot token resolves to.
///
/// Handed back by [`crate::traits::Bots::authenticate`] to the gateway, which builds the
/// bot's request identity from it: the account it speaks as, priced at
/// [`TrustTier::Bot`], scoped by [`Self::scopes`]. It
/// carries no token and no hash — authentication is complete by the time this exists.
#[derive(Clone, Debug)]
pub struct BotIdentity {
    /// The bot row's id.
    pub bot_id: Id,
    /// The account the bot speaks as.
    pub account_id: Id,
    /// The human who owns it.
    pub owner_id: Id,
    /// The bot's display name.
    pub name: String,
    /// The permissions the gateway must check each action against.
    pub scopes: Scopes,
}

impl BotIdentity {
    /// Whether the bot holds every scope in `wanted`.
    ///
    /// The gateway calls this before dispatching an action; a `false` is the point at which a
    /// `BOT_PERMISSION_MISSING` is returned. Convenience over [`Scopes::contains`] so a call
    /// site reads as `identity.may(Scopes::SEND_MESSAGES)`.
    #[must_use]
    pub fn may(&self, wanted: Scopes) -> bool {
        self.scopes.contains(wanted)
    }
}

/// Deployment-tunable knobs for the bot subsystem.
///
/// Kept in code, not the store: these are policy, not per-bot state. Cloned into the service
/// at construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotsConfig {
    /// The most bots one owner may register.
    pub max_bots_per_owner: u16,
    /// The locale a bot's backing account takes when its owner names none.
    pub default_locale: String,
}

impl Default for BotsConfig {
    fn default() -> Self {
        Self {
            max_bots_per_owner: DEFAULT_MAX_BOTS_PER_OWNER,
            default_locale: "en".to_string(),
        }
    }
}
