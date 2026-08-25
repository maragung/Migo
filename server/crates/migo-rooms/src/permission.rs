//! What a member of a room is allowed to do, as one machine word.
//!
//! Brief section 48 is the registry: the names below are *product* permissions,
//! scoped to one room, and every one of them appears there. A name that is not in
//! that section has no business being a bit here, because a bit nobody can trace to
//! the registry is a permission nobody can reason about.
//!
//! # Why a bitmask
//!
//! Because two of these are already columns. `room_members.permissions_grant` and
//! `permissions_deny` are `u64` in the store, so the choice was made when the schema
//! was written; this module is the meaning of those bits and the only place that
//! meaning exists. A set of strings in a side table would be a join on the hot path
//! of every message send.
//!
//! # Why deny wins
//!
//! [`resolve`] subtracts `deny` last, so a denial beats both the role default and an
//! explicit grant. That is the only order that makes a moderation action reliable: a
//! manager who takes `CHAT_SEND` away from one griefer must not have it handed back
//! by the role the griefer holds, and "we removed it but the role restored it" is not
//! an outcome anyone can explain to the person who filed the report.
//!
//! # What is deliberately not here
//!
//! No session capabilities. `migo_auth::capability` holds those, and the split is in
//! its module docs: a capability is minted at sign-in and describes the session, a
//! permission is resolved at the moment of the action from `(actor, room, target)`
//! and changes while the session is open. Putting a room permission in a token would
//! mean a member demoted ten minutes ago still holds it for the rest of the token's
//! life.
//!
//! No global moderation powers. A staff member acting across rooms is brief section
//! 49's subject, and their authority does not come from a membership row.

use migo_protocol::RoomRole;

/// Send messages in the room's conversation.
pub const CHAT_SEND: u64 = 1 << 0;

/// Delete somebody else's message.
///
/// Deleting one's *own* message is not this bit. An author can always retract what
/// they wrote, gated by authorship rather than by role: brief section 23 lists Delete
/// among the things a message supports, and requiring a permission for it would mean
/// a room could be configured so that a member cannot take back their own words.
pub const CHAT_DELETE: u64 = 1 << 1;

/// Pin and unpin messages.
pub const CHAT_PIN: u64 = 1 << 2;

/// Record and send a voice note.
pub const VOICE_NOTE_SEND: u64 = 1 << 3;

/// Delete somebody else's voice note.
pub const VOICE_NOTE_DELETE: u64 = 1 << 4;

/// Forward a voice note out of this room.
///
/// Separate from [`VOICE_NOTE_SEND`] because it is the bit that decides whether
/// audio recorded here can leave, which brief section 179 makes a policy of the room
/// and not of the recorder.
pub const VOICE_NOTE_FORWARD: u64 = 1 << 5;

/// Play a voice note posted here.
pub const VOICE_NOTE_PLAY: u64 = 1 << 6;

/// Start a call in the room.
pub const CALL_START: u64 = 1 << 7;

/// Join a call already running in the room.
pub const CALL_JOIN: u64 = 1 << 8;

/// Mute a member for a while.
pub const USER_MUTE: u64 = 1 << 9;

/// Remove a member, who may come back.
pub const USER_KICK: u64 = 1 << 10;

/// Remove a member and keep them out.
pub const USER_BAN: u64 = 1 << 11;

/// Change name, topic, and slow mode.
pub const ROOM_EDIT: u64 = 1 << 12;

/// Change roles, permissions, and the join policy; archive the room.
pub const ROOM_MANAGE: u64 = 1 << 13;

/// Invite people into a room that does not admit them on their own.
pub const ROOM_INVITE: u64 = 1 << 14;

/// Post an announcement every member is notified of.
pub const ROOM_ANNOUNCE: u64 = 1 << 15;

/// Act on reports and see the moderation log.
pub const ROOM_MODERATE: u64 = 1 << 16;

/// Use a bot the room already has.
pub const BOT_USE: u64 = 1 << 17;

/// Add, configure, and remove bots.
pub const BOT_MANAGE: u64 = 1 << 18;

/// Every bit this build knows.
///
/// Used for the owner's default and, more importantly, to reject a grant of a bit
/// that means nothing here. A client that sets bit 40 has a bug or a newer build,
/// and storing the number anyway would make the difference invisible until the bit
/// acquired a meaning and started granting something nobody asked for.
pub const ALL: u64 = CHAT_SEND
    | CHAT_DELETE
    | CHAT_PIN
    | VOICE_NOTE_SEND
    | VOICE_NOTE_DELETE
    | VOICE_NOTE_FORWARD
    | VOICE_NOTE_PLAY
    | CALL_START
    | CALL_JOIN
    | USER_MUTE
    | USER_KICK
    | USER_BAN
    | ROOM_EDIT
    | ROOM_MANAGE
    | ROOM_INVITE
    | ROOM_ANNOUNCE
    | ROOM_MODERATE
    | BOT_USE
    | BOT_MANAGE;

/// What a member can do with no overrides at all.
///
/// Deliberately generous on speech and deliberately empty on moderation: a Public
/// Room whose members cannot talk is not a room, and a member who can mute other
/// people is not a member.
pub const MEMBER_DEFAULT: u64 =
    CHAT_SEND | VOICE_NOTE_SEND | VOICE_NOTE_PLAY | VOICE_NOTE_FORWARD | CALL_JOIN | BOT_USE;

/// What a mute takes away.
///
/// A mute silences; it does not blindfold. A muted member keeps reading the room,
/// keeps hearing the voice notes already posted, and keeps their place in the roster
/// — the sanction is against publishing, so it is defined as the set of publishing
/// bits rather than as a flag consulted at the top of every check.
///
/// `CALL_JOIN` is absent and `CALL_START` is present on purpose: a muted member may
/// sit in a call, because the call has its own per-participant audio state and the
/// room mute is not the microphone. Starting one is the act of summoning the room's
/// attention, which is the thing being withheld.
pub const SILENCED_BY_MUTE: u64 = CHAT_SEND | VOICE_NOTE_SEND | CALL_START | ROOM_ANNOUNCE;

/// A Helper cleans up. Additions over [`MEMBER_DEFAULT`].
const HELPER_ADDS: u64 = CHAT_DELETE | VOICE_NOTE_DELETE | USER_MUTE | ROOM_MODERATE;

/// A Moderator removes people and shapes the room's surface.
const MODERATOR_ADDS: u64 = USER_KICK | CHAT_PIN | ROOM_INVITE | CALL_START;

/// An Administrator bans and edits settings.
const ADMIN_ADDS: u64 = USER_BAN | ROOM_EDIT | ROOM_ANNOUNCE;

/// A Manager changes who the other roles are, and runs the bots.
const MANAGER_ADDS: u64 = ROOM_MANAGE | BOT_MANAGE;

/// The permissions a role carries before per-member overrides.
///
/// Cumulative by construction — each rank is the rank below it plus its own
/// additions — so a new bit added to Helper cannot be missing from Moderator by
/// omission. Brief section 145 asks for `>=` comparisons over hardcoded role lists
/// for exactly this reason, and a cumulative table is that rule applied to the
/// defaults rather than only to the checks.
#[must_use]
pub const fn of_role(role: RoomRole) -> u64 {
    match role {
        // Not a member of anything. A decoded-from-the-future role lands here, so
        // an unknown rank grants nothing rather than everything.
        RoomRole::Unknown => 0,
        RoomRole::Member => MEMBER_DEFAULT,
        RoomRole::Helper => MEMBER_DEFAULT | HELPER_ADDS,
        RoomRole::Moderator => MEMBER_DEFAULT | HELPER_ADDS | MODERATOR_ADDS,
        RoomRole::Admin => MEMBER_DEFAULT | HELPER_ADDS | MODERATOR_ADDS | ADMIN_ADDS,
        RoomRole::Manager => {
            MEMBER_DEFAULT | HELPER_ADDS | MODERATOR_ADDS | ADMIN_ADDS | MANAGER_ADDS
        }
        // The owner is the one account that cannot be locked out of its own room.
        RoomRole::Owner => ALL,
    }
}

/// The effective permissions of one membership row.
///
/// Role default, plus the grant, minus the deny. The order is the contract; see the
/// module docs for why the subtraction is last.
#[must_use]
pub const fn resolve(role: RoomRole, grant: u64, deny: u64) -> u64 {
    (of_role(role) | (grant & ALL)) & !deny
}

/// Whether `mask` contains every bit in `needed`.
///
/// `needed` is a mask rather than a single bit because one opcode can require more
/// than one permission — brief section 48 says so — and an all-of check written once
/// here cannot be turned into an accidental any-of check at a call site.
#[must_use]
pub const fn allows(mask: u64, needed: u64) -> bool {
    mask & needed == needed
}

/// Whether `actor` outranks `target` strictly.
///
/// Strictly, because equal ranks moderating each other is how two moderators ban one
/// another in a loop, and because the alternative reading — "a moderator may remove a
/// moderator" — makes every promotion irreversible by the person who granted it.
///
/// Written as a `>` on the wire values rather than as a match over pairs, per brief
/// section 145's note on `RoomRole`: a role added later sorts into place instead of
/// falling through a list that was exhaustive when it was written.
#[must_use]
pub const fn outranks(actor: RoomRole, target: RoomRole) -> bool {
    actor.to_wire() > target.to_wire()
}

/// The bits in `mask` that this build does not define.
///
/// Returned rather than logged: the service refuses the request and names the
/// number, and a caller that cannot see which bits were wrong cannot fix its own
/// build.
#[must_use]
pub const fn unknown_bits(mask: u64) -> u64 {
    mask & !ALL
}
