//! The group-call seat: §163's call-key triggers on an SFU roster.
//!
//! A group call is not a second 1:1 engine. It is a *seat* the server keeps per device —
//! joined through `CALL_SFU_JOIN`, left through the 1:1 `CALL_END` the group service answers
//! for its own ids — plus one [`CallKeyState`] the seated devices hold in common. This module
//! is the seat and the three things §163 says must happen around it:
//!
//! * **The first key.** The roster snapshot that answers a join holds every seat. A joiner who
//!   finds themselves alone mints the call's first key; a joiner who finds others asks for the
//!   running one. The mint draws `CallKeyState::from_session` over a secret this device
//!   generated for exactly this call — an interpretation of the spec's "derived from the
//!   pairwise session", made because a group call's first member has no pairwise peer the call
//!   as a whole could derive from; every later member receives the key through the sealed
//!   paths below, never by deriving it themselves.
//! * **Rotation on roster movement.** When a participant joins or leaves, one seated device —
//!   chosen by a rule every device can compute from the same roster, see
//!   `designated_rotator_for_join` — mints the next epoch's key under the current one and
//!   sends one `CALL_KEY_UPDATE`, which the server fans out to the whole roster minus the
//!   rotating connection. The rotator's own state advances before the frame is sent: §163's
//!   own line that a device creating a rotation must not depend on hearing its own update
//!   back, and the server honours it by excluding the sender's connection.
//! * **The mid-call joiner's ask.** A joiner who lands on an in-progress call cannot read
//!   `CALL_KEY_UPDATE` frames — those are sealed under the very key the joiner lacks — so the
//!   joiner asks a seated participant for the running key (`CALL_RENEGOTIATE`, sealed under
//!   the *pairwise* session the two devices already share) and the participant answers with
//!   [`CallKeyState::sealed_join_distribution`] — the key sealed under the HKDF
//!   "migo-call-join-v1" wrapper derived from that same pairwise secret — riding `CALL_SDP`.
//!
//! # One rotator, not every device
//!
//! The obvious rule — everyone rotates when the roster moves — cannot work: two devices that
//! mint different keys at the same epoch have permanently diverged, because `adopt` refuses
//! the loser's update and no later frame reconciles them. One device must rotate, and every
//! device must agree on which one without speaking. The roster is the shared fact: it is the
//! server's own join order, every seated device hears the same announcements on the
//! conversation's topic, and the snapshot the joiner receives names the same seats in the same
//! order. The rule and its rationale are stated on `designated_rotator_for_join`.
//!
//! # What this build does not have
//!
//! A media plane. The seat is the signalling half alone — the join, the leave, the roster, and
//! the keys — because that is the half §163 specifies. The join's `sealed_offer` is a
//! placeholder this build's peers ignore, and the TURN list the reply carries has no media
//! engine to hand it to yet.
//!
//! The engine follows the call module's shape: state in [`GroupCalls`] plus methods on the
//! worker, because every transition either sends a frame or emits an event — the same
//! single-owner discipline [`super::call`] follows for the same reasons.
//!
//! # The call a member is not in
//!
//! The conversation topic's announcements reach every member, seated or not, so a device
//! that holds no seat still hears every join and departure of its conversations' calls.
//! That discovery is what makes "join a call already in progress" possible: the ledger
//! [`GroupCalls::in_progress`] keeps the running call's id and size per conversation, the
//! header offers "Join call in progress (N)" off it, and the press passes the call id back
//! so the join seats the running call — where a fresh mint would start a second call
//! beside it, the one mistake the idempotency key exists to prevent.

use std::collections::HashMap;

use migo_core::id::ID_BYTE_LEN;
use migo_core::{Id, OsRandom, Random, Timestamp};
use migo_crypto::CallKeyState;
use migo_protocol::{
    CallEnd, CallInvite, CallKeyUpdate, CallRenegotiate, CallSdp, CallSfuParticipant,
    CallStateEvent, KeyBundleRequest, Opcode,
};

use super::call_signal::{self, CallEndReason};
use super::gateway;
use super::{Event, Worker};
use crate::crypto::content::{self, Content};
use crate::crypto::envelope::Envelope;
use crate::model::ToastKind;

/// The control-event name a mid-call joiner's key ask rides under, sealed in the pairwise
/// channel the two devices already share. The distributor recognises the ask by this name the
/// way the message layer recognises "sender-key" — one vocabulary per sealed channel.
const KEY_ASK_EVENT: &str = "call-key-ask";

/// The wire's `Connected` state, the state a join announcement carries.
const STATE_CONNECTED: u32 = 2;

/// The wire's `Ended` state, the state a departure announcement carries — the vocabulary has
/// no "left", so a departure is the seat's own small ending.
const STATE_ENDED: u32 = 4;

/// The call-key seat: the group-call state the worker holds, beside the 1:1 engine's own.
pub(crate) struct GroupCalls {
    /// The seat this device holds, if any. One at a time, like the 1:1 engine's active call:
    /// a person joins one call at a time, and a second conversation's join while a seat lives
    /// is the UI's button to refuse, not the worker's to guess at.
    seat: Option<GroupSeat>,
    /// A join asked and not yet seated: the call id (the join's idempotency key) and the
    /// conversation it belongs to. The ask stands until the roster snapshot that answers it
    /// arrives, so a press that lands before the snapshot re-sends the *same* join — the
    /// server re-seats the same call — rather than minting a second call id.
    joining: Option<JoinAsk>,
    /// Calls running in conversations this device holds no seat in, as the conversation
    /// topic's announcements reported them: conversation → (call, size after the last
    /// change). This is the discovery the header's join-in-progress offer is built on, and
    /// an entry dies only two ways — the roster empties (the call retired) or this device
    /// takes a seat in the conversation (it is nobody's spectator anymore).
    in_progress: HashMap<Id, (Id, u32)>,
}

/// A join in flight.
struct JoinAsk {
    call_id: Id,
    conversation_id: Id,
}

/// One seated group call: the roster as this device holds it, and the frame key.
struct GroupSeat {
    call_id: Id,
    conversation_id: Id,
    /// The roster, `(account, device)` in the server's own join order — the order the
    /// designated-rotator rule reads. Ourselves included; one seat per account, so a seat
    /// replacement is a departure followed by an arrival like the server's own bookkeeping.
    seats: Vec<(Id, Id)>,
    /// The call's frame key. `None` between seating and the first answer or mint — the
    /// joiner's state, the one the ask exists to end.
    key: Option<CallKeyState>,
    /// A key ask that could not be sealed yet (no pairwise session and no bundle for the
    /// participant it was aimed at), waiting for the bundle fetch to answer. The ask is
    /// retried the moment a bundle lands; a lost fetch leaves the seat keyless until the next
    /// membership event re-triggers the ask, which is the honest limit of a signalling-only
    /// retry budget.
    key_ask_waiting: bool,
}

/// Which seated device rotates when a participant `joiner` joins: the first seat, in the
/// roster's own join order, held by an account other than the joiner's.
///
/// The rule has to be one every device computes the same way from facts it already holds, or
/// two devices mint different keys at the same epoch and the loser's update is refused forever.
/// The roster order is that fact: the snapshot names the seats in join order, and every seated
/// device hears the same announcements on the conversation's topic in the same order. "First
/// seat that is not the joiner" keeps the earliest participant rotating — the one most likely
/// to hold a key, since every later member received theirs from someone — and it is §163's own
/// line that the distributor is expected to rotate when a participant joins, which is this
/// rule said plainly. The joiner's own seat is excluded because a joiner rotating on their own
/// arrival would hand the roster a key sealed under a key the joiner does not hold yet.
fn designated_rotator_for_join(seats: &[(Id, Id)], joiner: Id) -> Option<(Id, Id)> {
    seats
        .iter()
        .find(|(account, _)| *account != joiner)
        .copied()
}

/// Which seated device rotates when a seat departs: the first remaining seat. Any account —
/// the leaver is gone, and the earliest survivor is the likeliest key-holder for the same
/// reason the join rule's is.
fn designated_rotator_for_departure(seats: &[(Id, Id)]) -> Option<(Id, Id)> {
    seats.first().copied()
}

/// Adds a seat, one per account: a seat whose account already holds one is replaced (the
/// server's own seat-replacement shape, an arrival that follows a departure), and a seat that
/// is already present exactly is a duplicate arrival that moves nothing. Returns whether the
/// roster changed — the exactly-once signal the rotation turns on, because the announcement
/// and the joiner's ask can arrive in either order and whichever lands second must not rotate
/// a second time.
fn add_seat(seats: &mut Vec<(Id, Id)>, account: Id, device: Id) -> bool {
    if seats.iter().any(|&(a, d)| a == account && d == device) {
        return false;
    }
    seats.retain(|&(a, _)| a != account);
    seats.push((account, device));
    true
}

/// Removes a departing seat by account. Returns whether the roster changed — the same
/// exactly-once signal, for a departure announcement the server may redeliver.
fn remove_seat(seats: &mut Vec<(Id, Id)>, account: Id) -> bool {
    let before = seats.len();
    seats.retain(|&(a, _)| a != account);
    before != seats.len()
}

impl GroupCalls {
    pub(crate) fn new() -> Self {
        Self {
            seat: None,
            joining: None,
            in_progress: HashMap::new(),
        }
    }

    /// The id a join for one conversation carries, and the ask that remembers it. Three
    /// sources, in precedence order: an ask already in flight for the conversation (the
    /// press is a retry of a commitment the server may already be seating, and the ask's
    /// id is what makes the retry the *same* join), the id the caller offered (a call the
    /// header learned is running — joining it seats the call everyone else is in, where a
    /// fresh mint would start a second call beside it), and a fresh mint (this device is
    /// starting the conversation's call).
    fn join_target(&mut self, conversation_id: Id, offered: Option<Id>) -> Id {
        if let Some(ask) = &self.joining {
            if ask.conversation_id == conversation_id {
                return ask.call_id;
            }
        }
        let call_id = offered.unwrap_or_else(|| Id::generate_at(Timestamp::now(), &mut OsRandom));
        self.joining = Some(JoinAsk {
            call_id,
            conversation_id,
        });
        call_id
    }

    /// Records one announcement's worth of spectator news: a device with no seat and no
    /// join in the event's conversation hears the announcement as discovery — the call is
    /// running, at the size the event names — and the ledger keeps it for the header's
    /// join-in-progress offer. Returns what the announcement amounted to, so the worker
    /// can say it as an event; `None` when the event was not the spectator's to keep — a
    /// conversation this device is bound to (its own call's announcements belong to the
    /// roster path, and a join in flight hears its *own* join announced before the
    /// snapshot answers), or a shape with no count to show for it.
    fn spectate(&mut self, event: &CallStateEvent) -> Option<Spectated> {
        let conversation_id = event.conversation_id?;
        let bound = self
            .seat
            .as_ref()
            .is_some_and(|seat| seat.conversation_id == conversation_id)
            || self
                .joining
                .as_ref()
                .is_some_and(|ask| ask.conversation_id == conversation_id);
        if bound {
            return None;
        }
        match event.participant_count {
            // The retirement the seated path knows by its emptied roster: the last seat
            // left, the call no longer exists server-side, and the offer to join it goes
            // with it. A retirement of a call the ledger never held has nothing to clear
            // and nothing to say.
            Some(0) => self
                .in_progress
                .remove(&conversation_id)
                .map(|_| Spectated::Retired { conversation_id }),
            Some(count) => {
                self.in_progress
                    .insert(conversation_id, (event.call_id, count));
                Some(Spectated::Running {
                    conversation_id,
                    call_id: event.call_id,
                    count,
                })
            }
            None => None,
        }
    }
}

/// What one spectator announcement amounted to, for the event the worker emits beside it.
#[derive(Debug, PartialEq, Eq)]
enum Spectated {
    /// The conversation's call is running at this size.
    Running {
        conversation_id: Id,
        call_id: Id,
        count: u32,
    },
    /// The call retired: its last seat left.
    Retired { conversation_id: Id },
}

impl Worker {
    /// Joins the group call of one conversation — or re-sends the join already in flight.
    ///
    /// The id the join carries is its idempotency key, and it has three sources (see
    /// [`GroupCalls::join_target`]): the ask already standing for the conversation, the
    /// running call the caller named — the header's join-in-progress offer, joining the
    /// call everyone else is in rather than minting a second beside it — and a fresh
    /// mint for the conversation's first call. Whichever it is, the ask keeps it, because
    /// a press that lands before the roster snapshot re-sends the same frame and the
    /// server re-seats the same call, while a fresh mint would build a *second* call
    /// for the same conversation — the one mistake a client-minted id allows that no
    /// server would ever forgive.
    pub(super) async fn join_group_call(&mut self, conversation_id: Id, call_id: Option<Id>) {
        // Already seated in this conversation's call: the press is a duplicate of a join the
        // snapshot already answered.
        if let Some(seat) = &self.group_calls.seat {
            if seat.conversation_id == conversation_id {
                return;
            }
        }
        let call_id = self.group_calls.join_target(conversation_id, call_id);
        let Some(my_device) = self.device_id() else {
            return;
        };
        // The offer slot is required by the join's shape (the 1:1 invite's own frame), and
        // this build has no media description to put in it — a placeholder, sealed under a
        // key minted for exactly this call so the bytes are opaque to the server the way a
        // real offer would be, and ignored by every peer the way a real offer is not.
        let offer_key = call_signal::generate_call_key();
        let Ok(sealed_offer) =
            call_signal::seal_call_signal(b"migo-group-seat", &offer_key, &call_id)
        else {
            self.sink
                .toast("could not seal the group-call join", ToastKind::Error);
            return;
        };
        let request = CallInvite {
            call_id,
            conversation_id,
            callee_id: Id::NIL,
            media_kind: 0,
            caller_device: my_device,
            capabilities: 0,
            sealed_offer,
        };
        self.request(Opcode::CallSfuJoin, &request).await;
    }

    /// Leaves the group call of one conversation: the seat goes, the 1:1 end frame the group
    /// service answers for its own ids goes with it, and the departure announcement the roster
    /// hears is the server's business.
    pub(super) async fn leave_group_call(&mut self, conversation_id: Id) {
        // A join that never seated can still be abandoned: the ask retires, and the leave
        // tells the server the seat it may already have counted is not wanted. A late roster
        // snapshot for the retired id is then ignored — the ask it would answer is gone.
        if let Some(ask) = &self.group_calls.joining {
            if ask.conversation_id == conversation_id {
                let call_id = ask.call_id;
                self.group_calls.joining = None;
                let end = CallEnd {
                    call_id,
                    reason: CallEndReason::ByCaller.to_wire(),
                };
                self.request(Opcode::CallEnd, &end).await;
                return;
            }
        }
        let Some(seat) = self.group_calls.seat.take() else {
            return;
        };
        if seat.conversation_id != conversation_id {
            // A leave pressed for a conversation this seat does not belong to is not this
            // seat's to take: the seat goes back, untouched.
            self.group_calls.seat = Some(seat);
            return;
        }
        let end = CallEnd {
            call_id: seat.call_id,
            reason: CallEndReason::ByCaller.to_wire(),
        };
        self.request(Opcode::CallEnd, &end).await;
        self.sink.send(Event::GroupCallEnded { conversation_id });
    }

    /// One `CALL_SFU_EVENT`, classified the way the SDK classifies it: a frame carrying
    /// `participants` is a roster snapshot (published to a joiner's own topic), `Connected`
    /// without them is a join announcement (the conversation's topic), `Ended` is a
    /// departure. Any other shape is a future server's and is dropped quietly, the same
    /// patience the resume counter shows an unknown opcode.
    pub(super) async fn on_sfu_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<CallStateEvent>(frame) else {
            return;
        };
        if let Some(participants) = &event.participants {
            self.on_group_roster(&event, participants).await;
        } else if event.state == STATE_CONNECTED {
            self.on_group_join_announcement(&event).await;
        } else if event.state == STATE_ENDED {
            self.on_group_departure(&event).await;
        }
    }

    /// The roster snapshot that answers this device's own join: the seat is taken here, the
    /// first key is minted or asked for, and the ask retires.
    ///
    /// The snapshot is also forwarded to this account's *other* devices, so the one fact that
    /// separates "seat" from "spectate" is the ask: a snapshot for a call this device did not
    /// join is news about a sibling's call, and this device stays out of it.
    async fn on_group_roster(
        &mut self,
        event: &CallStateEvent,
        participants: &[CallSfuParticipant],
    ) {
        let Some(ask) = &self.group_calls.joining else {
            return;
        };
        if ask.call_id != event.call_id {
            return;
        }
        let conversation_id = ask.conversation_id;
        self.group_calls.joining = None;
        let seats: Vec<(Id, Id)> = participants
            .iter()
            .map(|participant| (participant.user_id, participant.device_id))
            .collect();
        let count = seats.len() as u32;
        // Alone on the roster, this join is the call's founding seat: the first key is minted
        // here, from a secret drawn for this call alone (see the module header for why the
        // derivation's input is a local secret rather than a session). Beside others, the call
        // is already running and the running key is asked for — a key minted here would be a
        // key nobody else holds, and the call would fork.
        let key = if count <= 1 {
            let mut secret = [0u8; 32];
            OsRandom.fill_bytes(&mut secret);
            Some(CallKeyState::from_session(&secret, event.call_id))
        } else {
            None
        };
        self.group_calls.seat = Some(GroupSeat {
            call_id: event.call_id,
            conversation_id,
            seats,
            key,
            key_ask_waiting: false,
        });
        // The seat is also the end of spectating: whatever the ledger held for the
        // conversation, this device is part of the call now, and the header's
        // join-in-progress offer must not outlive the seat it offered to.
        self.group_calls.in_progress.remove(&conversation_id);
        self.sink.send(Event::GroupCallSeated {
            conversation_id,
            participant_count: count,
        });
        if count > 1 {
            self.send_group_key_ask().await;
        }
    }

    /// The spectator half of an announcement, shared by the join and departure paths: the
    /// ledger records what a not-bound device learned about a conversation's running call
    /// and the event says it to the UI. Returns whether the event was spectator news and
    /// is fully handled — a bound device's announcements are the roster path's, not this
    /// one's.
    fn spectate_group_call(&mut self, event: &CallStateEvent) -> bool {
        match self.group_calls.spectate(event) {
            Some(Spectated::Running {
                conversation_id,
                call_id,
                count,
            }) => {
                self.sink.send(Event::GroupCallInProgress {
                    conversation_id,
                    call_id,
                    count,
                });
                true
            }
            Some(Spectated::Retired { conversation_id }) => {
                self.sink
                    .send(Event::GroupCallInProgressEnded { conversation_id });
                true
            }
            None => false,
        }
    }

    /// A join announcement off the conversation's topic: the roster gains a seat, and exactly
    /// one device — the designated rotator — rotates the frame key so what the new participant
    /// cannot read is what sealed before them and what they can is what follows.
    async fn on_group_join_announcement(&mut self, event: &CallStateEvent) {
        let Some(user) = event.user_id else {
            return;
        };
        let Some(device) = event.device_id else {
            return;
        };
        // The spectator half comes first: a device with no seat and no join in the event's
        // conversation hears the announcement as discovery, not roster movement, and the
        // ledger is where it lands. A bound device falls through to the roster path its
        // own call's announcements belong to.
        if self.spectate_group_call(event) {
            return;
        }
        let mine = self.own_seat_ids();
        let mut rotate_now = false;
        {
            let Some(seat) = self.group_calls.seat.as_mut() else {
                return;
            };
            if seat.call_id != event.call_id {
                return;
            }
            // The exactly-once gate: a seat the roster already counts is an announcement the
            // ask path (or a redelivery) preceded, and the rotation it would trigger has
            // either happened or was never this device's to make.
            if add_seat(&mut seat.seats, user, device) {
                if let Some(mine) = mine {
                    if designated_rotator_for_join(&seat.seats, user) == Some(mine) {
                        rotate_now = true;
                    }
                }
            }
            self.sink.send(Event::GroupCallSeated {
                conversation_id: seat.conversation_id,
                participant_count: seat.seats.len() as u32,
            });
        }
        if rotate_now {
            self.rotate_group_call_key().await;
        }
    }

    /// A departure announcement off the conversation's topic: the roster loses a seat, the
    /// first survivor rotates so the departed cannot read what follows, and a roster that
    /// empties — or names this device's own seat — ends the call for this device.
    async fn on_group_departure(&mut self, event: &CallStateEvent) {
        let Some(user) = event.user_id else {
            return;
        };
        let Some(device) = event.device_id else {
            return;
        };
        // The same spectator half: a departure is discovery too — the running call's size
        // moved, or its last seat left and the offer to join it must not outlive it.
        if self.spectate_group_call(event) {
            return;
        }
        let mine = self.own_seat_ids();
        let mut rotate_now = false;
        let mut ended_conversation: Option<Id> = None;
        {
            let Some(seat) = self.group_calls.seat.as_mut() else {
                return;
            };
            if seat.call_id != event.call_id {
                return;
            }
            // Our own seat's departure is this device's call ending — a seat replacement by a
            // sibling device, or the sweep retiring a session this device lost. Either way
            // the seat is not ours to hold anymore.
            if let Some((account, my_device)) = mine {
                if account == user && my_device == device {
                    seat.seats.clear();
                }
            }
            let removed = remove_seat(&mut seat.seats, user);
            if seat.seats.is_empty() || event.participant_count == Some(0) {
                // The retirement: the last seat has left, and the call no longer exists
                // server-side. Ours goes with it, whatever the roster's last line said.
                ended_conversation = Some(seat.conversation_id);
            } else {
                if removed {
                    if let Some(mine) = mine {
                        if designated_rotator_for_departure(&seat.seats) == Some(mine) {
                            rotate_now = true;
                        }
                    }
                }
                self.sink.send(Event::GroupCallSeated {
                    conversation_id: seat.conversation_id,
                    participant_count: seat.seats.len() as u32,
                });
            }
        }
        if let Some(conversation_id) = ended_conversation {
            self.group_calls.seat = None;
            self.sink.send(Event::GroupCallEnded { conversation_id });
            return;
        }
        if rotate_now {
            self.rotate_group_call_key().await;
        }
    }

    /// One `CALL_KEY_UPDATE`: the rotator's sealed next epoch, fanned out by the server to the
    /// roster minus the rotating connection. Adoption is the reference crate's own refusal —
    /// an epoch that does not advance is a replay, and a blob that does not open under the
    /// held key is not this call's rotation — so a refused adopt is a no-op, not an error: the
    /// state keeps the key that works.
    pub(super) fn on_group_key_update(&mut self, frame: &migo_protocol::Frame) {
        let Ok(update) = gateway::decode::<CallKeyUpdate>(frame) else {
            return;
        };
        let Some(seat) = self.group_calls.seat.as_mut() else {
            return;
        };
        if seat.call_id != update.call_id {
            return;
        }
        // A keyless seat has no key to adopt onto: its baseline arrives by the join path, and
        // after that the next update — or the one this frame redelivers — is adoptable.
        let Some(key) = seat.key.as_mut() else {
            return;
        };
        let _ = key.adopt(update.epoch, &update.sealed_key_material);
    }

    /// The group half of a `CALL_SDP` relay: returns whether the frame belonged to this
    /// device's seat, so the 1:1 engine knows whether it was addressed at all.
    ///
    /// Both halves of the mid-call join flow arrive here, because the server projects a
    /// `CALL_RENEGOTIATE` to its target as the `CALL_SDP` frame it relays — the name the
    /// sender used is for the sender's own charge, not for what the target decodes. The ask
    /// opens under the pairwise session (the joiner has no call key — that is the ask's whole
    /// reason); the answer opens under the "migo-call-join-v1" wrapper of that same session.
    /// A blob that opens as neither is not this seat's business, and the 1:1 engine never
    /// shares a call id with a group call — the ids live in the server's distinct stores.
    pub(super) async fn on_group_call_relay(&mut self, relay: &CallSdp) -> bool {
        let Some(seat) = self.group_calls.seat.as_ref() else {
            return false;
        };
        if seat.call_id != relay.call_id {
            return false;
        }
        let (conversation_id, call_id) = (seat.conversation_id, seat.call_id);
        if let Some(joiner) =
            self.open_group_key_ask(conversation_id, relay.from_device, &relay.sealed_sdp)
        {
            self.answer_group_key_ask(joiner, relay.from_device).await;
            return true;
        }
        // The answer half: only a keyless seat installs. A seat that already holds a key — a
        // rotation landed between the ask and this answer — keeps its own, because
        // `from_join_distribution` is a baseline constructor with no epoch guard of its own,
        // and installing a stale baseline over a live key is the one way this path could go
        // backwards.
        let Some(seat) = self.group_calls.seat.as_mut() else {
            return true;
        };
        if seat.key.is_none() {
            let secret = self.signed.as_ref().and_then(|signed| {
                signed
                    .sessions
                    .pairwise_secret(conversation_id, relay.from_device)
            });
            if let Some(secret) = secret {
                if let Ok(key) =
                    CallKeyState::from_join_distribution(&secret, call_id, &relay.sealed_sdp)
                {
                    seat.key = Some(key);
                }
            }
        }
        true
    }

    /// Opens a key ask from one device, if the bytes are one: the pairwise envelope the
    /// joiner sealed, carrying the joiner's account id — the one fact the frame's own
    /// `from_device` cannot say, and the one the designated-rotator rule needs.
    fn open_group_key_ask(
        &mut self,
        conversation_id: Id,
        from_device: Id,
        sealed: &[u8],
    ) -> Option<Id> {
        let signed = self.signed.as_mut()?;
        let plaintext = Envelope::decode(sealed)
            .and_then(|envelope| {
                signed
                    .sessions
                    .open(conversation_id, from_device, &envelope)
            })
            .ok()?;
        let Ok(Content::ControlEvent { event, data }) = content::decode(&plaintext) else {
            return None;
        };
        if event != KEY_ASK_EVENT {
            return None;
        }
        let bytes = data?;
        let joined: [u8; ID_BYTE_LEN] = bytes.as_slice().try_into().ok()?;
        Some(Id::from_bytes(joined))
    }

    /// Answers a key ask: rotate first if this device is the join the rotator rule names,
    /// then hand the joiner the running key sealed under the join wrapper.
    ///
    /// The rotation comes first because the wrapper's own contract says so — a joiner handed
    /// the key that was current while they were outside the call can open the media that key
    /// sealed, and rotation is what makes "sealed for them at join" also mean "sealed against
    /// them until join". The answer goes out whether or not this device rotated: a keyless
    /// distributor cannot rotate (documented limit — the next membership event re-triggers),
    /// but a joiner is still better served by the key the distributor *does* hold.
    async fn answer_group_key_ask(&mut self, joiner: Id, joiner_device: Id) {
        let mine = self.own_seat_ids();
        let mut rotate_now = false;
        {
            let Some(seat) = self.group_calls.seat.as_mut() else {
                return;
            };
            // The same exactly-once gate the announcement path uses: whichever of the two
            // learns of the joiner first adds the seat and (if designated) rotates; the
            // second is a no-op on the roster and a plain answer on the key.
            if add_seat(&mut seat.seats, joiner, joiner_device) {
                if let Some(mine) = mine {
                    if designated_rotator_for_join(&seat.seats, joiner) == Some(mine) {
                        rotate_now = true;
                    }
                }
            }
        }
        if rotate_now {
            self.rotate_group_call_key().await;
        }
        let Some(my_device) = self.device_id() else {
            return;
        };
        let (call_id, sealed) = {
            let Some(seat) = self.group_calls.seat.as_mut() else {
                return;
            };
            // The ask opened under the pairwise session, so the secret exists. A key the seat
            // does not hold is the keyless-distributor limit, and there is nothing honest to
            // answer with — the joiner's next roster event re-triggers the ask elsewhere.
            let Some(key) = seat.key.as_ref() else {
                return;
            };
            let Some(secret) = self.signed.as_ref().and_then(|signed| {
                signed
                    .sessions
                    .pairwise_secret(seat.conversation_id, joiner_device)
            }) else {
                return;
            };
            let Ok(sealed) = key.sealed_join_distribution(&secret, &mut OsRandom) else {
                return;
            };
            (seat.call_id, sealed)
        };
        let answer = CallSdp {
            call_id,
            from_device: my_device,
            to_device: joiner_device,
            sealed_sdp: sealed,
        };
        self.request(Opcode::CallSdp, &answer).await;
    }

    /// Sends the mid-call joiner's key ask to the deterministic participant: the first seat
    /// on the roster held by an account other than this one.
    ///
    /// The ask rides `CALL_RENEGOTIATE` because that frame is the wire's own "a sealed blob
    /// for one device of a call I am in" channel, sealed under the pairwise session — the one
    /// key the joiner and the participant already share, which is exactly the property the
    /// answer's wrapper derivation will need. A participant with no pairwise session yet
    /// (this conversation's keys never crossed between the two devices) cannot be asked
    /// blind: the bundle is fetched and the ask retried when it lands.
    async fn send_group_key_ask(&mut self) {
        let Some(mine) = self.own_seat_ids() else {
            return;
        };
        let target = {
            let Some(seat) = self.group_calls.seat.as_ref() else {
                return;
            };
            // The distributor-choice rule, stated where a reader of the flow looks for it:
            // the first seat in the roster's join order that is not this account's own — the
            // same seat every other device names from the same roster, see
            // `designated_rotator_for_join`.
            seat.seats
                .iter()
                .find(|(account, _)| *account != mine.0)
                .copied()
        };
        let Some((to_account, to_device)) = target else {
            return;
        };
        let Some(seat) = self.group_calls.seat.as_ref() else {
            return;
        };
        let (call_id, conversation_id) = (seat.call_id, seat.conversation_id);
        let Ok(payload) = content::encode(
            &Content::ControlEvent {
                event: KEY_ASK_EVENT.to_owned(),
                data: Some(mine.0.as_bytes().to_vec()),
            },
            true,
        ) else {
            return;
        };
        let envelope = {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            let bundle = signed.bundles.get(&to_device).cloned();
            signed
                .sessions
                .seal(conversation_id, to_device, bundle.as_ref(), &payload)
        };
        let Ok(envelope) = envelope else {
            // No session and no bundle: the fetch is the only way forward, and the ask waits
            // on its answer (see `key_ask_waiting`).
            if let Some(seat) = self.group_calls.seat.as_mut() {
                seat.key_ask_waiting = true;
            }
            let request = KeyBundleRequest {
                user_id: to_account,
                device_id: Some(to_device),
            };
            self.request(Opcode::KeyBundleFetch, &request).await;
            return;
        };
        let Ok(sealed) = envelope.encode() else {
            return;
        };
        if let Some(seat) = self.group_calls.seat.as_mut() {
            seat.key_ask_waiting = false;
        }
        let ask = CallRenegotiate {
            call_id,
            from_device: mine.1,
            to_device,
            sealed_sdp: sealed,
        };
        self.request(Opcode::CallRenegotiate, &ask).await;
    }

    /// Retries a key ask that was waiting on a bundle, when a bundle fetch has answered.
    /// Called from the bundle path so the ask needs no timer of its own: the fetch is the
    /// only thing the ask was waiting on.
    pub(super) async fn retry_group_key_ask(&mut self) {
        let waiting = self
            .group_calls
            .seat
            .as_ref()
            .is_some_and(|seat| seat.key_ask_waiting);
        if waiting {
            self.send_group_key_ask().await;
        }
    }

    /// Mints the next epoch of the call's frame key and sends one `CALL_KEY_UPDATE` for the
    /// whole roster.
    ///
    /// The state advances *before* the frame is sent — the rotator never depends on hearing
    /// its own update back, and the server honours that by excluding the sender's connection
    /// from the fan-out. A keyless seat cannot rotate and simply stands down; the next
    /// membership event re-triggers the rule, and the honest limit is that a keyless
    /// designated rotator stalls one rotation.
    async fn rotate_group_call_key(&mut self) {
        let (call_id, epoch, sealed) = {
            let Some(seat) = self.group_calls.seat.as_mut() else {
                return;
            };
            let Some(key) = seat.key.as_mut() else {
                return;
            };
            let Ok(sealed) = key.rotate(&mut OsRandom) else {
                return;
            };
            (seat.call_id, key.epoch(), sealed)
        };
        let update = CallKeyUpdate {
            call_id,
            epoch,
            sealed_key_material: sealed,
        };
        self.request(Opcode::CallKeyUpdate, &update).await;
    }

    /// This device's own `(account, device)`, the pair the rotator rules compare against.
    fn own_seat_ids(&self) -> Option<(Id, Id)> {
        self.signed
            .as_ref()
            .map(|signed| (signed.account.account_id, signed.account.device_id))
    }

    /// The gateway died mid-call: there is no signalling left to leave with, so the seat goes
    /// the way the 1:1 engine's active call goes. A join in flight retires too — the reply
    /// and the snapshot it was waiting for died with the socket.
    pub(super) fn group_calls_offline(&mut self) {
        self.group_calls.joining = None;
        if let Some(seat) = self.group_calls.seat.take() {
            self.sink.send(Event::GroupCallEnded {
                conversation_id: seat.conversation_id,
            });
        }
    }

    /// Sign-out: nothing of any call survives the session that held it, the group seat no
    /// more than the 1:1 engine's own state — and the spectator's ledger no more than the
    /// seat, since the announcements that fill it were this session's to hear.
    pub(super) fn group_calls_sign_out(&mut self) {
        self.group_calls.joining = None;
        self.group_calls.seat = None;
        self.group_calls.in_progress.clear();
    }

    /// Drops the seat of one conversation, for the conversation's own teardown (a leave, a
    /// kick): the membership that authorised the seat is gone. Returns whether a seat went,
    /// so the caller can say the call ended for the UI. The spectator's ledger entry goes
    /// with it — a conversation this account is no longer in is not one whose calls it
    /// will be offered.
    pub(super) fn forget_group_call(&mut self, conversation_id: Id) -> bool {
        let dropped = self
            .group_calls
            .seat
            .as_ref()
            .is_some_and(|seat| seat.conversation_id == conversation_id);
        if dropped {
            self.group_calls.seat = None;
        }
        if self
            .group_calls
            .joining
            .as_ref()
            .is_some_and(|ask| ask.conversation_id == conversation_id)
        {
            self.group_calls.joining = None;
        }
        self.group_calls.in_progress.remove(&conversation_id);
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::crypto::session::{bundle_from_wire, DeviceKeys, SessionStore};

    /// An id whose first byte names it, so a roster reads as a cast list rather than hex.
    fn id_of(byte: u8) -> Id {
        let mut bytes = [0u8; ID_BYTE_LEN];
        bytes[0] = byte;
        Id::from_bytes(bytes)
    }

    /// The join rule picks the first seat the joiner's own account does not hold — and it is a
    /// *roster* question, not a popularity one: whatever the order, every device that sees the
    /// same roster names the same rotator, which is the property the divergence argument needs.
    #[test]
    fn the_join_rotator_is_the_first_seat_the_joiner_does_not_hold() {
        let seats = [(id_of(1), id_of(2)), (id_of(3), id_of(4))];

        // A joiner from the second account: the first seat rotates.
        assert_eq!(
            designated_rotator_for_join(&seats, id_of(3)),
            Some((id_of(1), id_of(2)))
        );
        // A joiner from the first account: their own seat is skipped, the next one rotates.
        assert_eq!(
            designated_rotator_for_join(&seats, id_of(1)),
            Some((id_of(3), id_of(4)))
        );
        // A roster the joiner holds alone has nobody to rotate: the founding seat's own mint
        // is the call's first key, and there is no arrival to answer.
        assert_eq!(designated_rotator_for_join(&[seats[0]], id_of(1)), None);
        assert_eq!(designated_rotator_for_join(&[], id_of(1)), None);
    }

    /// The departure rule picks the first survivor — any account, because the leaver is gone
    /// and the earliest remaining seat is the likeliest key-holder.
    #[test]
    fn the_departure_rotator_is_the_first_remaining_seat() {
        let seats = [(id_of(1), id_of(2)), (id_of(3), id_of(4))];

        assert_eq!(
            designated_rotator_for_departure(&seats),
            Some((id_of(1), id_of(2)))
        );
        assert_eq!(
            designated_rotator_for_departure(&seats[1..]),
            Some((id_of(3), id_of(4)))
        );
        // An emptied roster has no rotator: the call itself has ended, and the teardown path
        // takes the seat before any rotation could matter.
        assert_eq!(designated_rotator_for_departure(&[]), None);
    }

    /// Seat changes are exactly-once: the announcement and the joiner's ask can arrive in
    /// either order, and whichever lands second must read a roster that already moved — the
    /// `bool` is what the rotation turns on, so a duplicate must be `false`.
    #[test]
    fn a_seat_arrives_once_and_leaves_once() {
        let mut seats: Vec<(Id, Id)> = Vec::new();

        // The first arrival changes the roster; the same arrival again does not.
        assert!(add_seat(&mut seats, id_of(1), id_of(2)));
        assert!(!add_seat(&mut seats, id_of(1), id_of(2)));

        // One seat per account: a second device of the same account replaces the first, the
        // server's own seat-replacement shape, and the change counts exactly once.
        assert!(add_seat(&mut seats, id_of(1), id_of(9)));
        assert_eq!(seats, vec![(id_of(1), id_of(9))]);
        assert!(!add_seat(&mut seats, id_of(1), id_of(9)));

        // A different account adds, not replaces.
        assert!(add_seat(&mut seats, id_of(3), id_of(4)));
        assert_eq!(seats.len(), 2);

        // Departures mirror arrivals: once, and a redelivery moves nothing.
        assert!(remove_seat(&mut seats, id_of(1)));
        assert!(!remove_seat(&mut seats, id_of(1)));
        assert_eq!(seats, vec![(id_of(3), id_of(4))]);
    }

    /// A spectator-shaped announcement: any count, over a conversation and call the event
    /// names — the three things the ledger reads, with the participant filled in because
    /// the handlers require one before they ask the ledger anything.
    fn spectator_event(conversation_id: Id, call_id: Id, count: Option<u32>) -> CallStateEvent {
        CallStateEvent {
            call_id,
            state: STATE_CONNECTED,
            reason: None,
            conversation_id: Some(conversation_id),
            user_id: Some(id_of(9)),
            device_id: Some(id_of(9)),
            participant_count: count,
            sealed_offer: None,
            participants: None,
        }
    }

    /// The spectator's ledger: a join announcement for a call this device holds no seat in
    /// records the running call (the header's join-in-progress offer is built on it), later
    /// announcements move the count — a departure's as much as a join's, which is how a
    /// device that connects mid-call learns of one — and the count-0 announcement that
    /// names the call's retirement clears the entry, so the offer cannot outlive the call.
    #[test]
    fn a_spectator_records_the_running_call_and_clears_it_when_it_retires() {
        let mut calls = GroupCalls::new();
        let conversation = id_of(1);
        let call = id_of(2);

        // A join announcement, no seat and no ask anywhere: the call is running at three.
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(3))),
            Some(Spectated::Running {
                conversation_id: conversation,
                call_id: call,
                count: 3,
            })
        );
        assert_eq!(calls.in_progress.get(&conversation), Some(&(call, 3)));

        // A departure's count moves the same entry — the size after the change, whichever
        // direction the change went.
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(2))),
            Some(Spectated::Running {
                conversation_id: conversation,
                call_id: call,
                count: 2,
            })
        );
        assert_eq!(calls.in_progress.get(&conversation), Some(&(call, 2)));

        // The retirement: the last seat left, the call no longer exists server-side.
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(0))),
            Some(Spectated::Retired {
                conversation_id: conversation
            })
        );
        assert!(calls.in_progress.get(&conversation).is_none());

        // A retirement of a call the ledger never held has nothing to clear and nothing to
        // say, and a count the event did not carry is not a size to offer anyone.
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(0))),
            None
        );
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, None)),
            None
        );
    }

    /// The ledger is not this device's own call's: a seat in the conversation — or a join
    /// asked and still waiting on its snapshot — binds the announcements to the roster
    /// path instead. Otherwise a joiner would hear its *own* join announced before the
    /// snapshot answered, and be offered the call it is already entering.
    #[test]
    fn a_bound_device_is_not_a_spectator_of_its_own_call() {
        let mut calls = GroupCalls::new();
        let conversation = id_of(1);
        let call = id_of(2);

        calls.seat = Some(GroupSeat {
            call_id: call,
            conversation_id: conversation,
            seats: vec![(id_of(3), id_of(4))],
            key: None,
            key_ask_waiting: false,
        });
        // Seated in the conversation: even another call's announcement (a shape only a
        // server running two calls in one conversation could send) is not recorded — the
        // seat is the header's whole truth for the conversation.
        assert_eq!(
            calls.spectate(&spectator_event(conversation, id_of(5), Some(2))),
            None
        );
        assert!(calls.in_progress.get(&conversation).is_none());

        // A seat in some *other* conversation's call does not bind this one: the device is
        // a spectator of the event's conversation, and the announcement is discovery.
        calls.seat = Some(GroupSeat {
            call_id: id_of(7),
            conversation_id: id_of(6),
            seats: vec![(id_of(3), id_of(4))],
            key: None,
            key_ask_waiting: false,
        });
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(2))),
            Some(Spectated::Running {
                conversation_id: conversation,
                call_id: call,
                count: 2,
            })
        );

        // A join asked and not yet seated binds the same way: what it hears before the
        // snapshot is its own call's business, not the ledger's.
        calls.seat = None;
        calls.in_progress.clear();
        calls.joining = Some(JoinAsk {
            call_id: call,
            conversation_id: conversation,
        });
        assert_eq!(
            calls.spectate(&spectator_event(conversation, call, Some(2))),
            None
        );
        assert!(calls.in_progress.get(&conversation).is_none());
    }

    /// The join's target id, in precedence order: an ask already in flight is the
    /// commitment a second press re-sends (whatever the caller offers — the ask's id is
    /// what makes the retry the *same* join), an offered id is the running call the
    /// header learned of (the whole point of join-in-progress: seat the call everyone
    /// else is in, not a second one minted beside it), and only a press with nothing to
    /// offer mints. The id this returns is the id the `CALL_SFU_JOIN` frame carries —
    /// the same binding flows into the `CallInvite` unchanged.
    #[test]
    fn a_join_targets_the_ask_then_the_offered_call_then_a_mint() {
        let conversation = id_of(1);
        let running = id_of(2);

        // Nothing in flight, a running call offered: the offer is the target, and the ask
        // carries it — a press before the snapshot re-sends the same join of the same
        // call.
        let mut calls = GroupCalls::new();
        assert_eq!(calls.join_target(conversation, Some(running)), running);
        assert_eq!(calls.joining.as_ref().map(|ask| ask.call_id), Some(running));
        assert_eq!(calls.join_target(conversation, Some(running)), running);

        // The ask outranks a different offer: the second press is a retry, not a switch.
        assert_eq!(calls.join_target(conversation, Some(id_of(3))), running);

        // Nothing offered, nothing in flight: a mint, remembered the same way.
        let mut fresh = GroupCalls::new();
        let minted = fresh.join_target(conversation, None);
        assert_eq!(fresh.joining.as_ref().map(|ask| ask.call_id), Some(minted));
        assert_eq!(fresh.join_target(conversation, None), minted);
    }

    /// The whole mid-call join hand-shake in one test: two devices hold a pairwise session,
    /// the seated one seals the running key under the "migo-call-join-v1" wrapper of the
    /// session secret, and the joiner opens it into the same state — same epoch, and a frame
    /// sealed by one opens for the other.
    #[test]
    fn a_sealed_join_distribution_opens_under_the_pairwise_secret() {
        let alice_keys = DeviceKeys::additional();
        let bob_keys = DeviceKeys::additional();
        let alice_device = id_of(1);
        let bob_device = id_of(2);
        let conversation = id_of(3);
        let call_id = id_of(4);
        let mut alice = SessionStore::new(alice_keys);
        let mut bob = SessionStore::new(bob_keys);

        // The session the ask itself would have ridden: Alice initiates against Bob's bundle,
        // Bob answers, and both ends have recorded the same X3DH secret.
        let envelope = alice
            .seal(
                conversation,
                bob_device,
                Some(&published_bundle(bob.keys())),
                b"the ask",
            )
            .expect("seals against the bundle");
        bob.open(conversation, alice_device, &envelope)
            .expect("Bob answers the session");

        let alice_secret = alice
            .pairwise_secret(conversation, bob_device)
            .expect("Alice recorded the secret");
        let bob_secret = bob
            .pairwise_secret(conversation, alice_device)
            .expect("Bob recorded the secret");

        // Alice holds the running call key; the sealed distribution is what her answer carries.
        let distributor = CallKeyState::from_session(&[0xab; 32], call_id);
        let sealed = distributor
            .sealed_join_distribution(&alice_secret, &mut OsRandom)
            .expect("seals under the wrapper");

        // Bob installs it as his baseline — the joiner's first key — and the two agree.
        let joiner = CallKeyState::from_join_distribution(&bob_secret, call_id, &sealed)
            .expect("the joiner's secret opens the wrapper");
        assert_eq!(joiner.epoch(), distributor.epoch());
        let frame = b"the first frame of the call for the joiner";
        let sealed_frame = joiner
            .seal_frame(frame, &mut OsRandom)
            .expect("the joiner seals with the installed key");
        assert_eq!(
            distributor
                .open_frame(&sealed_frame)
                .expect("Alice opens it"),
            frame
        );

        // A stranger's secret — a device that never ran the handshake — cannot open the
        // wrapper, which is the whole point of deriving it from the pairwise session.
        assert!(CallKeyState::from_join_distribution(&[0x41; 32], call_id, &sealed).is_err());
        // And the wrapper is bound to the call: the same secret under another call id fails.
        assert!(CallKeyState::from_join_distribution(&bob_secret, id_of(5), &sealed).is_err());
    }

    /// The rotation round trip: the rotator mints the next epoch under the running key, the
    /// roster adopts it, and the adoption refuses both a replay and a regression — the two
    /// ways a stale or duplicated `CALL_KEY_UPDATE` could otherwise pull the call apart.
    #[test]
    fn a_rotation_adopts_once_and_refuses_to_go_backwards() {
        let call_id = id_of(4);
        let mut rotator = CallKeyState::from_session(&[0xcd; 32], call_id);
        let mut seated = CallKeyState::from_session(&[0xcd; 32], call_id);

        let sealed = rotator
            .rotate(&mut OsRandom)
            .expect("rotation seals under the running key");
        let epoch = rotator.epoch();

        seated
            .adopt(epoch, &sealed)
            .expect("the roster adopts the rotator's epoch");
        assert_eq!(seated.epoch(), epoch);
        // The two states now seal and open the same frames — the rotation handed them a
        // shared key, not a shared number.
        let frame = b"the next epoch's frames";
        let sealed_frame = rotator
            .seal_frame(frame, &mut OsRandom)
            .expect("the rotator seals with the new key");
        assert_eq!(seated.open_frame(&sealed_frame).expect("opens"), frame);

        // The same update again is a replay: adoption refuses, and the state keeps the key
        // that works rather than resetting.
        assert!(seated.adopt(epoch, &sealed).is_err());
        assert_eq!(seated.epoch(), epoch);
        // A blob sealed under a different key does not adopt either, whatever epoch it claims.
        let mut other = CallKeyState::from_session(&[0xef; 32], call_id);
        let stranger = other
            .rotate(&mut OsRandom)
            .expect("a stranger rotates their own call");
        assert!(seated.adopt(epoch + 1, &stranger).is_err());
        assert_eq!(seated.epoch(), epoch);
    }

    /// The bundle a store publishes, as the peer's `KEY_PUBLISH` would deliver it.
    fn published_bundle(keys: &DeviceKeys) -> migo_crypto::x3dh::PrekeyBundle {
        let signed = keys.signed_prekey_signed();
        let one_time = keys.one_time_public().into_iter().next();
        bundle_from_wire(
            &keys.identity_public().to_bytes(),
            signed.key_id,
            &signed.public_key,
            &signed.signature,
            one_time.as_ref().map(|(id, key)| (*id, key.as_slice())),
        )
        .expect("the store's own halves parse")
    }
}
