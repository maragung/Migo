//! What the interface draws from.
//!
//! The rule this module exists to enforce: the UI thread never touches a socket, a database or a
//! ratchet. It reads these plain structs, and they only ever change when [`crate::app`] drains an
//! event from the network worker. That is what makes the render path unable to block — there is no
//! lock to contend for and no future to await inside `update()`, so a slow server cannot freeze the
//! window, which is the failure every hand-rolled desktop client eventually ships.
//!
//! Everything here is already decrypted. Ciphertext stops at the worker; a `Message` holds text.

use std::collections::HashMap;

use migo_core::{Id, Timestamp};

/// Where the gateway connection is, as far as the interface needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// No socket, and none wanted — the user has not signed in.
    Offline,
    /// A socket is being opened, or the handshake is in flight.
    Connecting,
    /// Handshake complete and the session is authenticated.
    Online,
    /// The socket dropped or the handshake failed. Carries a message fit for a human.
    Failed(String),
}

impl Connection {
    /// A short label for the status pill.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Offline => "Offline",
            Self::Connecting => "Connecting",
            Self::Online => "Encrypted",
            Self::Failed(_) => "Disconnected",
        }
    }

    #[must_use]
    pub fn is_online(&self) -> bool {
        matches!(self, Self::Online)
    }
}

/// How a message we sent is getting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Sealed and handed to the worker; the server has not acknowledged it.
    Sending,
    /// The server accepted it and assigned a sequence number.
    Sent,
    /// The server refused it, or the socket dropped before it was accepted.
    Failed,
    /// It arrived from the server — someone else wrote it.
    Received,
}

/// What a message turned out to be once it was opened.
#[derive(Debug, Clone)]
pub enum Body {
    /// Text, the only body this client composes.
    Text(String),
    /// A reference to encrypted media. Rendered as a chip; downloading is not wired up yet, and
    /// pretending otherwise with a dead button would be worse than saying so.
    Media { mime_type: String, size_bytes: u64 },
    /// A voice note reference, likewise a chip.
    VoiceNote { duration_ms: u32 },
    /// An emoji reaction to another message.
    Reaction { emoji: String, target: Id },
    /// A body this build understands the envelope of but not the content type — a newer peer.
    Unsupported { content_type: u8 },
    /// The envelope opened but the plaintext did not parse, or it never opened at all. The string
    /// is a short reason, never key material and never a partial plaintext.
    Undecryptable(String),
}

impl Body {
    /// A one-line rendering for the conversation list preview.
    #[must_use]
    pub fn preview(&self) -> String {
        match self {
            Self::Text(text) => text.lines().next().unwrap_or_default().to_string(),
            Self::Media { mime_type, .. } => format!("Attachment ({mime_type})"),
            Self::VoiceNote { duration_ms } => {
                format!(
                    "Voice note ({}s)",
                    (*duration_ms as f32 / 1000.0).round() as u32
                )
            }
            Self::Reaction { emoji, .. } => format!("Reacted {emoji}"),
            Self::Unsupported { .. } => "Unsupported message".to_string(),
            Self::Undecryptable(_) => "Could not be decrypted".to_string(),
        }
    }
}

/// One message in a thread.
#[derive(Debug, Clone)]
pub struct Message {
    pub message_id: Id,
    pub conversation_id: Id,
    /// Server-assigned position in the conversation. `0` until the send is acknowledged.
    pub seq: u64,
    pub sender_id: Id,
    /// True when this device's account wrote it. Decides which side of the thread it lands on.
    pub outgoing: bool,
    pub body: Body,
    pub sent_at: Timestamp,
    pub delivery: Delivery,
}

/// One row in the conversation list.
#[derive(Debug, Clone)]
pub struct Conversation {
    pub conversation_id: Id,
    /// The title the server carries for groups and rooms; `None` for a direct chat, where the
    /// title is the peer's name and comes from a profile lookup instead.
    pub title: Option<String>,
    /// Everyone in it, when the server disclosed the membership.
    pub members: Vec<Id>,
    /// Whether this conversation is end-to-end encrypted. A room is not, and the interface must
    /// not imply it is (brief section 178).
    pub encrypted: bool,
    /// The highest sequence number the server has for it.
    pub last_seq: u64,
    /// The last message, for the preview line.
    pub preview: Option<String>,
    /// When the last message landed, for the row's timestamp.
    pub updated_at: Option<Timestamp>,
    /// How many messages arrived that the user has not looked at.
    pub unread: u32,
}

impl Conversation {
    /// The name to draw, given who we are and what we know about the other members.
    #[must_use]
    pub fn display_title(&self, me: Id, names: &HashMap<Id, String>) -> String {
        if let Some(title) = &self.title {
            if !title.is_empty() {
                return title.clone();
            }
        }
        let peers: Vec<&String> = self
            .members
            .iter()
            .filter(|id| **id != me)
            .filter_map(|id| names.get(id))
            .collect();
        match peers.len() {
            0 => short_id(self.conversation_id),
            1 => peers[0].clone(),
            _ => peers
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// Who is signed in on this device.
#[derive(Debug, Clone)]
pub struct Account {
    pub account_id: Id,
    pub device_id: Id,
    pub session_id: Id,
    pub username: String,
    /// The safety number for this device's identity key: the fingerprint a user reads aloud to a
    /// contact to confirm nobody is in the middle. Grouped for reading, never for parsing.
    pub safety_number: String,
}

/// One edge in the signed-in account's social graph, as the interface draws it.
///
/// The worker folds the server's relationship listing into these; the wire's `u32` kind has
/// already been through [`RelationshipKind::from_wire`], so a kind this build has no name for
/// arrives as [`RelationshipKind::Unknown`] rather than as a number the UI has to defend against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    pub user_id: Id,
    pub kind: RelationshipKind,
}

/// The kinds of edge a social graph can hold.
///
/// A client-owned mirror of the wire enum rather than the wire enum itself, so that adding a
/// variant server-side is a `Unknown` here to be filed or ignored — not a decode failure that
/// would take the whole friends list down with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipKind {
    Unknown,
    Friend,
    PendingOutgoing,
    PendingIncoming,
    Follow,
    Block,
    Favorite,
}

impl RelationshipKind {
    /// Maps the wire's `u32` onto a kind. Unknown values collapse to [`Self::Unknown`], which the
    /// friends panel files under "everything else" instead of rendering a number.
    #[must_use]
    pub const fn from_wire(kind: u32) -> Self {
        match kind {
            1 => Self::Friend,
            2 => Self::PendingOutgoing,
            3 => Self::PendingIncoming,
            4 => Self::Follow,
            5 => Self::Block,
            6 => Self::Favorite,
            _ => Self::Unknown,
        }
    }
}

/// Where an account stands, as far as this device has been told.
///
/// Seeded from profile fetches and corrected by presence events; `Unknown` means "never heard",
/// which the interface draws as absence of a dot rather than as "offline" — claiming someone is
/// offline when the truth is unobserved is exactly the mistake a presence UI must not make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presence {
    Unknown,
    Offline,
    Online,
    Away,
    Busy,
}

impl Presence {
    /// Maps the wire's `u32` presence state onto a presence.
    #[must_use]
    pub const fn from_wire(state: u32) -> Self {
        match state {
            1 => Self::Offline,
            2 => Self::Online,
            3 => Self::Away,
            4 => Self::Busy,
            _ => Self::Unknown,
        }
    }

    /// The word drawn beside the dot. Empty for `Unknown`, so an unobserved account shows
    /// nothing rather than a lie.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Offline => "Offline",
            Self::Online => "Online",
            Self::Away => "Away",
            Self::Busy => "Busy",
        }
    }

    /// Whether this state earns the green dot. Only `Online` does: away and busy are real
    /// presences that are not "available now".
    #[must_use]
    pub const fn is_online(self) -> bool {
        matches!(self, Self::Online)
    }
}

/// One row of the device/session list on the settings screen.
///
/// Reduced by the worker from the REST answer, so the panel never sees JSON field names or
/// missing-field shapes — only present facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub session_id: Id,
    /// What the device called itself when it signed in, e.g. "Migo Desktop (Linux)". Falls back
    /// to a short id when the server's row carries nothing readable.
    pub device: String,
    /// When the session opened, if the server said.
    pub created_at: Option<Timestamp>,
    /// When the session was last seen active, if the server said.
    pub last_active_at: Option<Timestamp>,
    /// Whether this row is the session this window is running on. Revoking it is sign-out by
    /// another name, so the panel refuses the button rather than offering it.
    pub current: bool,
}

/// One room in the public directory, reduced to what a join decision needs.
///
/// The wire's [`migo_protocol::RoomSummary`] carries more (public id, kind, language, country);
/// this is the projection a row draws, taken once where the wire answer is reduced so no UI code
/// touches protocol types.
#[derive(Debug, Clone)]
pub struct RoomRow {
    pub room_id: Id,
    pub name: String,
    pub topic: Option<String>,
    pub member_count: u32,
    pub online_count: u32,
    pub category: Option<String>,
    pub verified: bool,
}

/// One row of the durable notification inbox.
#[derive(Debug, Clone)]
pub struct AlertRow {
    pub id: Id,
    /// The wire's snake_case kind word, as-is: a closed server vocabulary.
    pub kind: String,
    pub title: Option<String>,
    pub at: Timestamp,
}

/// One line of the wallet's statement.
///
/// `credit` is the *reason's* direction, never a sign read off the amount: the wire's amount is a
/// magnitude, and a regression that guessed the direction would show money moving the wrong way.
#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub reason: String,
    pub amount: u64,
    pub credit: bool,
    pub balance_after: u64,
    pub at: Timestamp,
}

/// One standing on the XP leaderboard.
#[derive(Debug, Clone)]
pub struct LeaderRow {
    pub position: u32,
    pub account_id: Id,
    pub xp: u64,
    pub level: u32,
}

/// One listing in the gift shop.
#[derive(Debug, Clone)]
pub struct GiftRow {
    pub sku: String,
    pub name: String,
    pub price: u64,
    pub category: String,
}

/// One account found by search or offered as a suggestion.
#[derive(Debug, Clone)]
pub struct PersonRow {
    pub account_id: Id,
    pub username: String,
    pub display_name: String,
    pub mutual_friends: u32,
}

/// The account's XP progression, for the wallet's level card.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progression {
    pub level: u32,
    pub xp_into_level: u64,
    pub xp_for_next_level: u64,
}

impl Progression {
    /// The XP bar's filled fraction, clamped into 0..=1.
    ///
    /// A total of zero (or a negative a hostile node sent) renders an empty bar rather than `NaN`
    /// or `Infinity` — an unfilled bar is honest, a broken one is not.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.xp_for_next_level == 0 {
            return 0.0;
        }
        (self.xp_into_level as f32 / self.xp_for_next_level as f32).clamp(0.0, 1.0)
    }
}

/// One row of the Space activity stream.
///
/// A synthesis, not a wire type: a notification and a ledger line can describe the same gift, and
/// the merge happens once, in the app, where the two sources meet.
#[derive(Debug, Clone)]
pub struct ActivityRow {
    pub key: String,
    pub category: ActivityCategory,
    pub title: String,
    pub at: Timestamp,
}

/// The activity stream's categories, each a filter over the merged rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCategory {
    Social,
    Rooms,
    Games,
    Economy,
}

impl ActivityCategory {
    /// The filter's own word.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Social => "Social",
            Self::Rooms => "Rooms",
            Self::Games => "Games",
            Self::Economy => "Economy",
        }
    }
}

/// The closed reason-to-direction mapping for the ledger, identical to the web client's.
///
/// The spends debit, the receipts credit; an operator adjustment or an unknown word from a newer
/// node renders unsigned rather than guessing a direction for money.
#[must_use]
pub fn ledger_credit(reason: &str) -> bool {
    matches!(
        reason,
        "grant" | "gift_reputation" | "refund" | "game_payout"
    )
}

/// A snake_case wire word as readable words (`friend_request` → `Friend request`).
#[must_use]
pub fn spaced_words(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 4);
    for (index, part) in word.split('_').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(part);
    }
    let mut chars = out.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => out,
    }
}

/// A transient message shown in the corner: a send failure, a rate limit, a bad password.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    /// Frames-independent lifetime. Counted down in seconds by the app each repaint.
    pub remaining: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl Toast {
    /// Long enough to read a sentence, short enough not to sit in the way.
    pub const LIFETIME: f32 = 5.0;

    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Info,
            remaining: Self::LIFETIME,
        }
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Success,
            remaining: Self::LIFETIME,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Error,
            remaining: Self::LIFETIME,
        }
    }
}

/// The last eight characters of an id, for when there is nothing better to show. Enough to tell two
/// conversations apart in a debug view without putting a full identifier on screen.
#[must_use]
pub fn short_id(id: Id) -> String {
    let text = id.to_text();
    let start = text.len().saturating_sub(8);
    text[start..].to_string()
}

/// Groups a 32-byte fingerprint into a readable safety number: five-digit blocks, twelve of them.
///
/// The same presentation on every platform, because a user comparing this client's number against a
/// phone's has to be able to read them as the same thing.
#[must_use]
pub fn safety_number(fingerprint: &[u8; 32]) -> String {
    let mut digits = String::with_capacity(64);
    for chunk in fingerprint.chunks(4) {
        let mut value = 0u32;
        for byte in chunk {
            value = (value << 8) | u32::from(*byte);
        }
        // Five decimal digits per four bytes: the whole 32 bytes become sixty digits, matching the
        // grouping other clients use.
        digits.push_str(&format!("{:05}", value % 100_000));
    }
    digits
        .as_bytes()
        .chunks(5)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A local wall-clock rendering of a timestamp, `HH:MM`.
///
/// Deliberately not localised. Pulling a locale database in for a clock is a lot of bytes, and the
/// 24-hour form is unambiguous everywhere, which matters more here than matching desktop
/// conventions exactly.
#[must_use]
pub fn clock(ts: Timestamp) -> String {
    let seconds = ts.as_unix_ms().div_euclid(1000);
    let day_seconds = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}", day_seconds / 3600, (day_seconds % 3600) / 60)
}

/// A `YYYY-MM-DD` date for a timestamp.
///
/// Implemented directly because the alternative is a date-time crate for one function. The
/// algorithm is the standard days-from-civil inverse; it is exact for every date this program
/// will ever show. Shared by the thread's day separators and the settings screen's session rows,
/// which is why it lives in the model rather than in either screen.
#[must_use]
pub fn date(ts: Timestamp) -> String {
    let days = ts.as_unix_ms().div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_kind_maps_every_wire_value() {
        assert_eq!(RelationshipKind::from_wire(1), RelationshipKind::Friend);
        assert_eq!(
            RelationshipKind::from_wire(2),
            RelationshipKind::PendingOutgoing
        );
        assert_eq!(
            RelationshipKind::from_wire(3),
            RelationshipKind::PendingIncoming
        );
        assert_eq!(RelationshipKind::from_wire(4), RelationshipKind::Follow);
        assert_eq!(RelationshipKind::from_wire(5), RelationshipKind::Block);
        assert_eq!(RelationshipKind::from_wire(6), RelationshipKind::Favorite);
        // A kind a newer server knows about collapses, never crashes.
        assert_eq!(RelationshipKind::from_wire(99), RelationshipKind::Unknown);
        assert_eq!(RelationshipKind::from_wire(0), RelationshipKind::Unknown);
    }

    #[test]
    fn presence_maps_wire_values_and_labels_itself() {
        assert_eq!(Presence::from_wire(2), Presence::Online);
        assert_eq!(Presence::from_wire(3), Presence::Away);
        assert_eq!(Presence::from_wire(4), Presence::Busy);
        assert_eq!(Presence::from_wire(1), Presence::Offline);
        assert_eq!(Presence::from_wire(0), Presence::Unknown);
        assert_eq!(Presence::from_wire(77), Presence::Unknown);

        assert_eq!(Presence::Online.label(), "Online");
        assert_eq!(Presence::Offline.label(), "Offline");
        // Unobserved is not "offline": no label, no dot.
        assert_eq!(Presence::Unknown.label(), "");
        assert!(Presence::Online.is_online());
        assert!(!Presence::Away.is_online());
        assert!(!Presence::Unknown.is_online());
    }

    #[test]
    fn date_renders_known_days() {
        // 2026-08-30 00:00:00 UTC = 1788048000 s.
        let ts = Timestamp::from_unix_ms(1_788_048_000_000);
        assert_eq!(date(ts), "2026-08-30");
        // The Unix epoch itself.
        assert_eq!(date(Timestamp::from_unix_ms(0)), "1970-01-01");
        // A negative timestamp (pre-1970) must still come out a real date, not a panic.
        assert_eq!(date(Timestamp::from_unix_ms(-86_400_000)), "1969-12-31");
    }
}
