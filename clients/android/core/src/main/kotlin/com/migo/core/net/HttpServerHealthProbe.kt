package com.migo.core.net

import com.migo.core.store.ServerEndpoint
import com.migo.core.store.ServerHealthProbe
import com.migo.core.store.ServerPicker
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import okhttp3.OkHttpClient

/**
 * The auto mode's probe over real HTTP: [Rest.health] against every server, on a client whose
 * own deadlines match the picker's 3s budget.
 *
 * A dedicated [OkHttpClient] rather than the [Rest] default because the two clients want opposite
 * things from a timeout: a sign-in waits up to ten seconds for Argon2 on a slow link -- an answer
 * nobody minds waiting for -- while a probe is a race, and a node that has not said `ok` in three
 * seconds has lost it. Sharing the long-deadline client would make the timeout a caller-side
 * cancellation of a request the socket layer keeps chasing.
 *
 * One client for every probe (the pools live inside an [OkHttpClient], so one per request would
 * re-handshake and litter threads), and the measured latency it returns is the wall clock around
 * the call, which is the number the picker compares. Anything that is not a 2xx -- a refusal, a
 * reset, a malformed URL -- is a server that did not answer, and returns null rather than an
 * exception: "unreachable" and "unhealthy" are the same fact to a picker.
 */
object HttpServerHealthProbe : ServerHealthProbe {

    // The socket layer gives up on the same second the resolver's coroutine timeout does, so a
    // probe the picker cancelled is not still chasing a handshake underneath.
    private val client: OkHttpClient = OkHttpClient.Builder()
        .callTimeout(ServerPicker.PROBE_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        .connectTimeout(ServerPicker.PROBE_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        .build()

    override suspend fun probe(endpoint: ServerEndpoint): Long? = try {
        val rest = Rest(endpoint.restBaseUrl(), client)
        val started = System.nanoTime()
        val up = rest.health()
        val elapsedMs = (System.nanoTime() - started) / 1_000_000L
        if (up) elapsedMs else null
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        null
    }
}
