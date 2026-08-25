//! What a notification is, and what a push is allowed to be.
//!
//! The important type in this module is [`Wakeup`], and the important thing about it
//! is what it cannot hold. Everything else here is bookkeeping around it.

use migo_core::{Id, Timestamp};
use migo_protocol::{NotificationKind, Platform};
use migo_ratelimit::TrustTier;

/// How long one device's wake-up for one kind stays suppressed, in milliseconds.
///
/// Brief section 44: *"Jangan mengirim push notification untuk setiap event kecil."*
/// Thirty seconds is the window, and the choice is about what a person can act on
/// rather than about load. Two gifts ten seconds apart are one trip to the app, so
/// the second buzz buys nothing and costs the same as the first.
///
/// Per kind, not per account: a missed call arriving inside a gift's window must
/// still ring through, because coalescing across kinds would mean the quiet ones
/// silence the urgent ones.
pub const COALESCE_WINDOW_MS: u32 = 30_000;

/// How long a push registration is believed without a refresh, in milliseconds.
///
/// Sixty days. Both providers expire tokens without announcing it, and a send to a
/// dead token is charged, counted against the deployment's quota, and useless. A
/// client that is still installed refreshes on every foreground, so a registration
/// this old belongs to an app that was deleted.
pub const REGISTRATION_TTL_MS: i64 = 60 * 24 * 60 * 60 * 1000;

/// Largest inbox page.
pub const MAX_INBOX_PAGE: u16 = 50;

/// A wake-up, and nothing more.
///
/// # Why there is no text in here
///
/// Brief section 44 is explicit: *"Payload push TIDAK BOLEH memuat plaintext pesan,
/// plaintext audio voice note, atau isi signaling."* Section 77 adds *"Push payload
/// harus minimum"*, *"Push tidak berisi plaintext message"*, and *"Gunakan generic
/// notification: 'New message'"*.
///
/// A comment saying so would be a rule. This is the enforcement: every field is a
/// [`NotificationKind`], an [`Id`], a [`Timestamp`], or a `u32`, and not one of those
/// can hold a sentence. There is no `title`, no `body`, no `String`, and no byte
/// vector. A future author who wants to put the message preview in the push has to
/// change the type first, in a diff a reviewer will see, rather than filling in a
/// field that was waiting for it.
///
/// The words a phone displays come from [`Wakeup::alert`], which returns a
/// `&'static str` chosen by the kind — so even the visible text cannot be assembled
/// out of data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wakeup {
    /// What happened. The only thing the phone is told about content.
    pub kind: NotificationKind,
    /// The room this concerns, where the client needs it to route a tap.
    pub room_id: Option<Id>,
    /// What to fetch once awake: a conversation, a call, a game session.
    ///
    /// Which of those depends on [`Wakeup::kind`]. For an incoming call this is the
    /// call id and it is the whole payload — brief section 44: *"push memuat `call_id`
    /// dan penanda call saja, cukup untuk membangunkan aplikasi dan menampilkan UI
    /// incoming call. Tidak ada SDP, ICE candidate, atau isi signaling di dalam
    /// push."* An [`Id`] cannot hold an SDP offer, which is the point.
    pub subject_id: Option<Id>,
    /// The recipient's total unread count, for the icon badge.
    ///
    /// A number and not a list. A badge is the one piece of state a phone can render
    /// without unlocking, so it is the one piece worth carrying in a payload that
    /// arrives on a lock screen.
    pub badge: u32,
    /// When the thing that caused this happened.
    pub at: Timestamp,
}

impl Wakeup {
    /// The generic sentence for this kind.
    ///
    /// `&'static str`, deliberately. A function returning `String` here would compile
    /// with `format!("{sender}: {preview}")` inside it, and then the rule at the top
    /// of this type would be a rule again instead of a fact. These strings are in
    /// English because they are the fallback a client uses when it cannot render its
    /// own; a client that can localise ignores them and renders from
    /// [`Wakeup::kind`].
    #[must_use]
    pub const fn alert(&self) -> &'static str {
        match self.kind {
            NotificationKind::Message => "New message",
            NotificationKind::VoiceNote => "New voice message",
            NotificationKind::Mention => "You were mentioned",
            NotificationKind::Reply => "New reply",
            NotificationKind::IncomingCall => "Incoming call",
            NotificationKind::MissedCall => "Missed call",
            NotificationKind::FriendRequest => "New friend request",
            NotificationKind::Gift => "You received a gift",
            NotificationKind::LevelUp => "You levelled up",
            NotificationKind::Achievement => "Achievement unlocked",
            NotificationKind::RoomInvite => "Room invitation",
            NotificationKind::RoomAnnouncement => "Room announcement",
            NotificationKind::Event => "Upcoming event",
            NotificationKind::GameChallenge => "Game challenge",
            NotificationKind::Unknown => "Migo",
        }
    }

    /// Whether this wake-up must ring through a coalescing window.
    ///
    /// A ringing call has a few seconds of usefulness and then becomes a missed call.
    /// Holding one back to be polite about battery is holding back the only
    /// notification in the product with a deadline.
    #[must_use]
    pub const fn is_urgent(&self) -> bool {
        matches!(
            self.kind,
            NotificationKind::IncomingCall | NotificationKind::MissedCall
        )
    }
}

/// Something that happened, which somebody has to be told about.
///
/// The input to [`crate::traits::Notifier::notify`]. Whether it becomes an inbox row,
/// a push, both, or neither is decided inside the service — a caller in `migo-economy`
/// announcing a gift should not have to know that a gift is storable and a mention is
/// not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// Who has to be told.
    pub account_id: Id,
    /// What happened.
    pub kind: NotificationKind,
    /// Who caused it. `None` when nobody did, as for a level up.
    ///
    /// When this equals [`Event::account_id`] the event is dropped: the one person who
    /// certainly does not need telling is the one who did it.
    pub actor_id: Option<Id>,
    /// The room this happened in, where it happened in one.
    pub room_id: Option<Id>,
    /// What it points at, for the client to fetch once awake.
    pub subject_id: Option<Id>,
    /// Server time.
    pub at: Timestamp,
}

impl Event {
    /// An event with no actor, no room, and no subject.
    #[must_use]
    pub const fn new(account_id: Id, kind: NotificationKind, at: Timestamp) -> Self {
        Self {
            account_id,
            kind,
            actor_id: None,
            room_id: None,
            subject_id: None,
            at,
        }
    }

    /// Sets who caused it.
    #[must_use]
    pub const fn by(mut self, actor_id: Id) -> Self {
        self.actor_id = Some(actor_id);
        self
    }

    /// Sets the room.
    #[must_use]
    pub const fn in_room(mut self, room_id: Id) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Sets what to fetch.
    #[must_use]
    pub const fn about(mut self, subject_id: Id) -> Self {
        self.subject_id = Some(subject_id);
        self
    }

    /// Whether the recipient caused this themselves.
    #[must_use]
    pub fn is_self_inflicted(&self) -> bool {
        self.actor_id == Some(self.account_id)
    }
}

/// Why a device was not woken.
///
/// Every value here is a decision, not a failure. A push that was withheld because
/// the app is already on screen is the system working; a push that was withheld
/// because the provider rejected the token is a [`Failure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    /// The device has a live socket, so the event arrived on it already.
    Connected,
    /// A wake-up of this kind went to this device inside
    /// [`COALESCE_WINDOW_MS`].
    Coalesced,
    /// The device's wake-up budget is spent.
    ///
    /// Not an error to anybody. The event is still stored and still counted in the
    /// badge; what was dropped is the buzz. A budget that answered `RATE_LIMITED` to
    /// the sender would be punishing the recipient for being popular.
    Budget,
    /// The registration was last refreshed longer ago than
    /// [`REGISTRATION_TTL_MS`].
    Stale,
}

impl Withheld {
    /// Label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Coalesced => "coalesced",
            Self::Budget => "budget",
            Self::Stale => "stale",
        }
    }
}

/// Why a device could not be woken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The provider says this token no longer exists. The registration is retired.
    Unregistered,
    /// The provider is refusing traffic. Nothing is retired; the token is fine.
    Throttled,
    /// The provider, the network, or the sealed token failed. Nothing is retired.
    Error,
}

impl Failure {
    /// Label for a metric series.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::Throttled => "throttled",
            Self::Error => "error",
        }
    }
}

/// What one [`Event`] actually did.
///
/// Returned rather than logged, because the caller is usually a domain crate that
/// wants to know whether the person was reachable — and because the numbers that
/// belong in a log line are exactly these, none of which name anybody.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delivery {
    /// Whether an inbox row was written.
    pub stored: bool,
    /// How many devices were woken.
    pub woken: usize,
    /// How many were deliberately not woken.
    pub withheld: usize,
    /// How many could not be woken.
    pub failed: usize,
}

impl Delivery {
    /// Whether the event reached at least one device, by push or by socket.
    ///
    /// [`Withheld::Connected`] counts as reached, which is why this is not
    /// `woken > 0`: the most common reason not to push is that the person is looking
    /// at the app.
    #[must_use]
    pub const fn reached(&self) -> bool {
        self.woken > 0 || self.withheld > 0
    }
}

/// One page of somebody's inbox.
#[derive(Clone, Debug, Default)]
pub struct Inbox {
    /// Newest first.
    pub items: Vec<Item>,
    /// Unread count across the whole inbox, not just this page.
    pub unread: u32,
}

/// One inbox entry, as a client reads it.
///
/// A projection of the stored row with the kind decoded and nothing added. In
/// particular no title and no body: the sentence is the client's to write, in the
/// reader's language, from [`Item::kind`] and whatever it fetches with
/// [`Item::subject_id`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// Primary key, for the acknowledgement.
    pub notification_id: Id,
    /// What happened.
    pub kind: NotificationKind,
    /// Where, if anywhere.
    pub room_id: Option<Id>,
    /// Who, if anybody.
    pub actor_id: Option<Id>,
    /// What to fetch.
    pub subject_id: Option<Id>,
    /// When it happened.
    pub at: Timestamp,
    /// Whether it has been seen.
    pub read: bool,
}

/// Who is asking.
///
/// No `reauthenticated` flag: reading one's own notifications is not a step-up
/// operation, and registering a push token is something a client does on every cold
/// start, when there is no human present to re-prove anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on. A registration belongs to a device, so
    /// every write here needs it.
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

    /// Attaches a correlation id.
    #[must_use]
    pub fn with_request(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// A raw push token, on its way in.
///
/// Exists for one reason: to make the boundary visible. A token arrives inside this
/// type, is sealed and hashed by [`Notifier::register`](crate::traits::Notifier::register), and the
/// type is dropped. It never reaches `migo-store`, never reaches a metric, and its
/// `Debug` shows nothing.
///
/// Brief section 174 lists *"push token dalam bentuk mentah"* among the things that
/// must never appear in a log, without exception.
#[derive(Clone, PartialEq, Eq)]
pub struct RawToken {
    token: String,
    provider: i16,
    platform: Platform,
}

impl RawToken {
    /// Wraps a token the client just supplied.
    #[must_use]
    pub fn new(token: impl Into<String>, provider: i16, platform: Platform) -> Self {
        Self {
            token: token.into(),
            provider,
            platform,
        }
    }

    /// Reveals the token. Audit every call site; there should be exactly two.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.token
    }

    /// Which push service.
    #[must_use]
    pub const fn provider(&self) -> i16 {
        self.provider
    }

    /// Which platform the client says it is.
    #[must_use]
    pub const fn platform(&self) -> Platform {
        self.platform
    }

    /// Length in bytes, safe to log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.token.len()
    }

    /// Whether the client sent nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.token.trim().is_empty()
    }
}

impl core::fmt::Debug for RawToken {
    /// Prints the length and the provider, never the token.
    ///
    /// Not `"RawToken(…)"`: the length is what somebody debugging a rejected
    /// registration actually needs, and it discloses nothing.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "RawToken({} bytes, provider {})",
            self.token.len(),
            self.provider
        )
    }
}

/// Longest push token accepted.
///
/// FCM registration tokens run to roughly 200 characters and APNs device tokens to
/// 160; 512 is generous enough that no legitimate client is refused and small enough
/// that a client cannot use this column as free storage.
pub const MAX_TOKEN_LEN: usize = 512;

/// How the service behaves, per deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyConfig {
    /// Whether wake-ups are sent at all.
    ///
    /// Off in the deterministic simulator and in tests that do not care, because a
    /// [`crate::traits::PushSender`] returning `Delivered` for everything is still an
    /// await per device per event.
    pub push_enabled: bool,
    /// Coalescing window, in milliseconds.
    pub coalesce_window_ms: u32,
    /// How long a registration is believed without a refresh.
    pub registration_ttl_ms: i64,
    /// Whether a device with a live socket is skipped.
    ///
    /// On, always, in production. The switch exists so that a test can prove the
    /// skip happens by turning it off and watching the count change.
    pub skip_connected: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            push_enabled: true,
            coalesce_window_ms: COALESCE_WINDOW_MS,
            registration_ttl_ms: REGISTRATION_TTL_MS,
            skip_connected: true,
        }
    }
}
