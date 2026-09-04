package com.migo.core.net

import com.migo.core.wire.Id
import java.io.IOException
import java.util.Base64
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
    val passphrase: String,
    val locale: String,
    val device: DeviceRequest,
    // The account identity's ML-DSA-65 public key, base64, when the registering device already
    // holds the account root (§12). Null by default and the Json instance skips it, so a
    // passphrase-only caller's wire shape is unchanged.
    @SerialName("identity_public_key") val identityPublicKey: String? = null,
)

@Serializable
private data class LoginRequest(
    val identifier: String,
    val passphrase: String,
    val device: DeviceRequest,
)

@Serializable
private data class RefreshRequest(
    @SerialName("refresh_token") val refreshToken: String,
    @SerialName("device_id") val deviceId: String,
)

@Serializable
private data class LogoutRequest(@SerialName("session_id") val sessionId: String)

/** The passphrase-change body: the current one proves the caller, the next one replaces it. */
@Serializable
private data class PassphraseRequest(
    @SerialName("current_passphrase") val currentPassphrase: String,
    @SerialName("new_passphrase") val newPassphrase: String,
)

/** The recovery-contact body: one string the server judges the shape of. */
@Serializable
private data class ContactRequest(@SerialName("email_or_phone") val emailOrPhone: String)

/** One body shape covers both anonymous ceremonies; the purpose picks the reading. */
@Serializable
private data class ChallengeRequest(
    val purpose: String,
    val identifier: String? = null,
    @SerialName("device_id") val deviceId: String? = null,
    @SerialName("account_id") val accountId: String? = null,
    val device: DeviceRequest? = null,
)

@Serializable
private data class TwoSignatureAnswer(
    @SerialName("challenge_id") val challengeId: String,
    @SerialName("identity_signature") val identitySignature: String,
    @SerialName("device_signature") val deviceSignature: String,
)

@Serializable
private data class AddDeviceAnswer(
    @SerialName("challenge_id") val challengeId: String,
    @SerialName("identity_signature") val identitySignature: String,
    @SerialName("device_public_key") val devicePublicKey: String,
    @SerialName("device_signature") val deviceSignature: String,
)

@Serializable
private data class RotationAnswer(
    @SerialName("challenge_id") val challengeId: String,
    val signature: String,
    @SerialName("new_public_key") val newPublicKey: String,
)

@Serializable
private data class KeyPublication(
    @SerialName("identity_public_key") val identityPublicKey: String,
    @SerialName("device_public_key") val devicePublicKey: String?,
)

// No default on chainType on purpose: the Json instance skips default-equal fields, and the
// server's own default for a missing chain_type is the empty string, not "evm" — a wallet
// silently registered with no chain would be a row every client has to special-case.
@Serializable
private data class WalletBody(
    val address: String,
    @SerialName("chain_type") val chainType: String,
    val label: String? = null,
    @SerialName("derivation_index") val derivationIndex: Int,
)

@Serializable
private data class DevicesResponse(val devices: List<DeviceSummary> = emptyList())

/** The answer to a device revoke: how many sessions ended with the device. */
@Serializable
data class DeviceRevoked(val ok: Boolean = false, val revoked: Long = 0)

@Serializable
private data class WalletsResponse(val wallets: List<WalletSummary> = emptyList())

/**
 * One appointed global admin, as the owner's list renders it. The appointer is carried on the
 * wire but not read here: the screen names the appointer in prose (always the Owner/CEO in
 * this version), and a field kept only to be ignored is a field that lies about the shape the
 * screen draws.
 */
@Serializable
data class AdminView(
    @SerialName("account_id") val accountId: String,
    val username: String,
    @SerialName("granted_by") val grantedBy: String? = null,
    @SerialName("granted_at_ms") val grantedAtMs: Long,
)

/**
 * What the caller may open of the admin surface. Owner comes from configuration, not data --
 * the deployment names its Owner/CEO -- so `owner: false` is an answer a client can act on
 * (hide the surface) rather than a failure to catch.
 */
@Serializable
data class AdminStanding(val owner: Boolean = false, val admin: Boolean = false)

@Serializable
private data class AdminsResponse(val admins: List<AdminView> = emptyList())

@Serializable
private data class GrantAdminRequest(val username: String)

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
 * An issued ML-DSA challenge: the canonical payload to sign, byte for byte.
 *
 * The payload is base64 and the client decodes it and signs the decoded bytes — it never
 * re-encodes a challenge, which is what keeps three ports from disagreeing about what was
 * signed. `expiresAtMs` is display material for a "challenge expired" message, nothing more.
 */
@Serializable
class IdentityChallenge(
    val payload: String,
    @SerialName("challenge_id") val challengeId: String,
    @SerialName("device_id") val deviceId: String,
    @SerialName("expires_at_ms") val expiresAtMs: Long,
)

/** One device of the caller's account, for their own security screen. */
@Serializable
data class DeviceSummary(
    @SerialName("device_id") val deviceId: String,
    @SerialName("display_name") val displayName: String,
    val platform: String,
    val status: String,
    @SerialName("created_at_ms") val createdAtMs: Long,
    @SerialName("last_seen_at_ms") val lastSeenAtMs: Long,
    @SerialName("has_credential") val hasCredential: Boolean,
    @SerialName("is_current") val isCurrent: Boolean,
)

/** One registered wallet, for the caller's own wallet list. Address and metadata only. */
@Serializable
data class WalletSummary(
    @SerialName("wallet_id") val walletId: String,
    val address: String,
    @SerialName("chain_type") val chainType: String,
    val label: String? = null,
    @SerialName("derivation_index") val derivationIndex: Int,
    val status: String,
    @SerialName("created_at_ms") val createdAtMs: Long,
    @SerialName("archived_at_ms") val archivedAtMs: Long? = null,
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

    /**
     * Creates an account and a first device.
     *
     * [identityPublicKey] is the account identity's ML-DSA-65 public key when the caller already
     * holds the account root: it makes registration idempotent (§12), because a retry whose first
     * attempt already landed is answered with a reconciliation instead of USERNAME_TAKEN. Omitted
     * from the wire when null.
     */
    suspend fun register(
        username: String,
        passphrase: String,
        device: DeviceRequest,
        locale: String = "en",
        identityPublicKey: ByteArray? = null,
    ): Grant = post(
        "/v1/auth/register",
        RegisterRequest.serializer(),
        RegisterRequest(
            username,
            passphrase,
            locale,
            device,
            identityPublicKey?.let { Base64.getEncoder().encodeToString(it) },
        ),
    )

    /**
     * Signs in an existing account.
     *
     * One identifier field, not a username field and an email field, because a user does not think
     * of those as different kinds of thing. The server decides which it is.
     */
    suspend fun login(identifier: String, passphrase: String, device: DeviceRequest): Grant =
        post("/v1/auth/login", LoginRequest.serializer(), LoginRequest(identifier, passphrase, device))

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

    // --- the ML-DSA identity ceremonies ---------------------------------------------
    //
    // A second front door to a session: a client holding the account root asks for a challenge,
    // signs the canonical bytes it is given with both the identity key and the device credential,
    // and answers with the two signatures. The signatures arrive here already made — this class
    // transports bytes, it does not hold keys, which keeps the crypto in the account package and
    // the network in this one.

    /** Asks for a login challenge bound to a registered device. */
    suspend fun identityLoginChallenge(identifier: String, deviceId: Id): IdentityChallenge =
        respond(
            send(
                "POST",
                "/v1/auth/identity/challenge",
                json.encodeToString(
                    ChallengeRequest.serializer(),
                    ChallengeRequest(purpose = "login", identifier = identifier, deviceId = deviceId.value),
                ),
            ),
            IdentityChallenge.serializer(),
        )

    /** Asks for an add-device challenge: restoring the account onto a new device from a container. */
    suspend fun addDeviceChallenge(accountId: Id, device: DeviceRequest): IdentityChallenge =
        respond(
            send(
                "POST",
                "/v1/auth/identity/challenge",
                json.encodeToString(
                    ChallengeRequest.serializer(),
                    ChallengeRequest(purpose = "add-device", accountId = accountId.value, device = device),
                ),
            ),
            IdentityChallenge.serializer(),
        )

    /** Answers a login challenge with both signatures and receives a session. */
    suspend fun identityLogin(
        challengeId: Id,
        identitySignature: ByteArray,
        deviceSignature: ByteArray,
    ): Grant = respond(
        send(
            "POST",
            "/v1/auth/identity/login",
            json.encodeToString(
                TwoSignatureAnswer.serializer(),
                TwoSignatureAnswer(
                    challengeId = challengeId.value,
                    identitySignature = Base64.getEncoder().encodeToString(identitySignature),
                    deviceSignature = Base64.getEncoder().encodeToString(deviceSignature),
                ),
            ),
        ),
        Grant.serializer(),
    )

    /** Answers an add-device challenge: introduces the new device's credential, receives a session. */
    suspend fun addDevice(
        challengeId: Id,
        identitySignature: ByteArray,
        devicePublicKey: ByteArray,
        deviceSignature: ByteArray,
    ): Grant = respond(
        send(
            "POST",
            "/v1/auth/identity/add-device",
            json.encodeToString(
                AddDeviceAnswer.serializer(),
                AddDeviceAnswer(
                    challengeId = challengeId.value,
                    identitySignature = Base64.getEncoder().encodeToString(identitySignature),
                    devicePublicKey = Base64.getEncoder().encodeToString(devicePublicKey),
                    deviceSignature = Base64.getEncoder().encodeToString(deviceSignature),
                ),
            ),
        ),
        Grant.serializer(),
    )

    /** Asks, as the caller's own authenticated device, for a rotation challenge. */
    suspend fun rotationChallenge(accessToken: String): IdentityChallenge =
        respond(
            send("POST", "/v1/auth/identity/rotate/challenge", "", accessToken),
            IdentityChallenge.serializer(),
        )

    /** Answers a rotation challenge with the current key's signature and the successor's public key. */
    suspend fun rotateIdentity(
        accessToken: String,
        challengeId: Id,
        signature: ByteArray,
        newPublicKey: ByteArray,
    ) {
        send(
            "POST",
            "/v1/auth/identity/rotate",
            json.encodeToString(
                RotationAnswer.serializer(),
                RotationAnswer(
                    challengeId = challengeId.value,
                    signature = Base64.getEncoder().encodeToString(signature),
                    newPublicKey = Base64.getEncoder().encodeToString(newPublicKey),
                ),
            ),
            accessToken,
        ).use { if (!it.isSuccessful) throw failure(it) }
    }

    /**
     * Publishes the caller's identity (and optionally device) public keys on a passphrase-era
     * account — the legacy upgrade door, idempotent by design.
     */
    suspend fun publishIdentityKey(
        accessToken: String,
        identityPublicKey: ByteArray,
        devicePublicKey: ByteArray?,
    ) {
        send(
            "POST",
            "/v1/auth/identity/key",
            json.encodeToString(
                KeyPublication.serializer(),
                KeyPublication(
                    identityPublicKey = Base64.getEncoder().encodeToString(identityPublicKey),
                    devicePublicKey = devicePublicKey?.let { Base64.getEncoder().encodeToString(it) },
                ),
            ),
            accessToken,
        ).use { if (!it.isSuccessful) throw failure(it) }
    }

    // --- the device and wallet surfaces ----------------------------------------------
    //
    // The authenticated read/write surface of the account's own metadata. Nothing here moves a
    // secret in either direction: the device list carries a public key's presence, and the wallet
    // registry carries an address.

    /** The caller's own devices, for their security screen. */
    suspend fun devices(accessToken: String): List<DeviceSummary> =
        respond(send("GET", "/v1/devices", null, accessToken), DevicesResponse.serializer()).devices

    /**
     * Removes one of the caller's devices: `POST /v1/devices/{id}/revoke`.
     *
     * The device can no longer authenticate, refresh, or open a WebSocket, and every session on it
     * ends (brief section 18) — which is why the answer says how many died.
     */
    suspend fun revokeDevice(accessToken: String, deviceId: Id): DeviceRevoked = respond(
        send("POST", "/v1/devices/${deviceId.value}/revoke", "", accessToken),
        DeviceRevoked.serializer(),
    )

    /** The caller's registered wallet addresses. */
    suspend fun wallets(accessToken: String): List<WalletSummary> =
        respond(send("GET", "/v1/wallets", null, accessToken), WalletsResponse.serializer()).wallets

    /** Registers (or idempotently re-registers) a wallet address on the caller's account. */
    suspend fun registerWallet(
        accessToken: String,
        address: String,
        derivationIndex: Int,
        chainType: String = "evm",
        label: String? = null,
    ): WalletSummary = respond(
        send(
            "PUT",
            "/v1/wallets",
            json.encodeToString(
                WalletBody.serializer(),
                WalletBody(
                    address = address,
                    chainType = chainType,
                    label = label,
                    derivationIndex = derivationIndex,
                ),
            ),
            accessToken,
        ),
        WalletSummary.serializer(),
    )

    /** Archives one of the caller's wallets. */
    suspend fun archiveWallet(accessToken: String, walletId: Id) {
        send("POST", "/v1/wallets/${walletId.value}", "", accessToken)
            .use { if (!it.isSuccessful) throw failure(it) }
    }

    /**
     * Changes the account's sign-in passphrase: `POST /v1/auth/passphrase`, answered with a
     * replacement [Grant].
     *
     * The replacement is the point: the change ends every other session of the account, and this
     * device's answer is the fresh pair that keeps it signed in. The caller must install the
     * grant it gets back -- tokens, session id -- the same way a sign-in does, or the next
     * refresh will use a token the server has already revoked.
     */
    suspend fun changePassphrase(
        accessToken: String,
        currentPassphrase: String,
        newPassphrase: String,
    ): Grant = respond(
        send(
            "POST",
            "/v1/auth/passphrase",
            json.encodeToString(
                PassphraseRequest.serializer(),
                PassphraseRequest(currentPassphrase, newPassphrase),
            ),
            accessToken,
        ),
        Grant.serializer(),
    )

    /**
     * Records (or replaces) the caller's recoverable contact: `PUT /v1/auth/contact`, answered
     * 204.
     *
     * One string, and the server is the judge of the shape: an email containing `@` or a phone
     * starting with `+`, normalised on arrival so the store's unique index sees one canonical
     * value rather than every user's first guess. A replace rather than an append -- the account
     * keeps one contact, and saving a new one is the whole request.
     */
    suspend fun setContact(accessToken: String, emailOrPhone: String) {
        send(
            "PUT",
            "/v1/auth/contact",
            json.encodeToString(ContactRequest.serializer(), ContactRequest(emailOrPhone)),
            accessToken,
        ).use { if (!it.isSuccessful) throw failure(it) }
    }

    // --- the admin surface ------------------------------------------------------------
    //
    // The Owner/CEO's management surface over the global admins. Every route here except whoami
    // is owner-only on the server, so this client offers nothing it cannot honestly call: the
    // whoami gate decides whether the surface exists at all, and the reads and writes follow
    // only for the account the deployment names.

    /**
     * What the caller may open of the admin surface: `GET /v1/admins/whoami`.
     *
     * Never fails on standing -- an account that is neither owner nor admin gets
     * `{owner: false, admin: false}`, which is the answer, not an error -- so a client that
     * asks on sign-in can decide whether its owner surface exists without a refusal to catch.
     */
    suspend fun adminStanding(accessToken: String): AdminStanding =
        respond(send("GET", "/v1/admins/whoami", null, accessToken), AdminStanding.serializer())

    /** Every global admin, with usernames resolved: `GET /v1/admins`. Owner-only. */
    suspend fun globalAdmins(accessToken: String): List<AdminView> =
        respond(send("GET", "/v1/admins", null, accessToken), AdminsResponse.serializer()).admins

    /**
     * Appoints a global admin by username: `PUT /v1/admins`, idempotent -- a repeated
     * appointment keeps the original grant. Owner-only.
     */
    suspend fun grantGlobalAdmin(accessToken: String, username: String): AdminView = respond(
        send(
            "PUT",
            "/v1/admins",
            json.encodeToString(GrantAdminRequest.serializer(), GrantAdminRequest(username)),
            accessToken,
        ),
        AdminView.serializer(),
    )

    /**
     * Revokes a global admin: `DELETE /v1/admins/{id}`, answered 204. Owner-only. Revoking an
     * account that is not one is a quiet 204 -- the same shape rule the wallet archive
     * follows, so the list that follows a revoke is the truth rather than the echo.
     */
    suspend fun revokeGlobalAdmin(accessToken: String, accountId: Id) {
        send("DELETE", "/v1/admins/${accountId.value}", null, accessToken)
            .use { if (!it.isSuccessful) throw failure(it) }
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
     * Issues one request with an optional body and bearer token, shared by every endpoint above.
     *
     * A null body means the method carries none (GET, or a POST whose handler reads no body); a
     * GET must not be given a body at all, since OkHttp rejects one and the server would not read
     * it anyway.
     */
    private suspend fun send(method: String, path: String, body: String?, accessToken: String? = null): Response {
        val builder = Request.Builder().url(base + path)
        if (accessToken != null) {
            builder.header("Authorization", "Bearer $accessToken")
        }
        when {
            body != null -> builder.method(method, body.toRequestBody(JSON_MEDIA))
            method == "GET" -> builder.get()
            else -> builder.method(method, ByteArray(0).toRequestBody(JSON_MEDIA))
        }
        return execute(builder.build())
    }

    /**
     * Reads a 2xx response body into one shape, with the same failure mapping the bootstrap
     * endpoints use: the server's error envelope when it answered with one, [RestError.Malformed]
     * when a 2xx body was not the shape this client expects.
     */
    private suspend fun <T> respond(response: Response, serializer: KSerializer<T>): T =
        response.use {
            if (!it.isSuccessful) throw failure(it)
            val text = try {
                it.body?.string().orEmpty()
            } catch (_: IOException) {
                throw RestError.Transport
            }
            try {
                json.decodeFromString(serializer, text)
            } catch (_: Exception) {
                throw RestError.Malformed
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
