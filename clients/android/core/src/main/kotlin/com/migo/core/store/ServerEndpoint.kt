package com.migo.core.store

/**
 * The user's chosen server address.
 *
 * Mirrors `packages/sdk/src/server-endpoint.ts` (the canonical JS shape) and
 * `clients/desktop/src/config.rs::ServerEndpoint` (the Rust one), so a self-hoster who
 * types `migo.example.com` into the web form, the desktop settings, or the Android /
 * iOS bootstrap is configuring the same record. The fields are the same six everywhere,
 * the defaults are the same defaults, and the derived URLs come out the same way.
 *
 * The shape is explicit on purpose. A user-typed string that has to be parsed every time
 * it is read is a string that can be parsed two different ways on two different reads;
 * a record the form commits and the bootstrap reads is a record that has one meaning
 * across the whole app, including across a process death and a clean restart.
 *
 * # Why a record and not a URL
 *
 * The string form was `http://host:port/ws`-style, the caller had to build it, and a
 * typo in any byte silently routed the client at a different deployment. A `ServerEndpoint`
 * is host + two ports + two scheme choices, the URL it produces is a function of those
 * fields, and every byte of the URL is something a screen can show before it is asked to
 * be believed.
 *
 * # Why two ports
 *
 * The REST origin and the realtime socket are different listeners, and a reverse-proxy
 * setup (one node, two paths) means they do not have to share a port. The form lets a
 * user leave the gateway port blank and the default is `restPort + 1`, the same one the
 * dev policy uses (`migod` binds REST on 18080 and the gateway on 18081). A user who
 * needs a different split types it.
 *
 * # Why two schemes
 *
 * The realtime transport and the REST control plane are not the same protocol negotiation
 * even when they share a TLS posture. A deployment that fronts both with one TLS terminator
 * still announces WSS and HTTPS, not WSS for both. The split also keeps the loopback policy
 * expressible: a dev server on `localhost` is plain WS and plain HTTP, and a `wss://` to
 * `localhost` would be a user trying to talk to a deployment that is not on this machine.
 */
data class ServerEndpoint(
    /** The lowercased host (no scheme, no port, no path). */
    val host: String,
    /** The REST port. Must be in `[1, 65535]`. */
    val port: Int,
    /** The realtime gateway port. Defaults to [port] + 1. */
    val gatewayPort: Int,
    /** The realtime transport. WebSocket is the wired one; QUIC is shown but not yet carried. */
    val transport: Transport,
    /** The TLS posture of the realtime transport. */
    val gatewayScheme: GatewayScheme,
    /** The TLS posture of the REST control plane. */
    val restScheme: RestScheme,
) {
    init {
        require(host.isNotBlank()) { "host is required" }
        require(port in 1..65535) { "rest port is out of range (1..65535): $port" }
        require(gatewayPort in 1..65535) { "gateway port is out of range (1..65535): $gatewayPort" }
        require(transport == Transport.WebSocket) {
            "only WebSocket is wired; QUIC support is coming soon"
        }
    }

    /**
     * The REST origin, e.g. `http://localhost:18080`. No trailing slash.
     *
     * This is the form `MigoClientOptions.baseUrl` expects; the gateway URL is derived
     * by the SDK from this one when the caller does not override it.
     */
    fun restBaseUrl(): String = "${restScheme.prefix()}://$host:$port"

    /**
     * The realtime gateway URL, e.g. `ws://localhost:18081/ws`. No trailing slash.
     *
     * The `/ws` path is the path the server exposes; this client does not let the user
     * change it because exposing the path would mean a self-hoster setting it and never
     * knowing which one the server actually answers. The path is the contract.
     */
    fun gatewayUrl(): String = "${gatewayScheme.prefix()}://$host:$gatewayPort/ws"

    companion object {
        /** The REST port the dev policy defaults to, matching `migod` on a fresh install. */
        const val DEFAULT_REST_PORT: Int = 18080

        /**
         * The default endpoint for a host. A loopback gets the dev pair (plain WS, plain
         * HTTP, gateway on the next port), anything else gets the production pair (WSS,
         * HTTPS, gateway on the same port).
         *
         * Splitting the rule from the constructor means a settings field that just lost
         * focus can rebuild the pair without re-running the whole endpoint construction.
         */
        fun defaultFor(host: String, port: Int = DEFAULT_REST_PORT): ServerEndpoint {
            // defaultSchemesForHost returns a Pair<GatewayScheme, RestScheme>; destructure
            // it into named locals so the constructor below can refer to each side by name.
            // A Pair has no `gatewayScheme` / `restScheme` fields, so accessing them with
            // `schemes.gatewayScheme` (the previous form) would not compile.
            val (gatewayScheme, restScheme) = defaultSchemesForHost(host)
            return ServerEndpoint(
                host = host.lowercase(),
                port = port,
                gatewayPort = if (restScheme == RestScheme.Https) port else port + 1,
                transport = Transport.WebSocket,
                gatewayScheme = gatewayScheme,
                restScheme = restScheme,
            )
        }

        /**
         * The dev-policy default: plain WebSocket on plain HTTP, with the gateway on the
         * next port. `ws://localhost:18080` for REST, `ws://localhost:18081/ws` for the
         * gateway.
         */
        fun loopbackDefault(host: String = "localhost", port: Int = DEFAULT_REST_PORT): ServerEndpoint =
            ServerEndpoint(
                host = host.lowercase(),
                port = port,
                gatewayPort = port + 1,
                transport = Transport.WebSocket,
                gatewayScheme = GatewayScheme.Ws,
                restScheme = RestScheme.Http,
            )

        /**
         * The production default: WSS over HTTPS, with the gateway on the same port as
         * REST. `https://migo.example.com:443` for REST, `wss://migo.example.com:443/ws`
         * for the gateway.
         */
        /** The VPS deployment's single-host endpoint, matching the running migod. */
        fun publicDeploymentDefault(): ServerEndpoint = ServerEndpoint(
            host = "152.53.102.150",
            port = 8080,
            gatewayPort = 8080,
            transport = Transport.WebSocket,
            gatewayScheme = GatewayScheme.Ws,
            restScheme = RestScheme.Http,
        )

        fun internetDefault(host: String, port: Int = 443): ServerEndpoint =
            ServerEndpoint(
                host = host.lowercase(),
                port = port,
                gatewayPort = port,
                transport = Transport.WebSocket,
                gatewayScheme = GatewayScheme.Wss,
                restScheme = RestScheme.Https,
            )

        /**
         * Resolves a REST origin (a `http://host:port` or `https://host:port` string) into
         * a [ServerEndpoint], the same way the legacy sign-in screen did.
         *
         * The bridge the resume path uses: the [SavedSession] still carries a single URL
         * string from before the form was structured, and the bootstrap needs the
         * structured record to build a [MigoClient]. The URL's own scheme is the ground
         * truth for TLS — a session saved against `http://152.53.102.150:8080` must not
         * come back as a TLS endpoint, or the resume handshake dies at the socket and
         * the user is dumped back on the sign-in screen.
         */
        fun fromRestUrl(url: String): ServerEndpoint {
            val trimmed = url.trim()
            if (trimmed.isEmpty()) return loopbackDefault()
            val lower = trimmed.lowercase()
            val (restScheme, authority) = when {
                lower.startsWith("https://") -> RestScheme.Https to trimmed.substring("https://".length)
                lower.startsWith("http://") -> RestScheme.Http to trimmed.substring("http://".length)
                else -> return loopbackDefault()
            }
            val hostAndPort = authority.substringBefore('/').substringBefore('?')
            val (host, port) = when (val colon = hostAndPort.lastIndexOf(':')) {
                -1 -> hostAndPort.lowercase() to if (restScheme == RestScheme.Https) 443 else 80
                else -> hostAndPort.substring(0, colon).lowercase() to
                    (hostAndPort.substring(colon + 1).toIntOrNull() ?: (if (restScheme == RestScheme.Https) 443 else 80))
            }
            // The origin's own scheme decides the posture. A `https://` origin keeps the
            // TLS pair with the gateway on the same port; an `http://` origin keeps the
            // plain pair, with the gateway on the next port only under the dev policy
            // (loopback) — a plain origin on a public host is a single-port deployment
            // like this build's, which serves `/ws` on its HTTP listener.
            return when (restScheme) {
                RestScheme.Https -> internetDefault(host, port)
                RestScheme.Http ->
                    if (isLoopbackHost(host)) {
                        loopbackDefault(host, port)
                    } else {
                        ServerEndpoint(
                            host = host,
                            port = port,
                            gatewayPort = port,
                            transport = Transport.WebSocket,
                            gatewayScheme = GatewayScheme.Ws,
                            restScheme = RestScheme.Http,
                        )
                    }
            }
        }

        /**
         * Picks a default scheme pair for a host. Loopback defaults to plain (dev),
         * anything else to TLS.
         */
        fun defaultSchemesForHost(host: String): Pair<GatewayScheme, RestScheme> =
            if (isLoopbackHost(host)) {
                GatewayScheme.Ws to RestScheme.Http
            } else {
                GatewayScheme.Wss to RestScheme.Https
            }

        /** Hosts the dev policy exempts from the "always TLS" default. */
        fun isLoopbackHost(host: String): Boolean {
            val lowered = host.lowercase()
            return lowered == "localhost" || lowered == "127.0.0.1" || lowered == "::1"
        }
    }
}

/** The realtime transport the user picked. */
enum class Transport {
    /** WebSocket, the only transport this build actually carries on the wire. */
    WebSocket,

    /**
     * QUIC. Shown in the form so the user can see the choice; selecting it is blocked
     * because the path is not yet built. A future commit that wires QUIC changes this
     * enum, not the form.
     */
    Quic,
}

/** The TLS posture of the realtime gateway. */
enum class GatewayScheme {
    /** Plain WebSocket. Allowed only for loopback hosts; see [ServerEndpoint.defaultFor]. */
    Ws,

    /** WebSocket over TLS. The production form. */
    Wss,
    ;

    /** The URL scheme prefix, taking the value's own posture only. */
    fun prefix(): String = when (this) {
        Ws -> "ws"
        Wss -> "wss"
    }
}

/** The TLS posture of the REST control plane. */
enum class RestScheme {
    /** Plain HTTP. Allowed only for loopback hosts. */
    Http,

    /** HTTPS. The production form. */
    Https,
    ;

    /** The URL scheme prefix, taking the value's own posture only. */
    fun prefix(): String = when (this) {
        Http -> "http"
        Https -> "https"
    }
}
