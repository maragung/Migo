//! The seam between the transport and everything above it.
//!
//! The gateway drives the connection lifecycle, the heartbeat, backpressure, resume, and the
//! handshake opcodes (`HELLO`, `PING`, `AUTHENTICATE`, `SUBSCRIBE`, …). Everything *application*
//! — sending a message, joining a room, playing a move — is opaque to it. Those opcodes are
//! handed to a [`Dispatcher`], the one trait a composition root (the `migod` binary) implements
//! to wire the domain crates in behind the transport.
//!
//! This inversion is what keeps the layering honest (brief section 177): the gateway is
//! transport, the domain is logic, and neither depends on the other's internals. The gateway
//! calls *up* through this trait; it never names a domain crate.
//!
//! # What a dispatcher is handed
//!
//! A [`ClientContext`] carries the authenticated [`Identity`], the request's opcode and
//! correlation, and the two verbs a handler needs: [`reply`](ClientContext::reply) to answer the
//! caller (reusing the request opcode and correlation, per section 139) and
//! [`publish`](ClientContext::publish) to fan a server-initiated event out to a topic's
//! subscribers (encoded once, correlation `0`). A handler either answers with `reply`/`reply_error`
//! and returns `Ok(())`, or returns `Err` and lets the driver send the error frame — both are
//! valid, and the driver never sends a second error for an `Ok` return.
//!
//! # The second question a dispatcher answers
//!
//! `SUBSCRIBE` is a lifecycle opcode the driver owns, so it never reaches
//! [`dispatch`](Dispatcher::dispatch) — but *which* topics a caller may receive is a domain
//! question the gateway cannot answer, because a topic id is a conversation, a room or an account
//! and this crate knows what none of those are. So the driver asks the same trait, through
//! [`authorize_topics`](Dispatcher::authorize_topics), and files only what comes back granted. The
//! subscription registry is the one place where a frame's own contents would otherwise decide what
//! the server sends back: authorization here is *read* from the domain, never trusted from the
//! frame.

use async_trait::async_trait;
use bytes::Bytes;

use migo_auth::Identity;
use migo_core::{Error as CoreError, Id, Timestamp};
use migo_protocol::{fault, DeliveryClass, Encode, Frame, Opcode, Topic};

use crate::codec::{encode_error, encode_message};
use crate::hub::Hub;
use crate::metrics::Meters;
use crate::outbound::PushOutcome;
use crate::session::SessionHandle;

/// Everything one application request is handed: who is asking, what they asked, and the means
/// to answer.
///
/// A [`Dispatcher`] receives this by reference for the duration of one `dispatch` call and must
/// not retain it — every borrow inside is scoped to the request. To decode the request body, a
/// handler calls [`migo_protocol::from_frame`] on the frame it is given; the message type is
/// determined by [`opcode`](ClientContext::opcode).
pub struct ClientContext<'a> {
    identity: &'a Identity,
    session: &'a SessionHandle,
    hub: &'a Hub,
    meters: &'a Meters,
    now: Timestamp,
    opcode: Opcode,
    correlation: u32,
    compression: bool,
}

impl<'a> ClientContext<'a> {
    /// Assembles a context for one request. Called only by the connection driver.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: &'a Identity,
        session: &'a SessionHandle,
        hub: &'a Hub,
        meters: &'a Meters,
        now: Timestamp,
        opcode: Opcode,
        correlation: u32,
        compression: bool,
    ) -> Self {
        Self {
            identity,
            session,
            hub,
            meters,
            now,
            opcode,
            correlation,
            compression,
        }
    }

    /// The authenticated identity behind this request.
    #[must_use]
    pub fn identity(&self) -> &Identity {
        self.identity
    }

    /// The id of the session this request arrived on.
    #[must_use]
    pub fn session_id(&self) -> Id {
        self.session.session_id()
    }

    /// The server's notion of now, sampled once when the frame arrived.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// The opcode of the request being dispatched.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// The correlation of the request, to be echoed on the reply (section 139).
    #[must_use]
    pub fn correlation(&self) -> u32 {
        self.correlation
    }

    /// Answers this request, reusing its opcode and correlation (section 139).
    ///
    /// The reply inherits the request opcode's [`DeliveryClass`], so a reply to a Critical
    /// request is itself Critical and cannot be dropped under backpressure.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the message cannot be encoded — a fault in the server's own
    /// message, never the client's.
    pub fn reply<T: Encode>(&self, message: &T) -> Result<(), CoreError> {
        let encoded = encode_message(
            self.opcode.to_wire(),
            self.correlation,
            message,
            self.compression,
        )?;
        self.send_to_self(encoded, self.opcode.class(), None);
        Ok(())
    }

    /// Answers this request with an error, reusing its opcode and correlation and setting the
    /// `ERROR` flag (section 139/140). Only the error's public face crosses the wire (section 161).
    ///
    /// An error reply is always Critical: a client that asked for something is owed the verdict.
    ///
    /// # Errors
    ///
    /// Returns an internal error only if the tiny error frame itself cannot be encoded.
    pub fn reply_error(&self, error: &CoreError) -> Result<(), CoreError> {
        let encoded = encode_error(
            self.opcode.to_wire(),
            self.correlation,
            error,
            self.compression,
        )?;
        self.send_to_self(encoded, DeliveryClass::Critical, None);
        Ok(())
    }

    /// Fans a server-initiated event out to every subscriber of a topic, encoding it once
    /// (section 136). The event carries correlation `0` (section 139) and the given opcode's
    /// delivery class; `coalesce_key` is honoured only for a [`DeliveryClass::Coalescable`]
    /// opcode, where a newer value for the same key replaces an older one still queued.
    ///
    /// Every subscriber receives it, the requester included. Use
    /// [`publish_excluding_self`](ClientContext::publish_excluding_self) when the requester has
    /// already been told the outcome by [`reply`](ClientContext::reply) and should not also see
    /// the fan-out echo.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the event cannot be encoded.
    pub fn publish<T: Encode>(
        &self,
        topic: &Topic,
        opcode: Opcode,
        message: &T,
        coalesce_key: Option<u64>,
    ) -> Result<(), CoreError> {
        self.publish_inner(topic, opcode, message, coalesce_key, None)
    }

    /// Fans an event out to every subscriber of a topic *except the connection this request
    /// arrived on*, encoding it once.
    ///
    /// This is the verb a mutating handler wants. The handler answers the caller with
    /// [`reply`](ClientContext::reply) — so the caller already knows the outcome — and then
    /// publishes the same change to the topic for everyone else. A domain fan-out is defined to
    /// exclude the originating device (section 156); mapping that to "skip this session" here
    /// means the sender's *other* devices and every other member still receive the event, while
    /// the origin connection is not asked to render a change it just performed.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the event cannot be encoded.
    pub fn publish_excluding_self<T: Encode>(
        &self,
        topic: &Topic,
        opcode: Opcode,
        message: &T,
        coalesce_key: Option<u64>,
    ) -> Result<(), CoreError> {
        self.publish_inner(
            topic,
            opcode,
            message,
            coalesce_key,
            Some(self.session.session_id()),
        )
    }

    /// The shared body of [`publish`](Self::publish) and
    /// [`publish_excluding_self`](Self::publish_excluding_self): encode once, fan out, optionally
    /// skipping one session.
    fn publish_inner<T: Encode>(
        &self,
        topic: &Topic,
        opcode: Opcode,
        message: &T,
        coalesce_key: Option<u64>,
        exclude: Option<Id>,
    ) -> Result<(), CoreError> {
        let encoded = encode_message(opcode.to_wire(), 0, message, self.compression)?;
        self.hub.broadcast(
            topic,
            &encoded,
            opcode.class(),
            coalesce_key,
            self.now,
            exclude,
        );
        Ok(())
    }

    /// Pushes a frame into the requester's own mailbox, counting a drop if backpressure discards it.
    fn send_to_self(&self, encoded: Bytes, class: DeliveryClass, coalesce_key: Option<u64>) {
        let outcome = self
            .session
            .outbound()
            .push(encoded, class, coalesce_key, self.now);
        if let PushOutcome::Dropped(dropped) = outcome {
            self.meters.frame_dropped(dropped);
        }
    }
}

/// Everything a [`Dispatcher`] is told about one `SUBSCRIBE`: who is asking, on which session, and
/// when.
///
/// Deliberately not a [`ClientContext`]. An authorization decision has no business replying to the
/// caller or publishing to a topic, and a type that cannot do either cannot do it by accident. The
/// topics themselves are passed alongside this, as a slice, because the answer has to be a batch:
/// one round trip per topic would put 512 domain reads behind a single frame.
pub struct TopicRequest<'a> {
    identity: &'a Identity,
    session_id: Id,
    now: Timestamp,
}

impl<'a> TopicRequest<'a> {
    /// Assembles the context for one subscription decision.
    ///
    /// Called by the connection driver on every `SUBSCRIBE`, and by the composition root's
    /// integration harness when it drives a dispatcher directly against a built graph.
    #[must_use]
    pub fn new(identity: &'a Identity, session_id: Id, now: Timestamp) -> Self {
        Self {
            identity,
            session_id,
            now,
        }
    }

    /// The authenticated identity behind the `SUBSCRIBE`.
    #[must_use]
    pub fn identity(&self) -> &Identity {
        self.identity
    }

    /// The id of the session that would hold the subscriptions.
    #[must_use]
    pub fn session_id(&self) -> Id {
        self.session_id
    }

    /// The server's notion of now, sampled once when the frame arrived.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.now
    }
}

/// The one trait a composition root implements to give the transport its application logic.
///
/// The gateway calls [`dispatch`](Dispatcher::dispatch) for every application opcode on a
/// `Ready` session — after the handshake, authentication, capability, and rate
/// checks have already passed. Handshake and lifecycle opcodes never reach here; the driver owns
/// them.
///
/// An implementation lives in `migod`, where it may depend on the domain crates the gateway must
/// not. It is held as `Arc<dyn Dispatcher>`, so it must be `Send + Sync`.
#[async_trait]
pub trait Dispatcher: Send + Sync {
    /// Handles one application request.
    ///
    /// Decode the body with [`migo_protocol::from_frame`] against the type named by
    /// [`ClientContext::opcode`]. Answer with [`ClientContext::reply`] /
    /// [`ClientContext::reply_error`] and return `Ok(())`, or return `Err` to have the driver
    /// send the error frame for you — do not do both.
    ///
    /// # Errors
    ///
    /// Returns the error to send back to the client (reusing the request's opcode and
    /// correlation) when the handler chooses not to send its own reply.
    async fn dispatch(&self, context: &ClientContext<'_>, frame: &Frame) -> Result<(), CoreError>;

    /// Decides which of the topics a `SUBSCRIBE` asked for this caller may actually receive.
    ///
    /// Returns one verdict per requested topic, in the requested order: `true` grants the
    /// subscription, `false` refuses it. The driver files the granted ones and reports the rest in
    /// the response's `rejected` list, which carries no reason — so a topic the caller may not have
    /// and a topic that does not exist are the same answer, and `SUBSCRIBE` cannot be used to probe
    /// for which conversations or rooms are real. That conflation is the point, not an omission:
    /// it is the same one every read path in the domain crates already makes.
    ///
    /// # Why the default refuses everything
    ///
    /// A dispatcher that has not thought about topics gets a server that delivers no events, which
    /// is a loud, harmless failure that shows up the first time anybody subscribes. The other
    /// default — granting what was asked for — is a silent one: every session would receive every
    /// conversation it can name, and nothing in the transport would look wrong. So this fails
    /// closed, and [`NoopDispatcher`] inherits that: a node with no domain wired in has nothing to
    /// authorize against and therefore authorizes nothing.
    ///
    /// # Not an error path
    ///
    /// A mask rather than a `Result`: a batch answer cannot report per-topic errors without either
    /// failing the whole frame for one bad topic or leaking which topic was bad. An implementation
    /// whose own lookup fails should answer `false` for the topics it could not decide — the same
    /// posture as everything else here, refuse rather than guess.
    async fn authorize_topics(&self, request: &TopicRequest<'_>, topics: &[Topic]) -> Vec<bool> {
        let _ = request;
        vec![false; topics.len()]
    }
}

/// A dispatcher with no application logic: every application opcode is answered `FEATURE_DISABLED`.
///
/// This is the gateway's self-contained default, so it stands up and speaks the full transport
/// protocol without any domain crate wired in — useful for transport-level tests and for a node
/// deliberately serving only the handshake surface. A real deployment supplies its own.
///
/// It grants no topic either, by inheriting
/// [`authorize_topics`](Dispatcher::authorize_topics)'s refusing default: there is no domain here
/// to ask whether a conversation exists or who is in it, and a node that cannot answer that
/// question must not answer it optimistically.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopDispatcher;

#[async_trait]
impl Dispatcher for NoopDispatcher {
    async fn dispatch(&self, context: &ClientContext<'_>, _frame: &Frame) -> Result<(), CoreError> {
        Err(fault::feature_disabled(context.opcode().name()))
    }
}
