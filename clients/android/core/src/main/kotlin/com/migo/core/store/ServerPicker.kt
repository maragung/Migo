package com.migo.core.store

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.withTimeoutOrNull

/**
 * How the sign-in form chooses which server to talk to.
 *
 * A multi-node deployment asks a person a question they cannot answer -- "which node?" -- and the
 * honest default is not to ask it. [Auto] probes every known server in parallel and takes the
 * fastest one that answers, so a fresh install lands on a live node without its owner ever typing
 * an address. [Manual] is the self-hoster's and the operator's door: the endpoint on the settings
 * is the one the form committed, exactly as typed, and no probe rewrites it.
 *
 * Persisted by name in [Settings], the same forward-compatible rule every settings enum follows:
 * a value a build has never heard of reads as the default rather than crashing on launch.
 */
enum class ServerSelectionMode {
    /** Probe every known server and take the fastest responder. The default. */
    Auto,

    /** Use the endpoint exactly as the form committed it. */
    Manual,
}

/**
 * One server's answer to "are you there", as the auto mode needs it.
 *
 * Returns the elapsed milliseconds of a successful probe, or null when the server did not answer
 * in time (or answered with anything but a 2xx). The latency is the *probe's own* measurement, not
 * a wrapper's, so a fake in a test can name a latency directly and the pick logic stays the same
 * code path the real HTTP probe drives.
 *
 * An interface and not an OkHttp call site so the pick logic is testable without a socket: the
 * unit tests inject canned answers and hanging probes, and the HTTP implementation is a one-object
 * adapter over [com.migo.core.net.Rest].
 */
fun interface ServerHealthProbe {
    /**
     * Probes one server. Returns the elapsed milliseconds when the server is up, null when it is
     * not, and must honour cancellation (the auto resolver runs every probe under a timeout).
     */
    suspend fun probe(endpoint: ServerEndpoint): Long?
}

/**
 * The server picker: the list a deployment offers, and the auto mode that walks it.
 *
 * The list is a build-time fact -- a deployment's nodes are its operators' knowledge, not
 * something this device discovers -- so it arrives as one comma-separated string of REST origins
 * (the `migoServers` Gradle property, baked into the app's `BuildConfig`) and is parsed here into
 * the same structured [ServerEndpoint] the manual form commits. A self-hoster who builds their own
 * APK sets the property to their own nodes; the shipped default is this deployment's public node.
 *
 * # Why a health probe and not a sign-in attempt
 *
 * The probe is `GET {rest}/health`: anonymous, rate-limit-free (the operational tier, not the
 * bootstrap surface), and answered by a node that can serve a request at all. A sign-in attempt
 * would burn an Argon2id run on the server and a captcha gate's attention on a node the client
 * then abandons; the health route exists for exactly this question and answers it for nothing.
 *
 * # Never a silent pin
 *
 * When no server answers, the resolver returns null and the caller keeps auto mode with the last
 * resolution (or the default) on the settings -- the existing connection-error path then surfaces
 * the dead endpoint at sign-in. Pinning the first list entry would turn "every node is down" into
 * "this one address is broken", which is a different and wrong statement.
 */
object ServerPicker {

    /** The probe timeout. Short on purpose: a node that cannot say "ok" in three seconds is not
     *  the fastest responder, and the person is waiting on this answer before they can sign in. */
    const val PROBE_TIMEOUT_MS: Long = 3_000L

    /**
     * Parses the comma-separated server list into endpoints.
     *
     * Each entry is a REST origin (`http://host:port` or `https://host:port`) and is resolved by
     * [ServerEndpoint.fromRestUrl], so a list entry and a manually typed origin of the same string
     * produce the same record. Entries that do not name a scheme are dropped rather than parsed --
     * `fromRestUrl` answers a schemeless string with the loopback default, which would silently
     * turn a typo into `localhost` -- and blank entries (a trailing comma, an all-whitespace
     * property) are skipped. A string with no usable entries yields an empty list; the caller
     * decides what that means (the app falls back to the public deployment's single endpoint).
     */
    fun parseServers(raw: String): List<ServerEndpoint> = raw
        .split(',')
        .asSequence()
        .map { it.trim() }
        .filter { it.startsWith("http://") || it.startsWith("https://") }
        .map { ServerEndpoint.fromRestUrl(it) }
        .toList()

    /**
     * Picks the fastest responder from probe results.
     *
     * A null latency is a server that did not answer and is not a candidate; the smallest
     * non-null latency wins, and a tie keeps the earlier list entry -- the list's order is the
     * operator's preference, and a coin flip between equal latencies should still be
     * deterministic. All null (or no servers at all) yields null: no answer is no answer, never
     * a default in disguise.
     */
    fun fastestResponder(results: List<Pair<ServerEndpoint, Long?>>): ServerEndpoint? =
        results
            .asSequence()
            .filter { it.second != null }
            .minByOrNull { it.second!! }
            ?.first

    /**
     * Resolves the auto mode's server: probes every server in the list in parallel and returns
     * the fastest responder, or null when none answered.
     *
     * Every probe runs under [timeoutMs] however it is implemented, and a probe that hangs or
     * throws is a server that did not answer -- never a resolver that never returns or a round
     * that dies because one node reset the connection. The exceptions are swallowed *per probe*
     * (with [CancellationException] rethrown, so a cancelled caller still cancels), because one
     * broken node must not take the whole race down with it. The probes are launched together so
     * a slow node never delays the question to a fast one, and the winner is decided by
     * [fastestResponder] over the measured latencies, not by which coroutine resumed first: the
     * probe's own number is the fact being compared, and it is the same number the interface lets
     * a test name directly.
     */
    suspend fun resolveAuto(
        servers: List<ServerEndpoint>,
        probe: ServerHealthProbe,
        timeoutMs: Long = PROBE_TIMEOUT_MS,
    ): ServerEndpoint? {
        if (servers.isEmpty()) return null
        return coroutineScope {
            servers
                .map { server ->
                    async {
                        withTimeoutOrNull(timeoutMs) {
                            try {
                                probe.probe(server)
                            } catch (cancelled: CancellationException) {
                                throw cancelled
                            } catch (_: Exception) {
                                null
                            }
                        }
                    }
                }
                .awaitAll()
                // zip's own pair is (latency, server); the pick wants (server, latency), so the
                // transform names both halves rather than leaving the reader to un-swap it.
                .zip(servers) { latency, server -> server to latency }
                .let(::fastestResponder)
        }
    }
}
