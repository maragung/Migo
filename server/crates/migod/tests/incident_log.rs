//! A refused Server-auth frame leaves a durable incident record, not just a counter.
//!
//! Brief section 162 says a Server-auth-level frame arriving from a client socket "WAJIB
//! ditolak dan dicatat sebagai insiden" — must be refused *and recorded as an incident*.
//! The refusal half was already pinned: the session closes as a protocol violation and the
//! close is metered. But a metrics counter is a number that goes up, not a record an
//! operator can read; threat-model finding F3 (docs/03-security-threat-model.md section
//! 12.5) called that out, and this suite is the close.
//!
//! The test drives the real path the way the other wire suites do: a whole node built by
//! `App::build`, the native TCP listener bound on loopback, a real account registered
//! through the front door, and a real `TcpStream` speaking the length-prefixed framing. A
//! unit test on a helper could not have proven any of what matters here — that the record
//! is emitted from the live refusal site, that it carries the session, the account and
//! device of the authenticated session, the remote network class, the opcode and the
//! reason, and that it quotes nothing the wire is forbidden to carry.
//!
//! The record is captured through the production tracing pipeline — the same
//! `tracing_subscriber` JSON formatting `migo_core::telemetry` installs in a real node —
//! so what the assertions read is the line an operator's log pipeline would receive.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_auth::{DeviceClaim, Grant, Registration, RequestContext};
use migo_core::{Config, Secret};
use migo_protocol::{
    codes, from_frame, to_frame, Encode, Error as ErrorMessage, Frame, Hello, MessageEvent, Opcode,
    Platform, Welcome, PROTOCOL_VERSION,
};
use migod::App;

/// How long any single client-side step may take before the test declares the server stuck.
const STEP: Duration = Duration::from_secs(5);

/// A marker the offending frame's envelope carries, so the test can prove the incident
/// record describes the frame without quoting it.
const ENVELOPE_MARKER: &str = "incident-envelope-must-never-reach-a-log";

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// A development app with the TCP listener bound: the in-memory backends, the real
/// services over them, and the one environment pair that turns the native transport on.
async fn build_app() -> App {
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_TCP__BIND".to_string(), "127.0.0.1:0".to_string()),
        ],
    )
    .expect("configuration should parse");
    App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends")
}

/// Registers one account through the front door, so the session below is a real
/// authenticated session whose account and device the record should be able to name.
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
                device: DeviceClaim::new(Platform::Web, "incident test"),
                captcha: None,
                server: None,
                identity_public_key: None,
            },
            &RequestContext::at(app.clock.now()),
        )
        .await
        .expect("a development app registers an account")
}

/// Sends one frame as a length-prefixed record on the stream.
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

/// Reads one length-prefixed reply record from the session.
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
    .expect("the reply does not stall");
    Frame::decode(Bytes::from(body)).expect("the reply decodes")
}

// ---------------------------------------------------------------------------
// Capturing the log: the production tracing pipeline, aimed at a buffer.
// ---------------------------------------------------------------------------

/// Everything the installed subscriber writes, shared with the test thread.
type LogBuffer = Arc<Mutex<Vec<u8>>>;

/// A `Write` handle onto the shared buffer, cloned per `MakeWriter` call.
#[derive(Clone)]
struct LogSink(LogBuffer);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the log buffer is not poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs the one capturing subscriber this binary will ever have: the JSON formatting
/// production logs ride, filtered to the gateway's warnings, writing into a shared buffer.
/// Returns the buffer, so a test can read back what the node logged while it ran.
fn captured_logs() -> LogBuffer {
    static BUFFER: OnceLock<LogBuffer> = OnceLock::new();
    BUFFER
        .get_or_init(|| {
            let buffer: LogBuffer = Arc::default();
            let sink = LogSink(Arc::clone(&buffer));
            // A second `try_init` failure would mean something else already installed a
            // global subscriber; nothing else in this binary does, so silence is fine here.
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(tracing_subscriber::EnvFilter::new("migo_gateway=warn"))
                .with_writer(move || sink.clone())
                .with_ansi(false)
                .try_init();
            buffer
        })
        .clone()
}

/// The captured text, once it contains a line matching `needle`, polling briefly because
/// the record is written by the session task rather than the test thread.
async fn wait_for_line(buffer: &LogBuffer, needle: &str) -> String {
    let deadline = std::time::Instant::now() + STEP;
    loop {
        let text = String::from_utf8(
            buffer
                .lock()
                .expect("the log buffer is not poisoned")
                .clone(),
        )
        .expect("the subscriber writes UTF-8");
        if let Some(line) = text.lines().find(|line| line.contains(needle)) {
            return line.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no captured log line contains {needle:?}; captured so far:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_server_auth_frame_from_a_client_socket_is_refused_and_recorded_as_an_incident() {
    let buffer = captured_logs();
    let app = build_app().await;
    let addr = app.tcp_bind.expect("the listener is bound");
    let grant = registered_grant(&app, "incidentowner").await;

    // A real authenticated session over a real socket, the way a client gets one.
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
    let welcome: Welcome = from_frame(&welcome_frame).expect("the WELCOME decodes");
    assert_eq!(
        welcome.authenticated_user,
        Some(grant.account_id),
        "the inline token must promote the session, or nothing after this is meaningful"
    );

    // The violation: a server-to-client opcode sent by the client, carrying a payload the
    // incident record must describe but never quote.
    let violation = MessageEvent {
        envelope: ENVELOPE_MARKER.as_bytes().to_vec(),
        ..Default::default()
    };
    send(&mut stream, Opcode::MessageEvent, 2, &violation).await;

    // The refusal half, already the law of the wire: an error frame, then the session ends.
    let error_frame = recv(&mut stream).await;
    assert!(error_frame.header.is_error(), "the violation is answered");
    let error: ErrorMessage = from_frame(&error_frame).expect("the error frame decodes");
    assert_eq!(
        error.code,
        codes::UNEXPECTED_OPCODE,
        "a Server-auth frame from a client is an unexpected opcode"
    );
    tokio::time::timeout(STEP, async {
        let mut scratch = [0u8; 8];
        loop {
            match stream.read(&mut scratch).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("the session ends promptly after the refusal");

    // The record half, which this test exists to pin: one structured incident line,
    // filed at the refusal site, carrying what an operator needs and nothing the wire
    // forbids.
    let record = wait_for_line(&buffer, "server_auth_frame_from_client").await;
    let opcode_number = format!("\"opcode\":{}", Opcode::MessageEvent.to_wire());
    let account_field = format!("\"account_id\":\"{}\"", grant.account_id);
    let device_field = format!("\"device_id\":\"{}\"", grant.device_id);
    assert!(
        record.contains("\"incident\":\"server_auth_frame_from_client\""),
        "the record names the incident kind as a queryable field: {record}"
    );
    assert!(
        record.contains(&opcode_number),
        "the record names the offending opcode number: {record}"
    );
    assert!(
        record.contains("\"opcode_name\":\"MESSAGE_EVENT\""),
        "the record names the offending opcode in words: {record}"
    );
    assert!(
        record.contains(&account_field),
        "the record names the account behind the authenticated session: {record}"
    );
    assert!(
        record.contains(&device_field),
        "the record names the device behind the authenticated session: {record}"
    );
    assert!(
        record.contains("\"session_id\":\""),
        "the record names the session, even though the test cannot know its minted id: {record}"
    );
    assert!(
        record.contains("\"remote_network\":\"127.0.0.0/24\""),
        "the record names the remote network class, the loopback /24: {record}"
    );
    assert!(
        record.contains("\"reason\":\"protocol_violation\""),
        "the record names the refusal reason: {record}"
    );

    // The security half: the record is identification only. No whole address — the brief
    // truncates IP data to the network class, and a security log is the last place to
    // breach that. No token, no envelope content, nothing that would turn an incident
    // trail into the leak it exists to investigate.
    assert!(
        !record.contains("127.0.0.1"),
        "the whole peer address must not appear; only its network class may: {record}"
    );
    assert!(
        !record.contains(&grant.access_token),
        "the access token must not appear in the incident record: {record}"
    );
    assert!(
        !record.contains(ENVELOPE_MARKER),
        "the offending frame's payload must not appear in the incident record: {record}"
    );
}
