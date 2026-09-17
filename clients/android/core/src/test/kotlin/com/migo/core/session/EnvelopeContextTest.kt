package com.migo.core.session

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.CryptoErrorKind
import com.migo.core.crypto.Envelope
import com.migo.core.crypto.IdentityPublic
import com.migo.core.crypto.OneTimePrekey
import com.migo.core.crypto.PrekeyBundle
import com.migo.core.crypto.SignedPrekey
import com.migo.core.crypto.Sodium
import com.migo.core.domain.KeyStore
import com.migo.core.domain.SdkError
import com.migo.core.wire.Id
import com.migo.core.wire.parseId
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.BeforeClass
import org.junit.Test

/**
 * What envelope version 2 binds, at the layer that has to get it right rather than at the ratchet.
 *
 * [com.migo.core.crypto.RatchetTest] pins that the ratchet feeds its context to the AEAD, and
 * `CryptoVectorsTest` pins the context's bytes. Neither pins the thing that actually protects a
 * conversation: that *this* layer builds the context out of the frame's claimed metadata and hands
 * it to the ratchet on both sides. A layer that accepted a context parameter and passed an empty
 * one would leave every test above green.
 *
 * Section 11 states the stake exactly. Content never travels through the pairwise layer -- every
 * message is sealed once under a sender key and this layer carries the *distribution* of that key
 * to one device. Relocating a distribution is therefore worse than a denial of service: it installs
 * a chain in a conversation the sender never authorised, and the recipient then reads whatever the
 * relocator sends under it. The first test below is that attack, and it is the only test here that
 * would have passed before the version-2 flip.
 *
 * The rest pin the mechanics it rests on: that the version byte really selects the associated data
 * at this layer, that a version no build writes is refused rather than guessed at, and that the
 * sender-key envelope did not move with the pairwise one -- the two schemes reach their binding
 * differently, so one version byte shared between them would have been a claim about group messages
 * that was not true.
 *
 * Real libsodium, through the desktop handle the other crypto suites load: the Android artifact's
 * native library only loads on a device.
 */
class EnvelopeContextTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }

    @Test
    fun `a distribution relocated to another conversation does not open`() {
        val f = DevicePair()
        val prekeysBefore = f.ada.keys.oneTimePrekeyCount()

        // A first message, which is the shape that makes the attack work: it carries X3DH material,
        // so a receiver with no session for the sender derives one rather than refusing outright.
        // Nothing in that derivation mentions the conversation -- the identities and the prekeys are
        // the same in every conversation the two devices share -- so before version 2 the ciphertext
        // opened wherever the server chose to put it.
        val sealed = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, CHAIN)
        }

        // The attack first, while the prekey is still unspent, so the assertion below is about a
        // prekey that must survive rather than one the genuine open has already taken.
        assertThrows(CryptoError::class.java) {
            runBlocking { f.ada.crypto.open(ELSEWHERE, ME, ME_DEVICE, sealed.envelope) }
        }
        assertEquals(
            "a message that failed to authenticate must not spend the prekey it named",
            prekeysBefore,
            f.ada.keys.oneTimePrekeyCount(),
        )

        // And the control: in the conversation it was sealed for, the same bytes open.
        val opened = runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, sealed.envelope) }
        assertArrayEquals("a distribution opens in its own conversation", CHAIN, opened)
        assertEquals(
            "and once it has genuinely opened, the prekey is spent",
            prekeysBefore - 1,
            f.ada.keys.oneTimePrekeyCount(),
        )
    }

    @Test
    fun `the version byte selects the associated data this layer builds`() {
        val f = DevicePair()
        val sealed = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, CHAIN)
        }

        // The envelope says what it is, so the receiver knows which associated data to rebuild
        // without being told out of band.
        assertEquals("the pairwise layer writes version 2", 2, sealed.envelope[0].toInt())

        // Relabelling it version 1 must break it. The ratchet would then seam an empty context where
        // the sender sealed a real one, so a tag that verifies can only mean the context never
        // reached the tag -- which is exactly the failure this whole step exists to rule out.
        val relabelled = sealed.envelope.copyOf()
        relabelled[0] = 1
        assertThrows(CryptoError::class.java) {
            runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, relabelled) }
        }

        // The genuine envelope still opens, so the failure above is the relabelling and not a
        // session the failed attempt damaged -- nothing is committed on a message that does not
        // authenticate.
        val opened = runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, sealed.envelope) }
        assertArrayEquals(CHAIN, opened)
    }

    @Test
    fun `a version no build writes is refused rather than guessed at`() {
        val f = DevicePair()
        val sealed = runBlocking {
            f.me.crypto.seal(CONVERSATION, ME_DEVICE, ADA, ADA_LAPTOP, CHAIN)
        }
        val future = sealed.envelope.copyOf()
        future[0] = 3

        // The parser is where it has to be refused: a version accepted here and then handed to a
        // context builder that has no rule for it would be decoded and then unopenable.
        val error = assertThrows(CryptoError::class.java) { Envelope.decode(future) }
        assertEquals(CryptoErrorKind.MalformedHeader, error.kind)

        assertThrows(CryptoError::class.java) {
            runBlocking { f.ada.crypto.open(CONVERSATION, ME, ME_DEVICE, future) }
        }
    }

    @Test
    fun `the sender-key envelope keeps its own version`() {
        // Content travels under scheme 3, which reaches its binding a different way: the conversation
        // id is the AEAD's associated data outright rather than a context appended to it. Sharing one
        // version constant across the two schemes would have made the pairwise flip relabel every
        // group message as carrying something it does not carry.
        val senderKey = GroupCrypto(KeyStore.create())
        val sealed = senderKey.sealContent(CONVERSATION, CHAIN)

        assertEquals("content still travels under scheme 3", 3, sealed.scheme)
        assertEquals("the sender-key envelope is still version 1", 1, sealed.envelope[0].toInt())
    }

    @Test
    fun `content is addressed by its conversation and its sending device`() {
        // The other half of the version-byte decision, and the reason scheme 3 keeps version 1: the
        // two values section 11 would bind here are already what selects the key. A receiver looks
        // the chain up by conversation and sending device, so an envelope relabelled into another
        // conversation, or as another device of the same account, is decrypted against a chain that
        // cannot have produced it and the tag refuses. Binding them into the associated data as well
        // would restate the addressing rather than add to it.
        val laptop = GroupCrypto(KeyStore.create())
        val phone = GroupCrypto(KeyStore.create())
        val bob = GroupCrypto(KeyStore.create())

        // Both of the account's devices distribute in the same conversation, which is the
        // arrangement that makes the second assertion below mean something: the receiver really does
        // hold two chains, so a refusal is the key being wrong rather than the key being absent.
        val laptopChain = laptop.distributionFor(CONVERSATION)
        bob.acceptDistribution(CONVERSATION, ME_DEVICE, laptopChain)
        bob.acceptDistribution(CONVERSATION, ME_OTHER_DEVICE, phone.distributionFor(CONVERSATION))
        // And the very same chain bytes under a second conversation: the same key, the same chain
        // id, a different conversation. That is what makes the third assertion below about the
        // associated data rather than about a missing key or a mismatched chain.
        bob.acceptDistribution(ELSEWHERE, ME_DEVICE, laptopChain)

        // Distributed before sealing, the order the messaging domain uses: the distribution captures
        // the chain at message 0, so the receiver opens the message sealed at that same position.
        val sealed = laptop.sealContent(CONVERSATION, CHAIN)

        // Its own conversation and its own device: the one arrangement that opens.
        assertArrayEquals(
            CHAIN,
            bob.open(CONVERSATION, ME_DEVICE, sealed.envelope),
        )

        // Another device of the same account: a different chain, so the tag refuses.
        assertThrows(CryptoError::class.java) {
            bob.open(CONVERSATION, ME_OTHER_DEVICE, sealed.envelope)
        }

        // Another conversation: the same key and the same chain id, so the only thing that differs
        // is the associated data, which is what the refusal has to be for this to say anything.
        assertThrows(CryptoError::class.java) {
            bob.open(ELSEWHERE, ME_DEVICE, sealed.envelope)
        }
    }
}

private val ME: Id = parseId("0123456789ABCDEFGHJKMNPQRV")
private val ME_DEVICE: Id = parseId("0123456789ABCDEFGHJKMNPQRW")
private val ME_OTHER_DEVICE: Id = parseId("0123456789ABCDEFGHJKMNPQRX")
private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
private val ADA_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRZ")
private val CONVERSATION: Id = parseId("0123456789ABCDEFGHJKMNPQ25")
private val ELSEWHERE: Id = parseId("0123456789ABCDEFGHJKMNPQ26")
private val CHAIN = "the sender key we distribute".toByteArray()

/** One side of the fixture: its key material and its session layer. */
private class DeviceSide(val keys: KeyStore, val crypto: SessionCrypto)

/**
 * Two devices and the bridge between them. The bridge is the whole "server" of this suite: it serves
 * each side's published prekey bundle to the other, the same public material the real key directory
 * would, so the X3DH handshake in either direction is the real one.
 */
private class DevicePair {
    private val bridge = ContextBridge()
    val me = side(ME_DEVICE)
    val ada = side(ADA_LAPTOP)

    private fun side(device: Id): DeviceSide {
        val keys = KeyStore.create(oneTimePrekeyCount = 4)
        bridge.publish(device, keys)
        return DeviceSide(keys, SessionCrypto(keys, bridge))
    }
}

/** Serves each device's own published material back to whoever asks, as the key directory would. */
private class ContextBridge : PeerBundleSource {
    private val stores = HashMap<Id, KeyStore>()

    fun publish(deviceId: Id, store: KeyStore) {
        stores[deviceId] = store
    }

    override suspend fun fetchBundle(userId: Id, deviceId: Id): PrekeyBundle {
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
