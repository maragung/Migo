//! The connection driver: one task per socket, running the section 149 lifecycle state machine
//! from the first `HELLO` to the final close.
//!
//! # The shape of a connection
//!
//! A socket arrives as a [`Transport`]. The driver reads the opening `HELLO`, decides whether it
//! is a fresh session or a resume of a dropped one (section 150), sends `WELCOME`, and then loops:
//! read a frame, flush anything queued, tick the heartbeat, honour a shutdown. The loop owns the
//! socket exclusively — every frame the server sends passes through the session's [`Outbound`]
//! mailbox, and the loop is the one writer that drains it — so backpressure, coalescing, and
//! resume (section 151) all have a single, race-free point of enforcement.
//!
//! # Two phases past the handshake, one gate
//!
//! Before a session exists a connection is simply awaiting its `HELLO`; that interval is owned by
//! the handshake step, not modelled as a stored phase. Once established a session is
//! [`AwaitingAuth`] or [`Ready`] (section 149), and the phase gate is expressed through
//! [`Opcode::auth`]: an [`AuthLevel::None`] opcode (`PING`, `ACK`, `AUTHENTICATE`) is legal before
//! authentication; anything needing a user session is a protocol violation until the session is
//! [`Ready`], and the driver closes on it rather than answering. A second `HELLO`, a server-only
//! opcode from the client, or a frame that does not decode are all violations too.
//!
//! # What the driver handles, and what it delegates
//!
//! Handshake and lifecycle opcodes — `HELLO`, `PING`, `ACK`, `AUTHENTICATE`, `SUBSCRIBE`,
//! `UNSUBSCRIBE` — are answered here. Every *application* opcode is handed to the
//! [`Dispatcher`](crate::dispatch::Dispatcher) after the authentication, phase, and rate checks
//! have passed, so the transport never names a domain crate (section 177).
//!
//! [`AwaitingAuth`]: crate::session::Phase::AwaitingAuth
//! [`Ready`]: crate::session::Phase::Ready

use std::net::IpAddr;
use std::sync::Arc;

use tokio::time::{interval, timeout, MissedTickBehavior};

use migo_auth::{Identity, RequestContext};
use migo_core::{Error as CoreError, Id, Timestamp};
use migo_protocol::{
    codes, fault, from_frame, Ack, Acknowledged, AuthLevel, Authenticate, Authenticated,
    CloseReason, DeliveryClass, Encode, Frame, Hello, Limits, Opcode, Ping, Pong, ReconnectHint,
    ResumeRequest, SubscribeRequest, SubscribeResponse, Topic, Welcome, WireError,
    PROTOCOL_VERSION,
};
use migo_ratelimit::{BucketKey, TrustTier, Verdict};

use crate::codec::{encode_error, encode_message};
use crate::config::MAX_SUBSCRIPTIONS;
use crate::dispatch::{ClientContext, TopicRequest};
use crate::metrics::{Closed, HandshakeReject, Meters, Refused, ResumeOutcome};
use crate::outbound::{Outbound, PushOutcome, ResumeBuffer};
use crate::session::{Phase, SessionHandle};
use crate::transport::{Transport, TransportError};
use crate::GatewayInner;

/// The upper bound on the randomized reconnect delay a graceful close suggests (section 149), so a
/// draining node sheds its sessions over a window rather than in a thundering herd.
const RECONNECT_JITTER_MS: u64 = 30_000;

/// Runs one connection to completion: handshake, ready loop, teardown. Returns when the socket is
/// closed for any reason. This is the whole public surface of the module — the driver's internals
/// never leak out.
pub(crate) async fn run<T: Transport>(gateway: &GatewayInner, transport: T, base: RequestContext) {
    let connection = Connection {
        gateway,
        transport,
        compression: gateway.settings.compression,
        base,
    };
    connection.drive().await;
}

/// What handling one frame decided: keep going, or close with a reason.
enum FrameOutcome {
    /// The frame was handled; the session continues.
    Continue,
    /// The session must close, with the given reason for the metric and any reconnect hint.
    Close(Closed),
}

/// A session that has completed its handshake and is being driven.
struct Established {
    session_id: Id,
    outbound: Arc<Outbound>,
    handle: SessionHandle,
    phase: Phase,
    identity: Option<Identity>,
}

/// Fresh-versus-resume, decided from the `HELLO` before a session slot is taken.
enum Plan {
    /// A brand-new session under a freshly minted id.
    Fresh { session_id: Id },
    /// A resume of a dropped session, reusing its id and the retained backlog to redeliver.
    Resume {
        session_id: Id,
        buffer: ResumeBuffer,
        last_seq: u64,
    },
}

impl Plan {
    /// The session id either plan will run under.
    fn session_id(&self) -> Id {
        match self {
            Plan::Fresh { session_id } | Plan::Resume { session_id, .. } => *session_id,
        }
    }
}

/// One connection and everything the driver needs to run it: the shared node state, the socket,
/// and the per-connection request context and compression toggle.
struct Connection<'g, T: Transport> {
    gateway: &'g GatewayInner,
    transport: T,
    base: RequestContext,
    compression: bool,
}

impl<T: Transport> Connection<'_, T> {
    /// The whole lifecycle: handshake, then drive until close, then tear down.
    async fn drive(mut self) {
        let Some(mut established) = self.handshake().await else {
            return;
        };
        let reason = self.ready_loop(&mut established).await;
        self.teardown(established, reason).await;
    }

    /// Establishes or resumes a session and sends `WELCOME`, returning the running session.
    ///
    /// Returns `None` when the connection was rejected or dropped before a session existed — every
    /// such path has already answered (where an answer was owed) and closed the socket, and has
    /// touched no session-lifetime metric, so the live-sessions gauge never moves for a handshake
    /// that did not complete.
    async fn handshake(&mut self) -> Option<Established> {
        let (hello, correlation, now) = self.receive_hello().await?;
        let plan = self.plan_session(hello.resume, now, correlation).await?;
        let session_id = plan.session_id();

        // Admission control: every established session, fresh or resumed, takes one slot.
        if !self.gateway.try_admit() {
            if let Plan::Resume {
                session_id, buffer, ..
            } = plan
            {
                // Hand the buffer back so a later, less loaded moment can still resume it.
                self.gateway.store_resume(session_id, buffer);
            }
            let error = fault::error(codes::TOO_MANY_SESSIONS, "node session ceiling reached")
                .public("server overloaded");
            self.reject(
                HandshakeReject::Overloaded,
                Opcode::Hello.to_wire(),
                correlation,
                &error,
            )
            .await;
            return None;
        }

        // The slot is now held. The only exits below are a failed WELCOME (which releases it and
        // returns None) and success (which hands it to the session, released in teardown).
        let outbound = Arc::new(Outbound::new(
            self.gateway.settings.queue_capacity,
            self.gateway.settings.resume_buffer_frames,
            self.gateway.settings.resume_window_ms,
        ));
        let (resumed, resume_from_seq) = match &plan {
            Plan::Resume {
                buffer, last_seq, ..
            } => {
                outbound.seed_resume(buffer, *last_seq);
                (Some(true), Some(*last_seq))
            }
            Plan::Fresh { .. } => (None, None),
        };

        let (phase, identity) = self
            .inline_auth(hello.access_token.as_deref(), hello.device_id, now)
            .await;

        if !self
            .send_welcome(
                session_id,
                correlation,
                hello.features,
                now,
                resumed,
                resume_from_seq,
                identity.as_ref(),
            )
            .await
        {
            return None;
        }

        let handle = SessionHandle::new(session_id, Arc::clone(&outbound), hello.bandwidth_mode);
        self.gateway.hub.register(handle.clone());
        self.gateway.meters.session_opened();

        Some(Established {
            session_id,
            outbound,
            handle,
            phase,
            identity,
        })
    }

    /// Reads and validates the opening `HELLO`: decodes it, checks the opcode and protocol
    /// version, and charges the pre-auth rate limit. Returns the greeting, its correlation, and
    /// the arrival time, or `None` after answering and closing on any failure.
    async fn receive_hello(&mut self) -> Option<(Hello, u32, Timestamp)> {
        let gateway = self.gateway;

        let Ok(Ok(Some(first))) =
            timeout(gateway.settings.handshake_timeout, self.transport.recv()).await
        else {
            // No HELLO within the deadline, or the socket closed or erred first: nothing to
            // answer, so just let go of the socket.
            self.transport.close().await;
            return None;
        };
        gateway.meters.frame_in();
        let now = gateway.now();

        let frame = match Frame::decode(first) {
            Ok(frame) => frame,
            Err(error) => {
                self.reject(
                    HandshakeReject::ProtocolViolation,
                    Opcode::Error.to_wire(),
                    0,
                    &fault::from_wire(error),
                )
                .await;
                return None;
            }
        };
        let correlation = frame.header.correlation;

        if Opcode::from_wire(frame.header.opcode) != Some(Opcode::Hello) {
            let error =
                fault::unexpected_opcode(frame.header.opcode, "the session is awaiting HELLO")
                    .public("expected HELLO");
            self.reject(
                HandshakeReject::ProtocolViolation,
                frame.header.opcode,
                correlation,
                &error,
            )
            .await;
            return None;
        }

        let hello = match from_frame::<Hello>(&frame) {
            Ok(hello) => hello,
            Err(error) => {
                self.reject(
                    HandshakeReject::ProtocolViolation,
                    Opcode::Hello.to_wire(),
                    correlation,
                    &fault::from_wire(error),
                )
                .await;
                return None;
            }
        };

        if hello.protocol_version != PROTOCOL_VERSION {
            let error = fault::error(
                codes::PROTOCOL_VERSION_UNSUPPORTED,
                format!(
                    "client requested protocol version {}",
                    hello.protocol_version
                ),
            )
            .public("unsupported protocol version");
            self.reject(
                HandshakeReject::VersionUnsupported,
                Opcode::Hello.to_wire(),
                correlation,
                &error,
            )
            .await;
            return None;
        }

        // Pre-auth rate limit: the opening HELLO is charged against the peer IP at the anonymous
        // tier, so a single address cannot open sessions without bound.
        match rate_check(gateway, None, self.base.ip, Opcode::Hello, now).await {
            Ok(verdict) if verdict.is_allowed() => {}
            Ok(verdict) => {
                gateway.meters.rate_limited();
                let error = fault::rate_limited(verdict.retry_after_ms().unwrap_or(0));
                self.fail(Opcode::Hello.to_wire(), correlation, &error)
                    .await;
                return None;
            }
            Err(error) => {
                self.fail(Opcode::Hello.to_wire(), correlation, &error)
                    .await;
                return None;
            }
        }

        Some((hello, correlation, now))
    }

    /// Decides fresh-versus-resume from the `HELLO`'s resume request, before a session slot is
    /// taken (section 150). A resume that cannot be served is answered `RESUME_REQUIRED` and
    /// yields `None`; the caller then never admits.
    async fn plan_session(
        &mut self,
        resume: Option<ResumeRequest>,
        now: Timestamp,
        correlation: u32,
    ) -> Option<Plan> {
        let gateway = self.gateway;
        let Some(request) = resume else {
            return Some(Plan::Fresh {
                session_id: gateway.new_session_id(now),
            });
        };
        match gateway.take_resume(request.session_id) {
            Some(buffer) if buffer.covers(request.last_frame_seq, now) => Some(Plan::Resume {
                session_id: request.session_id,
                buffer,
                last_seq: request.last_frame_seq,
            }),
            Some(_) => {
                // The buffer no longer bridges the gap: a Critical frame aged out or was evicted
                // unacknowledged, so only a full resync can repair it.
                gateway.meters.resume(ResumeOutcome::Rejected);
                let error = fault::error(
                    codes::RESUME_REQUIRED,
                    "resume window does not cover the requested sequence",
                )
                .public("resume required");
                self.fail(Opcode::Hello.to_wire(), correlation, &error)
                    .await;
                None
            }
            None => {
                gateway.meters.resume(ResumeOutcome::Unknown);
                let error =
                    fault::error(codes::RESUME_REQUIRED, "no resumable session for that id")
                        .public("resume required");
                self.fail(Opcode::Hello.to_wire(), correlation, &error)
                    .await;
                None
            }
        }
    }

    /// Optionally authenticates from a token carried in the `HELLO`, promoting straight to
    /// [`Ready`](Phase::Ready). A bad inline token is not fatal — the session stays in
    /// [`AwaitingAuth`](Phase::AwaitingAuth) and may still present a valid one with `AUTHENTICATE`.
    async fn inline_auth(
        &self,
        token: Option<&str>,
        device_id: Option<Id>,
        now: Timestamp,
    ) -> (Phase, Option<Identity>) {
        if let (Some(token), Some(device_id)) = (token, device_id) {
            let mut context = self.base.clone();
            context.now = now;
            if let Ok(identity) = self
                .gateway
                .authenticator
                .authenticate(token, device_id, &context)
                .await
            {
                return (Phase::Ready, Some(identity));
            }
        }
        (Phase::AwaitingAuth, None)
    }

    /// Builds and sends `WELCOME`, the first frame out (section 139: it reuses the `HELLO` opcode
    /// and correlation). Returns whether it was sent; on failure the admission slot is released
    /// and the socket closed, and the caller returns `None`.
    #[allow(clippy::too_many_arguments)]
    async fn send_welcome(
        &mut self,
        session_id: Id,
        correlation: u32,
        client_features: u64,
        now: Timestamp,
        resumed: Option<bool>,
        resume_from_seq: Option<u64>,
        identity: Option<&Identity>,
    ) -> bool {
        let gateway = self.gateway;
        let welcome = Welcome {
            session_id,
            node: gateway.node.clone(),
            features: client_features & gateway.features,
            server_time: now,
            limits: Limits {
                max_frame_bytes: u32::try_from(migo_wire::limits::MAX_FRAME_BYTES)
                    .unwrap_or(u32::MAX),
                max_batch_items: u32::try_from(migo_wire::limits::MAX_BATCH_ITEMS)
                    .unwrap_or(u32::MAX),
                max_subscriptions: u32::try_from(MAX_SUBSCRIPTIONS).unwrap_or(u32::MAX),
                heartbeat_ms: u32::try_from(gateway.settings.heartbeat.as_millis())
                    .unwrap_or(u32::MAX),
            },
            resumed,
            resume_from_seq,
            authenticated_user: identity.map(Identity::account_id),
        };
        let bytes = match encode_message(
            Opcode::Hello.to_wire(),
            correlation,
            &welcome,
            self.compression,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(?error, "failed to encode WELCOME");
                gateway.release();
                self.transport.close().await;
                return false;
            }
        };
        if self.transport.send(bytes).await.is_err() {
            gateway.release();
            self.transport.close().await;
            return false;
        }
        gateway.meters.frames_out(1);
        if resumed.is_some() {
            gateway.meters.resume(ResumeOutcome::Resumed);
        }
        true
    }

    /// The steady-state loop: flush, then wait on the socket, the mailbox, the heartbeat tick, or
    /// shutdown, until one of them ends the session. Returns the close reason.
    async fn ready_loop(&mut self, established: &mut Established) -> Closed {
        let shutdown = self.gateway.shutdown.clone();
        let outbound = Arc::clone(&established.outbound);
        let heartbeat_deadline_ms = u64::try_from(self.gateway.settings.heartbeat.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_mul(2);
        let lagging_deadline_ms = self.gateway.settings.lagging_deadline_ms;
        let mut ticker = interval(self.gateway.settings.tick);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_seen = self.gateway.now();

        loop {
            if !self.flush(&outbound).await {
                return Closed::TransportError;
            }
            tokio::select! {
                () = shutdown.cancelled() => {
                    return self.graceful_shutdown(&outbound).await;
                }
                incoming = self.transport.recv() => {
                    match incoming {
                        Ok(Some(bytes)) => {
                            self.gateway.meters.frame_in();
                            last_seen = self.gateway.now();
                            let frame = match Frame::decode(bytes) {
                                Ok(frame) => frame,
                                Err(error) => {
                                    push_error(
                                        &outbound,
                                        &self.gateway.meters,
                                        Opcode::Error.to_wire(),
                                        0,
                                        &fault::from_wire(error),
                                        last_seen,
                                        self.compression,
                                    );
                                    let _ = self.flush(&outbound).await;
                                    return Closed::ProtocolViolation;
                                }
                            };
                            match self.dispatch_frame(established, &outbound, frame).await {
                                FrameOutcome::Continue => {}
                                FrameOutcome::Close(reason) => {
                                    let _ = self.flush(&outbound).await;
                                    return reason;
                                }
                            }
                        }
                        Ok(None) | Err(TransportError::Closed) => return Closed::ClientRequest,
                        Err(TransportError::Protocol(_)) => return Closed::ProtocolViolation,
                        Err(TransportError::Io(_)) => return Closed::TransportError,
                    }
                }
                () = outbound.wait() => {
                    // A push woke us; the loop head will flush it. If the mailbox was closed out
                    // from under us, treat it as a server-side teardown.
                    if outbound.is_closed() {
                        return Closed::ServerShutdown;
                    }
                }
                _ = ticker.tick() => {
                    let now = self.gateway.now();
                    if now.saturating_since(last_seen) > heartbeat_deadline_ms {
                        return Closed::HeartbeatTimeout;
                    }
                    if outbound.lagging_expired(now, lagging_deadline_ms) {
                        return Closed::SessionLagging;
                    }
                    if let Some(identity) = established.identity.as_ref() {
                        if !identity.claims.is_live(now) {
                            return Closed::AuthExpired;
                        }
                    }
                }
            }
        }
    }

    /// Sends a randomized `RECONNECT_HINT` and closes, so a draining node's sessions reconnect
    /// spread over a window rather than all at once (section 149).
    async fn graceful_shutdown(&mut self, outbound: &Outbound) -> Closed {
        let _ = self.flush(outbound).await;
        let after_ms = u32::try_from(self.gateway.jitter(RECONNECT_JITTER_MS)).unwrap_or(u32::MAX);
        let hint = ReconnectHint {
            reason: CloseReason::ServerShutdown,
            after_ms,
            endpoint: None,
        };
        // Server-initiated, so correlation 0 (section 139). Sent directly as the last frame out.
        match encode_message(Opcode::ReconnectHint.to_wire(), 0, &hint, self.compression) {
            Ok(bytes) => {
                if self.transport.send(bytes).await.is_ok() {
                    self.gateway.meters.frames_out(1);
                }
            }
            Err(error) => tracing::warn!(?error, "failed to encode the reconnect hint"),
        }
        Closed::ServerShutdown
    }

    /// Drains every queued frame to the socket. Returns `false` if the socket write failed, which
    /// the caller turns into a transport-error close.
    async fn flush(&mut self, outbound: &Outbound) -> bool {
        let ready = outbound.take_ready();
        if ready.is_empty() {
            return true;
        }
        let count = ready.len() as u64;
        for bytes in ready {
            if self.transport.send(bytes).await.is_err() {
                return false;
            }
        }
        self.gateway.meters.frames_out(count);
        true
    }

    /// Applies the section 149 phase gate to one decoded frame, then either answers a lifecycle
    /// opcode directly or delegates an application opcode to its handler.
    async fn dispatch_frame(
        &self,
        established: &mut Established,
        outbound: &Outbound,
        frame: Frame,
    ) -> FrameOutcome {
        let now = self.gateway.now();
        let opcode_raw = frame.header.opcode;
        let correlation = frame.header.correlation;
        let meters = &self.gateway.meters;

        let Some(opcode) = Opcode::from_wire(opcode_raw) else {
            // An opcode this build does not know: answer and keep going, since a newer client
            // speaking an unknown verb is not a framing violation.
            let error = fault::error(codes::UNKNOWN_OPCODE, "opcode not known to this build")
                .public("unknown opcode");
            push_error(
                outbound,
                meters,
                opcode_raw,
                correlation,
                &error,
                now,
                self.compression,
            );
            return FrameOutcome::Continue;
        };

        // A server-only or server-to-client opcode arriving from a client is a violation.
        if !opcode.accepts_from_client() {
            let error =
                fault::unexpected_opcode(opcode_raw, "this opcode is not accepted from a client");
            push_error(
                outbound,
                meters,
                opcode_raw,
                correlation,
                &error,
                now,
                self.compression,
            );
            return FrameOutcome::Close(Closed::ProtocolViolation);
        }

        // A second HELLO after the handshake is complete is a violation.
        if opcode == Opcode::Hello {
            let error = fault::unexpected_opcode(opcode_raw, "the handshake is already complete");
            push_error(
                outbound,
                meters,
                opcode_raw,
                correlation,
                &error,
                now,
                self.compression,
            );
            return FrameOutcome::Close(Closed::ProtocolViolation);
        }

        // Section 149 phase gate: an opcode needing a user session may not arrive before one
        // exists. AuthLevel::None opcodes (PING, ACK, AUTHENTICATE) are legal in any phase.
        if !matches!(opcode.auth(), AuthLevel::None) && established.phase != Phase::Ready {
            let error = fault::unexpected_opcode(opcode_raw, "the session is not authenticated");
            push_error(
                outbound,
                meters,
                opcode_raw,
                correlation,
                &error,
                now,
                self.compression,
            );
            return FrameOutcome::Close(Closed::ProtocolViolation);
        }

        match opcode {
            Opcode::Ping => self.handle_ping(outbound, &frame, correlation, now),
            Opcode::Ack => {
                // A cumulative watermark that retires held Critical frames (section 151). It is
                // advisory and uncorrelated, so a malformed ACK is ignored rather than fatal.
                if let Ok(ack) = from_frame::<Ack>(&frame) {
                    outbound.acknowledge(ack.frame_seq);
                }
                FrameOutcome::Continue
            }
            Opcode::Authenticate => {
                self.handle_authenticate(established, outbound, &frame, correlation, now)
                    .await
            }
            Opcode::Subscribe => {
                self.handle_subscribe(established, outbound, &frame, correlation, now)
                    .await
            }
            Opcode::Unsubscribe => {
                self.handle_unsubscribe(established, outbound, &frame, correlation, now)
                    .await
            }
            _ => {
                self.handle_application(established, outbound, opcode, &frame, correlation, now)
                    .await
            }
        }
    }

    /// Answers a `PING` with a `PONG` (section 139: reusing the `PING` opcode — there is no
    /// distinct pong). A malformed body closes the session.
    fn handle_ping(
        &self,
        outbound: &Outbound,
        frame: &Frame,
        correlation: u32,
        now: Timestamp,
    ) -> FrameOutcome {
        match from_frame::<Ping>(frame) {
            Ok(ping) => {
                let reply = Pong {
                    client_time: ping.client_time,
                    server_time: now,
                };
                push_message(
                    outbound,
                    &self.gateway.meters,
                    Opcode::Ping,
                    correlation,
                    &reply,
                    DeliveryClass::Critical,
                    now,
                    self.compression,
                );
                FrameOutcome::Continue
            }
            Err(error) => {
                self.close_malformed(outbound, Opcode::Ping.to_wire(), correlation, error, now)
            }
        }
    }

    /// Verifies an `AUTHENTICATE`, promoting the session to [`Ready`](Phase::Ready) and answering
    /// `AUTHENTICATED` on success, or an opaque error on failure (section 161: a bad token and a
    /// missing one look the same to the client).
    async fn handle_authenticate(
        &self,
        established: &mut Established,
        outbound: &Outbound,
        frame: &Frame,
        correlation: u32,
        now: Timestamp,
    ) -> FrameOutcome {
        if !self
            .charge_or_reject(
                outbound,
                established.identity.as_ref(),
                Opcode::Authenticate,
                correlation,
                now,
            )
            .await
        {
            return FrameOutcome::Continue;
        }
        let request = match from_frame::<Authenticate>(frame) {
            Ok(request) => request,
            Err(error) => {
                return self.close_malformed(
                    outbound,
                    Opcode::Authenticate.to_wire(),
                    correlation,
                    error,
                    now,
                );
            }
        };
        let mut context = self.base.clone();
        context.now = now;
        match self
            .gateway
            .authenticator
            .authenticate(&request.access_token, request.device_id, &context)
            .await
        {
            Ok(identity) => {
                let acknowledged = Authenticated {
                    user_id: identity.account_id(),
                    device_id: identity.device_id(),
                    capabilities: identity.capabilities.bits(),
                    profile: None,
                };
                established.identity = Some(identity);
                established.phase = Phase::Ready;
                push_message(
                    outbound,
                    &self.gateway.meters,
                    Opcode::Authenticate,
                    correlation,
                    &acknowledged,
                    DeliveryClass::Critical,
                    now,
                    self.compression,
                );
            }
            Err(error) => {
                push_error(
                    outbound,
                    &self.gateway.meters,
                    Opcode::Authenticate.to_wire(),
                    correlation,
                    &error,
                    now,
                    self.compression,
                );
            }
        }
        FrameOutcome::Continue
    }

    /// Adds the requested topics to the session and answers with the accepted and rejected sets.
    async fn handle_subscribe(
        &self,
        established: &mut Established,
        outbound: &Outbound,
        frame: &Frame,
        correlation: u32,
        now: Timestamp,
    ) -> FrameOutcome {
        if !self
            .charge_or_reject(
                outbound,
                established.identity.as_ref(),
                Opcode::Subscribe,
                correlation,
                now,
            )
            .await
        {
            return FrameOutcome::Continue;
        }
        let request = match from_frame::<SubscribeRequest>(frame) {
            Ok(request) => request,
            Err(error) => {
                return self.close_malformed(
                    outbound,
                    Opcode::Subscribe.to_wire(),
                    correlation,
                    error,
                    now,
                );
            }
        };
        let Some(identity) = established.identity.as_ref() else {
            // Unreachable: SUBSCRIBE carries AuthLevel::User, so the phase gate above has already
            // refused this frame on a session that has not authenticated.
            return FrameOutcome::Close(Closed::ProtocolViolation);
        };

        // The surplus over the per-session ceiling is refused before the dispatcher is asked
        // anything, and that ordering is the point rather than an optimisation. A frame is bounded
        // by MAX_FRAME_BYTES, and a topic costs about eighteen bytes on the wire, so one frame can
        // name tens of thousands of them -- and asking the domain about each would turn a single
        // client frame into tens of thousands of membership reads. Nothing past the ceiling could
        // ever be held by this session anyway, so refusing it costs the caller nothing it could
        // have had.
        let (asked, surplus) = if request.topics.len() > MAX_SUBSCRIPTIONS {
            request.topics.split_at(MAX_SUBSCRIPTIONS)
        } else {
            (&request.topics[..], &[][..])
        };
        self.gateway
            .meters
            .subscriptions_refused(Refused::Cap, surplus.len() as u64);

        // Authorization is read from the dispatcher, never taken from the frame. The gateway
        // cannot make this decision itself: a topic id is a conversation, a room or an account, and
        // this crate knows what none of those are (section 177).
        let granted = self
            .gateway
            .dispatcher
            .authorize_topics(
                &TopicRequest::new(identity, established.session_id, now),
                asked,
            )
            .await;
        // A mask that does not line up with the topics it answers is a bug in the dispatcher, and
        // the only safe reading of a bug in an authorization answer is that nothing was granted.
        let aligned = granted.len() == asked.len();
        let mut permitted = Vec::with_capacity(asked.len());
        let mut rejected: Vec<Topic> = surplus.to_vec();
        for (index, topic) in asked.iter().enumerate() {
            if aligned && granted[index] {
                permitted.push(topic.clone());
            } else {
                rejected.push(topic.clone());
            }
        }
        self.gateway.meters.subscriptions_refused(
            Refused::Unauthorized,
            (rejected.len() - surplus.len()) as u64,
        );

        let subscribed = self
            .gateway
            .hub
            .subscribe(established.session_id, &permitted);
        // One refusal list for three reasons -- over the ceiling, not granted, and the hub's own
        // cap -- carrying no reason for any of them. That is deliberate: a caller who could tell
        // "you may not have this topic" from "no such topic" would have a probe for which
        // conversations and rooms exist, and SUBSCRIBE would answer it 512 topics at a time. The
        // ordering within the list is not a contract; the set is.
        rejected.extend(subscribed.rejected);
        let response = SubscribeResponse {
            accepted: subscribed.accepted,
            rejected: if rejected.is_empty() {
                None
            } else {
                Some(rejected)
            },
        };
        push_message(
            outbound,
            &self.gateway.meters,
            Opcode::Subscribe,
            correlation,
            &response,
            DeliveryClass::Critical,
            now,
            self.compression,
        );
        FrameOutcome::Continue
    }

    /// Removes the requested topics from the session and acknowledges.
    async fn handle_unsubscribe(
        &self,
        established: &mut Established,
        outbound: &Outbound,
        frame: &Frame,
        correlation: u32,
        now: Timestamp,
    ) -> FrameOutcome {
        if !self
            .charge_or_reject(
                outbound,
                established.identity.as_ref(),
                Opcode::Unsubscribe,
                correlation,
                now,
            )
            .await
        {
            return FrameOutcome::Continue;
        }
        let request = match from_frame::<SubscribeRequest>(frame) {
            Ok(request) => request,
            Err(error) => {
                return self.close_malformed(
                    outbound,
                    Opcode::Unsubscribe.to_wire(),
                    correlation,
                    error,
                    now,
                );
            }
        };
        self.gateway
            .hub
            .unsubscribe(established.session_id, &request.topics);
        let acknowledged = Acknowledged { ok: true };
        push_message(
            outbound,
            &self.gateway.meters,
            Opcode::Unsubscribe,
            correlation,
            &acknowledged,
            DeliveryClass::Critical,
            now,
            self.compression,
        );
        FrameOutcome::Continue
    }

    /// Hands one application opcode to the [`Dispatcher`](crate::dispatch::Dispatcher) with a
    /// context scoped to this single request, after charging its rate-limit buckets.
    async fn handle_application(
        &self,
        established: &mut Established,
        outbound: &Outbound,
        opcode: Opcode,
        frame: &Frame,
        correlation: u32,
        now: Timestamp,
    ) -> FrameOutcome {
        if !self
            .charge_or_reject(
                outbound,
                established.identity.as_ref(),
                opcode,
                correlation,
                now,
            )
            .await
        {
            return FrameOutcome::Continue;
        }
        let Some(identity) = established.identity.as_ref() else {
            // Unreachable: the phase gate guarantees an identity on a Ready session.
            return FrameOutcome::Close(Closed::ProtocolViolation);
        };
        let context = ClientContext::new(
            identity,
            &established.handle,
            &self.gateway.hub,
            &self.gateway.meters,
            now,
            opcode,
            correlation,
            self.compression,
        );
        if let Err(error) = self.gateway.dispatcher.dispatch(&context, frame).await {
            // The handler chose to let the driver send the error (section 139 reply rules).
            let _ = context.reply_error(&error);
        }
        FrameOutcome::Continue
    }

    /// Answers a malformed request body with a decode error and closes the session, since a frame
    /// that decoded as a header but not as its declared message is a framing violation.
    fn close_malformed(
        &self,
        outbound: &Outbound,
        opcode: u32,
        correlation: u32,
        error: WireError,
        now: Timestamp,
    ) -> FrameOutcome {
        push_error(
            outbound,
            &self.gateway.meters,
            opcode,
            correlation,
            &fault::from_wire(error),
            now,
            self.compression,
        );
        FrameOutcome::Close(Closed::ProtocolViolation)
    }

    /// Charges one frame against the rate limiter, answering `RATE_LIMITED` and returning `false`
    /// if it is rejected. A rejection is transient — the session is not closed, only throttled.
    async fn charge_or_reject(
        &self,
        outbound: &Outbound,
        identity: Option<&Identity>,
        opcode: Opcode,
        correlation: u32,
        now: Timestamp,
    ) -> bool {
        let meters = &self.gateway.meters;
        match rate_check(self.gateway, identity, self.base.ip, opcode, now).await {
            Ok(verdict) if verdict.is_allowed() => true,
            Ok(verdict) => {
                meters.rate_limited();
                let error = fault::rate_limited(verdict.retry_after_ms().unwrap_or(0));
                push_error(
                    outbound,
                    meters,
                    opcode.to_wire(),
                    correlation,
                    &error,
                    now,
                    self.compression,
                );
                false
            }
            Err(error) => {
                push_error(
                    outbound,
                    meters,
                    opcode.to_wire(),
                    correlation,
                    &error,
                    now,
                    self.compression,
                );
                false
            }
        }
    }

    /// Sends an error frame directly on the socket and closes it. Used only during the handshake,
    /// before a mailbox exists; it touches no session-lifetime metric.
    async fn fail(&mut self, opcode: u32, correlation: u32, error: &CoreError) {
        if let Ok(bytes) = encode_error(opcode, correlation, error, self.compression) {
            if self.transport.send(bytes).await.is_ok() {
                self.gateway.meters.frames_out(1);
            }
        }
        self.transport.close().await;
    }

    /// As [`fail`](Self::fail), but first counts a categorized handshake rejection (section 174:
    /// closed enums only, never a per-peer label).
    async fn reject(
        &mut self,
        reason: HandshakeReject,
        opcode: u32,
        correlation: u32,
        error: &CoreError,
    ) {
        self.gateway.meters.handshake_rejected(reason);
        self.fail(opcode, correlation, error).await;
    }

    /// Deregisters the session, retains resume state for an involuntary close, and closes the
    /// socket — balancing the admission slot and the live-sessions gauge taken at handshake.
    async fn teardown(&mut self, established: Established, reason: Closed) {
        let gateway = self.gateway;
        gateway.hub.deregister(established.session_id);
        // Retain resume state only for an involuntary close of an authenticated session, so a
        // reconnect can bridge the gap (section 150). A clean client-driven close keeps nothing.
        if established.identity.is_some() && retains_resume(reason) {
            let buffer = established.outbound.resume_buffer(gateway.now());
            gateway.store_resume(established.session_id, buffer);
        }
        established.outbound.close();
        gateway.release();
        gateway.meters.session_closed(reason);
        self.transport.close().await;
    }
}

/// Whether a close reason warrants keeping the session's resume backlog for a reconnect. Only
/// involuntary closes — the server's doing or the network's — retain; a deliberate client close,
/// a protocol violation, or an expired credential keep nothing.
fn retains_resume(reason: Closed) -> bool {
    matches!(
        reason,
        Closed::ServerShutdown
            | Closed::NodeDraining
            | Closed::SessionLagging
            | Closed::HeartbeatTimeout
            | Closed::TransportError
            | Closed::Rebalance
    )
}

/// Builds the rate-limit bucket keys for one frame and charges them, treating an empty key set as
/// free.
///
/// Pre-auth, the only key is the peer IP and its endpoint bucket at the anonymous tier; post-auth,
/// the account and its endpoint bucket join, at the identity's tier. When there is no addressable
/// bucket at all — an in-memory transport with no peer IP on an unauthenticated session — the
/// frame is free, because charging an empty key set is itself a `VALIDATION_FAILED`.
async fn rate_check(
    gateway: &GatewayInner,
    identity: Option<&Identity>,
    ip: Option<IpAddr>,
    opcode: Opcode,
    now: Timestamp,
) -> Result<Verdict, CoreError> {
    let mut keys = Vec::with_capacity(3);
    match identity {
        Some(identity) => {
            let account = identity.account_id();
            keys.push(BucketKey::endpoint_of_account(account, opcode));
            keys.push(BucketKey::account(account));
            if let Some(ip) = ip {
                keys.push(BucketKey::ip(ip));
            }
        }
        None => {
            if let Some(ip) = ip {
                keys.push(BucketKey::endpoint_of_ip(ip, opcode));
                keys.push(BucketKey::ip(ip));
            }
        }
    }
    if keys.is_empty() {
        return Ok(Verdict::Free);
    }
    let tier = identity.map_or(TrustTier::Anonymous, |identity| identity.tier);
    gateway
        .rate_limiter
        .charge_opcode(&keys, opcode, tier, now)
        .await
}

/// Encodes a message and pushes it into a mailbox, counting a drop if backpressure discards it. An
/// encode failure is the server's own bug, logged and swallowed rather than killing the session.
#[allow(clippy::too_many_arguments)]
fn push_message<M: Encode>(
    outbound: &Outbound,
    meters: &Meters,
    opcode: Opcode,
    correlation: u32,
    message: &M,
    class: DeliveryClass,
    now: Timestamp,
    compression: bool,
) {
    match encode_message(opcode.to_wire(), correlation, message, compression) {
        Ok(bytes) => {
            if let PushOutcome::Dropped(dropped) = outbound.push(bytes, class, None, now) {
                meters.frame_dropped(dropped);
            }
        }
        Err(error) => {
            tracing::warn!(
                opcode = opcode.name(),
                ?error,
                "failed to encode an outbound reply"
            );
        }
    }
}

/// Encodes an error and pushes it into a mailbox as a Critical frame — a client that asked for
/// something is owed the verdict, so an error reply is never dropped.
fn push_error(
    outbound: &Outbound,
    meters: &Meters,
    opcode: u32,
    correlation: u32,
    error: &CoreError,
    now: Timestamp,
    compression: bool,
) {
    match encode_error(opcode, correlation, error, compression) {
        Ok(bytes) => {
            if let PushOutcome::Dropped(dropped) =
                outbound.push(bytes, DeliveryClass::Critical, None, now)
            {
                meters.frame_dropped(dropped);
            }
        }
        Err(inner) => {
            tracing::warn!(?inner, "failed to encode an error reply");
        }
    }
}
