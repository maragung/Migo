//! The auto mode's probe: which of the candidate nodes answers first.
//!
//! # What a probe is
//!
//! One `GET {rest}/health` per candidate, all in flight at once, each under a three-second
//! timeout. migod serves `/health` unauthenticated, and any 2xx means the node is up — the probe
//! deliberately reads no body and no version, because auto mode's question is only "who is
//! fastest to answer", not "who is newest". The first responder wins on elapsed time; ties go to
//! the earlier list entry, so an operator's ordering stays meaningful when two nodes answer in
//! the same millisecond.
//!
//! # Why this is a thread, not the worker
//!
//! The probe runs while the user is looking at the server disclosure on the auth screen, before
//! any session exists — the net worker's whole life is one signed-in session, so borrowing it
//! for a pre-auth question would tangle two lifetimes. Instead a short-lived thread builds a
//! one-thread tokio runtime, probes, and delivers the answer on a plain channel, waking the UI
//! through the egui context the same way the worker's event sink does. The paint loop never
//! awaits: it polls the channel with `try_recv` once per frame.
//!
//! # The transport seam
//!
//! [`resolve`] takes the probe as a closure rather than opening sockets itself, so the pick
//! logic is pinned by tests that answer in microseconds without touching the network — the one
//! untested line left is the real HTTP call, which is exactly the line that cannot be honest in
//! a unit test.

use std::future::Future;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use futures_util::future::join_all;

use crate::config::{rest_base_url, ServerEndpoint};

/// How long one candidate gets to answer `/health`. Three seconds is long enough for a node on
/// another continent under load and short enough that a black-holed address surfaces as "no
/// answer" rather than a hang the user has to outwait.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The index of the fastest responder, or `None` when nobody answered.
///
/// Ties go to the earlier candidate: an operator's `MIGO_SERVERS` ordering is the only ordering
/// the deployment has, and a pick that ignored it would make the list's order a lie.
#[must_use]
pub fn pick_fastest(latencies: &[Option<Duration>]) -> Option<usize> {
    latencies
        .iter()
        .enumerate()
        .filter_map(|(index, latency)| latency.map(|value| (value, index)))
        .min()
        .map(|(_, index)| index)
}

/// Probes every candidate through `probe`, all in flight at once, and returns the fastest
/// responder — or `None` when no candidate answered in time.
///
/// The transport is a closure so tests can answer without opening a socket: the function under
/// test is the racing and the picking, not the HTTP.
pub async fn resolve<F, Fut>(candidates: &[ServerEndpoint], probe: F) -> Option<ServerEndpoint>
where
    F: Fn(&ServerEndpoint) -> Fut,
    Fut: Future<Output = Option<Duration>>,
{
    let probes: Vec<Fut> = candidates.iter().map(|endpoint| probe(endpoint)).collect();
    let latencies: Vec<Option<Duration>> = join_all(probes).await;
    let fastest = pick_fastest(&latencies)?;
    Some(candidates[fastest].clone())
}

/// One candidate's health check: how long `/health` took to answer 2xx, or `None` for anything
/// else — unreachable, refused, too slow, or a non-success status. The distinction does not
/// matter to the pick: a node that answers anything but 2xx is not a node auto mode may choose.
async fn health_latency(http: &reqwest::Client, endpoint: &ServerEndpoint) -> Option<Duration> {
    let started = Instant::now();
    let response = http
        .get(format!("{}/health", rest_base_url(endpoint)))
        .send()
        .await
        .ok()?;
    response.status().is_success().then(|| started.elapsed())
}

/// Spawns the probe thread: every candidate checked in parallel, the fastest responder (or
/// `None`, when nobody answered) delivered on `reply`, and the UI woken through `ctx` — without
/// the repaint a finished probe would wait invisibly for the next input to cause a frame.
///
/// A thread that cannot start, or a runtime that cannot be built, still delivers `None`: the
/// honest answer "auto mode could not resolve" beats a form stuck on a spinner, and the failure
/// then surfaces through the same path a dead network does.
pub fn spawn(
    candidates: Vec<ServerEndpoint>,
    ctx: egui::Context,
    reply: Sender<Option<ServerEndpoint>>,
) {
    let spawned = std::thread::Builder::new()
        .name("migo-server-probe".to_owned())
        .spawn(move || {
            let outcome = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                let http = reqwest::Client::builder()
                    .timeout(PROBE_TIMEOUT)
                    .user_agent(concat!("migo-desktop/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .ok()?;
                Some(runtime.block_on(resolve(&candidates, |endpoint| {
                    let http = http.clone();
                    async move { health_latency(&http, endpoint).await }
                })))
            })()
            .flatten();
            if reply.send(outcome).is_ok() {
                ctx.request_repaint();
            }
        });
    // A thread that never started never sends, and the channel's sender dying is the receiver's
    // "disconnected" — the poll treats that exactly like an empty channel: no answer this frame.
    if spawned.is_err() {
        // Nothing to deliver: the sender drops, the receiver's try_recv reports disconnect, and
        // the poll leaves the form in its probing state rather than inventing a resolution.
        tracing::warn!("migo-desktop: could not start the server probe thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_server_list;

    /// The pick takes the lowest latency, not the first entry: a list ordered by an operator is
    /// a list of candidates, not a ranking.
    #[test]
    fn pick_fastest_takes_the_lowest_latency() {
        let latencies = [
            Some(Duration::from_millis(200)),
            Some(Duration::from_millis(80)),
            Some(Duration::from_millis(400)),
        ];
        assert_eq!(pick_fastest(&latencies), Some(1));
    }

    /// A tie goes to the earlier candidate, so the operator's ordering stays the tiebreaker.
    #[test]
    fn pick_fastest_breaks_ties_by_list_order() {
        let latencies = [
            Some(Duration::from_millis(120)),
            None,
            Some(Duration::from_millis(120)),
        ];
        assert_eq!(pick_fastest(&latencies), Some(0));
    }

    /// Nobody answering is `None`, never a guess at an index: the caller must keep auto mode
    /// honest rather than pinning a server nothing vouched for.
    #[test]
    fn pick_fastest_without_a_responder_is_none() {
        assert_eq!(pick_fastest(&[None, None, None]), None);
        assert_eq!(pick_fastest(&[]), None);
    }

    /// The resolve path races the injected transport and returns the endpoint that answered
    /// fastest, not its index — the caller is the auth form, and it speaks endpoints.
    #[tokio::test]
    async fn resolve_picks_the_fastest_responder_through_the_injected_transport() {
        let candidates = parse_server_list(
            "http://node1.example.com:8080,http://node2.example.com:8080,http://node3.example.com:8080",
        );
        let picked = resolve(&candidates, |endpoint| async move {
            match endpoint.host.as_str() {
                "node1.example.com" => Some(Duration::from_millis(200)),
                "node2.example.com" => Some(Duration::from_millis(80)),
                _ => None,
            }
        })
        .await;
        assert_eq!(
            picked.as_ref().map(|endpoint| endpoint.host.as_str()),
            Some("node2.example.com")
        );
    }

    /// A probe where every candidate fails resolves to `None`: the failure is the answer, and
    /// the form must show it rather than falling back to the first list entry.
    #[tokio::test]
    async fn resolve_without_any_responder_is_none() {
        let candidates =
            parse_server_list("http://node1.example.com:8080,http://node2.example.com:8080");
        let picked = resolve(&candidates, |_endpoint| async move { None }).await;
        assert!(picked.is_none());
    }
}
