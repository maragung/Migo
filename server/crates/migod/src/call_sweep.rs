//! The call sweeper: the ring's undertaker on a timer.
//!
//! # Why this lives in the composition root and not in `migo-calls`
//!
//! A ring that nobody answers has two survivors, and neither can be trusted to
//! end it. The caller whose browser died cannot cancel — nothing is running to
//! send the cancel — and the callee left ringing has nothing to decline, because
//! the service only hears a decline from a client that chose to send one. The
//! one party that always knows the ring is dead is the node holding the row, so
//! the node tells both of them: every tick,
//! [`sweep`](migo_calls::Callkeeper::sweep) retires the rings past
//! `expires_at`, and each retired call's
//! [`ended_event`](migo_calls::Call::ended_event) is published to *both* user
//! topics — the caller's screen gives up, and the callee's ring stops without
//! anyone having to decline a call that was never going to connect. Both
//! halves ride the user-topic tier across the mesh (section 170, in the
//! `FED_CALL_RELAY` envelope): a ring that dies must die everywhere it rings,
//! and the node holding the row is the one node that knows it died.
//!
//! # The group seats ride the same tick
//!
//! A group seat whose session died is the ring's problem wearing a roster: the
//! participant cannot leave — nothing is running to send the leave — and the
//! roster left behind cannot unseat them, because a leave is the leaver's own
//! fact. The dispatcher stamps the seat gone on the session edge, and every
//! tick [`group_sweep`](migo_calls::Callkeeper::group_sweep) retires the seats
//! whose grace window passed without the re-join, returning each departure as
//! the same `CallSfuEvent` an explicit leave publishes — this task then sends
//! it to the conversation's topic, where the remaining roster hears it and
//! stops rendering a participant whose session is dead (section 166, audit
//! area 6), and across the mesh on the same fan-out tier an explicit leave's
//! departure rides, so the rosters on far nodes hear the retirement too. No
//! coalescing key, for the same reason the explicit-leave handler passes none:
//! no two membership facts may collapse into one.
//!
//! `migo-calls` itself owns no timer (see that crate's docs): the sweep also
//! runs opportunistically inside `invite`, which keeps a quiet node's rows
//! honest between ticks. This task is the half the opportunistic pass cannot
//! do — the *publishing* — because a callee nobody re-invites still deserves to
//! hear the ring die.
//!
//! # The tick
//!
//! One second, and deliberately not derived from `ring_ttl_ms`: the tick is how
//! long a dead ring can outlive its expiry, not how long a ring lasts, and a
//! fixed short interval means the worst-case silence after a browser dies is
//! the TTL plus a second — bounded by configuration, not by this constant.
//! Each tick is one indexed store query on a quiet system, which is the whole
//! cost of never leaving a callee ringing forever.

use std::sync::Arc;
use std::time::Duration;

use migo_calls::SharedCallkeeper;
use migo_core::{Clock, Shutdown};
use migo_gateway::Gateway;
use migo_protocol::{Opcode, Topic, TopicKind};

use crate::compose::App;
use crate::presence_relay::PresenceRelay;

/// How often the sweeper looks for expired rings.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

impl App {
    /// Spawns the call sweeper, returning its handle.
    ///
    /// [`App::serve`] spawns this in production; it is public because the
    /// behaviour it owns — a callee hearing `Ended(NoAnswer)` without anybody
    /// sending anything — is only observable from outside, so the wire test
    /// starts the task by hand against an app that never serves.
    #[must_use]
    pub fn spawn_call_sweeper(&self) -> tokio::task::JoinHandle<()> {
        spawn(
            self.calls.clone(),
            self.gateway.clone(),
            self.presence_relay.clone(),
            Arc::clone(&self.room_relay),
            Arc::clone(&self.conversation_relay),
            self.clock.clone(),
            self.shutdown.clone(),
        )
    }
}

/// Runs the sweep forever, or until `shutdown` fires.
fn spawn(
    calls: SharedCallkeeper,
    gateway: Arc<Gateway>,
    relay: Arc<PresenceRelay>,
    rooms: Arc<crate::room_relay::RoomRelay>,
    conversations: Arc<crate::conversation_relay::ConversationRelay>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(SWEEP_INTERVAL) => {
                    sweep_once(&calls, &gateway, &relay, &rooms, &conversations, clock.as_ref()).await;
                }
            }
        }
    })
}

/// One tick: retire what expired, tell both parties, log rather than die —
/// a sweeper that crashed on one bad row would leave every later ring ringing.
async fn sweep_once(
    calls: &SharedCallkeeper,
    gateway: &Gateway,
    relay: &PresenceRelay,
    rooms: &crate::room_relay::RoomRelay,
    conversations: &crate::conversation_relay::ConversationRelay,
    clock: &dyn Clock,
) {
    let now = clock.now();
    match calls.sweep(now).await {
        Ok(retired) => {
            for call in retired {
                let event = call.ended_event();
                // Both parties, not just the caller: the callee is the one
                // with a screen still ringing, and the caller is the one whose
                // client may have died — the sweep exists for exactly the
                // calls where neither can be relied on to say so.
                for account in [call.caller_id, call.callee_id] {
                    gateway.broadcast_to_topic(
                        &Topic {
                            kind: TopicKind::User,
                            id: account,
                        },
                        Opcode::CallStateEvent,
                        &event,
                        now,
                    );
                    // The federated half: either party's sessions may sit on
                    // another node (section 170's user-topic watch table, in
                    // the FED_CALL_RELAY envelope), and a ring that dies must
                    // die everywhere it rings. Warn-not-fail, exactly as the
                    // local half: a sweeper that died on one bad enqueue would
                    // leave every later ring ringing.
                    if let Err(error) = relay
                        .forward_call(account, Opcode::CallStateEvent, &event, now)
                        .await
                    {
                        tracing::warn!(
                            %error,
                            "cannot enqueue the federated half of an expired ring"
                        );
                    }
                }
                tracing::info!(call_id = %call.call_id, "an unanswered ring expired; both parties told");
            }
        }
        Err(error) => tracing::warn!(%error, "the call sweep failed"),
    }
    // The group seats, same tick: each stamped-gone seat whose grace window
    // passed is retired and its departure announced to the conversation —
    // published here rather than in the handler above because the retirement
    // has no author to exclude, and there is no request in hand to ride on.
    match calls.group_sweep(now).await {
        Ok(departures) => {
            for event in departures {
                let topic = Topic {
                    kind: TopicKind::Conversation,
                    id: event.conversation_id.unwrap_or_default(),
                };
                // Plain broadcast, no coalescing key — the same rule the
                // explicit-leave handler keeps: membership facts never
                // collapse, whatever the opcode's class allows.
                gateway.broadcast_to_topic(&topic, Opcode::CallSfuEvent, &event, now);
                // And the same federated half the explicit leave sends, over
                // the same tier, because a dead seat's roster on another node
                // must hear the retirement or it renders a ghost — the
                // crossing is the sweeper's to owe too, not only the request
                // path's.
                crate::dispatch::calls::forward_announcement(
                    rooms,
                    conversations,
                    event.conversation_id.unwrap_or_default(),
                    &event,
                    now,
                )
                .await;
                tracing::info!(call_id = %event.call_id, "a dead group seat retired; the roster told");
            }
        }
        Err(error) => tracing::warn!(%error, "the group seat sweep failed"),
    }
}
