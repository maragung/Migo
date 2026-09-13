//! The typing sweeper: the deadline's other half, spoken out loud.
//!
//! # Why this lives in the composition root and not in `migo-messaging`
//!
//! A typing mark whose TTL lapsed used to die silently, inside the cache,
//! because the only party that could end an indicator was the typer's own
//! client — and the client that most needs to say "stop" is exactly the one
//! that cannot: the app killed mid-word, the tab frozen, the phone in a tunnel
//! past the last refresh. [`migo-messaging`] owns no timer (the same rule that
//! keeps the message sweeper out of it — see
//! [`Messaging::purge_expired`](migo_messaging::Messaging::purge_expired)), so
//! the composition root is where the task that watches the deadline belongs,
//! in the same seat the call sweeper sits in and for the same reason: it must
//! outlive every request and die with the node.
//!
//! Every tick, [`sweep_typing`](migo_messaging::Messaging::sweep_typing)
//! claims the marks whose deadline passed — claiming, not just reading, so the
//! marks leave the cache as they are returned and no second sweep can publish
//! the same expiry — and each claimed mark becomes one `Stop` on the
//! conversation's topic, coalesced by conversation and typer exactly the way
//! the `Start` it answers was. The receiver's local timeout (brief section 15)
//! remains the floor; this is the server-side backstop that says the same
//! thing out loud, so an indicator never outlives its last refresh by more
//! than the TTL plus one tick.
//!
//! # The tick
//!
//! One second, and deliberately not derived from `TYPING_TTL_MS`: the tick is
//! how long a dead typer's mark can outlive its deadline, not how long the
//! mark lasts. Each tick on the memory backend is one map walk, and on Redis
//! one `SCAN` over a keyspace whose keys expire on their own — bounded work
//! either way, which is the whole cost of never leaving "typing…" on a screen
//! forever.

use std::sync::Arc;
use std::time::Duration;

use migo_core::{Clock, Shutdown};
use migo_gateway::Gateway;
use migo_messaging::{Broadcast, SharedMessaging};
use migo_protocol::{Opcode, Topic, TopicKind};

use crate::compose::App;
use crate::dispatch::stream_key;

/// How often the sweeper looks for expired typing marks.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

impl App {
    /// Spawns the typing sweeper, returning its handle.
    ///
    /// [`App::serve`] spawns this for the whole serve, exactly as it spawns the
    /// call and message sweepers. Public for the same reason theirs are: the
    /// behaviour it owns — a recipient hearing `Stop` after a typer who never
    /// sent one — is observable only from outside, so the wire test starts the
    /// task by hand against an app that never serves.
    #[must_use]
    pub fn spawn_typing_sweeper(&self) -> tokio::task::JoinHandle<()> {
        spawn(
            self.messaging.clone(),
            self.gateway.clone(),
            self.clock.clone(),
            self.shutdown.clone(),
            SWEEP_INTERVAL,
        )
    }
}

/// Runs the sweep forever, or until `shutdown` fires. The interval is a
/// parameter so the test can prove the loop publishes without waiting out the
/// production second between ticks.
fn spawn(
    messaging: SharedMessaging,
    gateway: Arc<Gateway>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    sweep_once(messaging.as_ref(), &gateway, clock.as_ref()).await;
                }
            }
        }
    })
}

/// One tick: claim what expired, tell the conversation, log rather than die —
/// a sweeper that crashed on one failure would leave every later expiry
/// ringing on screens that cannot end it themselves.
async fn sweep_once(
    messaging: &dyn migo_messaging::Messaging,
    gateway: &Gateway,
    clock: &dyn Clock,
) {
    let now = clock.now();
    match messaging.sweep_typing(now).await {
        Ok(stops) => {
            for fanout in stops {
                // The frame the handler would have published, minus the socket:
                // the same topic, the same opcode, and the same coalescing key
                // the `Start` it ends was published under, so a subscriber
                // whose queue is backed up collapses the pair into the latest —
                // which is the Stop.
                let Broadcast::Typing(event) = fanout.event else {
                    continue;
                };
                let Some(typer) = event.user_id else {
                    continue;
                };
                gateway.broadcast_to_topic_coalesced(
                    &Topic {
                        kind: TopicKind::Conversation,
                        id: fanout.conversation_id,
                    },
                    Opcode::Typing,
                    &event,
                    stream_key(&(fanout.conversation_id, typer)),
                    now,
                );
                tracing::debug!(
                    conversation = %fanout.conversation_id.to_text(),
                    "a typing mark expired; the conversation was told"
                );
            }
        }
        Err(error) => tracing::warn!(%error, "the typing sweep failed"),
    }
}
