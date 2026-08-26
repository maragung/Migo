package com.migo.app.session

import android.content.Context
import android.os.Build
import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.MigoClientOptions
import com.migo.core.domain.KeyStore
import com.migo.core.store.SessionStore
import com.migo.core.store.Vault
import com.migo.core.store.VaultError
import com.migo.core.wire.Id
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The default server for a debug build: the emulator's route to the host, where migod binds 8080. */
const val DEFAULT_SERVER_URL = "http://10.0.2.2:8080"

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
     * Seals the current identity, prekeys and grant into the vault.
     *
     * Call it after connecting and after anything that changes key material, because both refreshing
     * and replenishing rotate values the next launch needs: a refresh token that was rotated but not
     * saved means the next launch cannot resume, and a prekey published without its private half saved
     * means a session formed against it cannot be opened.
     *
     * @throws VaultError.NotWritten when the file could not be replaced
     */
    suspend fun persist() {
        val keys = client.snapshot(username)
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
                saved.serverUrl,
                appVersion,
                saved.deviceId,
                KeyStore.restore(keys),
                store,
                hooks,
            )
            val session = MigoSession(client, saved.username, vault, store)
            // Refresh before connecting: the stored access token is minutes old at best and hours old
            // in practice, and a handshake with an expired one fails in a way that looks like a bad
            // password. The refresh rotates the token, so the persist below is not optional.
            val grant = client.refreshWith(saved.refreshToken, saved.deviceId)
            client.resume(grant)
            session.persist()
            return session
        }

        /** Registers a new account and a first device, minting a fresh identity for it. */
        suspend fun register(
            context: Context,
            appVersion: String,
            serverUrl: String,
            username: String,
            password: String,
            hooks: SessionHooks = SessionHooks(),
        ): MigoSession {
            val (vault, store) = reset(context)
            val client = build(serverUrl, appVersion, null, KeyStore.create(), store, hooks)
            val session = MigoSession(client, username, vault, store)
            client.register(username, password)
            session.persist()
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
            serverUrl: String,
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
                    build(serverUrl, appVersion, deviceId, KeyStore.restore(keys), store, hooks)
                val session = MigoSession(client, identifier, vault, store)
                client.login(identifier, password)
                session.persist()
                return session
            }

            val (vault, store) = reset(context)
            val client = build(serverUrl, appVersion, null, KeyStore.create(), store, hooks)
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
            serverUrl: String,
            appVersion: String,
            deviceId: Id?,
            keyStore: KeyStore,
            store: SessionStore,
            hooks: SessionHooks,
        ): MigoClient = MigoClient.create(
            MigoClientOptions(
                baseUrl = serverUrl,
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
    }
}
