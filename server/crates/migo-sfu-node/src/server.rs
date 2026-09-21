//! The socket: a QUIC listener that admits a device with a ticket and then moves its frames.
//!
//! # Why QUIC
//!
//! A media plane needs two things one ordered stream cannot give it at once: a reliable, ordered
//! control channel, and a way to carry frames where a lost one costs the frame and not the
//! stream. QUIC supplies both on one connection — a bidirectional stream for control and
//! datagrams for media — over TLS 1.3, so the sealed frames ride a channel an observer cannot even
//! measure the shape of. The gateway's own optional QUIC listener takes the same posture (section
//! 138), and this module mirrors it: a self-signed leaf minted at boot, one stream per session, a
//! keep-alive pair sized from the configured heartbeat.
//!
//! A self-signed certificate gives the connection confidentiality and integrity; it does not
//! prove the server's identity, and that is the right trade here rather than a shortcut. A
//! client's admission is decided by the ticket, which the node it already trusts signed, and the
//! media it sends is sealed under the call's own keys. A forged certificate would buy an attacker
//! the ability to forward sealed frames, which is what an SFU is for.
//!
//! # One connection, one session
//!
//! A stream's first control frame must be `JOIN`: nothing is read, seated or answered before the
//! ticket has been verified, and a frame that is not a join closes the connection without a reply,
//! because there is nobody to reply to yet. A connection that opens a second stream is a client
//! reconnecting, and that stream becomes its own session the same way a second socket connection
//! would — which the seat map resolves by device, so it takes the seat rather than doubling it.
//!
//! # What ends a session
//!
//! An explicit `LEAVE`, the end of the control stream, or the transport's idle timeout. The last
//! is the backstop for a device that vanished without saying goodbye. There is no heartbeat of this
//! plane's own: media flows or it does not, and a call where nobody is publishing still keeps its
//! connection alive through QUIC's keep-alive, which is why the timeout is sized from the node's
//! gateway heartbeat rather than from a cadence invented here.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use migo_core::config::SfuNodeConfig;
use migo_core::metrics::Registry;
use migo_core::{Error, Shutdown, Timestamp};
use migo_protocol::{fault, BandwidthMode};
use migo_sfu::{InboundFrame, SealedFrame};

use crate::plane::{Outbound, Plane, Session};
use crate::ticket::{TicketError, TicketKey};
use crate::wire::{
    body_len, decode_publish_datagram, decode_request, encode_reply, frame_bytes, Reply, Request,
    WireError, MAX_CONTROL_BYTES,
};

/// The QUIC application error code a refused admission closes the connection with.
///
/// Not a protocol status code — QUIC's application codes belong to this process — and it exists so
/// a client's logs can tell "the media plane hung up on my ticket" from "the network went away".
const CLOSE_REFUSED: u32 = 1;

/// The QUIC application error code a shutdown closes connections with.
const CLOSE_SHUTDOWN: u32 = 2;

/// How long a refused connection is given to read its refusal before it is closed.
///
/// The refusal is a courtesy, and a client that is not reading it must not cost more than this:
/// the stream is finished first, so a client that is reading gets the whole frame, and a client
/// that is not costs a bounded wait rather than a connection held until the idle timeout.
const REFUSAL_GRACE: Duration = Duration::from_secs(5);

/// The media plane's listener and the sessions it serves.
pub struct Server {
    plane: Arc<Plane>,
    key: TicketKey,
    config: SfuNodeConfig,
    heartbeat: Duration,
}

impl Server {
    /// Builds the media plane and its verifier.
    ///
    /// `heartbeat` is the node's gateway heartbeat, and it sizes the transport's keep-alive pair.
    /// A call whose participants are all muted sends nothing at all for minutes, so a connection
    /// with no keep-alive would be timed out mid-call by the transport between two perfectly
    /// healthy frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the ticket key is unusable or the plane's product limits could not
    /// hold — either way a deployment error, refused at startup rather than at the first call.
    pub fn new(
        config: &SfuNodeConfig,
        heartbeat: Duration,
        registry: &Registry,
    ) -> anyhow::Result<Self> {
        let key = TicketKey::from_config(config.ticket_key.expose())
            .map_err(|error| anyhow::anyhow!("sfu.ticket_key is unusable: {error}"))?;
        let plane = Plane::new(config, registry)
            .map_err(|error| anyhow::anyhow!("the media plane's limits cannot hold: {error}"))?;
        Ok(Self {
            plane: Arc::new(plane),
            key,
            config: config.clone(),
            heartbeat,
        })
    }

    /// The plane this listener drives, for the process's startup and shutdown lines.
    #[must_use]
    pub fn plane(&self) -> &Arc<Plane> {
        &self.plane
    }

    /// Binds `sfu.bind` and serves until `shutdown` fires.
    ///
    /// Returns the address actually bound, so a `:0` bind reports the port the OS chose — which is
    /// what the tests dial and what an operator's log should show.
    ///
    /// # Errors
    ///
    /// Returns an error when no bind address is configured, the certificate cannot be minted, or
    /// the socket cannot be bound.
    pub async fn bind(self: &Arc<Self>, shutdown: Shutdown) -> anyhow::Result<SocketAddr> {
        let bind = self.config.bind.as_deref().ok_or_else(|| {
            anyhow::anyhow!("sfu.bind is not set: this process has no media plane to run")
        })?;
        let addr: SocketAddr = bind.parse().map_err(|error| {
            anyhow::anyhow!("sfu.bind {bind:?} is not a socket address: {error}")
        })?;

        let certificate =
            rcgen::generate_simple_self_signed(vec!["migo-sfu".to_owned()]).map_err(|error| {
                anyhow::anyhow!("cannot mint the media plane's certificate: {error}")
            })?;
        let mut server_config = quinn::ServerConfig::with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(certificate.cert)],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der())
                .into(),
        )
        .map_err(|error| anyhow::anyhow!("cannot build the media plane's TLS config: {error}"))?;

        // The keep-alive pair, sized the way the gateway's own QUIC listener sizes it: ping at
        // half the heartbeat, and time out past the slowest beat a session can run. The timeout
        // has to clear `UltraLowData`, which quadruples the advertised interval, or the transport
        // would drop a quiet but healthy call between two punctual frames.
        let keep_alive = self.heartbeat / 2;
        let idle = self.heartbeat * 4 + keep_alive;
        let idle_ms = u32::try_from(idle.as_millis()).unwrap_or(u32::MAX);
        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(keep_alive));
        transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            idle_ms,
        ))));
        server_config.transport_config(Arc::new(transport));

        let endpoint = quinn::Endpoint::server(server_config, addr)
            .map_err(|error| anyhow::anyhow!("cannot bind the media plane to {bind}: {error}"))?;
        let bound = endpoint.local_addr()?;

        let server = Arc::clone(self);
        let connections = Arc::new(AtomicU64::new(0));
        tokio::spawn(async move {
            loop {
                // Biased so a shutdown is always observed, even under a flood of connections.
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    incoming = endpoint.accept() => match incoming {
                        None => break,
                        Some(incoming) => {
                            let server = Arc::clone(&server);
                            let connections = Arc::clone(&connections);
                            tokio::spawn(async move {
                                let connection = match incoming.await {
                                    Ok(connection) => connection,
                                    Err(error) => {
                                        tracing::debug!(%error, "media handshake abandoned by peer");
                                        return;
                                    }
                                };
                                let remote = connection.remote_address();
                                let open = connections.fetch_add(1, Ordering::Relaxed) + 1;
                                tracing::debug!(%remote, open, "media connection accepted");
                                loop {
                                    match connection.accept_bi().await {
                                        Ok((write, read)) => {
                                            server.serve_stream(&connection, write, read).await;
                                        }
                                        Err(quinn::ConnectionError::LocallyClosed) => break,
                                        Err(error) => {
                                            tracing::debug!(%error, %remote, "media connection ended");
                                            break;
                                        }
                                    }
                                }
                                let left = connections.fetch_sub(1, Ordering::Relaxed) - 1;
                                tracing::debug!(%remote, open = left, "media connection closed");
                            });
                        }
                    }
                }
            }
            endpoint.close(CLOSE_SHUTDOWN.into(), b"shutdown");
        });

        Ok(bound)
    }

    /// Serves one control stream, from `JOIN` to the seat's end.
    async fn serve_stream(
        &self,
        connection: &quinn::Connection,
        mut write: quinn::SendStream,
        mut read: quinn::RecvStream,
    ) {
        let remote = connection.remote_address();
        let mut buf = BytesMut::new();

        // Nothing is read, seated or answered before the ticket has been verified.
        let first = match read_frame(&mut read, &mut buf).await {
            Ok(Some(body)) => body,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, %remote, "media control stream ended before a join");
                return;
            }
        };
        let ticket = match decode_request(&first) {
            Ok(Request::Join { ticket }) => ticket,
            Ok(_) => {
                tracing::debug!(%remote, "media connection opened with something other than a join");
                connection.close(CLOSE_REFUSED.into(), b"join first");
                return;
            }
            Err(error) => {
                tracing::debug!(%error, %remote, "media connection opened with an unreadable frame");
                connection.close(CLOSE_REFUSED.into(), b"unreadable");
                return;
            }
        };

        let now = Timestamp::now();
        let claim = match self.key.verify(&ticket, now) {
            Ok(claim) => claim,
            Err(error) => {
                // Every reason is answered with the same shape, so a prober learns that its
                // ticket did not admit it and nothing about which part of the claim failed. The
                // one distinction kept is expiry, because an honest client whose ticket aged out
                // can act on it — mint another and reconnect — while a forger cannot.
                let refusal = match error {
                    TicketError::Expired => fault::permission_denied(
                        "ticket",
                        "the ticket expired; join the call again for a fresh one",
                    ),
                    _ => {
                        fault::permission_denied("ticket", "the ticket is not one this node minted")
                    }
                };
                tracing::debug!(%error, %remote, "media admission refused");
                self.plane.admission_refused();
                refuse(connection, &mut write, &refusal, b"refused").await;
                return;
            }
        };

        let (session, outbound) = match self.plane.seat(
            claim.call_id,
            claim.member,
            BandwidthMode::Auto,
            claim.expires_at,
            now,
        ) {
            Ok(seated) => seated,
            Err(error) => {
                tracing::debug!(%error, %remote, "media seat refused");
                self.plane.admission_refused();
                refuse(connection, &mut write, &refusal_reply(&error), b"no seat").await;
                return;
            }
        };
        tracing::debug!(
            %remote,
            call = %claim.call_id.to_text(),
            account = %claim.member.account_id.to_text(),
            "media session seated"
        );

        // The writer task owns the send half from here on, so every reply from this task goes
        // through the same channel as the media: the connection observes one order, and the send
        // half is never written from two places.
        let writer = tokio::spawn(write_loop(connection.clone(), write, outbound));

        self.plane.reply(
            &session,
            &Reply::Seated {
                expires_at: session.expires_at,
                seats: self.plane.seats(session.call_id),
            },
        );

        self.pump(connection, &mut read, &mut buf, &session).await;

        self.plane.close(&session);
        writer.abort();
        tracing::debug!(%remote, call = %session.call_id.to_text(), "media session ended");
    }

    /// Runs a session's loop: control frames on its stream, sealed frames on datagrams.
    async fn pump(
        &self,
        connection: &quinn::Connection,
        read: &mut quinn::RecvStream,
        buf: &mut BytesMut,
        session: &Session,
    ) {
        loop {
            tokio::select! {
                biased;
                // The control stream is the session's lifetime: it ends when the client's half
                // does, and nothing on this connection outlives it.
                frame = read_frame(read, buf) => match frame {
                    Ok(Some(body)) => {
                        let request = match decode_request(&body) {
                            Ok(request) => request,
                            Err(error) => {
                                // An unreadable frame is refused rather than guessed at, and the
                                // session stays up: the next frame may well be readable, and one
                                // bad frame is not grounds for dropping a call.
                                tracing::debug!(%error, "media control frame refused");
                                self.plane.reply(session, &malformed(&error));
                                continue;
                            }
                        };
                        let leave = matches!(request, Request::Leave);
                        let reply = self.plane.handle(session, &request, Timestamp::now());
                        self.plane.reply(session, &reply);
                        if leave {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        tracing::debug!(%error, "media control stream ended");
                        return;
                    }
                },
                datagram = connection.read_datagram() => match datagram {
                    Ok(bytes) => self.publish(session, &bytes),
                    // A datagram failing ends the session here rather than being waited on: the
                    // only errors quinn reports on this path mean the connection is gone, and
                    // the stream read above would report the same thing a moment later.
                    Err(error) => {
                        tracing::debug!(%error, "media datagram read ended");
                        return;
                    }
                },
            }
        }
    }

    /// Moves one sealed frame from the session that sent it to the seats subscribed to it.
    fn publish(&self, session: &Session, bytes: &Bytes) {
        let (stream_id, sequence, layer, payload) = match decode_publish_datagram(bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                // A frame this plane cannot route is dropped, never guessed at. Both kinds of
                // refusal are counted, so an operator sees an unreadable datagram as a number
                // rather than as a mystery.
                tracing::trace!(%error, "media datagram refused");
                self.plane.datagram_refused();
                return;
            }
        };
        let frame = InboundFrame {
            stream_id,
            sequence,
            layer,
            payload: SealedFrame::from_slice(payload),
        };
        if let Err(error) = self.plane.forward(session.call_id, session.member, frame) {
            // The core refuses a frame whose stream it does not know, which is what a client
            // publishing before its publish landed looks like. Counted, not logged per frame:
            // this is a datagram path, and a log line per frame is its own outage.
            tracing::trace!(%error, "media frame not forwardable");
            self.plane.frame_refused();
        }
    }
}

/// Serves `/metrics` on the configured address until `shutdown` fires.
///
/// A separate socket from the media plane and a separate address from `migod`'s. The point of
/// section 92 is that this process is scraped as itself: a media plane whose observable state is
/// visible only through the signalling node is one an operator cannot diagnose during the incident
/// it exists for.
///
/// # Errors
///
/// Returns an error when the address cannot be parsed or bound.
pub async fn serve_metrics(
    bind: &str,
    registry: Arc<Registry>,
    shutdown: Shutdown,
) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = bind.parse().map_err(|error| {
        anyhow::anyhow!("sfu.metrics_bind {bind:?} is not a socket address: {error}")
    })?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| anyhow::anyhow!("cannot bind the metrics listener to {bind}: {error}"))?;
    let bound = listener.local_addr()?;

    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let registry = Arc::clone(&registry);
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4",
                    )],
                    registry.render(),
                )
            }
        }),
    );
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
        {
            tracing::error!(%error, "metrics listener ended");
        }
    });
    Ok(bound)
}

/// Answers a refusal on a stream that has no writer task yet, and closes the connection.
///
/// The frame is written and the stream finished before the connection is closed, so a client that
/// is reading gets the whole refusal; the wait that follows is bounded by [`REFUSAL_GRACE`], and a
/// client that is not reading costs that and nothing more. Closing a connection whose stream data
/// is still in flight would discard the refusal, which is the one thing this exists to deliver.
async fn refuse(
    connection: &quinn::Connection,
    write: &mut quinn::SendStream,
    reply: &Reply,
    reason: &[u8],
) {
    if write
        .write_all(&frame_bytes(&encode_reply(reply)))
        .await
        .is_ok()
    {
        let _ = write.finish();
        let _ = tokio::time::timeout(REFUSAL_GRACE, connection.closed()).await;
    }
    connection.close(CLOSE_REFUSED.into(), reason);
}

/// Drains a session's outbound channel onto its connection, until the channel closes.
///
/// One task per session owns the send half, so a reply and a frame can never interleave inside a
/// control message. It exits when the session's channel closes, which happens when the session's
/// own task ends and drops its sender.
async fn write_loop(
    connection: quinn::Connection,
    mut write: quinn::SendStream,
    mut outbound: mpsc::Receiver<Outbound>,
) {
    while let Some(item) = outbound.recv().await {
        match item {
            Outbound::Control(body) => {
                if write.write_all(&frame_bytes(&body)).await.is_err() {
                    break;
                }
            }
            Outbound::Media(datagram) => {
                // A datagram the transport refuses is gone: there is no retry for a media frame,
                // because a retried frame arrives late enough to be worse than the gap it fills.
                // The session's queue already counted it on the way in.
                let _ = connection.send_datagram(datagram);
            }
        }
    }
    let _ = write.finish();
}

/// Renders a core refusal as the wire's error reply.
fn refusal_reply(error: &Error) -> Reply {
    Reply::Error {
        code: error.code(),
        retry_after_ms: error.retry_after().unwrap_or(0),
        message: error.public_message().to_string(),
    }
}

/// Renders an unreadable control frame as the wire's error reply.
fn malformed(error: &WireError) -> Reply {
    refusal_reply(&fault::malformed_frame(error.to_string()))
}

/// Reads one length-prefixed control frame, banking partial reads in `buf`.
///
/// Cancel-safe: every byte read lands in `buf`, which the caller owns across calls, so a future
/// dropped mid-frame by the `select!` above loses nothing and the next call resumes from the bytes
/// already banked.
async fn read_frame(
    read: &mut quinn::RecvStream,
    buf: &mut BytesMut,
) -> Result<Option<Bytes>, ReadError> {
    loop {
        if let Some(frame) = take_frame(buf)? {
            return Ok(Some(frame));
        }
        if buf.len() > MAX_CONTROL_BYTES + 4 {
            return Err(ReadError::Oversized);
        }
        let before = buf.len();
        let read_bytes = read.read_buf(buf).await.map_err(ReadError::Transport)?;
        if read_bytes == 0 {
            // A clean end with bytes banked is a truncated frame, not an end of stream: the peer
            // closed mid-message, which is an error and not a goodbye.
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(ReadError::Truncated)
            };
        }
        debug_assert!(buf.len() > before);
    }
}

/// Peels one whole control frame off the front of `buf`, if one is there.
fn take_frame(buf: &mut BytesMut) -> Result<Option<Bytes>, ReadError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = body_len(&buf[..4]).map_err(ReadError::Wire)?;
    if buf.len() < 4 + len {
        return Ok(None);
    }
    buf.advance(4);
    Ok(Some(buf.split_to(len).freeze()))
}

/// Why a control stream could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The transport failed.
    #[error("the control stream failed: {0}")]
    Transport(#[from] quinn::ReadError),
    /// The frame was not one this protocol has.
    #[error("{0}")]
    Wire(WireError),
    /// The peer closed in the middle of a frame.
    #[error("the control stream ended inside a frame")]
    Truncated,
    /// The peer announced a frame past the protocol's ceiling.
    #[error("the control stream announced an oversized frame")]
    Oversized,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wire::encode_request;

    #[test]
    fn frames_are_peeled_one_at_a_time_and_partials_stay_banked() {
        let mut buf = BytesMut::new();
        let body = encode_request(&Request::Roster);
        let framed = frame_bytes(&body);
        buf.extend_from_slice(&framed);
        buf.extend_from_slice(&framed[..2]);
        let first = take_frame(&mut buf).expect("reads").expect("one frame");
        assert_eq!(first, body);
        assert_eq!(buf.len(), 2, "the partial second frame stays banked");
        assert!(take_frame(&mut buf).expect("reads").is_none());
    }

    #[test]
    fn a_length_prefix_past_the_ceiling_is_refused_rather_than_waited_for() {
        let mut buf = BytesMut::new();
        let huge = u32::try_from(MAX_CONTROL_BYTES + 1).expect("fits");
        buf.extend_from_slice(&huge.to_be_bytes());
        assert!(matches!(take_frame(&mut buf), Err(ReadError::Wire(_))));
    }

    #[test]
    fn a_refusal_is_a_readable_error_reply() {
        let reply = malformed(&WireError::Short);
        match reply {
            Reply::Error { message, .. } => assert!(!message.is_empty()),
            other => panic!("expected an error reply, got {other:?}"),
        }
    }
}
