//! The user-configured server endpoint, the persistent form the web and Android clients use.
//!
//! The shape is the same as the TypeScript SDK's `ServerEndpoint`: host, REST port, gateway port,
//! transport (`WebSocket` or `Quic`), the realtime scheme (`Ws`, `Wss`, `Quic`, `QuicTls`), and the
//! REST scheme (`Http`, `Https`). The transport enum's values are `WebSocket` (the default) and
//! `Quic` (a real second option); QUIC needs a server with its optional QUIC listener enabled, and
//! this client still connects over WebSocket on the wire, exactly as the web form does.
//!
//! A self-hoster types a host and a port and picks a TLS posture. The form then derives the REST
//! origin and the gateway WebSocket URL from the same fields, so the two endpoints can never
//! disagree about which deployment is meant. That rule is what makes the persistence simple: the
//! stored form is a single record, and the desktop reads it on every launch instead of rebuilding
//! from a string the user had no way to validate.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The realtime transport the user picked. `Quic` is a real second option: a server advertises it
/// via the `QUIC` feature bit only when its optional QUIC listener is enabled. The desktop persists
/// and validates the choice, and still speaks WebSocket on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    WebSocket,
    Quic,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::WebSocket => f.write_str("WebSocket"),
            Transport::Quic => f.write_str("QUIC"),
        }
    }
}

/// The TLS posture of the WebSocket transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WsScheme {
    Ws,
    Wss,
}

/// The TLS posture of the QUIC transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuicScheme {
    Quic,
    QuicTls,
}

/// The TLS posture of the REST surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestScheme {
    Http,
    Https,
}

/// The transport-paired realtime scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    Ws(WsScheme),
    Quic(QuicScheme),
}

/// The full user-configured server: host, two ports, and the scheme pair.
///
/// The two ports are split on purpose. They default together (gateway = rest + 1) but the form
/// lets a user who has to -- because of a reverse-proxy setup -- point the REST origin and the
/// realtime socket at different listeners without either side being magic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEndpoint {
    pub host: String,
    pub port: u16,
    pub gateway_port: u16,
    pub transport: Transport,
    pub scheme: Scheme,
    pub rest_scheme: RestScheme,
}

/// The set of hosts the local-dev policy applies to: a plain WebSocket is allowed there.
#[allow(dead_code)] // Used once the auth form's "is this dev?" branch is wired.
pub fn is_loopback_host(host: &str) -> bool {
    let lowered = host.to_ascii_lowercase();
    lowered == "localhost" || lowered == "127.0.0.1" || lowered == "::1"
}

/// The dev-policy default: plain WebSocket on plain HTTP, with the gateway on the next port.
pub fn default_loopback_server_endpoint(host: impl Into<String>, port: u16) -> ServerEndpoint {
    ServerEndpoint {
        host: host.into().to_ascii_lowercase(),
        port,
        gateway_port: port.saturating_add(1),
        transport: Transport::WebSocket,
        scheme: Scheme::Ws(WsScheme::Ws),
        rest_scheme: RestScheme::Http,
    }
}

/// This deployment's single-host endpoint. The public IP is baked here so a first-run install
/// talks to the live server immediately: plain HTTP and plain WS on one port (this deployment).
/// The local-dev policy (plain HTTP, gateway on the next port) now lives in the legacy
/// `default_loopback_server_endpoint` for tests and as the fallback when a user's hand-typed
/// `server_endpoint_from_url` parse fails.
pub fn default_production_server_endpoint() -> ServerEndpoint {
    ServerEndpoint {
        host: "152.53.102.150".to_owned(),
        port: 8080,
        gateway_port: 8080,
        transport: Transport::WebSocket,
        scheme: Scheme::Ws(WsScheme::Ws),
        rest_scheme: RestScheme::Http,
    }
}

/// The production default: WSS over HTTPS, with the gateway on the same port as REST.
#[allow(dead_code)] // Used once the auth form's "this is a public host" branch is wired.
pub fn default_internet_server_endpoint(host: impl Into<String>, port: u16) -> ServerEndpoint {
    ServerEndpoint {
        host: host.into().to_ascii_lowercase(),
        port,
        gateway_port: port,
        transport: Transport::WebSocket,
        scheme: Scheme::Ws(WsScheme::Wss),
        rest_scheme: RestScheme::Https,
    }
}

/// What a {@link parse_host} call rejects. Surfaced as the form-level error the rest of the
/// desktop auth form uses, never written to a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEndpointError(pub String);

impl fmt::Display for ServerEndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServerEndpointError {}

/// Parses a `host` or `host:port` shorthand into its parts.
///
/// Three inputs are recognised:
///   - `host` -- bare, e.g. `migo.example.com`. The port is the `port_fallback`.
///   - `host:port` -- a single colon and a numeric port, e.g. `migo.example.com:8443`.
///   - Anything else (multiple colons, non-numeric port) is rejected.
///
/// Reserved for the desktop settings UI; not yet wired in this build. The flag is
/// here so the public API does not get pruned.
#[allow(dead_code)]
pub fn parse_host(input: &str, port_fallback: u16) -> Result<(String, u16), ServerEndpointError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ServerEndpointError("host is required".to_owned()));
    }
    let colon = trimmed.find(':');
    let (host, port) = match colon {
        None => (trimmed.to_ascii_lowercase(), port_fallback),
        Some(index) => {
            if trimmed[index + 1..].contains(':') {
                return Err(ServerEndpointError(format!(
                    "host cannot contain more than one colon: {trimmed}"
                )));
            }
            let host = trimmed[..index].to_ascii_lowercase();
            if host.is_empty() {
                return Err(ServerEndpointError(format!("host is empty: {trimmed}")));
            }
            let port_text = &trimmed[index + 1..];
            let port = port_text.parse::<u16>().map_err(|_| {
                ServerEndpointError(format!("port is not a whole number: {port_text}"))
            })?;
            if port == 0 {
                return Err(ServerEndpointError(format!(
                    "port is out of range (1..65535): {port_text}"
                )));
            }
            (host, port)
        }
    };
    Ok((host, port))
}

/// Validates the numeric fields. Split out so the constructor and the parser share it.
///
/// Reserved for the desktop settings UI; not yet wired in this build. The flag is
/// here so the public API does not get pruned.
#[allow(dead_code)]
pub fn validate_ports(port: u16, gateway_port: u16) -> Result<(), ServerEndpointError> {
    if port == 0 {
        return Err(ServerEndpointError(format!(
            "rest port is out of range (1..65535): {port}"
        )));
    }
    if gateway_port == 0 {
        return Err(ServerEndpointError(format!(
            "gateway port is out of range (1..65535): {gateway_port}"
        )));
    }
    Ok(())
}

/// The REST origin, e.g. `http://localhost:18080`. No trailing slash.
pub fn rest_base_url(endpoint: &ServerEndpoint) -> String {
    format!(
        "{}://{}:{}",
        rest_scheme_prefix(endpoint),
        endpoint.host,
        endpoint.port
    )
}

/// The gateway WebSocket URL, e.g. `ws://localhost:18081/ws`. No trailing slash.
#[allow(dead_code)]
pub fn gateway_url(endpoint: &ServerEndpoint) -> String {
    format!(
        "{}://{}:{}/ws",
        gateway_scheme_prefix(endpoint),
        endpoint.host,
        endpoint.gateway_port
    )
}

/// The REST scheme prefix, taking the `rest_scheme` field on its own.
pub fn rest_scheme_prefix(endpoint: &ServerEndpoint) -> &'static str {
    match endpoint.rest_scheme {
        RestScheme::Http => "http",
        RestScheme::Https => "https",
    }
}

/// The gateway scheme prefix, taking the transport into account.
pub fn gateway_scheme_prefix(endpoint: &ServerEndpoint) -> &'static str {
    match endpoint.scheme {
        Scheme::Ws(WsScheme::Wss) => "wss",
        Scheme::Ws(WsScheme::Ws) => "ws",
        // Both QUIC variants are spelled `quic` at the URL level today; the TLS posture is
        // expressed via ALPN, not the URL.
        Scheme::Quic(_) => "quic",
    }
}

/// Resolves a `http(s)://host[:port]` string into a {@link ServerEndpoint}. The shape a user or an
/// env var can supply, the form is the structured form the rest of the desktop speaks.
pub fn server_endpoint_from_url(url: &str) -> ServerEndpoint {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return default_loopback_server_endpoint("localhost", 18080);
    }
    let (scheme_text, rest) = match trimmed.split_once("://") {
        Some(pair) => pair,
        None => return default_loopback_server_endpoint("localhost", 18080),
    };
    let rest_scheme = match scheme_text.to_ascii_lowercase().as_str() {
        "https" => RestScheme::Https,
        "http" => RestScheme::Http,
        _ => return default_loopback_server_endpoint("localhost", 18080),
    };
    let (authority, _path) = match rest.split_once('/') {
        Some(pair) => pair,
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) if !host.is_empty() && !port_text.is_empty() => {
            let parsed = port_text.parse::<u16>().ok();
            match parsed {
                Some(0) | None => (host.to_ascii_lowercase(), default_port_for(rest_scheme)),
                Some(value) => (host.to_ascii_lowercase(), value),
            }
        }
        _ => (
            authority.to_ascii_lowercase(),
            default_port_for(rest_scheme),
        ),
    };
    let scheme = match rest_scheme {
        RestScheme::Https => Scheme::Ws(WsScheme::Wss),
        RestScheme::Http => Scheme::Ws(WsScheme::Ws),
    };
    ServerEndpoint {
        host,
        port,
        gateway_port: port.saturating_add(1),
        transport: Transport::WebSocket,
        scheme,
        rest_scheme,
    }
}

fn default_port_for(scheme: RestScheme) -> u16 {
    match scheme {
        RestScheme::Https => 443,
        RestScheme::Http => 80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_accepts_a_bare_host() {
        let (host, port) = parse_host("migo.example.com", 18080).unwrap();
        assert_eq!(host, "migo.example.com");
        assert_eq!(port, 18080);
    }

    #[test]
    fn parse_host_lowercases_and_splits_a_shorthand() {
        let (host, port) = parse_host("Migo.Example.com:8443", 18080).unwrap();
        assert_eq!(host, "migo.example.com");
        assert_eq!(port, 8443);
    }

    #[test]
    fn parse_host_trims_whitespace() {
        let (host, port) = parse_host("  migo.example.com  ", 18080).unwrap();
        assert_eq!(host, "migo.example.com");
        assert_eq!(port, 18080);
    }

    #[test]
    fn parse_host_rejects_an_empty_input() {
        assert!(parse_host("", 18080).is_err());
        assert!(parse_host("   ", 18080).is_err());
    }

    #[test]
    fn parse_host_rejects_an_out_of_range_or_non_numeric_port() {
        assert!(parse_host("migo.example.com:0", 18080).is_err());
        assert!(parse_host("migo.example.com:65536", 18080).is_err());
        assert!(parse_host("migo.example.com:abc", 18080).is_err());
        assert!(parse_host("a:b:c", 18080).is_err());
    }

    #[test]
    fn default_loopback_uses_plain_pair() {
        let endpoint = default_loopback_server_endpoint("localhost", 18080);
        assert_eq!(endpoint.host, "localhost");
        assert_eq!(endpoint.port, 18080);
        assert_eq!(endpoint.gateway_port, 18081);
        assert_eq!(endpoint.transport, Transport::WebSocket);
        assert_eq!(endpoint.scheme, Scheme::Ws(WsScheme::Ws));
        assert_eq!(endpoint.rest_scheme, RestScheme::Http);
    }

    #[test]
    fn default_internet_uses_tls_pair() {
        let endpoint = default_internet_server_endpoint("migo.example.com", 443);
        assert_eq!(endpoint.host, "migo.example.com");
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.gateway_port, 443);
        assert_eq!(endpoint.transport, Transport::WebSocket);
        assert_eq!(endpoint.scheme, Scheme::Ws(WsScheme::Wss));
        assert_eq!(endpoint.rest_scheme, RestScheme::Https);
    }

    #[test]
    fn is_loopback_recognises_the_three_loopback_spellings() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(!is_loopback_host("migo.example.com"));
        assert!(!is_loopback_host("192.168.1.1"));
    }

    #[test]
    fn derived_urls_match_the_documented_shapes() {
        let endpoint = default_loopback_server_endpoint("localhost", 18080);
        assert_eq!(rest_base_url(&endpoint), "http://localhost:18080");
        assert_eq!(gateway_url(&endpoint), "ws://localhost:18081/ws");
    }

    #[test]
    fn derived_urls_for_tls_keep_the_ws_path() {
        let endpoint = default_internet_server_endpoint("migo.example.com", 443);
        assert_eq!(rest_base_url(&endpoint), "https://migo.example.com:443");
        assert_eq!(gateway_url(&endpoint), "wss://migo.example.com:443/ws");
    }
}
