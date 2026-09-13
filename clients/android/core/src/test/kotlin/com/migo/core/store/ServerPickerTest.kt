package com.migo.core.store

import java.io.IOException
import kotlin.system.measureTimeMillis
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Tests for [ServerPicker]: the comma-separated server list a build carries, and the auto mode
 * that probes it.
 *
 * Runs on the JUnit task `:core:testDebugUnitTest`, the same plain-JVM runner the other core
 * tests use -- no emulator, no Compose, and deliberately no socket: the probe is the
 * [ServerHealthProbe] interface, and the HTTP object that implements it is a thin adapter over
 * [com.migo.core.net.Rest] whose own mapping (an exception to a transport failure) is exercised
 * through fakes that throw and hang here. The parallelism is asserted with wall-clock bounds on
 * coroutine delays, which are scheduled and not slept, so a slow runner widens the numbers
 * without flipping the verdict.
 */
class ServerPickerTest {

    // --- the list -------------------------------------------------------------------------

    @Test
    fun parseServers_deploymentOriginIsTheDeploymentEndpoint() {
        val servers = ServerPicker.parseServers("http://152.53.102.150:8080")
        assertEquals(listOf(ServerEndpoint.publicDeploymentDefault()), servers)
    }

    @Test
    fun parseServers_commaSeparatedOriginsWithWhitespace() {
        val servers = ServerPicker.parseServers(
            " http://node-a.example.com:8080 ,\thttps://node-b.example.com:8443\t, http://node-c.example.com:8080",
        )
        assertEquals(listOf("node-a.example.com", "node-b.example.com", "node-c.example.com"), servers.map { it.host })
        assertEquals(listOf(8080, 8443, 8080), servers.map { it.port })
        // The origin's own scheme decides the posture, the same rule the manual form follows:
        // the https entry is the TLS pair, the http entries are not.
        assertEquals(RestScheme.Https, servers[1].restScheme)
        assertEquals(RestScheme.Http, servers[0].restScheme)
        assertEquals(RestScheme.Http, servers[2].restScheme)
    }

    @Test
    fun parseServers_skipsSchemelessAndBlankEntries() {
        // A schemeless entry is dropped rather than parsed: fromRestUrl answers it with the
        // loopback default, which would silently turn a typo into `localhost`.
        val servers = ServerPicker.parseServers("152.53.102.150:8080, http://ok.example.com:9000, , ,")
        assertEquals(listOf("ok.example.com"), servers.map { it.host })
    }

    @Test
    fun parseServers_unusableStringYieldsEmptyList() {
        assertEquals(emptyList<ServerEndpoint>(), ServerPicker.parseServers(""))
        assertEquals(emptyList<ServerEndpoint>(), ServerPicker.parseServers("   "))
        assertEquals(emptyList<ServerEndpoint>(), ServerPicker.parseServers("migo.example.com"))
    }

    // --- the pick -------------------------------------------------------------------------

    @Test
    fun fastestResponder_picksTheSmallestLatency() {
        val a = ServerEndpoint.publicDeploymentDefault()
        val b = ServerEndpoint.loopbackDefault("localhost", 18080)
        val picked = ServerPicker.fastestResponder(listOf(a to 120L, b to 40L))
        assertEquals(b, picked)
    }

    @Test
    fun fastestResponder_allNullYieldsNull() {
        val a = ServerEndpoint.publicDeploymentDefault()
        val b = ServerEndpoint.loopbackDefault("localhost", 18080)
        assertNull(ServerPicker.fastestResponder(listOf(a to null, b to null)))
        assertNull(ServerPicker.fastestResponder(emptyList()))
    }

    @Test
    fun fastestResponder_tieKeepsTheEarlierEntry() {
        val a = ServerEndpoint.publicDeploymentDefault()
        val b = ServerEndpoint.internetDefault("other.example.com", 443)
        // Equal latencies: the list's order is the operator's preference, not a coin flip.
        assertEquals(a, ServerPicker.fastestResponder(listOf(a to 80L, b to 80L)))
    }

    // --- the auto resolution --------------------------------------------------------------

    @Test
    fun resolveAuto_picksTheFastestResponderFromAProbedList() = runBlocking {
        val fast = ServerEndpoint.publicDeploymentDefault()
        val slow = ServerEndpoint.loopbackDefault("localhost", 18080)
        val probe = ServerHealthProbe { endpoint ->
            if (endpoint == fast) 20L else if (endpoint == slow) 90L else null
        }
        assertEquals(fast, ServerPicker.resolveAuto(listOf(slow, fast), probe, timeoutMs = 5_000L))
    }

    @Test
    fun resolveAuto_probesInParallelNotInTurn() = runBlocking {
        val a = ServerEndpoint.publicDeploymentDefault()
        val b = ServerEndpoint.loopbackDefault("localhost", 18080)
        val c = ServerEndpoint.internetDefault("other.example.com", 443)
        // Every probe answers after the same delay; in turn they would take three delays, in
        // parallel one. The latencies name the winner, the clock proves the parallelism.
        val probe = ServerHealthProbe { endpoint ->
            delay(150)
            when (endpoint) {
                a -> 30L
                b -> 10L
                else -> 20L
            }
        }
        val elapsed = measureTimeMillis {
            assertEquals(b, ServerPicker.resolveAuto(listOf(a, b, c), probe, timeoutMs = 5_000L))
        }
        assertTrue("probes ran sequentially: ${elapsed}ms", elapsed < 400L)
    }

    @Test
    fun resolveAuto_noResponderYieldsNullAndPinsNothing() = runBlocking {
        val a = ServerEndpoint.publicDeploymentDefault()
        val b = ServerEndpoint.loopbackDefault("localhost", 18080)
        val probe = ServerHealthProbe { null }
        assertNull(ServerPicker.resolveAuto(listOf(a, b), probe, timeoutMs = 5_000L))
    }

    @Test
    fun resolveAuto_hangingProbeIsADownServer() = runBlocking {
        val hanging = ServerEndpoint.publicDeploymentDefault()
        val alive = ServerEndpoint.loopbackDefault("localhost", 18080)
        val probe = ServerHealthProbe { endpoint ->
            if (endpoint == hanging) {
                delay(60_000)
                1L
            } else {
                25L
            }
        }
        val elapsed = measureTimeMillis {
            assertEquals(alive, ServerPicker.resolveAuto(listOf(hanging, alive), probe, timeoutMs = 150L))
        }
        assertTrue("the hanging probe outlived its timeout: ${elapsed}ms", elapsed < 5_000L)
    }

    @Test
    fun resolveAuto_throwingProbeIsADownServer() = runBlocking {
        val broken = ServerEndpoint.publicDeploymentDefault()
        val alive = ServerEndpoint.loopbackDefault("localhost", 18080)
        val probe = ServerHealthProbe { endpoint ->
            if (endpoint == broken) throw IOException("unreachable") else 15L
        }
        assertEquals(alive, ServerPicker.resolveAuto(listOf(broken, alive), probe, timeoutMs = 5_000L))
    }

    @Test
    fun resolveAuto_emptyListYieldsNull() = runBlocking {
        assertNull(ServerPicker.resolveAuto(emptyList(), ServerHealthProbe { 1L }))
    }
}
