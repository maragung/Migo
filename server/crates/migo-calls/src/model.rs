//! Types the call service takes and returns.
//!
//! # Why the wire types are aliased rather than restated
//!
//! The call frames are generated from the same schema as everything else, so
//! this crate has no struct of its own for an invite or a candidate batch:
//! [`CallInviteWire`] *is* [`migo_protocol::CallInvite`]. The aliases exist so
//! the service's signatures read as domain language ("a wire invite") while
//! the dispatcher still hands over the frame it decoded, with no projection
//! layer in between to drift out of sync with the schema.
//!
//! # The one field this crate never fills from the frame
//!
//! Every routing identity that the connection already proved — the calling
//! account, the calling device — is taken from the [`Caller`], never from the
//! frame. A frame that names a different `caller_device` is not an attack this
//! crate needs to defend against so much as a lie it has no reason to believe:
//! the server watched the frame arrive on a device it authenticated, and that
//! is the device the call is credited to.

use migo_core::{Id, Timestamp};
use migo_protocol::{CallStateEvent, TurnServer};
use migo_ratelimit::TrustTier;

/// How long an unanswered invite rings before it becomes `NoAnswer`.
///
/// Thirty seconds. Longer than a person takes to find a phone that is buzzing
/// in another room, shorter than the caller's own patience — and the callee's
/// client learns the same deadline from the invite event's `expires_at`, so
/// the two sides give up at the same moment instead of one of them ringing
/// forever against a call the server already retired.
pub const RING_TTL_MS: i64 = 30_000;

/// Largest sealed blob one relay frame may carry.
///
/// The codec's own ceiling, restated so a caller can size a buffer without
/// importing the wire crate. Sealed SDP and ICE batches land far below it; the
/// bound exists so a hostile peer cannot frame an unbounded allocation in a
/// component whose whole promise is that it does not look at the bytes.
pub const MAX_SEALED_LEN: usize = migo_protocol::limits::MAX_BYTES_LEN;

/// A call carries voice only.
pub const MEDIA_AUDIO: u32 = 0;

/// A call carries voice and video.
pub const MEDIA_VIDEO: u32 = 1;

/// The `CallInviteResult.status` vocabulary.
pub mod invite_status {
    /// The invite is ringing; the callee has until `expires_at`.
    pub const RINGING: u32 = 0;
    /// The callee declined before this invite was retried.
    pub const DECLINED: u32 = 1;
    /// The invite expired unanswered (or the call otherwise ended unpicked).
    pub const EXPIRED: u32 = 2;
    /// A block in either direction stops the invite before it rings.
    pub const BLOCKED: u32 = 3;
    /// The callee declined *busy* before this invite was retried.
    ///
    /// A re-invite against a call that ended `Busy` owes the caller the same
    /// distinction the original decline carried: "busy" invites a retry in a
    /// minute, "declined" does not, and a retry that reports the gentler of
    /// the two is a lie the caller acts on.
    pub const BUSY: u32 = 4;
}

/// Who is asking.
///
/// No `request_id`: every method here either mutates one call — whose id is
/// already the correlation a trace needs — or relays bytes whose provenance is
/// the connection, not the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated account.
    pub account_id: Id,
    /// The connection the request arrived on.
    pub device_id: Id,
    /// Standing, for the rate limiter.
    pub tier: TrustTier,
    /// Server time for this request.
    pub now: Timestamp,
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
        }
    }
}

/// Where a call is in its lifecycle.
///
/// `Reconnecting` (wire value 3) is deliberately absent: this build owns 1:1
/// calls, where a connectivity blip is a client-side matter the media stack
/// papers over and the signalling never sees. The SFU build that needs the
/// state will add it, and `to_wire` keeps the wire numbering it will need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    /// The invite is out and the deadline is running.
    Ringing,
    /// Answered; the devices are exchanging SDP and ICE.
    Connecting,
    /// The first sealed answer was relayed; media may flow.
    Connected,
    /// Over, with a reason.
    Ended,
}

impl CallState {
    /// The wire's numbering, shared with `CallStateEvent.state`.
    #[must_use]
    pub const fn to_wire(self) -> u32 {
        match self {
            Self::Ringing => 0,
            Self::Connecting => 1,
            Self::Connected => 2,
            // Three is `Reconnecting`, reserved for the SFU build.
            Self::Ended => 4,
        }
    }

    /// Whether the call still exists as far as signalling is concerned.
    #[must_use]
    pub const fn is_live(self) -> bool {
        !matches!(self, Self::Ended)
    }
}

/// Why a call ended.
///
/// The reason on an [`Opcode::CallEnd`](migo_protocol::Opcode::CallEnd) is the
/// sender's own claim and is recorded as claimed: the two devices know who hung
/// up, and the server's job is to relay the claim, not to adjudicate it. The
/// two reasons the server *does* own — [`Self::Declined`] and
/// [`Self::NoAnswer`] — are produced by the decline path and the expiry sweep
/// respectively, which is why a client can trust them to mean what they say.
/// [`Self::Busy`] joins them from the decline path: it is the callee's own
/// claim about *why* they declined, relayed the same way, and it exists so a
/// caller's screen can say "busy" rather than accusing a device that was
/// merely occupied of a human refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The caller withdrew the call.
    ByCaller,
    /// The callee hung up an answered call.
    ByCallee,
    /// The callee declined the ring.
    Declined,
    /// The ring ran out of time.
    NoAnswer,
    /// A device reported failure (media, codec, hardware).
    Failed,
    /// Connectivity was lost.
    Network,
    /// The callee's devices were occupied — a decline, not a hang-up.
    Busy,
}

impl EndReason {
    /// Every reason this build knows, for metric registration.
    pub(crate) const ALL: [Self; 7] = [
        Self::ByCaller,
        Self::ByCallee,
        Self::Declined,
        Self::NoAnswer,
        Self::Failed,
        Self::Network,
        Self::Busy,
    ];

    /// The wire's numbering, shared with `CallEnd.reason` and the optional
    /// `reason` on a `CallStateEvent`.
    #[must_use]
    pub const fn to_wire(self) -> u32 {
        match self {
            Self::ByCaller => 0,
            Self::ByCallee => 1,
            Self::Declined => 2,
            Self::NoAnswer => 3,
            Self::Failed => 4,
            Self::Network => 5,
            Self::Busy => 6,
        }
    }

    /// Inverse of [`EndReason::to_wire`]. `None` for the wire's `Reconnecting`
    /// shape (which is a state, not a reason) and for any number a newer build
    /// invented.
    #[must_use]
    pub const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::ByCaller),
            1 => Some(Self::ByCallee),
            2 => Some(Self::Declined),
            3 => Some(Self::NoAnswer),
            4 => Some(Self::Failed),
            5 => Some(Self::Network),
            6 => Some(Self::Busy),
            _ => None,
        }
    }
}

/// One call, as the server models it.
///
/// There is no participant list: a 1:1 call is two named parties and the two
/// devices they chose, and everything this crate routes — an invite, a sealed
/// answer, an end — is a question about exactly those four ids. An SFU call
/// will need a roster; that is a different struct, not a generalisation of
/// this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// Client-minted, and the idempotency key for the invite.
    pub call_id: Id,
    /// The conversation the call belongs to, for the membership check.
    pub conversation_id: Id,
    /// The account that placed the call.
    pub caller_id: Id,
    /// The device that placed the call.
    pub caller_device: Id,
    /// The account being rung.
    pub callee_id: Id,
    /// The device that answered, set at answer time.
    pub callee_device: Option<Id>,
    /// [`MEDIA_AUDIO`] or [`MEDIA_VIDEO`].
    pub media_kind: u32,
    /// Where the call is.
    pub state: CallState,
    /// Set when `state` is [`CallState::Ended`].
    pub end_reason: Option<EndReason>,
    /// When the ring gives up.
    pub expires_at: Timestamp,
    /// When the callee answered, if they did.
    pub answered_at: Option<Timestamp>,
    /// When the call ended, if it has.
    pub ended_at: Option<Timestamp>,
}

impl Call {
    /// Whether an invite still has time left at `now`.
    #[must_use]
    pub const fn invite_is_live(&self, now: Timestamp) -> bool {
        matches!(self.state, CallState::Ringing) && !now.is_at_or_after(self.expires_at)
    }

    /// The `Ended` state event for this call, carrying its reason.
    ///
    /// Public because the node that runs the sweeper — the composition root,
    /// not this crate — publishes one of these to both parties when a ring
    /// dies of old age: the one call event neither participant's client can
    /// be trusted to originate, because a caller whose browser died cannot
    /// cancel and a callee left ringing has nothing to decline.
    #[must_use]
    pub fn ended_event(&self) -> CallStateEvent {
        CallStateEvent {
            call_id: self.call_id,
            state: CallState::Ended.to_wire(),
            reason: self.end_reason.map(|reason| reason.to_wire()),
        }
    }

    /// The other party in the call, if `account_id` is one of the two.
    ///
    /// `None` for a stranger — the same nothing a lookup of an id that was
    /// never a call would return, so a caller cannot use this to learn which
    /// calls exist.
    #[must_use]
    pub fn other_party(&self, account_id: Id) -> Option<Id> {
        if account_id == self.caller_id {
            Some(self.callee_id)
        } else if account_id == self.callee_id {
            Some(self.caller_id)
        } else {
            None
        }
    }

    /// The account a call device belongs to, if it is one of the two.
    ///
    /// The relay path's routing question: `to_device` names a device, and the
    /// topic that will reach it belongs to the account that owns it.
    #[must_use]
    pub fn account_of_device(&self, device_id: Id) -> Option<Id> {
        if device_id == self.caller_device {
            Some(self.caller_id)
        } else if Some(device_id) == self.callee_device {
            Some(self.callee_id)
        } else {
            None
        }
    }

    /// The device this account is using in the call, if it has joined one.
    #[must_use]
    pub fn device_of(&self, account_id: Id) -> Option<Id> {
        if account_id == self.caller_id {
            Some(self.caller_device)
        } else if account_id == self.callee_id {
            self.callee_device
        } else {
            None
        }
    }
}

/// What an invite came to, for the caller's own screen.
///
/// The status is a `u32` rather than an enum because it is the wire's own
/// vocabulary (see [`invite_status`]): the caller's client matches on the
/// number, and the server has nothing to add to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteOutcome {
    /// One of [`invite_status`].
    pub status: u32,
    /// When the ring gives up. For a blocked invite, `now`: there is nothing
    /// to wait for.
    pub expires_at: Timestamp,
}

/// The largest group call one conversation may hold.
///
/// The brief's ceiling for a group call (section 166): an SFU that forwards
/// for more participants than this is a different deployment, and a join
/// that would cross the line is refused with a validation error the client
/// can render ("the call is full") rather than a conflict it must guess at.
pub const MAX_GROUP_PARTICIPANTS: usize = 25;

/// The wire's invite frame, under the service's own name.
pub type CallInviteWire = migo_protocol::CallInvite;

/// The wire's SDP relay frame, under the service's own name.
pub type CallSdpWire = migo_protocol::CallSdp;

/// The wire's ICE relay frame, under the service's own name.
pub type CallIceWire = migo_protocol::CallIce;

/// The wire's TURN relay description, under the service's own name.
pub type TurnServerWire = TurnServer;

/// Tuning this crate refuses to hard-code.
#[derive(Clone, Debug, PartialEq)]
pub struct CallsConfig {
    /// How long an unanswered invite rings. Defaults to [`RING_TTL_MS`].
    pub ring_ttl_ms: i64,
    /// The TURN relays a fetch may return.
    ///
    /// Empty until credentials are configured: an honest empty list is better
    /// than a fabricated one, because a client that believes it has a relay
    /// will route media at an address that answers nothing.
    pub turn_servers: Vec<TurnServerWire>,
}

impl Default for CallsConfig {
    fn default() -> Self {
        Self {
            ring_ttl_ms: RING_TTL_MS,
            turn_servers: Vec::new(),
        }
    }
}

// --- the group call, as this node forwards it ------------------------------
//
// The SFU the brief describes is a *forwarder*: every participant seals their
// media for the call's members, and the server's whole job is to seat the
// roster, tell it about itself, and move sealed bytes between its devices. The
// roster below is the state that job needs; the sealed offers are its cargo,
// stored and re-served but never opened — the same promise the 1:1 relay makes,
// extended from "two named devices" to "a roster".

/// One seat in a group call.
///
/// A device, not an account: a person may rejoin from a new device, and the old
/// seat must be replaced rather than joined by a twin that would receive the
/// roster twice and relay through two `from_device`s. The sealed offer is the
/// participant's own media description, sealed for the call's members — this
/// crate moves it and stores it, and reads it never.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupParticipant {
    /// The account that joined.
    pub account_id: Id,
    /// The device they joined with, from the authenticated connection.
    pub device_id: Id,
    /// When they joined, for the roster's own ordering.
    pub joined_at: Timestamp,
    /// Their sealed media description, passed through unopened.
    pub sealed_offer: Vec<u8>,
}

/// A group call bound to a conversation, held in memory for this node's life.
///
/// The call id is client-minted and is the join's idempotency key: a retried
/// join from the same device is the same seat, answered with the same roster
/// and no second announcement. A join from a *different* device of the same
/// account replaces the seat, exactly as a second answer wins a 1:1 ring's
/// device only when it is the first from that device — and the replaced
/// device's clients learn they left through the departure event, not silence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupCall {
    /// Client-minted, and the idempotency key for the first join.
    pub call_id: Id,
    /// The conversation the call belongs to; the membership gate's subject.
    pub conversation_id: Id,
    /// The account that created the call by joining first.
    pub founder_id: Id,
    /// The roster, in join order.
    pub participants: Vec<GroupParticipant>,
    /// When the call began (the first join).
    pub started_at: Timestamp,
    /// When the last participant left, if the call has emptied.
    pub ended_at: Option<Timestamp>,
}

impl GroupCall {
    /// The seat an account holds, if any.
    #[must_use]
    pub fn seat_of(&self, account_id: Id) -> Option<&GroupParticipant> {
        self.participants
            .iter()
            .find(|participant| participant.account_id == account_id)
    }

    /// The account a device in this call belongs to, if it is on the roster.
    ///
    /// The relay path's routing question, the group twin of
    /// [`Call::account_of_device`].
    #[must_use]
    pub fn account_of_device(&self, device_id: Id) -> Option<Id> {
        self.participants
            .iter()
            .find(|participant| participant.device_id == device_id)
            .map(|participant| participant.account_id)
    }
}

/// What a join came to, for the joining client's own screen.
///
/// `Duplicate` is the re-join of the same seat: the same roster, no second
/// announcement. `Full` is the ceiling; `Blocked` is the gate's refusal, named
/// for what the caller's screen should render rather than which table said no.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupJoinOutcome {
    /// Seated (or re-seated); the roster is attached.
    Joined,
    /// The same call id and conversation from a device already seated.
    Duplicate,
    /// The roster is at [`MAX_GROUP_PARTICIPANTS`].
    Full,
    /// Membership, a block, or the conversation's call policy said no.
    Blocked,
    /// The call id was already used for a different conversation's call.
    Conflict,
}
