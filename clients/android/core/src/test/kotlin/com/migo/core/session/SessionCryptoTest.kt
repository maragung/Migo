package com.migo.core.session

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.IdentityPublic
import com.migo.core.crypto.OneTimePrekey
import com.migo.core.crypto.PrekeyBundle
import com.migo.core.crypto.RatchetSession
import com.migo.core.crypto.SHARED_SECRET_LEN
import com.migo.core.crypto.SignedPrekey
import com.migo.core.crypto.Sodium
import com.migo.core.domain.KeyStore
import com.migo.core.domain.SdkError
import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test

/**
 * The X3DH shared secret [SessionCrypto] retains per session — the input section 163's call-key
 * derivations read, and the one byte string both ends of a pairwise session hold identically once
 * the ratchet's root key has moved on.
 *
 * The secret is a cross-client contract before it is a local one: the SDK's `sessionSecret` and
 * the desktop store's `pairwise_secret` answer the same question with the same bytes, and the
 * wrapper key that seals a mid-call joiner's first frame key is derived from them on every build.
 * These tests pin the properties that make that agreement possible rather than any one vector:
 *
 *   1. **Both halves hold the same secret.** The initiator and the responder of one X3DH run
 *      retain byte-identical copies — if they ever diverged, the holder's answer would be a joiner
 *      that cannot open it.
 *   2. **The secret survives persistence.** A record written through [SessionPersistence] loads
 *      into a fresh instance with the same secret — and the restored ratchet still carries traffic,
 *      which is the point of the store.
 *   3. **Reads answer copies.** A caller zeroing what [SessionCrypto.sessionSecret] handed it
 *      cannot burn the stored copy.
 *   4. **A read never establishes.** Asking for a session that does not exist spends no prekey and
 *      fetches no bundle.
 *   5. **A record written before retention loads with a null secret.** The session still carries
 *      traffic; it just cannot take part in a unified join ask — legal state, not an error.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same
 * C code the device runs, loaded for the host JVM.
 */
class SessionCryptoTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }

    @Test
    fun `initiator and responder retain the same x3dh secret`() {
        val f = Fixture()

        val envelope = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, FIRST_MESSAGE)
        }
        val opened = runBlocking {
            f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, envelope.envelope)
        }
        assertArrayEquals("the handshake still carries traffic", FIRST_MESSAGE, opened)

        val initiatorSecret = runBlocking { f.me.crypto.sessionSecret(CONVERSATION, ADA_LAPTOP) }
        val responderSecret = runBlocking { f.ada.crypto.sessionSecret(CONVERSATION, ME_DEVICE) }
        assertNotNull("the initiator retained its half", initiatorSecret)
        assertNotNull("the responder retained its half", responderSecret)
        assertEquals("the secret is the X3DH shared secret, 32 bytes", SHARED_SECRET_LEN, initiatorSecret!!.size)
        assertArrayEquals(
            "both halves of one X3DH hold the same bytes, which is what makes the join wrapper agree",
            initiatorSecret,
            responderSecret,
        )
    }

    @Test
    fun `the secret survives persistence into a fresh instance`() {
        val f = Fixture()
        val envelope = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, FIRST_MESSAGE)
        }
        runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, envelope.envelope) }
        val before = runBlocking { f.ada.crypto.sessionSecret(CONVERSATION, ME_DEVICE) }
        assertTrue("the save carried the secret into the record", f.ada.persistence.savedWithSecret > 0)

        // A process restart: same keys, same records, a fresh SessionCrypto over both.
        val restarted = SessionCrypto(f.ada.keys, f.bridge, f.ada.persistence)
        val after = runBlocking { restarted.sessionSecret(CONVERSATION, ME_DEVICE) }
        assertArrayEquals("the secret survives the restart", before, after)

        // And the restored ratchet is not just present but working: the next message the peer
        // seals opens on the hydrated session, so the record round-trips the whole entry.
        val second = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, SECOND_MESSAGE)
        }
        val reopened = runBlocking { restarted.open(CONVERSATION, ME, ME_DEVICE, second.envelope) }
        assertArrayEquals("the restored session carries traffic", SECOND_MESSAGE, reopened)
    }

    @Test
    fun `sessionSecret answers a copy, not the stored bytes`() {
        val f = Fixture()
        val envelope = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, FIRST_MESSAGE)
        }
        runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, envelope.envelope) }

        val handed = runBlocking { f.ada.crypto.sessionSecret(CONVERSATION, ME_DEVICE) }!!
        // The caller burns its copy — a derivation wrapper zeroing its input, say — and asks again.
        handed.fill(0)
        val again = runBlocking { f.ada.crypto.sessionSecret(CONVERSATION, ME_DEVICE) }
        val intact = runBlocking { f.me.crypto.sessionSecret(CONVERSATION, ADA_LAPTOP) }
        assertArrayEquals(
            "zeroing a handed copy leaves the stored secret intact",
            intact,
            again,
        )
    }

    @Test
    fun `a read for a session that does not exist establishes nothing`() {
        val f = Fixture()

        val secret = runBlocking { f.me.crypto.sessionSecret(CONVERSATION, ADA_LAPTOP) }
        assertNull("no session answers null", secret)
        assertEquals(
            "a read must not fetch a bundle or mint a session — that would spend one of the peer's one-time prekeys",
            0,
            f.bridge.fetches,
        )
    }

    @Test
    fun `a record written before retention loads with a null secret but still carries traffic`() {
        val f = Fixture()
        val envelope = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, FIRST_MESSAGE)
        }
        runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, envelope.envelope) }

        // Rewrite the record as an older build would have written it: a session, no secret.
        f.ada.persistence.asLegacyRecords()
        val restarted = SessionCrypto(f.ada.keys, f.bridge, f.ada.persistence)
        assertNull(
            "a legacy record answers null — legal state, not an error",
            runBlocking { restarted.sessionSecret(CONVERSATION, ME_DEVICE) },
        )

        // The session itself never stopped working: it just cannot take part in a unified join ask.
        val second = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, SECOND_MESSAGE)
        }
        val reopened = runBlocking { restarted.open(CONVERSATION, ME, ME_DEVICE, second.envelope) }
        assertArrayEquals("the legacy record's session still carries traffic", SECOND_MESSAGE, reopened)
    }
}

private val ME: Id = parseId("0123456789ABCDEFGHJKMNPQRV")
private val ME_DEVICE: Id = parseId("0123456789ABCDEFGHJKMNPQRW")
private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
private val ADA_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRZ")
private val CONVERSATION: Id = parseId("0123456789ABCDEFGHJKMNPQ25")
private val FIRST_MESSAGE = "the first message over a fresh session".toByteArray()
private val SECOND_MESSAGE = "and a second one behind it".toByteArray()

/** One side of the pairwise fixture: its keys, its session layer, and its record store. */
private class Side(
    val keys: KeyStore,
    val crypto: SessionCrypto,
    val persistence: MemoryPersistence,
)

/**
 * Two devices and the bridge between them. The bridge is the whole "server" of this suite: it
 * serves each side's published prekey bundle to the other, the same public material the real key
 * directory would, so the X3DH handshake in either direction is the real one.
 */
private class Fixture {
    val bridge = Bridge()
    val me = side(ME_DEVICE)
    val ada = side(ADA_LAPTOP)

    private fun side(device: Id): Side {
        val keys = KeyStore.create(oneTimePrekeyCount = 4)
        bridge.publish(device, keys)
        val persistence = MemoryPersistence()
        return Side(keys, SessionCrypto(keys, bridge, persistence), persistence)
    }
}

/**
 * Serves each device's own published material back to whoever asks, as the key directory would,
 * and counts the fetches so a test can prove a read established nothing.
 */
private class Bridge : PeerBundleSource {
    private val stores = HashMap<Id, KeyStore>()
    var fetches = 0
        private set

    fun publish(deviceId: Id, store: KeyStore) {
        stores[deviceId] = store
    }

    override suspend fun fetchBundle(userId: Id, deviceId: Id): PrekeyBundle {
        fetches += 1
        val published = stores[deviceId]?.publish()
            ?: throw SdkError("no bundle published for device $deviceId")
        val oneTime = published.oneTimePrekeys.firstOrNull()
        return PrekeyBundle(
            IdentityPublic.parse(published.identityKey),
            SignedPrekey(published.signedPrekeyId, published.signedPrekey, published.signedPrekeySignature),
            oneTime?.let { OneTimePrekey(it.keyId, it.publicKey) },
        )
    }
}

/**
 * An in-memory [SessionPersistence] that round-trips each record through the ratchet's real
 * snapshot format, so "the secret survives persistence" is tested against the bytes a store
 * actually serialises, not against a held reference to the live session.
 */
private class MemoryPersistence : SessionPersistence {
    private class Record(val snapshot: ByteArray, var secret: ByteArray?)

    private val records = HashMap<String, Record>()

    /** How many saves carried a secret — the store contract's "both ways" half. */
    var savedWithSecret = 0
        private set

    /** Rewrites every record as an older build would have written it: a session, no secret. */
    fun asLegacyRecords() {
        for (record in records.values) {
            record.secret = null
        }
    }

    override fun load(conversationId: Id, deviceId: Id): StoredSession? {
        val record = records[key(conversationId, deviceId)] ?: return null
        return StoredSession(RatchetSession.restore(record.snapshot), record.secret?.copyOf())
    }

    override fun save(
        conversationId: Id,
        deviceId: Id,
        session: RatchetSession,
        sharedSecret: ByteArray?,
    ) {
        if (sharedSecret != null) savedWithSecret += 1
        records[key(conversationId, deviceId)] = Record(session.snapshot(), sharedSecret?.copyOf())
    }

    override fun delete(conversationId: Id, deviceId: Id) {
        records.remove(key(conversationId, deviceId))
    }

    override fun deleteConversation(conversationId: Id) {
        val prefix = "${conversationId.value}|"
        records.keys.removeAll { it.startsWith(prefix) }
    }

    private fun key(conversationId: Id, deviceId: Id) = "${conversationId.value}|${deviceId.value}"
}
