package com.migo.core.store

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Round-trip and shape tests for [ServerEndpoint], the data class the Android form
 * commits and the bootstrap reads.
 *
 * Runs on the JUnit task `:core:testDebugUnitTest`, which the Android CI gate runs
 * on a plain `gradle` invocation -- no emulator, no Compose, no DataStore -- so the
 * same code path the form and the bootstrap exercise is the one these tests cover.
 *
 * The round-trip part is the one the brief asks for: a structured record is built,
 * its `restBaseUrl` and `gatewayUrl` are computed, the URLs are re-parsed by
 * [ServerEndpoint.fromRestUrl], and the result is the same record. A `MigoClient`
 * built from the original record and one built from the re-parsed record end up
 * pointing at the same REST origin and the same gateway URL.
 */
class ServerEndpointTest {

    @Test
    fun loopbackDefault_matchesDevPolicy() {
        val endpoint = ServerEndpoint.loopbackDefault("localhost", 18080)
        assertEquals("localhost", endpoint.host)
        assertEquals(18080, endpoint.port)
        assertEquals(18081, endpoint.gatewayPort)
        assertEquals(Transport.WebSocket, endpoint.transport)
        assertEquals(GatewayScheme.Ws, endpoint.gatewayScheme)
        assertEquals(RestScheme.Http, endpoint.restScheme)
        assertEquals("http://localhost:18080", endpoint.restBaseUrl())
        assertEquals("ws://localhost:18081/ws", endpoint.gatewayUrl())
    }

    @Test
    fun internetDefault_usesTlsPair() {
        val endpoint = ServerEndpoint.internetDefault("migo.example.com", 443)
        assertEquals("migo.example.com", endpoint.host)
        assertEquals(443, endpoint.port)
        assertEquals(443, endpoint.gatewayPort)
        assertEquals(Transport.WebSocket, endpoint.transport)
        assertEquals(GatewayScheme.Wss, endpoint.gatewayScheme)
        assertEquals(RestScheme.Https, endpoint.restScheme)
        assertEquals("https://migo.example.com:443", endpoint.restBaseUrl())
        assertEquals("wss://migo.example.com:443/ws", endpoint.gatewayUrl())
    }

    @Test
    fun defaultFor_recognisesTheThreeLoopbackSpellings() {
        val cases = listOf("localhost", "127.0.0.1", "::1")
        for (host in cases) {
            val endpoint = ServerEndpoint.defaultFor(host, 18080)
            assertEquals("plain pair for $host", RestScheme.Http, endpoint.restScheme)
            assertEquals("plain pair for $host", GatewayScheme.Ws, endpoint.gatewayScheme)
        }
    }

    @Test
    fun defaultFor_lowercasesTheHost() {
        val endpoint = ServerEndpoint.defaultFor("MIGO.Example.COM", 8443)
        assertEquals("migo.example.com", endpoint.host)
        assertEquals(8443, endpoint.port)
    }

    @Test
    fun restBaseUrl_andGatewayUrl_followThePostures() {
        val plain = ServerEndpoint(
            host = "localhost",
            port = 18080,
            gatewayPort = 18081,
            transport = Transport.WebSocket,
            gatewayScheme = GatewayScheme.Ws,
            restScheme = RestScheme.Http,
        )
        assertEquals("http://localhost:18080", plain.restBaseUrl())
        assertEquals("ws://localhost:18081/ws", plain.gatewayUrl())

        val tls = ServerEndpoint(
            host = "migo.example.com",
            port = 8443,
            gatewayPort = 8443,
            transport = Transport.WebSocket,
            gatewayScheme = GatewayScheme.Wss,
            restScheme = RestScheme.Https,
        )
        assertEquals("https://migo.example.com:8443", tls.restBaseUrl())
        assertEquals("wss://migo.example.com:8443/ws", tls.gatewayUrl())
    }

    @Test
    fun roundTrip_survivesParsingTheRestOrigin() {
        // The shape the form commits. The resume path bridges it through
        // [ServerEndpoint.fromRestUrl] and must end up with the same fields.
        //
        // The gateway shares the REST port here because that is the TLS posture every client
        // agrees on: a deployment reachable from outside terminates both the origin and the
        // socket on one ingress, so `defaultFor` derives `gatewayPort == port` for HTTPS and
        // only steps to `port + 1` for the plain-HTTP dev policy. A fixture that stepped the
        // port under TLS would assert a round trip no REST origin can carry, since the origin
        // names one port and nothing else.
        val original = ServerEndpoint(
            host = "migo.example.com",
            port = 8443,
            gatewayPort = 8443,
            transport = Transport.WebSocket,
            gatewayScheme = GatewayScheme.Wss,
            restScheme = RestScheme.Https,
        )
        val reparsed = ServerEndpoint.fromRestUrl(original.restBaseUrl())
        assertEquals(original, reparsed)
    }

    @Test
    fun roundTrip_loopbackAlsoSurvives() {
        val original = ServerEndpoint.loopbackDefault("localhost", 18080)
        val reparsed = ServerEndpoint.fromRestUrl(original.restBaseUrl())
        assertEquals(original, reparsed)
    }

    @Test
    fun fromRestUrl_picksTheLoopbackPolicyForLocalhost() {
        val reparsed = ServerEndpoint.fromRestUrl("http://localhost:18080")
        assertEquals(ServerEndpoint.loopbackDefault("localhost", 18080), reparsed)
    }

    @Test
    fun fromRestUrl_picksTheInternetPolicyForRealHosts() {
        val reparsed = ServerEndpoint.fromRestUrl("https://migo.example.com")
        // The default for a non-loopback host is HTTPS on 443, gateway on 443.
        assertEquals(RestScheme.Https, reparsed.restScheme)
        assertEquals(GatewayScheme.Wss, reparsed.gatewayScheme)
        assertEquals("migo.example.com", reparsed.host)
        assertEquals(443, reparsed.port)
    }

    @Test
    fun fromRestUrl_honoursAPlainHttpOriginOnAPublicHost() {
        // The resume path bridges through here with the origin the session was saved
        // against. This deployment's origin is plain HTTP, so the record must come back
        // plain too — guessing TLS for a non-loopback host would break the resume
        // handshake at the socket.
        val reparsed = ServerEndpoint.fromRestUrl("http://152.53.102.150:8080")
        assertEquals(ServerEndpoint.publicDeploymentDefault(), reparsed)
        assertEquals("http://152.53.102.150:8080", reparsed.restBaseUrl())
        assertEquals("ws://152.53.102.150:8080/ws", reparsed.gatewayUrl())
    }

    @Test
    fun roundTrip_thePublicDeploymentOriginSurvives() {
        val original = ServerEndpoint.publicDeploymentDefault()
        assertEquals(original, ServerEndpoint.fromRestUrl(original.restBaseUrl()))
    }

    @Test
    fun fromRestUrl_toleratesTrailingPathAndQuery() {
        val reparsed = ServerEndpoint.fromRestUrl("https://migo.example.com:8443/some/path?q=1")
        assertEquals("migo.example.com", reparsed.host)
        assertEquals(8443, reparsed.port)
    }

    @Test
    fun fromRestUrl_fallsBackToLoopbackWhenUnparseable() {
        val reparsed = ServerEndpoint.fromRestUrl("not a url")
        // No scheme recognised -> the dev default. The bootstrap will still try
        // to talk to it; a connection error is better than a crash on launch.
        assertEquals("localhost", reparsed.host)
        assertEquals(ServerEndpoint.DEFAULT_REST_PORT, reparsed.port)
    }

    @Test
    fun fromRestUrl_blankFallsBackToLoopback() {
        assertEquals(ServerEndpoint.loopbackDefault(), ServerEndpoint.fromRestUrl(""))
        assertEquals(ServerEndpoint.loopbackDefault(), ServerEndpoint.fromRestUrl("   "))
    }

    @Test
    fun isLoopbackHost_recognisesTheThreeSpellings() {
        assertTrue(ServerEndpoint.isLoopbackHost("localhost"))
        assertTrue(ServerEndpoint.isLoopbackHost("127.0.0.1"))
        assertTrue(ServerEndpoint.isLoopbackHost("::1"))
        assertTrue(ServerEndpoint.isLoopbackHost("LOCALHOST"))
        assertTrue(!ServerEndpoint.isLoopbackHost("migo.example.com"))
        assertTrue(!ServerEndpoint.isLoopbackHost("192.168.1.1"))
    }

    @Test
    fun blankHostIsRejected() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            ServerEndpoint(
                host = "",
                port = 18080,
                gatewayPort = 18081,
                transport = Transport.WebSocket,
                gatewayScheme = GatewayScheme.Ws,
                restScheme = RestScheme.Http,
            )
        }
        assertTrue("error mentions host: ${error.message}", error.message!!.contains("host"))
    }

    @Test
    fun outOfRangePortIsRejected() {
        val tooSmall = assertThrows(IllegalArgumentException::class.java) {
            ServerEndpoint(
                host = "localhost",
                port = 0,
                gatewayPort = 1,
                transport = Transport.WebSocket,
                gatewayScheme = GatewayScheme.Ws,
                restScheme = RestScheme.Http,
            )
        }
        assertTrue("error mentions port", tooSmall.message!!.contains("port"))

        val tooBig = assertThrows(IllegalArgumentException::class.java) {
            ServerEndpoint(
                host = "localhost",
                port = 18080,
                gatewayPort = 70000,
                transport = Transport.WebSocket,
                gatewayScheme = GatewayScheme.Ws,
                restScheme = RestScheme.Http,
            )
        }
        assertTrue("error mentions gateway port", tooBig.message!!.contains("gateway"))
    }

    @Test
    fun quicTransportIsRejected() {
        // The form hides QUIC behind a disabled radio; this test pins the data
        // class's stance so a future refactor cannot quietly start honouring it.
        val error = assertThrows(IllegalArgumentException::class.java) {
            ServerEndpoint(
                host = "localhost",
                port = 18080,
                gatewayPort = 18081,
                transport = Transport.Quic,
                gatewayScheme = GatewayScheme.Ws,
                restScheme = RestScheme.Http,
            )
        }
        assertTrue(
            "error mentions QUIC or WebSocket: ${error.message}",
            error.message!!.contains("QUIC") || error.message!!.contains("WebSocket"),
        )
    }

    @Test
    fun equalRecordsAreEqual() {
        val a = ServerEndpoint.loopbackDefault("localhost", 18080)
        val b = ServerEndpoint.loopbackDefault("localhost", 18080)
        assertEquals(a, b)
        assertEquals(a.hashCode(), b.hashCode())
    }

    @Test
    fun differentRecordsAreNotEqual() {
        val plain = ServerEndpoint.loopbackDefault("localhost", 18080)
        val other = ServerEndpoint.internetDefault("migo.example.com", 443)
        assertNotEquals(plain, other)
    }

    /**
     * The deployment healing: a stored record naming this deployment's host with an older
     * layout (the TLS pair the form's rule guesses for any non-loopback host, or the split
     * ports an early default carried) is rewritten to the deployment's single-port endpoint,
     * because that host is ours. Any other host is a self-hoster's record and stays as typed.
     */
    @Test
    fun healDeploymentEndpoint_rewritesAStaleRecordForTheDeploymentHost() {
        val stale = ServerEndpoint(
            host = "152.53.102.150",
            port = 18080,
            gatewayPort = 18081,
            transport = Transport.WebSocket,
            gatewayScheme = GatewayScheme.Wss,
            restScheme = RestScheme.Https,
        )
        val healed = healDeploymentEndpoint(stale, ServerEndpoint.publicDeploymentDefault())
        assertEquals(ServerEndpoint.publicDeploymentDefault(), healed)
        assertEquals("http://152.53.102.150:8080", healed.restBaseUrl())
        assertEquals("ws://152.53.102.150:8080/ws", healed.gatewayUrl())
    }

    @Test
    fun healDeploymentEndpoint_keepsASelfHostedRecord() {
        val mine = ServerEndpoint.loopbackDefault("home.example.org", 18080)
        assertEquals(mine, healDeploymentEndpoint(mine, ServerEndpoint.publicDeploymentDefault()))
    }

    @Test
    fun healDeploymentEndpoint_keepsAnAlreadyCorrectRecord() {
        val current = ServerEndpoint.publicDeploymentDefault()
        assertEquals(current, healDeploymentEndpoint(current, current))
    }
}
