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
