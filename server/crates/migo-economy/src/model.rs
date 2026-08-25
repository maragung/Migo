//! What the economy is made of: gifts, currencies, the things they buy, the reasons a
//! transaction exists, XP sources, badges, and leaderboard windows.
//!
//! # The two rules this module encodes as types
//!
//! Brief section 87: *"Jangan membuat sistem yang menyerupai gambling atau memungkinkan
//! cash-out tanpa regulatory review."* Section 29: *"Jangan memberikan currency
//! berdasarkan message count secara naif karena dapat dieksploitasi spam bots."*
//!
//! The first shows up in [`Gift::Mystery`], whose price is fixed and whose delivered
//! value is fixed: it is a surprise, not a wager. A gift whose payout varied with a roll
//! would be a loot box, and the difference between a surprise and a loot box is exactly
//! whether the expected value is known before you pay. Here it is.
//!
//! The second shows up in [`Source`], the closed list of things that earn XP. There is no
//! `Message` variant, and there cannot be one without editing this enum — which is a diff
//! a reviewer sees, rather than a threshold somebody tunes down to zero and forgets. XP is
//! for activity that costs an author something to fake; a message is not.

use migo_core::{Id, Timestamp};
use migo_ratelimit::TrustTier;
use migo_store::model::Currency;

/// The reason a ledger transaction exists, owning the `reason: i16` column that
/// `migo-store` deliberately left as a raw integer.
///
/// The store holds a number because the list of reasons is the economy's, not storage's:
/// a store that held this enum would be edited every time a domain crate invented a way to
/// move currency. Section 30's note that `NewXpAward::source` is a raw `i16` for the same
/// stated reason applies here word for word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The platform issued currency into a user account. The Mint account funds it.
    Grant = 0,
    /// A gift was bought: the sender's balance to the Fee account.
    GiftPurchase = 1,
    /// The reputation half of a gift: the Mint to the recipient's Points.
    ///
    /// A separate transaction from [`Reason::GiftPurchase`] because a transaction carries
    /// one currency and this one is Points while the purchase was Coins or Gems.
    GiftReputation = 2,
    /// A catalogue item was bought for the buyer's own account.
    Purchase = 3,
    /// A purchase was reversed.
    Refund = 4,
    /// Currency moved into escrow for a game.
    GameStake = 5,
    /// Escrow paid out to a game's winner.
    GamePayout = 6,
    /// A manual correction by an operator, always with an audit row beside it.
    Adjustment = 7,
}

impl Reason {
    /// Numeric form, as stored in `LedgerTransaction::reason`.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::Grant,
            1 => Self::GiftPurchase,
            2 => Self::GiftReputation,
            3 => Self::Purchase,
            4 => Self::Refund,
            5 => Self::GameStake,
            6 => Self::GamePayout,
            7 => Self::Adjustment,
            _ => return None,
        })
    }
}

/// What a gift can be, as a bitset.
///
/// Section 28 lists five adjectives a gift may carry, and they are not exclusive: a gift
/// can be animated and limited and collectible at once. A bitset rather than an enum for
/// that reason, and a `u8` because five flags leave three spare and there will never be a
/// sixty-fourth adjective.
///
/// These are descriptive, not behavioural. Nothing in the ledger branches on whether a
/// gift is `RARE`; the flags are for the client to render a shelf and for the catalogue to
/// describe what it sells. A flag that changed the price would belong in [`Price`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Attributes(u8);

impl Attributes {
    /// Plays a short loop rather than sitting still.
    pub const ANIMATED: Self = Self(1 << 0);
    /// Sold only while a fixed supply lasts.
    pub const LIMITED: Self = Self(1 << 1);
    /// Sold only during a season, then withdrawn.
    pub const SEASONAL: Self = Self(1 << 2);
    /// Uncommon by design, for the shelf to mark.
    pub const RARE: Self = Self(1 << 3);
    /// Counts towards a collection.
    pub const COLLECTIBLE: Self = Self(1 << 4);

    /// No attributes.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// The two combined.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every flag in `other` is set here.
    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw bits, for storage or the wire.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Rebuilds from raw bits, dropping any that are not defined.
    #[must_use]
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & 0b0001_1111)
    }
}

/// The ten gifts of section 28.
///
/// A closed enum rather than pure catalogue data, because these ten are named in the brief
/// and referred to by name elsewhere — a `Dragon` is a specific thing a client draws, not
/// a row an operator invented. The catalogue *prices* them and may add more sellable items
/// on top (that is what [`Sku`] and [`Listing`] are for), but the ten themselves are fixed
/// at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gift {
    /// A rose.
    Rose,
    /// A heart.
    Heart,
    /// A cake.
    Cake,
    /// A star.
    Star,
    /// A diamond.
    Diamond,
    /// A crown.
    Crown,
    /// A rocket.
    Rocket,
    /// A dragon.
    Dragon,
    /// Fire.
    Fire,
    /// A surprise whose contents are fixed, not rolled.
    ///
    /// Section 87 forbids gambling mechanics. The reveal is a presentation choice — the
    /// recipient does not know what is inside until they open it — but what is inside does
    /// not vary, so the expected value equals the price and there is no wager. A Mystery
    /// whose payout depended on a random draw would be a loot box, and this crate has no
    /// randomness in the delivery path to build one with.
    Mystery,
}

impl Gift {
    /// Every gift, in catalogue order.
    pub const ALL: [Self; 10] = [
        Self::Rose,
        Self::Heart,
        Self::Cake,
        Self::Star,
        Self::Diamond,
        Self::Crown,
        Self::Rocket,
        Self::Dragon,
        Self::Fire,
        Self::Mystery,
    ];

    /// The catalogue slug, without the `gift.` prefix.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Rose => "rose",
            Self::Heart => "heart",
            Self::Cake => "cake",
            Self::Star => "star",
            Self::Diamond => "diamond",
            Self::Crown => "crown",
            Self::Rocket => "rocket",
            Self::Dragon => "dragon",
            Self::Fire => "fire",
            Self::Mystery => "mystery",
        }
    }

    /// The full catalogue code, e.g. `gift.dragon`.
    ///
    /// Returned owned because it is what goes into [`GiftOutcome::gift_code`] and the
    /// store's `gift_sent.gift_code` column, both of which want a `String`. The prefix is
    /// [`Category::Gift`]'s, so a gift's code and its SKU are the same string.
    #[must_use]
    pub fn code(self) -> String {
        format!("{}.{}", Category::Gift.slug(), self.slug())
    }

    /// Its position in [`Gift::ALL`], for a metric series index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Parses a slug back to a gift.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.slug() == slug)
    }
}

/// What a unit of currency buys, from section 29.
///
/// The prefix of every [`Sku`]. Seven categories, closed: section 29 lists exactly these
/// spending uses, and a metric labelled by category (which this crate's metrics publish)
/// stays bounded only because this list is. A new kind of purchasable thing is a new
/// variant here and a new metric series, deliberately — the alternative, labelling metrics
/// by SKU, would mint an unbounded series that every seasonal item leaves behind forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// A virtual gift (section 28).
    Gift,
    /// An avatar item.
    AvatarItem,
    /// A sticker.
    Sticker,
    /// A profile frame.
    ProfileFrame,
    /// A theme.
    Theme,
    /// A cosmetic.
    Cosmetic,
    /// Entry to an event.
    EventEntry,
}

impl Category {
    /// Every category, in section 29 order.
    pub const ALL: [Self; 7] = [
        Self::Gift,
        Self::AvatarItem,
        Self::Sticker,
        Self::ProfileFrame,
        Self::Theme,
        Self::Cosmetic,
        Self::EventEntry,
    ];

    /// The SKU prefix.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Gift => "gift",
            Self::AvatarItem => "avatar",
            Self::Sticker => "sticker",
            Self::ProfileFrame => "frame",
            Self::Theme => "theme",
            Self::Cosmetic => "cosmetic",
            Self::EventEntry => "event",
        }
    }

    /// Its position in [`Category::ALL`], for a metric series index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Parses a prefix back to a category.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.slug() == slug)
    }
}

/// Largest a SKU may be, in bytes.
///
/// The store's `entitlement.sku` and `gift_sent.gift_code` columns are bounded, and a code
/// that would not fit is refused here rather than truncated there into a different item.
pub const MAX_SKU_LEN: usize = 64;

/// A catalogue code: a [`Category`] and a slug, e.g. `theme.midnight`.
///
/// Parsed rather than free text so that a code reaching the ledger has already been shown
/// to name a real category with a well-formed slug. The slug alphabet is
/// `[a-z0-9_]` — lowercase because a case-sensitive code is a support ticket waiting for
/// the day two items differ only in capitalisation, and no punctuation because a code
/// travels in URLs, cache keys, and log lines where a dot already means something.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sku {
    category: Category,
    slug: String,
}

impl Sku {
    /// Parses `category.slug`.
    ///
    /// `None` when the prefix is not a known category, the slug is empty or malformed, or
    /// the whole thing is longer than [`MAX_SKU_LEN`]. Deliberately strict: the ledger's
    /// idempotency and the entitlement primary key both key on this string, so two spellings
    /// of the same intent must not exist.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_SKU_LEN {
            return None;
        }
        let (prefix, slug) = text.split_once('.')?;
        let category = Category::from_slug(prefix)?;
        if !Self::slug_ok(slug) {
            return None;
        }
        Some(Self {
            category,
            slug: slug.to_owned(),
        })
    }

    /// Builds a SKU from parts, validating the slug.
    #[must_use]
    pub fn new(category: Category, slug: &str) -> Option<Self> {
        let candidate = format!("{}.{slug}", category.slug());
        if candidate.len() > MAX_SKU_LEN || !Self::slug_ok(slug) {
            return None;
        }
        Some(Self {
            category,
            slug: slug.to_owned(),
        })
    }

    fn slug_ok(slug: &str) -> bool {
        !slug.is_empty()
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    /// Which category this belongs to.
    #[must_use]
    pub const fn category(&self) -> Category {
        self.category
    }

    /// The slug, without the prefix.
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// The full code as stored, e.g. `theme.midnight`.
    #[must_use]
    pub fn code(&self) -> String {
        format!("{}.{}", self.category.slug(), self.slug)
    }
}

/// A price: an amount in one currency.
///
/// One currency per price, matching the ledger's one-currency-per-transaction rule. A
/// thing that costs "100 coins or 5 gems" is two listings, not one price with two numbers,
/// because charging for it is one transaction in one currency and the client chose which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    /// The unit.
    pub currency: Currency,
    /// Minor units. Positive; a free item has no listing rather than a zero price.
    pub amount: i64,
}

impl Price {
    /// A price in coins.
    #[must_use]
    pub const fn coins(amount: i64) -> Self {
        Self {
            currency: Currency::Coins,
            amount,
        }
    }

    /// A price in gems.
    #[must_use]
    pub const fn gems(amount: i64) -> Self {
        Self {
            currency: Currency::Gems,
            amount,
        }
    }
}

/// One thing the catalogue sells.
///
/// `reputation` is the non-transferable [`Currency::Points`] the *recipient* gains when
/// this item is a gift — the mechanism by which a gift confers standing without conferring
/// anything cash-outable. It is zero for a self-purchase like a theme, which has a buyer
/// but no recipient. Section 87: reputation is Points, Points never leave the account, and
/// nothing converts them back to Coins or Gems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// What it is.
    pub sku: Sku,
    /// What it costs.
    pub price: Price,
    /// How it renders.
    pub attributes: Attributes,
    /// Points the recipient gains if this is gifted. Zero for non-gift items.
    pub reputation: i64,
}

/// Which of section 30's activities earned some XP.
///
/// # There is no `Message` variant, on purpose
///
/// Section 29: *"Jangan memberikan currency berdasarkan message count secara naif karena
/// dapat dieksploitasi spam bots."* Section 30 lists the activities that earn XP and a
/// message is not among them. Encoding that as a missing enum variant rather than a policy
/// note means a future author cannot award XP per message without adding a variant here, in
/// a diff that says exactly what it is doing.
///
/// Each source carries a per-source cap, and [`crate::EconomyConfig`] adds a global daily
/// cap across all of them. Both are enforced over a rolling 24-hour window rather than a
/// calendar day: a calendar boundary lets a farmer take the cap at 23:59 and again at
/// 00:01, and a rolling window needs no timezone to be fair to an audience that spans all
/// of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A daily check-in or streak.
    DailyActivity = 0,
    /// An achievement was unlocked.
    Achievement = 1,
    /// Participation in an event.
    Event = 2,
    /// A game result.
    Game = 3,
    /// A community contribution.
    Contribution = 4,
    /// A helpful action, e.g. an accepted answer.
    HelpfulAction = 5,
    /// Time spent participating in a room.
    RoomParticipation = 6,
}

impl Source {
    /// Every source, in section 30 order.
    pub const ALL: [Self; 7] = [
        Self::DailyActivity,
        Self::Achievement,
        Self::Event,
        Self::Game,
        Self::Contribution,
        Self::HelpfulAction,
        Self::RoomParticipation,
    ];

    /// Numeric form, as stored in `NewXpAward::source`.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        self as i16
    }

    /// Parses the stored form.
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        Some(match value {
            0 => Self::DailyActivity,
            1 => Self::Achievement,
            2 => Self::Event,
            3 => Self::Game,
            4 => Self::Contribution,
            5 => Self::HelpfulAction,
            6 => Self::RoomParticipation,
            _ => return None,
        })
    }

    /// Its position in [`Source::ALL`], for a metric series index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The metric label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DailyActivity => "daily_activity",
            Self::Achievement => "achievement",
            Self::Event => "event",
            Self::Game => "game",
            Self::Contribution => "contribution",
            Self::HelpfulAction => "helpful_action",
            Self::RoomParticipation => "room_participation",
        }
    }

    /// The default cap on how much this source may earn in a rolling 24 hours.
    ///
    /// A default, not a law: [`crate::EconomyConfig`] can override any of them for a
    /// deployment. The shape is what matters — a game can earn more per day than a daily
    /// check-in, and a check-in is capped low because it is the easiest to automate.
    #[must_use]
    // Each source's cap is an independent tuning knob; that two of them share a value today is
    // coincidence, not a rule, so the arms stay one-per-source rather than merged by value.
    #[allow(clippy::match_same_arms)]
    pub const fn default_daily_cap(self) -> i64 {
        match self {
            Self::DailyActivity => 100,
            Self::Achievement => 500,
            Self::Event => 1_000,
            Self::Game => 2_000,
            Self::Contribution => 1_000,
            Self::HelpfulAction => 500,
            Self::RoomParticipation => 300,
        }
    }
}

/// The thirteen badges of section 31.
///
/// A closed enum for the same reason [`Gift`] is: these are named in the brief and awarded
/// by name from elsewhere. The code is `badge.snake_case`, matching the store's
/// `BadgeAward::badge_code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Badge {
    /// Among the first accounts.
    EarlyUser,
    /// Long-standing.
    Veteran,
    /// Sends a great deal of chat.
    TopChatter,
    /// Created a room.
    RoomCreator,
    /// Manages a room.
    Manager,
    /// A moderator.
    Moderator,
    /// Leads a community.
    CommunityLeader,
    /// Consistently helpful.
    Helpful,
    /// Sends many gifts.
    GiftMaster,
    /// Wins games.
    GameChampion,
    /// Active across many countries.
    GlobalExplorer,
    /// Identity verified.
    Verified,
    /// Won an event.
    EventWinner,
}

impl Badge {
    /// Every badge, in section 31 order.
    pub const ALL: [Self; 13] = [
        Self::EarlyUser,
        Self::Veteran,
        Self::TopChatter,
        Self::RoomCreator,
        Self::Manager,
        Self::Moderator,
        Self::CommunityLeader,
        Self::Helpful,
        Self::GiftMaster,
        Self::GameChampion,
        Self::GlobalExplorer,
        Self::Verified,
        Self::EventWinner,
    ];

    /// The slug, without the `badge.` prefix.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::EarlyUser => "early_user",
            Self::Veteran => "veteran",
            Self::TopChatter => "top_chatter",
            Self::RoomCreator => "room_creator",
            Self::Manager => "manager",
            Self::Moderator => "moderator",
            Self::CommunityLeader => "community_leader",
            Self::Helpful => "helpful",
            Self::GiftMaster => "gift_master",
            Self::GameChampion => "game_champion",
            Self::GlobalExplorer => "global_explorer",
            Self::Verified => "verified",
            Self::EventWinner => "event_winner",
        }
    }

    /// Its position in [`Badge::ALL`], for a metric series index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The full code, e.g. `badge.veteran`.
    #[must_use]
    pub fn code(self) -> String {
        format!("badge.{}", self.slug())
    }

    /// Parses a code (with or without the `badge.` prefix) back to a badge.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        let slug = code.strip_prefix("badge.").unwrap_or(code);
        Self::ALL.into_iter().find(|b| b.slug() == slug)
    }
}

/// The span a leaderboard ranks over, from section 32.
///
/// Weekly and Monthly are *rolling* — the last seven or thirty days ending now — not
/// calendar weeks and months. The store's [`migo_store::traits::ProgressionStore::leaderboard`]
/// takes a `since` instant precisely so the window's start is the caller's decision; a
/// server that anchored a week to a calendar would be anchoring it to its own timezone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// The last seven days.
    Weekly,
    /// The last thirty days.
    Monthly,
    /// Everything.
    AllTime,
}

impl Window {
    /// The instant this window starts, given now, or `None` for all-time.
    ///
    /// Saturating: a clock close to the epoch cannot underflow into a window that ranks the
    /// future. Thirty days is thirty fixed days, not a calendar month — a month-length that
    /// varied would make two adjacent monthly boards cover different spans and rank
    /// differently for no reason a reader could see.
    #[must_use]
    pub fn since(self, now: Timestamp) -> Option<Timestamp> {
        let days = match self {
            Self::Weekly => 7,
            Self::Monthly => 30,
            Self::AllTime => return None,
        };
        let millis = days * 24 * 60 * 60 * 1000;
        Some(Timestamp::from_millis(now.as_millis().saturating_sub(millis)))
    }
}

/// One row of a leaderboard, as this crate returns it.
///
/// A [`migo_store::model::Standing`] with a rank stapled on. The position is computed at
/// read time from the row's place in the ordered result, so it is always dense and
/// one-based within the page; two pages of the same board therefore number continuously.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rank {
    /// Position on the board, one-based.
    pub position: u32,
    /// Whose standing this is.
    pub account_id: migo_core::Id,
    /// Their total experience over the window.
    pub xp: i64,
    /// Their current level.
    pub level: i32,
}

/// The XP a level requires.
///
/// `100 * (n - 1)^2`: level 1 needs 0, level 2 needs 100, level 3 needs 400, level 11 needs
/// 10 000. A quadratic curve so that each level costs more than the last, which is what
/// makes a high level mean sustained activity rather than a fixed grind. Saturating, so a
/// preposterous level cannot overflow the multiply and wrap to a small requirement.
#[must_use]
pub fn xp_for_level(level: i32) -> i64 {
    let n = i64::from(level.max(1)) - 1;
    n.saturating_mul(n).saturating_mul(100)
}

/// The level a total of XP has reached.
///
/// The inverse of [`xp_for_level`]: `1 + isqrt(xp / 100)`. Negative XP cannot happen — the
/// store refuses a non-positive award — but is clamped to level 1 rather than trusted,
/// because a projection that can be handed a bad input should not be the thing that
/// panics. `isqrt` is exact integer arithmetic, so the level a total maps to and the XP a
/// level requires never disagree at a boundary.
#[must_use]
pub fn level_for_xp(xp: i64) -> i32 {
    let base = xp.max(0) / 100;
    // isqrt is exact; the result fits i32 for any plausible XP total.
    let level = 1 + i64::isqrt(base);
    i32::try_from(level).unwrap_or(i32::MAX)
}

/// Deployment knobs for the economy.
///
/// Every field has a working default, so a development deployment gets a sane economy
/// with no configuration. The caps are the anti-farming controls of sections 29 and 30;
/// the leaderboard fields bound the cost of section 32's boards.
#[derive(Clone, Copy, Debug)]
pub struct EconomyConfig {
    /// Most XP one account may earn across all sources in a rolling 24 hours.
    ///
    /// The backstop above the per-source caps: even an account maxing several sources at
    /// once cannot exceed this. Set below the sum of the per-source caps on purpose, so the
    /// global limit is the one that binds a determined farmer.
    pub daily_xp_cap: i64,
    /// Per-source caps, indexed by [`Source::index`], seeded from
    /// [`Source::default_daily_cap`].
    pub source_daily_caps: [i64; 7],
    /// How long a computed leaderboard page is cached, in milliseconds.
    ///
    /// Short, because a board is a snapshot people expect to move: a minute of staleness is
    /// invisible, and it turns a top-N sort over a large table into one read per minute per
    /// board rather than one per viewer.
    pub leaderboard_ttl_ms: u32,
    /// Largest leaderboard page this service will return.
    pub leaderboard_max: u16,
}

impl Default for EconomyConfig {
    fn default() -> Self {
        let mut source_daily_caps = [0_i64; 7];
        let mut i = 0;
        while i < Source::ALL.len() {
            source_daily_caps[i] = Source::ALL[i].default_daily_cap();
            i += 1;
        }
        Self {
            daily_xp_cap: 3_000,
            source_daily_caps,
            leaderboard_ttl_ms: 60_000,
            leaderboard_max: 100,
        }
    }
}

impl EconomyConfig {
    /// The cap for one source, from the configured table.
    #[must_use]
    pub const fn source_cap(&self, source: Source) -> i64 {
        self.source_daily_caps[source.index()]
    }
}

/// Who is asking, and what the rate limiter needs to know about them.
///
/// The same five fields every caller-facing service in the tree takes. There is no
/// `ip_class` as in `migo-moderation`: nothing in this crate writes an audit row, so there
/// is nothing here that would want a network class, and a field a crate cannot use is a
/// field a caller is invited to fill in wrongly.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The authenticated account.
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

/// An account's three balances at one instant.
///
/// Named fields rather than a map, because the three currencies are fixed and a caller
/// asking for the coins balance should not have to handle the case where the map has no
/// `coins` key. A balance the account has never held reads as zero, which is what it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wallet {
    /// Purchasable soft currency.
    pub coins: i64,
    /// Premium currency.
    pub gems: i64,
    /// Non-transferable reputation.
    pub points: i64,
}

impl Wallet {
    /// The balance in one currency.
    #[must_use]
    pub const fn of(&self, currency: Currency) -> i64 {
        match currency {
            Currency::Coins => self.coins,
            Currency::Gems => self.gems,
            Currency::Points => self.points,
        }
    }

    /// Sets the balance in one currency.
    pub const fn set(&mut self, currency: Currency, amount: i64) {
        match currency {
            Currency::Coins => self.coins = amount,
            Currency::Gems => self.gems = amount,
            Currency::Points => self.points = amount,
        }
    }
}

/// A request to issue currency into an account.
///
/// Server-facing: there is no [`Caller`], because a grant is something the platform does —
/// a daily reward job, an admin correction, a promotional credit — not something an account
/// asks for. The `created_by` is the operator behind an [`Reason::Adjustment`], absent when
/// the system itself is the author. Whoever calls this is responsible for having decided the
/// account may be credited; the ledger records that it was, not whether it should have been.
#[derive(Clone, Debug)]
pub struct Grant {
    /// Who receives the currency.
    pub account_id: Id,
    /// Which unit.
    pub currency: Currency,
    /// How much, in minor units. Must be positive.
    pub amount: i64,
    /// Why, from the reason table. Typically [`Reason::Grant`] or [`Reason::Adjustment`].
    pub reason: Reason,
    /// What this refers to, where something nameable prompted it.
    pub ref_id: Option<Id>,
    /// Retry key, so a job that runs twice credits once.
    pub idempotency_key: String,
    /// The operator behind a manual adjustment, absent for a system grant.
    pub created_by: Option<Id>,
    /// Server time.
    pub at: Timestamp,
}

/// What issuing a grant produced.
#[derive(Clone, Copy, Debug)]
pub struct GrantReceipt {
    /// The transaction that carried it.
    pub tx_id: Id,
    /// Whether this call wrote it, or a repeated idempotency key returned the original.
    pub created: bool,
}

/// A request to award XP, from one of section 30's activities.
///
/// Server-facing for the same reason [`Grant`] is: XP is earned by doing something the
/// server observed — winning a game, completing an event — not by asking. The crate that
/// observed it (`migo-games`, a future events crate) calls this through its own port, which
/// is why awarding does not depend on who is connected right now.
#[derive(Clone, Debug)]
pub struct Award {
    /// Who earned it.
    pub account_id: Id,
    /// Which activity.
    pub source: Source,
    /// How much was earned before caps, in XP. Must be positive.
    pub amount: i64,
    /// The game, event, or room that produced it.
    pub ref_id: Option<Id>,
    /// Retry key, when the caller has something stable to key on. `None` for an award that
    /// cannot be safely replayed.
    pub idempotency_key: Option<String>,
    /// Server time.
    pub at: Timestamp,
}

/// What an XP award did, after caps and level projection.
///
/// Carries both levels so the caller can tell a level-up from an ordinary award without a
/// second read, and both the requested and granted amounts so a client can honestly show
/// "you hit your daily cap" rather than silently dropping the difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AwardOutcome {
    /// What was asked for, before caps.
    pub requested: i64,
    /// What was actually added, after caps. Zero when a cap was already met.
    pub granted: i64,
    /// The XP total before this award.
    pub before: i64,
    /// The XP total after it.
    pub after: i64,
    /// The level before this award.
    pub level_before: i32,
    /// The level after it.
    pub level_after: i32,
    /// Whether a cap reduced or refused the award.
    pub capped: bool,
}

impl AwardOutcome {
    /// Whether this award raised the account's level.
    #[must_use]
    pub const fn leveled_up(&self) -> bool {
        self.level_after > self.level_before
    }
}

/// A request to grant a badge.
#[derive(Clone, Copy, Debug)]
pub struct BadgeGrant {
    /// Who receives it.
    pub account_id: Id,
    /// Which badge.
    pub badge: Badge,
    /// What earned it, if anything nameable did.
    pub ref_id: Option<Id>,
    /// Server time.
    pub at: Timestamp,
}

/// An account's standing, as a reader sees it.
///
/// A [`migo_store::model::Progression`] with the curve made legible: `xp_into_level` and
/// `xp_for_next_level` are what a progress bar needs, computed here from [`xp_for_level`] so
/// that every client draws the same bar rather than each re-deriving the curve and one of
/// them getting it wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressionView {
    /// Whose standing this is.
    pub account_id: Id,
    /// Total experience.
    pub xp: i64,
    /// Current level.
    pub level: i32,
    /// XP earned into the current level, i.e. `xp - xp_for_level(level)`.
    pub xp_into_level: i64,
    /// XP the current level spans, i.e. the gap from this level to the next.
    pub xp_for_next_level: i64,
}

impl ProgressionView {
    /// Builds the view for a total of XP, deriving the level and the bar.
    ///
    /// Trusts `xp` and recomputes `level` from it rather than taking a stored level, because
    /// the store's level is a cache that can lag its XP by one write, and a reader would
    /// rather see a level that matches the number beside it.
    #[must_use]
    pub fn of(account_id: Id, xp: i64) -> Self {
        let level = level_for_xp(xp);
        let floor = xp_for_level(level);
        let ceil = xp_for_level(level.saturating_add(1));
        Self {
            account_id,
            xp,
            level,
            xp_into_level: xp.saturating_sub(floor),
            xp_for_next_level: ceil.saturating_sub(floor),
        }
    }
}

/// One line of an account statement.
///
/// The caller's own movement on one transaction, with the running balance after it. `amount`
/// is signed from this account's point of view — negative when it paid, positive when it was
/// paid — which is the sign a statement shows, not the raw leg order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The transaction.
    pub tx_id: Id,
    /// Why it happened, parsed; `None` if a newer node wrote a reason this build predates.
    pub reason: Option<Reason>,
    /// This account's signed movement.
    pub amount: i64,
    /// The balance after this transaction.
    pub balance_after: i64,
    /// What it referred to.
    pub ref_id: Option<Id>,
    /// When it posted.
    pub at: Timestamp,
}

/// How many of one gift code an account has been given, for the profile shelf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GiftTally {
    /// The gift's catalogue code, e.g. `gift.dragon`.
    pub gift_code: String,
    /// How many have been received.
    pub count: u32,
}

/// A gift to send: which one, to whom, and where to show it.
#[derive(Clone, Debug)]
pub struct SendGift {
    /// Who receives it.
    pub recipient_id: Id,
    /// Which of the ten gifts.
    pub gift: Gift,
    /// The conversation to show it in, if it is being sent inside one.
    pub conversation_id: Option<Id>,
    /// The client's idempotency key, namespaced by the payer before it reaches the ledger so
    /// that two accounts' keys cannot collide.
    pub client_key: String,
}

/// What sending a gift produced.
#[derive(Clone, Debug)]
pub struct GiftOutcome {
    /// The gift row's id.
    pub gift_id: Id,
    /// The gift's catalogue code.
    pub gift_code: String,
    /// What the sender paid.
    pub price: Price,
    /// The reputation the recipient gained, in points.
    pub reputation: i64,
    /// Who received it.
    pub recipient_id: Id,
    /// Whether a repeated idempotency key returned an earlier send rather than charging
    /// again.
    pub duplicate: bool,
}

/// What buying an item for oneself produced.
#[derive(Clone, Debug)]
pub struct PurchaseOutcome {
    /// What was bought.
    pub sku: Sku,
    /// What it cost.
    pub price: Price,
    /// Whether a repeated idempotency key returned an earlier purchase rather than charging
    /// again.
    pub duplicate: bool,
}

/// Which population a leaderboard ranks, owned so the trait method holds no borrow.
///
/// A mirror of [`migo_store::model::Scope`] that owns its country string. The store's `Scope`
/// borrows, which is right for a store call that outlives nothing; a trait method that might
/// await a cache round trip before it ever reaches the store cannot hold that borrow across
/// the await, so the boundary type owns and the service borrows from it at the last moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardScope {
    /// Everybody.
    Global,
    /// One ISO 3166-1 alpha-2 country.
    Country(String),
    /// The current members of one room.
    Room(Id),
}

/// Which leaderboard to read, over what window, and how much of it.
#[derive(Clone, Debug)]
pub struct Board {
    /// Which population.
    pub scope: BoardScope,
    /// Over what span.
    pub window: Window,
    /// How many rows.
    pub limit: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sku_round_trips_through_parse() {
        let sku = Sku::parse("theme.midnight").expect("valid");
        assert_eq!(sku.category(), Category::Theme);
        assert_eq!(sku.slug(), "midnight");
        assert_eq!(sku.code(), "theme.midnight");
    }

    #[test]
    fn sku_rejects_unknown_category_and_bad_slug() {
        assert!(Sku::parse("nonsense.thing").is_none());
        assert!(Sku::parse("theme.Midnight").is_none(), "uppercase");
        assert!(Sku::parse("theme.mid-night").is_none(), "hyphen");
        assert!(Sku::parse("theme.").is_none(), "empty slug");
        assert!(Sku::parse("theme").is_none(), "no dot");
        assert!(Sku::parse(&format!("theme.{}", "x".repeat(64))).is_none(), "too long");
    }

    #[test]
    fn gift_code_is_a_valid_sku_in_the_gift_category() {
        for gift in Gift::ALL {
            let code = gift.code();
            let sku = Sku::parse(&code).expect("gift code parses as a sku");
            assert_eq!(sku.category(), Category::Gift);
            assert_eq!(Gift::from_slug(sku.slug()), Some(gift));
        }
    }

    #[test]
    fn level_curve_and_inverse_agree_at_boundaries() {
        for level in 1..=200 {
            let need = xp_for_level(level);
            assert_eq!(level_for_xp(need), level, "exact threshold is the new level");
            if level > 1 {
                assert_eq!(
                    level_for_xp(need - 1),
                    level - 1,
                    "one short stays at the previous level"
                );
            }
        }
    }

    #[test]
    fn level_for_negative_or_zero_xp_is_one() {
        assert_eq!(level_for_xp(-500), 1);
        assert_eq!(level_for_xp(0), 1);
        assert_eq!(level_for_xp(99), 1);
        assert_eq!(level_for_xp(100), 2);
    }

    #[test]
    fn attributes_combine_and_test() {
        let a = Attributes::ANIMATED.with(Attributes::RARE);
        assert!(a.has(Attributes::ANIMATED));
        assert!(a.has(Attributes::RARE));
        assert!(!a.has(Attributes::LIMITED));
        assert_eq!(Attributes::from_bits_truncate(a.bits()), a);
    }

    #[test]
    fn reason_and_source_round_trip() {
        for r in [
            Reason::Grant,
            Reason::GiftPurchase,
            Reason::GiftReputation,
            Reason::Purchase,
            Reason::Refund,
            Reason::GameStake,
            Reason::GamePayout,
            Reason::Adjustment,
        ] {
            assert_eq!(Reason::from_i16(r.to_i16()), Some(r));
        }
        for s in Source::ALL {
            assert_eq!(Source::from_i16(s.to_i16()), Some(s));
        }
    }

    #[test]
    fn windows_are_rolling_and_ordered() {
        let now = Timestamp::from_millis(1_000 * 24 * 60 * 60 * 1000);
        let week = Window::Weekly.since(now).expect("bounded");
        let month = Window::Monthly.since(now).expect("bounded");
        assert!(month.as_millis() < week.as_millis(), "month reaches back further");
        assert!(week.as_millis() < now.as_millis());
        assert_eq!(Window::AllTime.since(now), None);
    }
}
