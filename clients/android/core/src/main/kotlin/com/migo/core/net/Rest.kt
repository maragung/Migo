package com.migo.core.net

import com.migo.core.wire.Id
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/**
 * The REST half of the client: everything that happens before a WebSocket exists.
 *
 * Authentication is HTTP rather than protocol frames on purpose. A token has to be obtained before a
 * connection can be authenticated, so putting it on the gateway would mean an unauthenticated
 * connection that exists only to get a credential -- one more state to reason about, and one more
 * thing an unauthenticated peer can hold open. HTTP already has the semantics: a request, a status
 * code, a body, and a connection that closes. Brief section 118 permits exactly this bootstrap over
 * REST and nothing else; every operation a session then performs happens on the socket.
 *
 * This mirrors `clients/desktop/src/net/rest.rs` endpoint for endpoint and field for field, because
 * the server has one contract and three clients disagreeing about it would show up as a login that
 * works on one platform.
 *
 * # JSON here, never on the realtime path
 *
 * Section 178 forbids JSON on the realtime wire. It does not forbid it here: these four endpoints
 * are plain HTTP, they run once per launch rather than once per message, and an HTTP body that is
 * not JSON would need a second codec on the server for no gain. The realtime path below in
 * [Gateway] is MSE frames only.
 */

/** A REST failure, already reduced to something worth showing a person. */
sealed class RestError(message: String) : Exception(message) {
    /** The request never reached a server, or the answer never came back. */
    object Transport : RestError("cannot reach the server")

    /**
     * The server answered with its error envelope.
     *
     * [message] is the server's own `public_message()`, which is the only string it ever puts on the
     * wire -- internal detail stays on the server by construction (brief section 161), so it is safe
     * to show verbatim. [retryAfterMs] is present only when the server said how long to wait.
     */
    class Server(
        val code: Long,
        val symbol: String,
        val publicMessage: String,
        val retryAfterMs: Long?,
    ) : RestError(publicMessage)

    /** A 2xx whose body was not the shape this client expects. */
    object Malformed : RestError("the server's answer was not in the expected form")

    /** The configured server address is not usable. */
    object BadUrl : RestError("that server address is not a valid URL")
}

/**
 * A device as described to the server at sign-in.
 *
 * [deviceId] is null on first registration and set afterwards, which is what makes the server reuse
 * the same device row rather than issuing a new one on every launch. A new device id would mean a new
 * identity key, and every peer would see an unfamiliar device and a changed safety number.
 */
@Serializable
data class DeviceRequest(
    @SerialName("device_id") val deviceId: String? = null,
    val platform: String,
    @SerialName("display_name") val displayName: String,
    @SerialName("app_version") val appVersion: String,
    @SerialName("os_version") val osVersion: String? = null,
    @SerialName("device_model") val deviceModel: String? = null,
) {
    companion object {
        /**
         * Describes this device, honestly but minimally.
         *
         * The Android release goes in; the build fingerprint, the serial, and the account name do
         * not. A device list is a security feature -- it lets someone spot a session they do not
         * recognise -- and "Migo for Android" plus a model name serves that purpose. Anything finer
         * only serves fingerprinting.
         */
        fun describe(
            deviceId: Id?,
            appVersion: String,
            osVersion: String?,
            deviceModel: String?,
        ): DeviceRequest = DeviceRequest(
            deviceId = deviceId?.value,
            platform = "android",
            displayName = "Migo for Android",
            appVersion = appVersion,
            osVersion = osVersion,
            deviceModel = deviceModel,
        )
    }
}

@Serializable
private data class RegisterRequest(
    val username: String,
    val password: String,
    val locale: String,
    val device: DeviceRequest,
)

@Serializable
private data class LoginRequest(
    val identifier: String,
    val password: String,
    val device: DeviceRequest,
)

@Serializable
private data class RefreshRequest(
    @SerialName("refresh_token") val refreshToken: String,
    @SerialName("device_id") val deviceId: String,
)

@Serializable
private data class LogoutRequest(@SerialName("session_id") val sessionId: String)

/**
 * A session, as the server issues it.
 *
 * Never logged and never written anywhere but the sealed vault. [toString] is overridden rather than
 * left to the data-class default, which is what makes that a property of the type instead of a rule
 * everyone has to remember: the derived one would have put both tokens in the first log line somebody
 * added (brief sections 77 and 145).
 *
 * The server's response also carries two expiry timestamps, a capability mask, and an is-new-account
 * flag. `ignoreUnknownKeys` lets them arrive and be dropped: a field this client never reads is a
 * field that will drift out of step with the server without anything noticing.
 */
@Serializable
class Grant(
    @SerialName("account_id") val accountId: String,
    @SerialName("device_id") val deviceId: String,
    @SerialName("session_id") val sessionId: String,
    @SerialName("access_token") val accessToken: String,
    @SerialName("refresh_token") val refreshToken: String,
) {
    /** Ids identify rows and are fine in a log; the two token fields are credentials and are not. */
    override fun toString(): String =
        "Grant(account_id: $accountId, device_id: $deviceId, session_id: $sessionId, " +
            "access_token: ***, refresh_token: ***)"
}

@Serializable
private data class ErrorEnvelope(val error: ErrorBody)

@Serializable
private data class ErrorBody(
    val code: Long,
    val symbol: String,
    val message: String,
    @SerialName("retry_after_ms") val retryAfterMs: Long? = null,
)

/**
 * An HTTP client bound to one server.
 *
 * One [OkHttpClient] per instance, and it is meant to be long-lived: OkHttp's connection and thread
 * pools live inside it, so a client built per request would open a fresh TCP and TLS handshake for
 * every call and leave threads behind.
 */
class Rest(baseUrl: String, client: OkHttpClient? = null) {
    /** The server this client talks to, without a trailing slash. */
    val base: String = baseUrl.trimEnd('/').also {
        if (it.isEmpty() || !(it.startsWith("http://") || it.startsWith("https://"))) {
            throw RestError.BadUrl
        }
    }

    private val http: OkHttpClient = client ?: OkHttpClient.Builder()
        // A person waiting on a sign-in wants an answer or an error, not a spinner. Ten seconds is
        // long enough for Argon2 on the server plus a slow mobile link, and short enough that a
        // black-holed connection surfaces as a failure rather than a hang.
        .callTimeout(10, TimeUnit.SECONDS)
        .connectTimeout(5, TimeUnit.SECONDS)
        .build()

    private val json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = false
        explicitNulls = false
    }

    /**
     * The gateway URL for this server: the same host, `ws`/`wss`, path `/ws`.
     *
     * Derived rather than configured separately so a user who typed one address cannot end up with a
     * client whose API and gateway point at different deployments.
     */
    fun gatewayUrl(): String = when {
        base.startsWith("https://") -> "wss://" + base.removePrefix("https://") + "/ws"
        base.startsWith("http://") -> "ws://" + base.removePrefix("http://") + "/ws"
        else -> "wss://$base/ws"
    }

    /** Creates an account and a first device. */
    suspend fun register(
        username: String,
        password: String,
        device: DeviceRequest,
        locale: String = "en",
    ): Grant = post(
        "/v1/auth/register",
        RegisterRequest.serializer(),
        RegisterRequest(username, password, locale, device),
    )

    /**
     * Signs in an existing account.
     *
     * One identifier field, not a username field and an email field, because a user does not think
     * of those as different kinds of thing. The server decides which it is.
     */
    suspend fun login(identifier: String, password: String, device: DeviceRequest): Grant =
        post("/v1/auth/login", LoginRequest.serializer(), LoginRequest(identifier, password, device))

    /** Exchanges a saved refresh token for a fresh pair. */
    suspend fun refresh(refreshToken: String, deviceId: Id): Grant = post(
        "/v1/auth/refresh",
        RefreshRequest.serializer(),
        RefreshRequest(refreshToken, deviceId.value),
    )

    /**
     * Ends a session server-side.
     *
     * Failure is the caller's to ignore: the local keys are already gone by the time this runs, so a
     * server that cannot be reached does not make the sign-out any less complete on this device.
     */
    suspend fun logout(accessToken: String, sessionId: Id) {
        val body = json.encodeToString(LogoutRequest.serializer(), LogoutRequest(sessionId.value))
        val request = Request.Builder()
            .url("$base/v1/auth/logout")
            .header("Authorization", "Bearer $accessToken")
            .post(body.toRequestBody(JSON_MEDIA))
            .build()
        val response = execute(request)
        response.use {
            if (!it.isSuccessful) throw failure(it)
        }
    }

    /**
     * Posts a JSON body to one of the four bootstrap endpoints and reads a [Grant] back.
     *
     * The request serializer arrives as a value rather than through a reified type parameter, because
     * an inline reified function cannot touch this class's private members and the alternative --
     * making the HTTP plumbing public so an inline function can reach it -- would widen the API to
     * suit the codec. Every one of these endpoints answers with the same success shape, so the
     * response serializer does not need to vary.
     */
    private suspend fun <B> post(path: String, serializer: KSerializer<B>, body: B): Grant {
        val request = Request.Builder()
            .url(base + path)
            .post(json.encodeToString(serializer, body).toRequestBody(JSON_MEDIA))
            .build()
        val response = execute(request)
        return response.use {
            if (!it.isSuccessful) throw failure(it)
            val text = try {
                it.body?.string().orEmpty()
            } catch (_: IOException) {
                throw RestError.Transport
            }
            try {
                json.decodeFromString(Grant.serializer(), text)
            } catch (_: Exception) {
                throw RestError.Malformed
            }
        }
    }

    /**
     * Issues one request and turns OkHttp's callback into a suspension.
     *
     * Cancellable, and the cancel hook cancels the OkHttp call. A plain `suspendCoroutine` here would
     * leave a cancelled caller's socket open until its own timeout fired -- which on a phone that
     * just left a screen means a radio kept awake for ten seconds for an answer nobody will read.
     *
     * `onFailure` is answered with [RestError.Transport] and not with the [IOException]: the
     * exception's message names hosts and system call failures, and this one is going to end up in a
     * crash report.
     */
    private suspend fun execute(request: Request): Response =
        suspendCancellableCoroutine { continuation ->
            val call = http.newCall(request)
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (continuation.isActive) continuation.resumeWith(Result.failure(RestError.Transport))
                }

                override fun onResponse(call: Call, response: Response) {
                    if (continuation.isActive) {
                        continuation.resumeWith(Result.success(response))
                    } else {
                        response.close()
                    }
                }
            })
        }

    /**
     * Reads an error response into a [RestError].
     *
     * A body that will not parse still has to become an error, so the fallback reports the HTTP
     * status as the code with an empty symbol rather than claiming success. Nothing from the body is
     * logged here: the caller decides what to show, and the server's public message is the only
     * string it is allowed to be.
     */
    private fun failure(response: Response): RestError {
        val text = try {
            response.body?.string().orEmpty()
        } catch (_: IOException) {
            ""
        }
        val envelope = try {
            json.decodeFromString(ErrorEnvelope.serializer(), text)
        } catch (_: Exception) {
            null
        }
        val retryHeader = response.header("Retry-After")?.toLongOrNull()?.times(1000L)
        return if (envelope != null) {
            RestError.Server(
                envelope.error.code,
                envelope.error.symbol,
                envelope.error.message,
                envelope.error.retryAfterMs ?: retryHeader,
            )
        } else {
            RestError.Server(response.code.toLong(), "", "", retryHeader)
        }
    }

    private companion object {
        val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()
    }
}
