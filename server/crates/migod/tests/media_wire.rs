//! The media pipeline answered on the wire, end to end.
//!
//! The media library's own tests call `begin`/`commit` directly, and the dispatch
//! handlers sit between those tests and every real client — which is how the ticket
//! bridge could ship broken: `begin` seals a MAC'd token that the wire's `MediaTicket`
//! has no field for, so the handlers passed the 16-byte `upload_id` where the library
//! wanted the token, and every commit from every client died with
//! `VALIDATION_FAILED: upload_ticket: unusable`. No service test could see it, because
//! no service test presents an id the way the socket does.
//!
//! So this is the caller the last bug hid from: a real app with both listeners bound —
//! the TCP transport for the control plane and the API router for the byte routes the
//! filesystem backend's URLs point at — a real account registered through the front
//! door, and then the whole journey as the client makes it: `BEGIN` for a ticket, an
//! HTTP `PUT` of the bytes, `STATUS` for the resume count, `COMMIT` for the row, and a
//! `FETCH_URL` plus HTTP `GET` to prove the bytes that came back are the bytes that
//! went in. The commit is the heart of it: the reply must be an `Acknowledged`, not an
//! error frame, because that is the exact reply production has never once sent.
//!
//! The refusal matters as much as the success: a commit naming an upload id the server
//! never issued must come back as a *reply* — a proper error frame with the request's
//! correlation — because silence is the failure mode the reply rule exists to prevent.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Config, Secret};
use migo_protocol::{
    codes, from_frame, to_frame, Acknowledged, Encode, Frame, Hello, MediaAbort, MediaBegin,
    MediaCommit, MediaFetch, MediaProgress, MediaStatusReq, MediaTicket, MediaUrl, Opcode,
    Platform, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single client-side step may take before the test declares the server
/// stuck. Silence is the bug class these tests guard against, so the timeout is the
/// assertion's clock.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with both of the listeners a media upload touches: the TCP
/// transport for the control plane, and — bound by the test itself, the way `serve.rs`
/// binds it in production — the API router whose `/media/{key}` byte routes the
/// filesystem backend's URLs point at. `public_url` is set to the test's own HTTP
/// listener so the signed upload URL routes back to this process.
async fn build_app(http_port: u16) -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
            (
                "MIGO_HTTP__PUBLIC_URL".to_string(),
                format!("http://127.0.0.1:{http_port}"),
            ),
            (
                "MIGO_MEDIA__LOCAL_DIR".to_string(),
                std::env::temp_dir()
                    .join(format!("migo-media-wire-{}", std::process::id()))
                    .display()
                    .to_string(),
            ),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, stamped with the node's own clock so
/// the inline token is not born expired.
async fn registered_grant(app: &App, username: &str) -> Grant {
    app.auth
        .register(
            Registration {
                username: username.to_string(),
                email: None,
                phone: None,
                passphrase: Secret::new("correct-horse-battery-staple"),
                locale: "en-US".to_string(),
                country: None,
                gender: None,
                device: DeviceClaim::new(Platform::Web, "media wire test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Sends one request frame as a length-prefixed record.
async fn send<M: Encode>(
    stream: &mut tokio::net::TcpStream,
    opcode: Opcode,
    correlation: u32,
    message: &M,
) {
    let frame = to_frame(opcode.to_wire(), correlation, message)
        .expect("a scripted client message must encode");
    let wire = frame.encode_length_prefixed().expect("the record encodes");
    tokio::time::timeout(STEP, stream.write_all(&wire))
        .await
        .expect("writing does not stall")
        .expect("the frame is written");
}

/// Reads one length-prefixed reply record.
async fn recv(stream: &mut tokio::net::TcpStream) -> Frame {
    let body = tokio::time::timeout(STEP, async {
        let mut head = [0u8; 4];
        stream
            .read_exact(&mut head)
            .await
            .expect("the length arrives");
        let len = u32::from_be_bytes(head) as usize;
        let mut body = vec![0u8; len];
        stream
            .read_exact(&mut body)
            .await
            .expect("the body arrives");
        body
    })
    .await
    .expect("the reply does not stall — silence here is the bug these tests exist to catch");
    Frame::decode(Bytes::from(body)).expect("the reply decodes")
}

/// A live authenticated TCP session: HELLO with the grant's inline token, answered by
/// a WELCOME that names the account.
struct LiveSession {
    stream: tokio::net::TcpStream,
}

impl LiveSession {
    async fn connect(addr: SocketAddr, grant: &Grant) -> Self {
        let mut stream = tokio::time::timeout(STEP, tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting does not stall")
            .expect("the connection is accepted");

        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            access_token: Some(grant.access_token.clone()),
            device_id: Some(grant.device_id),
            ..Default::default()
        };
        send(&mut stream, Opcode::Hello, 1, &hello).await;

        let welcome_frame = recv(&mut stream).await;
        assert_eq!(
            Opcode::from_wire(welcome_frame.header.opcode),
            Some(Opcode::Hello),
            "the handshake is answered with a WELCOME"
        );
        assert!(
            !welcome_frame.header.is_error(),
            "the handshake is not refused"
        );
        let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
        assert_eq!(
            welcome.authenticated_user,
            Some(grant.account_id),
            "the inline token must promote the session"
        );
        Self { stream }
    }

    /// Sends a request and returns the reply frame, asserting only the correlation —
    /// so a refusal can be inspected by the caller instead of panicking here.
    async fn ask_for_frame<M: Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> Frame {
        send(&mut self.stream, opcode, correlation, message).await;
        let frame = recv(&mut self.stream).await;
        assert_eq!(
            frame.header.correlation, correlation,
            "the reply must echo the request's correlation (section 139)"
        );
        frame
    }

    /// Sends a request that must succeed, returning the decoded reply.
    async fn ask<M: Encode, R: migo_protocol::Decode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        message: &M,
    ) -> R {
        let frame = self.ask_for_frame(opcode, correlation, message).await;
        assert!(
            !frame.header.is_error(),
            "the request was refused: {:?}",
            from_frame::<migo_protocol::Error>(&frame)
        );
        from_frame(&frame).expect("the reply decodes")
    }
}

/// The object's bytes: a PNG signature — what the sniff path needs, since a profile
/// avatar is server-readable and identified from its leading bytes — followed by
/// payload nobody needs to render.
fn png_bytes(len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    bytes.resize(len, 0x5A);
    bytes
}

/// One minimal HTTP/1.1 request over a fresh TCP connection: `Connection: close` so the
/// response ends at EOF, which is all the parsing a 204 and a 200 need. Returns the raw
/// response — headers and body — because the body is object bytes, not text.
async fn http(port: u16, request: &str, body: &[u8]) -> Vec<u8> {
    let mut stream =
        tokio::time::timeout(STEP, tokio::net::TcpStream::connect(("127.0.0.1", port)))
            .await
            .expect("connecting to the byte route does not stall")
            .expect("the byte route accepts the connection");

    let head = format!("{request} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).await.expect("head sent");
    stream.write_all(body).await.expect("body sent");

    let mut response = Vec::new();
    tokio::time::timeout(STEP, stream.read_to_end(&mut response))
        .await
        .expect("the byte route answers")
        .expect("the response arrives whole");
    response
}

/// The response's first line, for the assertions that only care about the status.
fn status_line(response: &[u8]) -> String {
    let end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(response.len());
    String::from_utf8_lossy(&response[..end]).into_owned()
}

#[tokio::test]
async fn an_upload_journeys_begin_put_status_commit_and_back_down() {
    // The HTTP listener is bound first so the app can mint URLs that point at it.
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the test binds an HTTP port");
    let http_port = http_listener
        .local_addr()
        .expect("the test knows its HTTP port")
        .port();

    let app = build_app(http_port).await;
    let addr = app.tcp_bind.expect("the TCP listener is bound");

    // Serve the API router exactly as serve.rs does, minus the gateway route this test
    // does not drive.
    let router = app.api_router.clone();
    tokio::spawn(async move {
        axum::serve(http_listener, router)
            .await
            .expect("the API serves");
    });

    let grant = registered_grant(&app, "mediaowner").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    // BEGIN: a ticket with an id the client can name the upload by and a URL that
    // points at this process's byte route.
    let bytes = png_bytes(1_024);
    let ticket: MediaTicket = session
        .ask(
            Opcode::MediaUploadBegin,
            61,
            &MediaBegin {
                kind: 0, // Avatar — profile media, so no conversation is needed.
                content_type: "image/png".to_string(),
                size: bytes.len() as u64,
                conversation_id: None,
                width: Some(64),
                height: Some(64),
                duration_ms: None,
            },
        )
        .await;
    assert!(
        !ticket.upload_id.is_nil(),
        "the ticket names the upload by a real id"
    );
    assert!(
        ticket
            .upload_url
            .starts_with(&format!("http://127.0.0.1:{http_port}/media/")),
        "the upload URL points at this process's byte route: {}",
        ticket.upload_url
    );

    // PUT: the bytes, on the URL the server signed.
    let path = ticket
        .upload_url
        .trim_start_matches(&format!("http://127.0.0.1:{http_port}"));
    let put = http(http_port, &format!("PUT {path}"), &bytes).await;
    assert!(
        status_line(&put).starts_with("HTTP/1.1 204"),
        "the byte route accepts the upload: {}",
        status_line(&put)
    );

    // STATUS: the resume count, which is the reply that proves the filed ticket
    // resolved — the v0.15.3-era bridge answered this with `upload_ticket: unusable`.
    let progress: MediaProgress = session
        .ask(
            Opcode::MediaUploadStatus,
            62,
            &MediaStatusReq {
                upload_id: ticket.upload_id,
            },
        )
        .await;
    assert_eq!(
        progress.received,
        bytes.len() as u64,
        "storage reports every byte the PUT left"
    );
    assert_eq!(progress.expected, bytes.len() as u64);

    // COMMIT: the reply production has never once sent.
    let acknowledged: Acknowledged = session
        .ask(
            Opcode::MediaUploadCommit,
            63,
            &MediaCommit {
                upload_id: ticket.upload_id,
                digest: vec![0xAB; 32],
            },
        )
        .await;
    assert!(acknowledged.ok, "the commit is acknowledged");

    // FETCH_URL + GET: the bytes come back down, which is the journey a recipient's
    // player makes.
    let url: MediaUrl = session
        .ask(
            Opcode::MediaFetchUrl,
            64,
            &MediaFetch {
                object_id: ticket.upload_id,
                conversation_id: None,
            },
        )
        .await;
    let path = url
        .url
        .trim_start_matches(&format!("http://127.0.0.1:{http_port}"));
    let get = http(http_port, &format!("GET {path}"), &[]).await;
    assert!(
        status_line(&get).starts_with("HTTP/1.1 200"),
        "the byte route serves the object: {}",
        status_line(&get)
    );
    let served = get
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| get[at + 4..].to_vec())
        .unwrap_or_default();
    assert_eq!(
        served, bytes,
        "the bytes that come back down are the bytes that went up"
    );
}

#[tokio::test]
async fn a_commit_for_an_upload_the_server_never_issued_is_refused_not_ignored() {
    let app = build_app(1).await; // The byte route is never driven; the port is nominal.
    let addr = app.tcp_bind.expect("the TCP listener is bound");
    let grant = registered_grant(&app, "mediaintruder").await;
    let mut session = LiveSession::connect(addr, &grant).await;

    // An id with the right shape and no ticket behind it, which is what a client
    // retries with after a restart wiped the filed tickets — and what a forger sends.
    let invented = migo_core::Id::from_bytes([0xDE; 16]);
    let frame = session
        .ask_for_frame(
            Opcode::MediaUploadCommit,
            71,
            &MediaCommit {
                upload_id: invented,
                digest: vec![0x01; 32],
            },
        )
        .await;
    assert!(
        frame.header.is_error(),
        "an id the server never issued is refused, not accepted"
    );
    let refusal: migo_protocol::Error = from_frame(&frame).expect("the error frame decodes");
    assert_eq!(
        refusal.code,
        codes::VALIDATION_FAILED,
        "the refusal is a validation failure about the ticket: {refusal:?}"
    );
    assert!(
        refusal
            .message
            .as_deref()
            .is_some_and(|message| message.contains("upload_ticket")),
        "the refusal is about the upload ticket, not some downstream field: {refusal:?}"
    );

    // The same must hold for abort, which a client's error path reaches for cleanup:
    // an error reply it can act on, never silence.
    let frame = session
        .ask_for_frame(
            Opcode::MediaUploadAbort,
            72,
            &MediaAbort {
                upload_id: invented,
            },
        )
        .await;
    assert!(frame.header.is_error());
}
