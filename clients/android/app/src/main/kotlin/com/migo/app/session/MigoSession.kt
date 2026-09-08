package com.migo.app.session

import android.content.Context
import android.os.Build
import com.migo.core.ConnectionState
import com.migo.core.MigoClient
import com.migo.core.MigoClientOptions
import com.migo.core.account.AccountError
import com.migo.core.account.AccountFile
import com.migo.core.account.DeviceCredential
import com.migo.core.account.EvmWallet
import com.migo.core.account.IdentityKey
import com.migo.core.account.MigoRoot
import com.migo.core.account.openContainer
import com.migo.core.account.sealContainer
import com.migo.core.crypto.PeerSafetyNumber
import com.migo.core.crypto.pairSafetyNumber
import com.migo.core.domain.KeyStore
import com.migo.core.domain.SdkError
import com.migo.core.net.CaptchaProof
import com.migo.core.store.DeviceKeys
import com.migo.core.store.GatewayScheme
import com.migo.core.store.RestScheme
import com.migo.core.store.SavedSession
import com.migo.core.store.ServerEndpoint
import com.migo.core.store.SessionStore
import com.migo.core.store.Transport
import com.migo.core.store.TxRecord
import com.migo.core.store.Vault
import com.migo.core.store.VaultError
import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

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
     * Publishes the account's material: the ML-DSA identity key, and any of the root's first
     * wallets the server does not know yet.
     *
     * The identity key is whichever one the account currently trusts: the rotated key when this
     * device has rotated one, and the root's derivation until then. Re-publishing the root's
     * derivation after a rotation is the conflict the server answers with "rotate instead" --
     * quiet here, because this whole method is best-effort, but wrong on every sign-in, and the
     * idempotent-reconcile property this call exists for only holds for the key that is active.
     *
     * Best-effort by design — a failure here is not a failed sign-in, because the passphrase already
     * worked and the calls are idempotent: the next sign-in tries again. The address is a pure
     * function of the root, so "which wallets exist" is server state, not a matter of opinion, and
     * every address the root derives that is not registered gets registered.
     */
    private suspend fun enrolAccountMaterial() {
        val root = client.keyStore.root ?: return
        try {
            val identity = client.keyStore.rotatedIdentity ?: IdentityKey.fromRoot(root)
            client.publishIdentityKey(identity.publicKey())
            // The registry speaks canonical form (lowercase, no prefix) and the derivation speaks
            // EIP-55; both are folded before comparing, and archived rows count as known — a
            // wallet the user archived stays archived, and enrolment must not resurrect it.
            val known = client.registeredWallets().map { canonicalAddress(it.address) }.toSet()
            val wallet = EvmWallet.fromRoot(root, 0)
            if (wallet.addressCanonical() !in known) {
                client.registerWallet(wallet.addressChecksummed(), 0)
            }
        } catch (_: Exception) {
            // Deliberately quiet: the material publishes again on the next sign-in.
        }
    }

    /**
     * Rotates the account's ML-DSA-65 identity key, on the device that holds the key being
     * retired.
     *
     * What rotates is the account's *signing* identity — the key the login and add-device
     * ceremonies verify against. What deliberately does not: this device's E2EE identity, every
     * ratchet, every safety number a peer sees (those are separate material the ceremony never
     * touches), and this session itself (the server leaves sessions alone through a rotation).
     *
     * # The lifecycle this method is responsible for
     *
     * The successor is minted inside the ceremony, and the vault is the only home it will ever
     * have: the `.migo` container seals the *root*, whose derivation is the key being retired, so
     * after a rotation a container can still open but its identity half can no longer vouch for
     * the account — restoring one onto a new device will be refused by the server, and only a
     * device holding the sealed successor can answer the account's signing ceremonies. That cost
     * is why the caller's confirmation dialog says it out loud before the button is pressed.
     *
     * The persist is the ceremony's last step and its most important one: an install without a
     * save is an account whose active key exists nowhere. A [VaultError] from it therefore
     * propagates rather than being swallowed, and the caller reports it as the serious state it
     * is — the rotation *happened*, and this device failed to keep the proof.
     */
    suspend fun rotateIdentity() {
        val current = client.keyStore.accountIdentityKey()
            ?: throw SdkError("this device holds no account identity key to rotate")
        val successor = client.rotateIdentity(current)
        client.keyStore.installRotatedIdentity(successor)
        persist()
    }

    /**
     * Reads a direct conversation's safety numbers: one per device the peer currently publishes.
     *
     * A Migo identity belongs to a device, so a peer signed in twice publishes two of them, and
     * the honest report is one number per device rather than one number blurred across them. Each
     * is the pair number — this device's fingerprint and that peer device's, hashed together in a
     * symmetric order — so both people read the same string off their own screens and an aloud
     * comparison is meaningful.
     *
     * The changed flag is the whole point of the read. The store holds the last fingerprint this
     * conversation *acknowledged* for each device: a first observation is recorded silently,
     * because nothing changed, and a differing one is reported and deliberately left
     * unacknowledged — which is what keeps the warning on the screen until a person has seen it,
     * rather than clearing it in the same breath that detected it.
     */
    suspend fun safetyNumbers(conversationId: Id, peerUserId: Id): List<PeerSafetyNumber> {
        val own = client.keyStore.identity().public().fingerprint()
        // A plain loop rather than a map: the body suspends for the store reads, and `map`'s
        // lambda is not a suspend context. (The reads would have to be gathered first and the
        // comparisons done after, which is the same loop wearing two passes.)
        val report = ArrayList<PeerSafetyNumber>()
        for (peer in client.peerIdentities(peerUserId)) {
            val fingerprint = peer.identity.fingerprint()
            val stored = withContext(Dispatchers.IO) {
                store.loadPeerIdentity(conversationId, peer.deviceId)
            }
            if (stored == null) {
                withContext(Dispatchers.IO) {
                    store.savePeerIdentity(conversationId, peer.deviceId, fingerprint)
                }
            }
            report.add(
                PeerSafetyNumber(
                    deviceId = peer.deviceId,
                    number = pairSafetyNumber(own, fingerprint),
                    changed = stored != null && !stored.contentEquals(fingerprint),
                ),
            )
        }
        return report
    }

    /**
     * Marks the peer's current identities as acknowledged for a conversation, clearing its change
     * warnings.
     *
     * Called by the person, from the warning itself — never by the read that detected the change.
     * The identities are re-read from the client's per-run cache, so an acknowledgment costs no
     * prekeys; the trade is that a peer who rotated *again* between the report and this call has
     * that newer key acknowledged unseen, and the next open will say nothing about it. That window
     * is seconds wide and closes on the next conversation open, and the alternative — re-reporting
     * a change the person is mid-way through acknowledging — warns about nothing usefully.
     */
    suspend fun acknowledgeSafetyNumbers(conversationId: Id, peerUserId: Id) {
        for (peer in client.peerIdentities(peerUserId)) {
            val fingerprint = peer.identity.fingerprint()
            withContext(Dispatchers.IO) {
                store.savePeerIdentity(conversationId, peer.deviceId, fingerprint)
            }
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
            // passphrase. The refresh rotates the token, so the persist below is not optional.
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
         * Registers a new account and its founding device, and seals the `.migo` file.
         *
         * A registration is the founding device of a brand-new account (§182), so it mints the
         * account root and derives the E2EE identity from the root's E2EE domain — recoverable from
         * a `.migo` container, which is the point. The root is reused across attempts (§12): a
         * retry after a failed request is the same account-to-be, not a new one, and the identity
         * key travels with the request so the server can reconcile a retry whose first attempt
         * already landed. After the account exists, the root's public material is published and
         * wallet 0 registered, idempotently.
         *
         * The container is sealed here, from the *same* root that registered, before the root's
         * home moves to the session vault — a container reconstructed later could only rebuild
         * from the vault, and the whole guarantee of §12 is that this root and no other is the
         * account. The registration passphrase is the recovery credential, exactly as the web
         * client seals it: one secret to keep straight, and the file plus that passphrase is the
         * account's whole recovery story. A seal the container format refuses costs only the
         * offer — the account exists and the vault holds the root, and the Profile screen's
         * backup flow can still seal one on demand.
         *
         * [captcha] is the human check's answer when the gate on this network demanded one; the
         * form owns fetching and answering the challenge, and a refusal that carries a replacement
         * challenge is retried through this same door with it.
         */
        suspend fun register(
            context: Context,
            appVersion: String,
            endpoint: ServerEndpoint,
            username: String,
            passphrase: String,
            hooks: SessionHooks = SessionHooks(),
            captcha: CaptchaProof? = null,
        ): RegisteredAccount {
            val (vault, store) = reset(context)
            val root = pendingRegistrationRoot ?: MigoRoot.generate().also { pendingRegistrationRoot = it }
            val client = build(endpoint, appVersion, null, KeyStore.founding(root), store, hooks)
            val session = MigoSession(client, username, vault, store)
            client.register(username, passphrase, IdentityKey.fromRoot(root).publicKey(), captcha)
            session.enrolAccountMaterial()
            session.persist()
            pendingRegistrationRoot = null
            val container = try {
                val file = AccountFile
                    .new(root, System.currentTimeMillis() / 1000)
                    .forAccount(client.accountId.value)
                // Argon2 at the container's own cost is CPU work, so the seal runs on the default
                // dispatcher, exactly as the restore path's open does.
                withContext(Dispatchers.Default) { sealContainer(passphrase, file) }
            } catch (_: AccountError) {
                null
            }
            return RegisteredAccount(session, container)
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
            passphrase: String,
            hooks: SessionHooks = SessionHooks(),
            captcha: CaptchaProof? = null,
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
                client.login(identifier, passphrase, captcha)
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
            client.login(identifier, passphrase, captcha)
            session.persist()
            return session
        }

        /**
         * Restores the account onto this device from a `.migo` container, through one of two doors.
         *
         * The known-device door comes first: when the container's account is the account this
         * device's vault already holds, and the root in the file is the same root the vault sealed,
         * this is not a new device at all — it is this device coming back with its account file.
         * Nothing is reset. The stored identity, the ratchets and the device credential are reused
         * and the login ceremony (not add-device) signs the device back in, which is what preserves
         * the E2EE identity: a peer's safety number does not change because somebody re-signed in
         * from their own backup. The device credential is the vault's own sealed copy
         * ([com.migo.core.store.DeviceKeys.deviceCredential]) — the ceremony needs exactly the
         * credential the server has on the device row, and the vault is where that credential has
         * lived since the add-device ceremony that minted it.
         *
         * The new-device door is the one a different account takes — restoring another account
         * onto this phone is the explicit thing the person pressing "restore" asked for — and the
         * one every miss falls back to: [reset] wipes this device's stores, a fresh credential is
         * minted, and the add-device ceremony introduces a new device with a fresh, random E2EE
         * identity; a restore is a new device, and new devices never inherit another device's
         * ratchets. A vault that exists but will not load takes this door too, and the reset wipes
         * it — the same replacement a sign-in as a different account performs — because a vault
         * this build cannot open is not a device that can be signed back in as. So does a vault
         * that names the account and holds the root but sealed no device credential: a founding
         * device that registered with a passphrase has no credential to answer the login
         * ceremony, and only the add-device door can take it.
         *
         * Either way the container opens *before* anything local is destroyed: a wrong recovery
         * credential is a typo, and a typo must not wipe whatever device state was here. Only once
         * the root is out and the account named does [reset] replace it — and on the known-device
         * door nothing is destroyed at all. A login ceremony the server refuses (a device since
         * revoked, say) propagates rather than falling through to the new-device door: the
         * fall-through would reset the very identity the first door exists to preserve, and the
         * honest answer to a revoked device is the server's own error, with the vault intact to
         * sign in with the passphrase instead.
         *
         * [username] is the greeting and nothing more: the grant identifies the account by id,
         * and a blank field falls back to the account's public id text.
         */
        suspend fun restore(
            context: Context,
            appVersion: String,
            endpoint: ServerEndpoint,
            containerBytes: ByteArray,
            credential: String,
            username: String,
            hooks: SessionHooks = SessionHooks(),
        ): MigoSession {
            // Argon2 at the container's own cost: CPU work, not file work, so the default
            // dispatcher rather than the IO one the store paths use.
            val file = withContext(Dispatchers.Default) { openContainer(credential, containerBytes) }
            val accountIdText = file.accountId
                ?: throw SdkError(
                    "this container does not name its account (it was sealed by an older build); " +
                        "sign in with your passphrase instead",
                )
            val accountId = parseId(accountIdText)
            val root = file.root()
            val name = username.trim().ifEmpty { accountIdText }

            // The known-device door, peeked before anything local is touched. The root comparison
            // is by root bytes — the vault seals the raw 32 bytes
            // ([com.migo.core.store.DeviceKeys.root]), so the file and the vault are compared on
            // exactly the secret they share.
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
                if (keys == null) {
                    null
                } else {
                    val saved = keys.session
                    val knownCredential = keys.deviceCredential
                    val sameRoot = keys.root?.asBytes()?.contentEquals(root.asBytes()) == true
                    if (saved != null && knownCredential != null &&
                        saved.accountId == accountId && sameRoot
                    ) {
                        StoredVault(vault, keys, saved, knownCredential)
                    } else {
                        null
                    }
                }
            }

            if (stored != null) {
                val store = withContext(Dispatchers.IO) { SessionStore.open(context) }
                val client = build(
                    endpoint,
                    appVersion,
                    stored.session.deviceId,
                    KeyStore.restore(stored.keys),
                    store,
                    hooks,
                )
                val session = MigoSession(client, name, stored.vault, store)
                session.trackedTxs.addAll(stored.keys.txs)
                // The login ceremony, not add-device: this device is already on the account, and
                // the stored credential — the one the vault sealed when the device joined — is the
                // only credential that can answer for it. The identity half is whichever key the
                // vault holds as the account's active one: the rotated key when this device
                // rotated one (the root's derivation was retired by the very ceremony that minted
                // it, and signing with it is refused), and the root's derivation until then. The
                // add-device door below has no such choice — the container seals the root, and
                // after a rotation that root's identity half can no longer vouch for the account,
                // which is the documented cost of rotating.
                client.identityLogin(
                    stored.session.username,
                    stored.session.deviceId,
                    stored.keys.rotatedIdentity ?: IdentityKey.fromRoot(root),
                    stored.credential,
                )
                session.enrolAccountMaterial()
                session.persist()
                return session
            }

            val (vault, store) = reset(context)
            val deviceCredential = DeviceCredential.generate()
            val client =
                build(endpoint, appVersion, null, KeyStore.restored(root, deviceCredential), store, hooks)
            val session = MigoSession(client, name, vault, store)
            client.addDevice(accountId, IdentityKey.fromRoot(root), deviceCredential)
            session.enrolAccountMaterial()
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
         * QUIC -> ws) but dials the WebSocket listener. A TCP endpoint's fallback also dials the
         * WebSocket listener -- but on the *REST* port, not the record's gateway port: the native
         * listener at the gateway port speaks only the length-prefixed framing and carries no
         * WebSocket upgrade, while the server merges its gateway's `/ws` route into the HTTP
         * listener that already serves REST (this deployment: `ws://152.53.102.150:8080/ws`). The
         * posture follows the REST scheme, so a TLS-fronted deployment's fallback is `wss`. The
         * persisted record is untouched; only the socket this process opens is decided here, which
         * is where the wire path is free to differ from the record the user typed.
         */
        private fun wireGatewayUrl(endpoint: ServerEndpoint): String =
            when (endpoint.transport) {
                Transport.Tcp -> {
                    val scheme = if (endpoint.restScheme == RestScheme.Https) "wss" else "ws"
                    "$scheme://${endpoint.host}:${endpoint.port}/ws"
                }
                Transport.WebSocket -> endpoint.gatewayUrl()
                Transport.Quic -> {
                    val scheme = if (endpoint.gatewayScheme == GatewayScheme.QuicTls) "wss" else "ws"
                    "$scheme://${endpoint.host}:${endpoint.gatewayPort}/ws"
                }
            }
    }
}

/**
 * What a registration hands back: the live session, and the sealed `.migo` container minted from
 * the same root that registered.
 *
 * The container is sealed inside [MigoSession.register] rather than reconstructed later, because
 * the root lives exactly there for exactly that long (§12) — by the time any caller might rebuild
 * a container, the session vault is the root's home and a re-derivation is a second copy that can
 * drift. [container] is null only when the seal itself refused; the account is real either way,
 * and the Profile screen's backup flow can still seal one on demand.
 */
class RegisteredAccount(
    /** The signed-in session the registration produced. */
    val session: MigoSession,
    /** The sealed container bytes, ciphertext under the registration passphrase, or null when the seal refused. */
    val container: ByteArray?,
) {
    /** Ciphertext and counts only; the sealed bytes are never rendered. */
    override fun toString(): String =
        "RegisteredAccount(container: ${if (container != null) "${container.size} sealed bytes" else "none"})"
}

/**
 * The known-device door's answer: the stored vault and the three things the door needs from it.
 *
 * Exists so [MigoSession.restore] can decide the whole question inside one dispatch — whether this
 * vault names the container's account, holds the same root, and sealed a credential the login
 * ceremony can answer with — and hand the answer out as one non-null object, rather than
 * re-checking nullable fields across a module boundary the compiler cannot smart-cast through.
 */
private class StoredVault(
    val vault: Vault,
    val keys: DeviceKeys,
    val session: SavedSession,
    val credential: DeviceCredential,
)

/**
 * An address text folded to the registry's canonical form: lowercase hex, no prefix — the form
 * the server stores and returns, and the only form a comparison against it should use. EIP-55
 * and canonical are the same address but not the same string, and comparing them unfolded is how
 * a registered wallet reads as missing on every sign-in.
 */
private fun canonicalAddress(address: String): String = address.trim().lowercase().removePrefix("0x")
