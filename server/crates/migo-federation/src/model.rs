//! The types the mesh speaks in: a peer's allow-list state, the request to admit a peer, the
//! identity a handshake resolves to, an event to carry, and the verdict a link's sequence
//! check returns.
//!
//! # Status is an enum here, a raw integer in the store
//!
//! The `node_peer` row keeps `status` as a raw `i16`, opaque to the storage layer exactly as
//! a bot's `scopes` are. What 0, 1 and 2 *mean* — allowed, paused, blocked — is a domain
//! fact, and domain facts live in this crate. [`PeerStatus`] is that mapping, with the
//! conversion to and from the stored integer beside it, and with one deliberate asymmetry:
//! an unrecognised stored value decodes to [`PeerStatus::Blocked`], the safe direction, so a
//! corrupt or hostile row closes a link rather than opening one.

use migo_core::{Id, Timestamp};

/// The lowest federation opcode, inclusive. Federated frames are binary MWP, never JSON
/// (section 169), and their opcodes live in a reserved band so a mesh frame is never
/// mistaken for a client one.
pub const FEDERATION_OPCODE_MIN: i32 = 208;

/// The highest federation opcode, inclusive. See [`FEDERATION_OPCODE_MIN`].
pub const FEDERATION_OPCODE_MAX: i32 = 223;

/// How long a handshake nonce is remembered, in milliseconds.
///
/// Must exceed twice [`MAX_CLOCK_SKEW_MS`](migo_crypto::node::MAX_CLOCK_SKEW_MS): a proof is
/// accepted within `±skew` of now, so the window in which a replay could still pass the
/// clock check spans `2 × skew`, and the nonce memory has to outlast it or a replay slips
/// through the gap between the two defences. This default leaves a comfortable margin.
pub const DEFAULT_NONCE_WINDOW_MS: i64 = 150_000;

/// The first retry's delay after a failed delivery, in milliseconds. Each further failure
/// doubles it, up to [`DEFAULT_BACKOFF_CAP_MS`].
pub const DEFAULT_BACKOFF_BASE_MS: i64 = 1_000;

/// The longest a retry is ever deferred, in milliseconds. A dead region settles into a
/// retry at this cadence rather than a hot loop; one hour by default.
pub const DEFAULT_BACKOFF_CAP_MS: i64 = 3_600_000;

/// How many times a sender should keep retrying before an operator treats an event as
/// dead-lettered. Advisory: the store keeps a failed event forever, and the give-up policy
/// is the drainer's, so this is the number it reads, not a limit this crate enforces.
pub const DEFAULT_MAX_ATTEMPTS: i32 = 12;

/// How many due events a single drain pass reads at once.
pub const DEFAULT_DUE_BATCH: u16 = 128;

/// A peer's place in the allow-list.
///
/// The one gate every handshake passes through: only [`Allowed`](Self::Allowed) federates.
/// [`Paused`](Self::Paused) and [`Blocked`](Self::Blocked) both refuse the handshake, and
/// they differ only in intent an operator reads — a paused peer is expected back, a blocked
/// one is not — because a blocked peer's row survives so it can be re-allowed without a fresh
/// key exchange. The peer cannot tell which state it is in: both answer the same opaque
/// error (section 48).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerStatus {
    /// Federation is permitted. The only state in which a handshake can succeed.
    Allowed,
    /// Temporarily suspended by an operator; the row and key are kept for a later resume.
    Paused,
    /// Refused by an operator; the row is kept so the peer can be re-allowed deliberately.
    Blocked,
}

impl PeerStatus {
    /// Each status paired with its stable slug, for the API layer and operator tooling.
    ///
    /// Never reworded once shipped: like an error symbol, an export and an operator's script
    /// both depend on the exact string.
    pub const NAMED: [(Self, &'static str); 3] = [
        (Self::Allowed, "allowed"),
        (Self::Paused, "paused"),
        (Self::Blocked, "blocked"),
    ];

    /// The raw `i16` the store column holds.
    #[must_use]
    pub const fn to_i16(self) -> i16 {
        match self {
            Self::Allowed => 0,
            Self::Paused => 1,
            Self::Blocked => 2,
        }
    }

    /// The status for a stored integer.
    ///
    /// An unrecognised value decodes to [`Self::Blocked`] — the safe direction, denying
    /// federation — rather than an error, so a single corrupt or hostile row closes one link
    /// instead of failing every handshake that reads the table.
    #[must_use]
    pub const fn from_i16(value: i16) -> Self {
        match value {
            0 => Self::Allowed,
            1 => Self::Paused,
            _ => Self::Blocked,
        }
    }

    /// Whether a handshake from a peer in this state may proceed. True only for
    /// [`Allowed`](Self::Allowed).
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// The stable slug for this status.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
        }
    }

    /// The status a slug names, or `None` if it is not one of the three.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::NAMED
            .iter()
            .find_map(|&(status, name)| (name == slug).then_some(status))
    }
}

/// The request to admit a peer to the allow-list.
///
/// A peer is admitted [`Allowed`](PeerStatus::Allowed); an operator pauses or blocks it
/// afterwards. There is no update-in-place for the identity fields — a peer's key, base URL,
/// or region changing is a remove-and-re-add the operator performs deliberately (section
/// 170), never a silent overwrite.
#[derive(Clone, Debug)]
pub struct NewPeerSpec {
    /// The peer's node id, the handle an operator reads and a handshake announces.
    pub node_id: Id,
    /// The peer's Ed25519 public key, exactly 32 bytes. The identity a signature is checked
    /// against; unique across the mesh.
    pub public_key: Vec<u8>,
    /// Where to reach the peer's mesh listener.
    pub base_url: String,
    /// The peer's region.
    pub region: String,
}

/// A peer as an operator is allowed to see it.
///
/// The raw public key is not here: what an operator verifies out of band is the
/// [`fingerprint`](Self::fingerprint), a short human-comparable digest, not 32 bytes of hex
/// to check by eye. `status` is decoded to the [`PeerStatus`] enum rather than left raw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerView {
    /// The peer's node id.
    pub node_id: Id,
    /// The peer's region.
    pub region: String,
    /// Where the peer's mesh listener is.
    pub base_url: String,
    /// The peer's allow-list state.
    pub status: PeerStatus,
    /// A short, human-comparable digest of the peer's public key, for out-of-band
    /// verification. Empty only if the stored key is corrupt and cannot be parsed.
    pub fingerprint: String,
    /// When the operator admitted the peer.
    pub added_at: Timestamp,
    /// When a handshake from the peer last succeeded, if ever.
    pub last_seen_at: Option<Timestamp>,
}

/// What a completed, verified handshake resolves to.
///
/// Handed back by [`Mesh::authenticate`](crate::traits::Mesh::authenticate) once the peer's
/// proof has verified against the key the allow-list holds for it. It carries no key and no
/// nonce — authentication is complete by the time this exists — only the identity the
/// transport layer builds the link's context from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The peer's node id, now proven rather than merely claimed.
    pub node_id: Id,
    /// The peer's region.
    pub region: String,
    /// Where the peer's mesh listener is.
    pub base_url: String,
}

/// An event to carry to another node.
///
/// The payload is an already-encoded MWP frame body: opaque bytes here, and a private
/// message inside one is a sealed envelope this layer never opens (section 169). The opcode
/// must fall in the federation band, [`FEDERATION_OPCODE_MIN`]`..=`[`FEDERATION_OPCODE_MAX`].
#[derive(Clone, Debug)]
pub struct FederatedEvent {
    /// The node id to deliver to.
    pub target_node: Id,
    /// The federation opcode the payload is framed as.
    pub opcode: i32,
    /// The encoded frame body. Opaque; a sealed envelope stays sealed.
    pub payload: Vec<u8>,
}

/// A queued event as a drainer sees it.
///
/// The delivery-relevant projection of an outbox row: what to send, where, how many attempts
/// have already failed, and the earliest time this attempt was allowed. The bookkeeping the
/// drainer does not need — when it was created, the last error text — is left in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEvent {
    /// The event's id, time-ordered so the queue drains roughly in creation order.
    pub event_id: Id,
    /// The node id this event is addressed to.
    pub target_node: Id,
    /// The federation opcode the payload is framed as.
    pub opcode: i32,
    /// The encoded frame body. Opaque bytes.
    pub payload: Vec<u8>,
    /// How many delivery attempts have already failed. Zero for a never-tried event; the
    /// exponent the next backoff is computed from.
    pub attempts: i32,
    /// The earliest time this attempt was permitted.
    pub next_attempt_at: Timestamp,
}

/// The verdict of checking a packet's sequence number against a link's history.
///
/// A link is a strictly increasing sequence with no gaps: the only good outcome is
/// [`Accept`](Self::Accept). Anything else is the transport layer's cue to act — drop the
/// packet for a [`Replay`](Self::Replay), tear the link down and re-handshake for a
/// [`Gap`](Self::Gap), which section 152 treats as a suspected replay or a lost segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceVerdict {
    /// The number was exactly one greater than the last: the packet is in order and the
    /// link has advanced to it.
    Accept,
    /// The number did not advance past the last seen. A duplicate or an out-of-order
    /// straggler; the packet is dropped and the link is left as it was.
    Replay,
    /// The number skipped ahead of the expected one. The link's state is cleared and the
    /// caller must reset the connection and re-handshake (section 152).
    Gap,
}

/// Deployment-tunable knobs for the mesh.
///
/// Kept in code, not the store: these are policy, not per-peer state. Cloned into the
/// service at construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshConfig {
    /// How long a handshake nonce is remembered, in milliseconds. See
    /// [`DEFAULT_NONCE_WINDOW_MS`] for the lower bound this must respect.
    pub nonce_window_ms: i64,
    /// The first retry's delay after a failed delivery, in milliseconds.
    pub backoff_base_ms: i64,
    /// The ceiling a doubling backoff is clamped to, in milliseconds.
    pub backoff_cap_ms: i64,
    /// How many failed attempts before a drainer should dead-letter an event. Advisory; see
    /// [`DEFAULT_MAX_ATTEMPTS`].
    pub max_attempts: i32,
    /// How many due events a single drain pass reads.
    pub due_batch: u16,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            nonce_window_ms: DEFAULT_NONCE_WINDOW_MS,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            backoff_cap_ms: DEFAULT_BACKOFF_CAP_MS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            due_batch: DEFAULT_DUE_BATCH,
        }
    }
}
