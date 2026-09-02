package com.migo.app.session

import android.content.Context
import android.os.Build
import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.MigoClientOptions
import com.migo.core.account.EvmWallet
import com.migo.core.account.IdentityKey
import com.migo.core.account.MigoRoot
import com.migo.core.domain.KeyStore
import com.migo.core.store.GatewayScheme
import com.migo.core.store.ServerEndpoint
import com.migo.core.store.SessionStore
import com.migo.core.store.Transport
import com.migo.core.store.TxRecord
import com.migo.core.store.Vault
import com.migo.core.store.VaultError
import com.migo.core.wire.Id
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The default server the form pre-fills with on first launch.
 *
 * Points at the emulator's route to the host machine (`10.0.2.2`) on `migod`'s default REST
 * port, with the gateway on the next one up. A user who has a server on the host picks
 * the same value by leaving the form at its defaults; a user with `migod` on a phone
 * changes the host to `127.0.0.1`. The form's "Use this server" button persists the
 * choice, so the defaults only ever show up on a fresh install.
 */
val DEFAULT_SERVER_ENDPOINT: ServerEndpoint = ServerEndpoint(
    host = "10.0.2.2",
    port = 8080,
    gatewayPort = 8081,
    transport = com.migo.core.store.Transport.WebSocket,
    gatewayScheme = com.migo.core.store.GatewayScheme.Ws,
    restScheme = com.migo.core.store.RestScheme.Http,
)

/** What the app wants told about a connection, as it happens rather than when something asks. */
class SessionHooks(
    /** Called on every transition, on whichever thread the SDK noticed it. */
    val onState: (ConnectionState) -> Unit = {},
    /** Called when a reconnect attempt fails. The client keeps retrying; this is for the banner. */
    val onError: (Throwable) -> Unit = {},
)

/**
 * One signed-in device: the SDK client, and the two stores that let it be the same device tomorrow.
 *
 * The SDK deliberately persists nothing on its own. [MigoClient] holds key material and ratchet state
 * in memory and hands it out through `snapshot` and the two persistence interfaces, leaving where any
 * of it lands to the application -- which is the only layer that knows about Android's directories and
 * its key store. This class is that decision, made once: a [Vault] for the identity, a [SessionStore]
 * for the ratchets, and a client wired to both.
 *
 * # Why the three are created together
 *
 * The wrong combination is worse than none. A client restored with a fresh identity but the previous
 * install's ratchets would decrypt nothing and produce a changed safety number for every peer; a
 * client with the stored identity but no ratchets recovers, because a session that cannot be read is
 * re-established from a fresh prekey bundle. So the constructors below either restore both or reset
 * both, and there is no way to hold this object with a mismatched pair.
 *
 * # Why the identity is reused only when the account matches
 *
 * On [signIn] the stored identity is reused when the stored [com.migo.core.store.SavedSession.username]
 * equals the identifier being signed in with. That is the ordinary case, a token that expired, and
 * reusing the identity keeps the peer's safety number unchanged rather than making a routine re-sign-in
 * look like a device compromise. When it does not match, a different account is being signed in on
 * this device: it gets a fresh identity, because one identity key serving two accounts would let any
 * peer of both link them to one device -- exactly what separate accounts are for.
 */
class MigoSession private constructor(
    /** The live SDK client. Connected by the time any constructor here returns. */
    val client: MigoClient,
    /** The account name, as typed. The client never learns it on a resume path; the vault holds it. */
    val username: String,
    private val vault: Vault,
    private val store: SessionStore,
) {
    /**
     * This device's tracked AVAX transactions, newest first — the wallet surface's live list.
     *
     * Seeded from the vault at construction and sealed back on every [persist], which is the same
     * trade the prekey pool makes: the list survives process death in the encrypted vault, and
     * between saves it lives here where the send flow can mutate it.
     */
    val trackedTxs: MutableList<TxRecord> = ArrayList()

    /**
     * Seals the current identity, prekeys, grant and tracked transactions into the vault.
     *
     * Call it after connecting and after anything that changes key material, because both refreshing
     * and replenishing rotate values the next launch needs: a refresh token that was rotated but not
     * saved means the next launch cannot resume, and a prekey published without its private half saved
     * means a session formed against it cannot be opened.
     *
     * @throws VaultError.NotWritten when the file could not be replaced
     */
    suspend fun persist() {
        val keys = client.snapshot(username, trackedTxs.toList())
        withContext(Dispatchers.IO) { vault.save(keys) }
    }

    /**
     * Disconnects and forgets everything this device knows.
     *
     * The client goes down first, so nothing is still writing sessions while they are being deleted.
     * Both stores are then destroyed with their wrapping keys, which is what makes the remaining files
     * undecryptable even if the delete did not reach the flash -- and what makes this object spent:
     * discard it, and go through the companion again to sign in.
     */
    suspend fun signOut() {
        client.close()
        withContext(Dispatchers.IO) {
            store.destroy()
            vault.destroy()
        }
    }

    /**
     * Disconnects without forgetting anything, for a process that is going away.
     *
     * The opposite of [signOut]: the next launch resumes this same device and the same sessions.
     */
    suspend fun close() {
        client.close()
    }

    /**
     * Publishes the root's account material: the ML-DSA identity key, and any of the root's first
     * wallets the server does not know yet.
     *
     * Best-effort by design — a failure here is not a failed sign-in, because the password already
     * worked and the calls are idempotent: the next sign-in tries again. The address is a pure
     * function of the root, so "which wallets exist" is server state, not a matter of opinion, and
     * every address the root derives that is not registered gets registered.
     */
    private suspend fun enrolAccountMaterial() {
        val root = client.keyStore.root ?: return
        try {
            client.publishIdentityKey(IdentityKey.fromRoot(root).publicKey())
            val known = client.registeredWallets().map { it.address }.toSet()
            val wallet = EvmWallet.fromRoot(root, 0)
            val address = wallet.addressChecksummed()
            if (address !in known) {
                client.registerWallet(address, 0)
            }
        } catch (_: Exception) {
            // Deliberately quiet: the material publishes again on the next sign-in.
        }
    }

    companion object {
        /**
         * Brings the stored device back online, or returns null if there is nothing stored.
         *
         * Null covers every reason a device has no session to resume: a fresh install, a vault the
         * platform key store can no longer open, and an identity that was saved before a sign-in ever
         * completed. All three lead to the same screen, and telling them apart would only give the
         * sign-in form three ways to say "sign in".
         *
         * A failure *after* the stored grant is read propagates instead, because a server that cannot
         * be reached is not a device that has been signed out, and answering that with a sign-in form
         * would throw away a working install over a flaky network.
         */
        suspend fun resumeStored(
            context: Context,
            appVersion: String,
            hooks: SessionHooks = SessionHooks(),
        ): MigoSession? {
            val vault = withContext(Dispatchers.IO) { Vault.open(context) }
            val keys = withContext(Dispatchers.IO) {
                if (!vault.exists()) return@withContext null
                try {
                    vault.load()
                } catch (_: VaultError) {
                    null
                }
            } ?: return null
            val saved = keys.session ?: return null

            val store = withContext(Dispatchers.IO) { SessionStore.open(context) }
            val client = build(
                ServerEndpoint.fromRestUrl(saved.serverUrl),
                appVersion,
                saved.deviceId,
                KeyStore.restore(keys),
                store,
                hooks,
            )
            val session = MigoSession(client, saved.username, vault, store)
            session.trackedTxs.addAll(keys.txs)
            // Refresh before connecting: the stored access token is minutes old at best and hours old
            // in practice, and a handshake with an expired one fails in a way that looks like a bad
            // password. The refresh rotates the token, so the persist below is not optional.
            val grant = client.refreshWith(saved.refreshToken, saved.deviceId)
            client.resume(grant)
            session.enrolAccountMaterial()
            session.persist()
            return session
        }

        /**
         * The root a registration attempt minted but has not yet made stick (§12). A registration
         * that fails after the server heard it must be retried with the *same* root: a fresh one
         * would be a different identity key, which the server can only answer with USERNAME_TAKEN.
         * Cleared the moment the account exists durably — from then on the session vault is the
         * root's home. Companion-scoped because each failed attempt tears the session down.
         */
        private var pendingRegistrationRoot: MigoRoot? = null

        /**
         * Registers a new account and its founding device.
         *
         * A registration is the founding device of a brand-new account (§182), so it mints the
         * account root and derives the E2EE identity from the root's E2EE domain — recoverable from
         * a `.migo` container, which is the point. The root is reused across attempts (§12): a
         * retry after a failed request is the same account-to-be, not a new one, and the identity
         * key travels with the request so the server can reconcile a retry whose first attempt
         * already landed. After the account exists, the root's public material is published and
         * wallet 0 registered, idempotently.
         */
        suspend fun register(
            context: Context,
            appVersion: String,
            endpoint: ServerEndpoint,
            username: String,
            password: String,
            hooks: SessionHooks = SessionHooks(),
        ): MigoSession {
            val (vault, store) = reset(context)
            val root = pendingRegistrationRoot ?: MigoRoot.generate().also { pendingRegistrationRoot = it }
            val client = build(endpoint, appVersion, null, KeyStore.founding(root), store, hooks)
            val session = MigoSession(client, username, vault, store)
            client.register(username, password, IdentityKey.fromRoot(root).publicKey())
            session.enrolAccountMaterial()
            session.persist()
            pendingRegistrationRoot = null
            return session
        }

        /**
         * Signs an existing account in, reusing this device's identity when it is the same account.
         *
         * [identifier] is a username, an email or a public id, in one field, because the server decides
         * which it is. The identity is only reused on an exact username match: an email that belongs to
         * the stored account still gets a fresh identity, which costs the peer a safety-number change
         * and is the safe direction to be wrong in.
         */
        suspend fun signIn(
            context: Context,
            appVersion: String,
            endpoint: ServerEndpoint,
            identifier: String,
            password: String,
            hooks: SessionHooks = SessionHooks(),
        ): MigoSession {
            val stored = withContext(Dispatchers.IO) {
                val vault = Vault.open(context)
                val keys = if (vault.exists()) {
                    try {
                        vault.load()
                    } catch (_: VaultError) {
                        null
                    }
                } else {
                    null
                }
                if (keys?.session?.username == identifier) Pair(vault, keys) else null
            }

            if (stored != null) {
                val (vault, keys) = stored
                val store = withContext(Dispatchers.IO) { SessionStore.open(context) }
                val deviceId = keys.session?.deviceId
                val client =
                    build(endpoint, appVersion, deviceId, KeyStore.restore(keys), store, hooks)
                val session = MigoSession(client, identifier, vault, store)
                session.trackedTxs.addAll(keys.txs)
                client.login(identifier, password)
                // A device that holds the root re-publishes its material on every sign-in: the
                // call is idempotent, and it is the legacy upgrade door that makes an account
                // created before the root existed ML-DSA-loginable the day its founding device
                // signs in again.
                session.enrolAccountMaterial()
                session.persist()
                return session
            }

            val (vault, store) = reset(context)
            val client = build(endpoint, appVersion, null, KeyStore.create(), store, hooks)
            val session = MigoSession(client, identifier, vault, store)
            client.login(identifier, password)
            session.persist()
            return session
        }

        /**
         * Wipes both stores and opens a fresh pair.
         *
         * Wipe before open, and through the static [SessionStore.wipe] rather than an instance: a
         * destroy deletes the wrapping key the instance is holding, so an instance that wiped itself
         * could no longer write and the first save would fail instead of starting clean. Opening
         * afterwards mints a new key, which is also what makes anything left behind on disk from the
         * previous account unreadable rather than merely deleted.
         */
        private suspend fun reset(context: Context): Pair<Vault, SessionStore> =
            withContext(Dispatchers.IO) {
                Vault.open(context).destroy()
                SessionStore.wipe(context)
                Pair(Vault.open(context), SessionStore.open(context))
            }

        /**
         * Builds the client. Nothing touches the network until the caller connects it.
         *
         * The same store instance is handed in as both persistence interfaces because it implements
         * both: the pairwise ratchets and the sender keys are halves of one conversation's state, and
         * deleting a conversation has to take both or leave a client that can still decrypt what it was
         * told to forget.
         *
         * The device description is the model and the Android release and nothing more. A device list
         * is a security feature -- it is how someone spots a session they do not recognise -- and the
         * build fingerprint would serve only whoever is fingerprinting.
         */
        private fun build(
            endpoint: ServerEndpoint,
            appVersion: String,
            deviceId: Id?,
            keyStore: KeyStore,
            store: SessionStore,
            hooks: SessionHooks,
        ): MigoClient = MigoClient.create(
            MigoClientOptions(
                baseUrl = endpoint.restBaseUrl(),
                gatewayUrl = wireGatewayUrl(endpoint),
                tcpGatewayAddress = wireTcpAddress(endpoint),
                appVersion = appVersion,
                osVersion = "Android ${Build.VERSION.RELEASE}",
                deviceModel = Build.MODEL,
                deviceId = deviceId,
                keyStore = keyStore,
                sessionPersistence = store,
                groupPersistence = store,
                onConnectionError = hooks.onError,
                onStateChange = hooks.onState,
            ),
        )

        /**
         * The raw TCP address this build dials first, or null when the endpoint does not ask for it.
         *
         * A TCP endpoint dials the native transport: host + gateway port, one connection, one
         * session, length-prefixed binary frames. A `TcpTls` posture is a production deployment's
         * TLS-fronted listener -- this build dials the address and the TLS posture is the
         * deployment's to terminate, the same trust story the desktop client tells. WebSocket and
         * QUIC endpoints return null and ride the WebSocket path: WebSocket is the web client's
         * transport and this build's fallback, and a QUIC endpoint has no Kotlin QUIC runtime to
         * dial yet. The MIGO_TCP_LIVE_ADDR contract covers this seam: the client tries TCP first
         * and falls back to WebSocket when the server does not negotiate the bit.
         */
        private fun wireTcpAddress(endpoint: ServerEndpoint): String? =
            when (endpoint.transport) {
                Transport.Tcp -> "${endpoint.host}:${endpoint.gatewayPort}"
                Transport.WebSocket, Transport.Quic -> null
            }

        /**
         * The gateway URL this build actually dials.
         *
         * [ServerEndpoint.gatewayUrl] is the canonical address the record derives, and for a QUIC
         * endpoint that is a `quic://` URL -- the realtime transport's second option, honoured by a
         * QUIC-capable client. This build has no Kotlin QUIC runtime, so its wire path always
         * connects over WebSocket: a QUIC endpoint keeps its TLS posture (QUIC-TLS -> wss, plain
         * QUIC -> ws) but dials the WebSocket listener. A TCP endpoint's fallback is the plain
         * WebSocket pair on the same host, because a server that is up but not speaking the native
         * transport still serves `/ws` on its HTTP listener. The persisted record is untouched;
         * only the socket this process opens is decided here, which is where the wire path is free
         * to differ from the record the user typed.
         */
        private fun wireGatewayUrl(endpoint: ServerEndpoint): String =
            when (endpoint.transport) {
                Transport.Tcp ->
                    "ws://${endpoint.host}:${endpoint.gatewayPort}/ws"
                Transport.WebSocket -> endpoint.gatewayUrl()
                Transport.Quic -> {
                    val scheme = if (endpoint.gatewayScheme == GatewayScheme.QuicTls) "wss" else "ws"
                    "$scheme://${endpoint.host}:${endpoint.gatewayPort}/ws"
                }
            }
    }
}
