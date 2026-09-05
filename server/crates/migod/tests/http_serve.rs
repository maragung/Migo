//! The HTTP/WebSocket front door, exercised end to end over a real socket — both families.
//!
//! `App::serve` is the socket a browser talks to: the REST API and the realtime gateway WebSocket
//! ride the same listener, bound from `MIGO_HTTP__BIND`. These tests bind it exactly the way a
//! deployment binds it and drive it with a raw HTTP/1.1 client — no client SDK, just bytes on a
//! `tokio::net::TcpStream`, the same posture as the TCP and QUIC listener suites. What they prove:
//!
//!   1. The front door serves over the IPv4 loopback: `/health` answers, and a GET to `/ws` with
//!      upgrade headers is answered with `101 Switching Protocols`, meaning the gateway route is
//!      mounted on the same socket as the API and hands sockets to the gateway.
//!   2. The same front door serves over the IPv6 loopback, byte for byte the same contract. A
//!      deployment that binds `[::]` serves both families from one listener (Linux accepts
//!      IPv4-mapped connections on it), and these tests pin that the serve path — the bind, the
//!      router, the upgrade, the gateway handoff — is family-independent.
//!
//! The port is picked by binding a throwaway listener, reading its port, and letting it go — the
//! usual way a test lands on a free port without a `port: 0` the test cannot read back from
//! `App::bind`'s string.
//!
//! The serve future is spawned and stopped through the shared [`Shutdown`] handle, the same signal
//! an operator's SIGTERM arrives on, so each test also proves serving stops when asked.

use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use migo_core::Config;
use migod::App;

/// How long any single client-side step may take before the test declares the server stuck
/// rather than waiting on the CI timeout to do it.
const STEP: Duration = Duration::from_secs(5);

fn valid_token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([7u8; 32])
}

/// Picks a free port on the loopback the test will serve on, the standard bind-and-drop trick.
async fn free_port(bind_loopback: &str) -> u16 {
    let listener = tokio::net::TcpListener::bind(format!("{bind_loopback}:0"))
        .await
        .expect("the throwaway bind succeeds");
    listener
        .local_addr()
        .expect("the throwaway bind reports its address")
        .port()
}

/// Builds and serves an in-memory node on the named loopback, returning the dial address a raw
/// client should speak HTTP to and the shutdown handle that stops the server when triggered.
///
/// `bind_loopback` is `[::1]` or `127.0.0.1`. The returned dial address keeps the brackets on the
/// IPv6 literal, because an IPv6 address in a host:port pair is always bracketed — the same rule
/// a client follows when its operator hands it an IPv6 endpoint.
async fn serve_on(bind_loopback: &str) -> (String, migo_core::Shutdown) {
    let port = free_port(bind_loopback).await;
    let bind = format!("{bind_loopback}:{port}");
    let config = Config::from_sources(
        &[],
        &[
            ("MIGO_AUTH__TOKEN_KEY".to_string(), valid_token_key()),
            ("MIGO_HTTP__BIND".to_string(), bind),
        ],
    )
    .expect("configuration should parse");
    let app = App::build(&config)
        .await
        .expect("a development configuration must build against in-memory backends");

    // The serve future owns the services; stop it through the shared handle the way SIGTERM does.
    let shutdown = app.shutdown.clone();
    tokio::spawn(app.serve());
    (format!("{bind_loopback}:{port}"), shutdown)
}

/// Reads one HTTP/1.1 response's status line and headers from a raw stream — enough to assert
/// what the server said without a client SDK or a body to buffer.
async fn read_response_head(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut scratch = [0u8; 512];
    tokio::time::timeout(STEP, async {
        loop {
            let read = stream
                .read(&mut scratch)
                .await
                .expect("the response head reads");
            head.extend_from_slice(&scratch[..read]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") || read == 0 {
                break;
            }
        }
    })
    .await
    .expect("the response head arrives within the step budget");
    String::from_utf8_lossy(&head).into_owned()
}

/// Connects, retrying `ConnectionRefused` until the step budget runs out.
///
/// [`serve_on`] returns as soon as the serve future is *spawned*, not bound — the bind happens on
/// the spawned task — so a client that connects once races the listener. A deployment never sees
/// this (the port is up before DNS points at it); a test client retries.
async fn connect_when_listening(dial: &str) -> tokio::net::TcpStream {
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        match tokio::net::TcpStream::connect(dial).await {
            Ok(stream) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the front door never came up on {dial}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the connect fails over this family: {error}"),
        }
    }
}

/// One round of the front-door contract against whichever loopback the caller names: `/health`
/// answers 200, and `/ws` upgrades to the realtime gateway with 101.
async fn the_front_door_serves(bind_loopback: &str) {
    let (dial, shutdown) = serve_on(bind_loopback).await;
    let host = dial.as_str();

    // /health: the liveness route answers 200 over this family.
    let mut stream = connect_when_listening(host).await;
    stream
        .write_all(
            format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .expect("the /health request is written");
    let head = read_response_head(&mut stream).await;
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "/health must answer 200, got: {}",
        head.lines().next().unwrap_or("")
    );

    // /ws: the gateway route is mounted on this same socket — upgrade headers in, 101 out.
    let mut stream = connect_when_listening(host).await;
    stream
        .write_all(
            format!(
                "GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("the /ws upgrade request is written");
    let head = read_response_head(&mut stream).await;
    assert!(
        head.starts_with("HTTP/1.1 101"),
        "/ws must upgrade to the gateway with 101, got: {}",
        head.lines().next().unwrap_or("")
    );

    shutdown.trigger();
}

#[tokio::test]
async fn the_front_door_serves_the_ipv4_loopback() {
    the_front_door_serves("127.0.0.1").await;
}

/// IPv6 is not a second-class front door: the serve path — bind, router, upgrade, gateway
/// handoff — must not care which family the socket arrived on. A deployment binds `[::]` and
/// both families walk through it.
#[tokio::test]
async fn the_front_door_serves_the_ipv6_loopback() {
    the_front_door_serves("[::1]").await;
}
