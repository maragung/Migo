package com.migo.core.domain

import com.migo.core.crypto.IdentityPublic
import com.migo.core.crypto.IdentitySecret
import com.migo.core.crypto.KeyPair
import com.migo.core.crypto.OneTimePrekey
import com.migo.core.crypto.PrekeyBundle
import com.migo.core.crypto.SignedPrekey
import com.migo.core.protocol.KeyBundle
import com.migo.core.protocol.KeyBundleRequest
import com.migo.core.protocol.KeyBundleResponse
import com.migo.core.protocol.KeyPublish
import com.migo.core.protocol.KeyPublishResult
import com.migo.core.protocol.Op
import com.migo.core.protocol.PrekeyEntry
import com.migo.core.session.IdentityProvider
import com.migo.core.session.LocalKeyStore
import com.migo.core.session.PeerBundleSource
import com.migo.core.store.DeviceKeys
import com.migo.core.store.SavedSession
import com.migo.core.wire.Id
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * Key material and the key-directory domain.
 *
 * Two things live here, mirroring `packages/sdk/src/domains/keys.ts`. [KeyStore] is this device's
 * private key material -- the identity, the current signed prekey, and the pool of one-time prekeys.
 * [KeysDomain] is the wire side: it publishes the *public* halves via `KEY_PUBLISH` and fetches peers'
 * bundles via `KEY_BUNDLE_FETCH`.
 *
 * The store satisfies both crypto-layer seams: [LocalKeyStore], which is what the 1:1 layer needs to
 * answer a first message (resolve a signed or one-time prekey by id), and [IdentityProvider], which is
 * what the group layer needs in order to sign a sender key. One object backs both, so a client wires a
 * single [KeyStore] into [com.migo.core.session.SessionCrypto] and
 * [com.migo.core.session.GroupCrypto] alike, and the two layers cannot end up signing with different
 * identities.
 *
 * # What crosses the wire
 *
 * Only public keys, key ids, and a signature. [KeyStore.publish] builds a [KeyPublish] out of public
 * halves; the seeds behind them stay inside the crypto objects, which hand out copies and never the
 * live buffers. That is the invariant making the server untrusted for confidentiality: it can choose
 * *which* bundle to serve, but every bundle it serves is verified against the claimed identity's
 * signature on the fetching device before any Diffie-Hellman happens -- [SignedPrekey.verify], called
 * by [com.migo.core.crypto.X3dh.initiate].
 *
 * # Why this file locks and the reference does not
 *
 * The web SDK runs on one event loop, so its store needs no synchronisation. Here the same store is
 * reached from two crypto objects that hold *different* locks: `SessionCrypto` guards its sessions
 * with a coroutine `Mutex`, `GroupCrypto` with a `ReentrantLock`, and neither knows about the other.
 * Two coroutines on different dispatchers can therefore be inside [consumeOneTimePrekey] and
 * [replenishOneTimePrekeys] at the same instant, which on a plain `HashMap` is not a lost entry but a
 * corrupted table. The lock here is what makes the store safe to share, and it is a plain
 * [ReentrantLock] rather than a `Mutex` because [LocalKeyStore] is a non-suspending interface -- it is
 * called from inside `SessionCrypto`'s own critical section, where suspending is not available.
 *
 * # No separate snapshot type
 *
 * The reference exports a `KeyStoreSnapshot` because a web caller persists the seeds itself. This
 * client already has exactly one place private key material is written -- [DeviceKeys], sealed by
 * [com.migo.core.store.Vault] under an Android Keystore key -- so [DeviceKeys] *is* the snapshot, and
 * [restore] and [export] are the two directions. A second representation of the same secret state
 * would be a second thing to keep in step, and the failure when it drifted would be a device that
 * silently loses its identity on restart.
 */

/** How many one-time prekeys a fresh store mints, matching the server's expected batch size. */
const val DEFAULT_ONE_TIME_PREKEYS = 64

/**
 * The most one-time prekeys the pool may hold at once.
 *
 * A hundred, because that is `migo_keys::model::MAX_ONE_TIME_PREKEYS` and the server refuses a
 * publication carrying more with `VALIDATION_FAILED`. [KeyStore.publish] publishes the *whole* pool
 * and publication replaces rather than merges, so a pool over the cap is not a partial publish: it is
 * a device that cannot publish at all until it drops keys. [KeyStore.replenishOneTimePrekeys]
 * enforces the bound so a caller cannot construct that state by asking twice.
 */
const val MAX_ONE_TIME_PREKEYS = 100

/**
 * The largest key id this client will mint.
 *
 * The server narrows every key id from `u32` to `i32` (`migo-keys/src/service.rs`, `fn key_id`) and
 * refuses anything that does not fit, so the wire's `u32` is not the real bound. Reaching this would
 * take four billion rotations on one device; the check exists so that if it ever did happen the
 * failure would be this local error rather than a publication the server rejects for reasons the
 * client cannot see.
 */
private const val MAX_KEY_ID = 2_147_483_647L

/** One published signed prekey: the id, the pair behind it, and the identity's signature over it. */
private class SignedPrekeyEntry(
    val keyId: Long,
    val pair: KeyPair,
    val signed: SignedPrekey,
)

/**
 * This device's private key material.
 *
 * Construct one with [KeyStore.create] for a new device, or [KeyStore.restore] to rebuild from the
 * [DeviceKeys] the vault held. Everything the two crypto layers ask of local material is answered
 * here; nothing here leaves the device except through [publish], which emits public data only.
 */
class KeyStore private constructor(
    private val identityKey: IdentitySecret,
    private var signedPrekey: SignedPrekeyEntry,
    private var nextSignedPrekeyId: Long,
    private var nextOneTimePrekeyId: Long,
) : LocalKeyStore, IdentityProvider {

    private val lock = ReentrantLock()
    private val oneTimePrekeys = HashMap<Long, KeyPair>()

    /**
     * This device's long-term identity secret. Backs both crypto layers.
     *
     * Returned rather than copied, because [IdentitySecret] is the holder of the seeds and there is
     * nothing to copy that would not be a second holder of the same secret. It is immutable, so
     * sharing it is safe; no lock is taken because the field never changes after construction.
     */
    override fun identity(): IdentitySecret = identityKey

    /** The pair for a published signed prekey id, or null once that id has been rotated away. */
    override fun signedPrekeyPair(signedPrekeyId: Long): KeyPair? = lock.withLock {
        if (signedPrekey.keyId == signedPrekeyId) signedPrekey.pair else null
    }

    /**
     * The pair for a published one-time prekey id *without* consuming it, or null if we do not hold
     * it.
     *
     * Peeking rather than consuming is what lets the 1:1 layer attempt a responder handshake for a
     * first message that was broadcast to every device and only spend the prekey once the decrypt
     * proves the message was for this one. Consuming here would burn a prekey on every sibling
     * device's copy of somebody else's message.
     */
    override fun oneTimePrekeyPair(keyId: Long): KeyPair? = lock.withLock {
        oneTimePrekeys[keyId]
    }

    /**
     * Permanently removes a one-time prekey from the pool, after a first message using it has opened.
     *
     * Idempotent: consuming an id that is already gone is a no-op, so a lost race cannot throw. A
     * replayed first message finds the prekey gone and can never derive a second session from it,
     * which is the property the one-time prekey exists for.
     */
    override fun consumeOneTimePrekey(keyId: Long) {
        lock.withLock { oneTimePrekeys.remove(keyId) }
    }

    /** How many unused one-time prekeys remain, so the client knows when to replenish. */
    fun oneTimePrekeyCount(): Int = lock.withLock { oneTimePrekeys.size }

    /**
     * Adds up to [count] fresh one-time prekeys and returns the public entries that were minted.
     *
     * Clamped to the headroom under [MAX_ONE_TIME_PREKEYS], and it returns what it actually minted
     * rather than what was asked for. Clamping instead of throwing is the right failure here: a
     * caller that asked for more than fits still ends up with a full pool and a publication the
     * server accepts, where a throw would leave a client unable to top up at all once its policy and
     * the server's cap disagreed.
     */
    fun replenishOneTimePrekeys(count: Int): List<PrekeyEntry> = lock.withLock {
        val room = MAX_ONE_TIME_PREKEYS - oneTimePrekeys.size
        val minting = minOf(count, room).coerceAtLeast(0)
        val added = ArrayList<PrekeyEntry>(minting)
        for (i in 0 until minting) {
            val keyId = takeOneTimePrekeyIdLocked()
            val pair = KeyPair.generate()
            oneTimePrekeys[keyId] = pair
            added.add(PrekeyEntry(keyId, pair.public()))
        }
        added
    }

    /**
     * Rotates in a new signed prekey, retiring the old one. The client republishes afterward.
     *
     * The retired pair is dropped, which is deliberate and is what forward secrecy for the signed
     * prekey means: a device seized tomorrow cannot answer a first message composed against the
     * prekey it published last month. The cost is that such a message no longer opens, which is why
     * section 163 gives the prekey a thirty-day lifetime rather than rotating it per message.
     */
    fun rotateSignedPrekey() {
        lock.withLock {
            val keyId = nextSignedPrekeyId
            if (keyId > MAX_KEY_ID) throw SdkError("keys: signed prekey ids exhausted")
            nextSignedPrekeyId = keyId + 1L
            signedPrekey = buildSignedPrekey(identityKey, keyId)
        }
    }

    /**
     * The public key material to publish to the server.
     *
     * The identity goes as its 64-byte wire form; the signed prekey carries the signature binding it
     * to that identity; every one-time prekey currently held is offered, because publication replaces
     * what the server has and a partial list would retire prekeys this device can still answer for.
     */
    fun publish(): KeyPublish = lock.withLock {
        val entries = ArrayList<PrekeyEntry>(oneTimePrekeys.size)
        for ((keyId, pair) in oneTimePrekeys) {
            entries.add(PrekeyEntry(keyId, pair.public()))
        }
        KeyPublish(
            identityKey = identityKey.public().toBytes(),
            signedPrekeyId = signedPrekey.keyId,
            signedPrekey = signedPrekey.signed.publicKey,
            signedPrekeySignature = signedPrekey.signed.signature,
            oneTimePrekeys = entries,
        )
    }

    /** The public identity, for fingerprint display and contact-change detection. */
    fun publicIdentity(): IdentityPublic = identityKey.public()

    /**
     * The full private state, for [com.migo.core.store.Vault] to seal.
     *
     * [session] is threaded through rather than held here because a sign-in is not key material: the
     * vault stores both in one blob, but the store has no business knowing a refresh token. The
     * returned map is a fresh copy, so a save that happens while a prekey is being consumed writes a
     * coherent pool rather than one being mutated underneath it.
     */
    fun export(session: SavedSession?): DeviceKeys = lock.withLock {
        DeviceKeys(
            identity = identityKey,
            signedPrekeyId = signedPrekey.keyId,
            signedPrekey = signedPrekey.pair,
            oneTime = HashMap(oneTimePrekeys),
            nextSignedPrekeyId = nextSignedPrekeyId,
            nextOneTimePrekeyId = nextOneTimePrekeyId,
            session = session,
        )
    }

    /** Public shape only. Never a seed, and never a count that is not already public. */
    override fun toString(): String =
        "KeyStore(identity: ${identityKey.public()}, signed_prekey_id: ${signedPrekey.keyId}, " +
            "one_time_count: ${oneTimePrekeyCount()})"

    /** Reserves the next one-time prekey id. Caller holds [lock]. */
    private fun takeOneTimePrekeyIdLocked(): Long {
        val keyId = nextOneTimePrekeyId
        if (keyId > MAX_KEY_ID) throw SdkError("keys: one-time prekey ids exhausted")
        nextOneTimePrekeyId = keyId + 1L
        return keyId
    }

    companion object {
        /**
         * Mints a new device's key material: a fresh identity, one signed prekey, and a batch of
         * one-time prekeys.
         *
         * The caller persists the result ([export], through the vault) and publishes the public
         * halves ([KeysDomain.publish]). Both matter: an identity that was generated but never saved
         * is a device that changes its safety number on every launch, and one that was saved but
         * never published is a device nobody can start a conversation with.
         */
        fun create(oneTimePrekeyCount: Int = DEFAULT_ONE_TIME_PREKEYS): KeyStore {
            val identity = IdentitySecret.generate()
            val store = KeyStore(identity, buildSignedPrekey(identity, 1L), 2L, 1L)
            store.replenishOneTimePrekeys(oneTimePrekeyCount)
            return store
        }

        /**
         * Rebuilds a store from the [DeviceKeys] the vault held.
         *
         * The signed prekey's signature is recomputed over the restored pair rather than stored,
         * because a signature is derivable from material already in the vault and storing it would
         * be one more thing that could be inconsistent with the key it signs. A restore therefore
         * round-trips to a byte-identical publication.
         */
        fun restore(keys: DeviceKeys): KeyStore {
            val entry = SignedPrekeyEntry(
                keys.signedPrekeyId,
                keys.signedPrekey,
                SignedPrekey.create(keys.identity, keys.signedPrekeyId, keys.signedPrekey),
            )
            val store = KeyStore(
                keys.identity,
                entry,
                keys.nextSignedPrekeyId,
                keys.nextOneTimePrekeyId,
            )
            store.oneTimePrekeys.putAll(keys.oneTime)
            return store
        }
    }
}

/** Rebuilds a signed prekey entry for [keyId] from a fresh pair signed by [identity]. */
private fun buildSignedPrekey(identity: IdentitySecret, keyId: Long): SignedPrekeyEntry {
    val pair = KeyPair.generate()
    return SignedPrekeyEntry(keyId, pair, SignedPrekey.create(identity, keyId, pair))
}

/** A peer device's id paired with its bundle, for enumerating a user's devices. */
class DeviceBundle(
    /** The device the bundle belongs to. */
    val deviceId: Id,
    /** Its published key material, rebuilt into the crypto layer's type. */
    val bundle: PrekeyBundle,
) {
    /** Public key material only; safe to log. */
    override fun toString(): String = "DeviceBundle(device_id: $deviceId)"
}

/**
 * The key-directory domain: publish our public keys, fetch peers'.
 *
 * Implements [PeerBundleSource] so the 1:1 layer can fetch one device's bundle on demand the first
 * time it needs to become an initiator. Also exposes [fetchDeviceBundles], which the messaging layer
 * needs because a sender key has to be distributed to every one of a user's devices, not to a user.
 */
class KeysDomain(
    private val rpc: Rpc,
    private val store: KeyStore,
) : PeerBundleSource {

    /**
     * Publishes this device's current public key material.
     *
     * Call it after [KeyStore.create], after [KeyStore.rotateSignedPrekey], and after
     * [KeyStore.replenishOneTimePrekeys]. A replenish that is not followed by a publish leaves the
     * server handing out the old batch, and every session formed from one of those is undecryptable
     * by this device.
     */
    suspend fun publish(): KeyPublishResult {
        val request = store.publish()
        return rpc.call(Op.KEY_PUBLISH, { w -> request.encode(w) }, { r -> KeyPublishResult.decode(r) })
    }

    /**
     * Fetches the bundle for one device of one user.
     *
     * Fails with [SdkError] when the server returns no bundle for the device that was asked for,
     * which covers both an empty response and a response about somebody else's devices. It is a
     * domain failure rather than a transport one: the connection is fine and exactly this operation
     * cannot proceed.
     *
     * The bundle is deliberately *not* verified here. [com.migo.core.crypto.X3dh.initiate] verifies
     * it immediately before the key agreement, and keeping verification in that one place is what
     * makes it impossible to reach a Diffie-Hellman through a path that forgot to check. Verifying
     * twice would read as a belt-and-braces improvement and would in fact make the single mandatory
     * check look optional.
     */
    override suspend fun fetchBundle(userId: Id, deviceId: Id): PrekeyBundle {
        val request = KeyBundleRequest(userId, deviceId)
        val response = rpc.call(
            Op.KEY_BUNDLE_FETCH,
            { w -> request.encode(w) },
            { r -> KeyBundleResponse.decode(r) },
        )
        val wire = response.bundles.firstOrNull { it.deviceId == deviceId }
            ?: throw SdkError("keys: server returned no bundle for device $deviceId")
        return toPrekeyBundle(wire)
    }

    /**
     * Fetches every device bundle a user currently publishes.
     *
     * The messaging layer uses this to learn which devices to seal a sender-key distribution for.
     * Each bundle returned consumes a one-time prekey on the server, so callers fetch once per
     * distribution round and never per message; the server caps one fetch at twenty devices
     * (`MAX_BUNDLES_PER_FETCH`) for the same reason.
     */
    suspend fun fetchDeviceBundles(userId: Id): List<DeviceBundle> {
        val request = KeyBundleRequest(userId)
        val response = rpc.call(
            Op.KEY_BUNDLE_FETCH,
            { w -> request.encode(w) },
            { r -> KeyBundleResponse.decode(r) },
        )
        return response.bundles.map { DeviceBundle(it.deviceId, toPrekeyBundle(it)) }
    }
}

/**
 * Rebuilds a crypto-layer [PrekeyBundle] from the wire [KeyBundle].
 *
 * A malformed identity key or a wrong-length prekey throws out of the constructors rather than being
 * repaired, and the throw is a [com.migo.core.crypto.CryptoError] that says which length was wrong
 * without carrying the bytes. A server that serves structurally broken material is a server whose
 * bundles this device must not use, and there is no partial bundle worth keeping.
 */
private fun toPrekeyBundle(wire: KeyBundle): PrekeyBundle {
    val identity = IdentityPublic.parse(wire.identityKey)
    val signedPrekey = SignedPrekey(
        wire.signedPrekeyId,
        wire.signedPrekey,
        wire.signedPrekeySignature,
    )
    // Both halves or neither. A key id with no key, or a key with no id, is a bundle the server
    // assembled wrongly, and treating it as "no one-time prekey" would silently drop this session
    // to the signed prekey alone -- a weaker guarantee, chosen by the untrusted party, with nothing
    // anywhere saying it happened.
    val keyId = wire.oneTimePrekeyId
    val publicKey = wire.oneTimePrekey
    val oneTimePrekey = when {
        keyId != null && publicKey != null -> OneTimePrekey(keyId, publicKey)
        keyId == null && publicKey == null -> null
        else -> throw SdkError("keys: bundle carries half a one-time prekey")
    }
    return PrekeyBundle(identity, signedPrekey, oneTimePrekey)
}
