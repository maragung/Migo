//! The server address a client typed, in the same explicit shape the web
//! form sends.
//!
//! The TypeScript SDK on the wire side has a [`ServerEndpoint`](https://github.com/example/migo)
//! — one host, one REST port, one gateway port, and a pair of scheme
//! choices — and the same shape lives here so the route layer can echo a
//! server fingerprint into an audit row without inventing a parallel
//! structure. The Rust side does not enforce the form's parsing rules
//! (those run on the client); the type here is the canonical record of
//! "this is what the client said the server was" plus a sensible
//! default that drops in when the request did not say.
//!
//! # Why a `default_for_host`
//!
//! The default the user-visible disclosure uses on the web form is
//! `https`/`wss` for a non-loopback host and `http`/`ws` for a
//! loopback. The route layer wants a value to put on the wire even
//! when the request did not name one — a self-hosted client that has
//! typed the host but not yet opened the disclosure sends a
//! partially-populated shape — and the rule is small enough to live
//! here rather than be re-derived at every call site. Putting it on
//! the type keeps "what does a missing server default to?" a single
//! read.

use std::fmt;

/// Hosts the dev policy treats as loopback: a plain WebSocket is
/// allowed there, and a `https://` request would point at a TLS-only
/// deployment that has nothing to do with this machine.
const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// The realtime transport the user picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    /// A WebSocket on `/ws`. The plain and TLS pairs are the
    /// [`WsScheme`] variants.
    WebSocket,
    /// A raw TCP socket speaking the length-prefixed frame stream —
    /// the native client's default (brief section 138). The plain and
    /// TLS pairs are the [`TcpScheme`] variants.
    Tcp,
    /// QUIC. The plain and TLS pairs are the [`QuicScheme`] variants.
    /// Currently exposed for symmetry with the form; the gateway
    /// does not yet answer on QUIC.
    Quic,
}

impl Transport {
    /// The wire-stable name. Used by anything that has to put a label
    /// into a log or a metric.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::WebSocket => "WebSocket",
            Transport::Tcp => "Tcp",
            Transport::Quic => "Quic",
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The TLS posture of the WebSocket transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WsScheme {
    /// Plain `ws://`. The dev posture for loopback.
    Ws,
    /// `wss://`. The posture for any non-loopback host.
    Wss,
}

/// The TLS posture of the QUIC transport. Mirrors [`WsScheme`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuicScheme {
    /// Plain `quic://`.
    Quic,
    /// `quic-tls://`.
    QuicTls,
}

/// The TLS posture of the TCP transport. Mirrors [`WsScheme`]: the
/// loopback dev listener is plain, and a deployment reachable from
/// outside this machine fronts the socket with a TLS 1.3 terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TcpScheme {
    /// Plain socket.
    Tcp,
    /// TLS 1.3 over the socket.
    TcpTls,
}

/// The scheme paired with the realtime transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// A WebSocket scheme, plain or TLS.
    Ws(WsScheme),
    /// A TCP scheme, plain or TLS.
    Tcp(TcpScheme),
    /// A QUIC scheme, plain or TLS.
    Quic(QuicScheme),
}

/// The TLS posture for the REST surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RestScheme {
    /// `http://`. Loopback.
    Http,
    /// `https://`. Non-loopback.
    Https,
}

impl RestScheme {
    /// The wire-stable name. Used by anything that has to put a label
    /// into a log or a metric.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RestScheme::Http => "Http",
            RestScheme::Https => "Https",
        }
    }
}

impl fmt::Display for RestScheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The user-configured server: one host, two ports, and the pair of
/// schemes that tell the client whether each side is plain or
/// encrypted.
///
/// The two ports are split on purpose. They default together (gateway
/// = rest + 1) but the form lets a user who has to, because of a
/// reverse-proxy setup, point the REST origin and the realtime socket
/// at different listeners without either side being magic. The
/// transport enum is the only one that has to grow when a new
/// realtime path lands; the schemes are already expressed at the
/// level the form and the protocol both speak.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerEndpoint {
    /// The lowercased host (no scheme, no port, no path).
    pub host: String,
    /// The REST port. Must be in `[1, 65535]`.
    pub port: u16,
    /// The gateway port. Defaults to `port + 1`; the form exposes an
    /// override.
    pub gateway_port: u16,
    /// The realtime transport.
    pub transport: Transport,
    /// The TLS posture of the realtime transport.
    pub scheme: Scheme,
    /// The TLS posture of REST, paired with the realtime choice when
    /// sensible.
    pub rest_scheme: RestScheme,
}

impl ServerEndpoint {
    /// Builds an endpoint for a development loopback host: a plain
    /// WebSocket and a plain HTTP on the same port, with the gateway
    /// on the next one up.
    #[must_use]
    pub fn default_loopback(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into().to_ascii_lowercase(),
            port,
            gateway_port: port.saturating_add(1),
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Ws),
            rest_scheme: RestScheme::Http,
        }
    }

    /// Builds an endpoint for a production-style address. A
    /// non-loopback host forces the TLS postures: the REST origin
    /// speaks HTTPS and the gateway speaks WSS, which is the only
    /// configuration the audit allows once a deployment is reachable
    /// from outside this machine.
    #[must_use]
    pub fn default_internet(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into().to_ascii_lowercase(),
            port,
            // Production deployments usually expose both REST and the
            // gateway on the same port, behind one TLS terminator.
            gateway_port: port,
            transport: Transport::WebSocket,
            scheme: Scheme::Ws(WsScheme::Wss),
            rest_scheme: RestScheme::Https,
        }
    }

    /// Picks a default endpoint for a host. Loopback defaults to the
    /// plain dev pair; anything else to the TLS pair. The REST port
    /// defaults to 443 for non-loopback and 18080 for loopback,
    /// matching the rest of the codebase.
    #[must_use]
    pub fn default_for_host(host: impl Into<String>) -> Self {
        let host_value = host.into();
        if is_loopback_host(&host_value) {
            Self::default_loopback(host_value, 18080)
        } else {
            Self::default_internet(host_value, 443)
        }
    }

    /// True when the host is one the dev policy applies to: a plain
    /// WebSocket is allowed there, a `https://` request would point
    /// at a TLS-only deployment that has nothing to do with this
    /// machine.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        is_loopback_host(&self.host)
    }
}

/// Whether the host the user typed should be treated as loopback for
/// the purposes of "is TLS required". See [`ServerEndpoint::default_for_host`].
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    LOOPBACK_HOSTS.contains(&host.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_default_is_plain_schemes() {
        let endpoint = ServerEndpoint::default_for_host("localhost");
        assert_eq!(endpoint.scheme, Scheme::Ws(WsScheme::Ws));
        assert_eq!(endpoint.rest_scheme, RestScheme::Http);
        assert_eq!(endpoint.transport, Transport::WebSocket);
        assert!(endpoint.is_loopback());
    }

    #[test]
    fn non_loopback_default_is_tls_schemes() {
        let endpoint = ServerEndpoint::default_for_host("migo.example.com");
        assert_eq!(endpoint.scheme, Scheme::Ws(WsScheme::Wss));
        assert_eq!(endpoint.rest_scheme, RestScheme::Https);
        assert_eq!(endpoint.transport, Transport::WebSocket);
        assert!(!endpoint.is_loopback());
    }

    #[test]
    fn host_is_lowercased() {
        let endpoint = ServerEndpoint::default_for_host("MIGO.Example.COM");
        assert_eq!(endpoint.host, "migo.example.com");
    }

    #[test]
    fn default_loopback_uses_default_rest_port() {
        let endpoint = ServerEndpoint::default_loopback("localhost", 18_080);
        assert_eq!(endpoint.port, 18_080);
        assert_eq!(endpoint.gateway_port, 18_081);
    }

    #[test]
    fn default_internet_uses_a_single_port() {
        let endpoint = ServerEndpoint::default_internet("migo.example.com", 443);
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.gateway_port, 443);
    }

    #[test]
    fn is_loopback_host_recognises_the_canonical_names() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("migo.example.com"));
        assert!(!is_loopback_host("0.0.0.0"));
    }

    #[test]
    fn transport_has_a_stable_wire_name() {
        assert_eq!(Transport::WebSocket.as_str(), "WebSocket");
        assert_eq!(Transport::Tcp.as_str(), "Tcp");
        assert_eq!(Transport::Quic.as_str(), "Quic");
    }
}
