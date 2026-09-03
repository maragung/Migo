//! The call sweeper: the ring's undertaker on a timer.
//!
//! # Why this lives in the composition root and not in `migo-calls`
//!
//! A ring that nobody answers has two survivors, and neither can be trusted to
//! end it. The caller whose browser died cannot cancel — nothing is running to
//! send the cancel — and the callee left ringing has nothing to decline, because
//! the service only hears a decline from a client that chose to send one. The
//! one party that always knows the ring is dead is the node holding the row, so
//! the node tells both of them: every tick, [`Callkeeper::sweep`] retires the
//! rings past `expires_at`, and each retired call's
//! [`ended_event`](migo_calls::Call::ended_event) is published to *both* user
//! topics — the caller's screen gives up, and the callee's ring stops without
//! anyone having to decline a call that was never going to connect.
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
            self.clock.clone(),
            self.shutdown.clone(),
        )
    }
}

/// Runs the sweep forever, or until `shutdown` fires.
fn spawn(
    calls: SharedCallkeeper,
    gateway: Arc<Gateway>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(SWEEP_INTERVAL) => {
                    sweep_once(&calls, &gateway, clock.as_ref()).await;
                }
            }
        }
    })
}

/// One tick: retire what expired, tell both parties, log rather than die —
/// a sweeper that crashed on one bad row would leave every later ring ringing.
async fn sweep_once(calls: &SharedCallkeeper, gateway: &Gateway, clock: &dyn Clock) {
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
                }
                tracing::info!(call_id = %call.call_id, "an unanswered ring expired; both parties told");
            }
        }
        Err(error) => tracing::warn!(%error, "the call sweep failed"),
    }
}
