package com.migo.core.crypto

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.wire.idFromBytes
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The ratchet's state discipline: what a frame that fails to authenticate is allowed to change, and
 * what a message is bound to.
 *
 * # Why this file exists
 *
 * Every other property of this ratchet is pinned somewhere — the associated data by the conformance
 * vectors, the round trip by [com.migo.core.session.SessionCryptoTest], the AEAD underneath by
 * [CryptoVectorsTest]. What nothing pinned is the order of two operations inside the receive path:
 * deriving along a chain, and committing the result. That order is invisible on the happy path and
 * is the whole security property on the unhappy one, because the receive path is reached by anyone
 * who can send a frame to the device.
 *
 * The ratchet public key travels in the header unencrypted, so a forged frame needs **nothing
 * secret at all** — a replay of the key the receiver already tracks, a message number at or past
 * what it expects, and arbitrary bytes for the ciphertext. When the chain was advanced before the
 * tag was checked, that frame left the chain moved while `receivedCount` still pointed at the old
 * step: the genuine message at that number then derived its key from the wrong position in the
 * chain and never opened again. One frame, and that conversation is broken from then on — not
 * delayed, not garbled, permanently undecryptable, and reported by a person as "it says delivered
 * but nothing arrives".
 *
 * Both receive paths had it: the message in the chain being tracked, and the finishing of the
 * previous chain on a DH step. The first two tests below are the two shapes, and each fails on the
 * old order for its own reason rather than as a side effect of the other. They are ports of the Rust
 * pair in `server/crates/migo-crypto/src/ratchet.rs` and the TypeScript pair in
 * `packages/crypto/test/ratchet.test.ts`, because all four builds carry the same receive path and
 * none of them ever calls another.
 *
 * # Why the context tests exist
 *
 * The context a version-2 envelope binds is what stops a server relocating a ciphertext into another
 * conversation: the receiver rebuilds the context from the metadata the frame claims, and a tag
 * computed over different bytes does not verify. [Aad] and its conformance vectors pin the *bytes*;
 * what nothing pinned until these tests is that the ratchet actually feeds them to the AEAD on both
 * sides rather than accepting the parameter and ignoring it. An ignored parameter is silent: every
 * vector still passes, every round trip still works, and the protection section 11 asks for is
 * absent. The four tests below are ports of the four in the Rust reference — the round trip, a
 * context one bit apart, the empty context's compatibility with version 1, and the binding surviving
 * a turn of the DH ratchet, which is the case a per-chain bug would leave passing above and failing
 * in a real conversation's second reply.
 *
 * # What these tests deliberately do not check
 *
 * That the forged frame is *rejected* is asserted only so the rest of the test means something; a
 * rejection is the easy half and was never in doubt. The assertion that matters is the one after
 * it: the genuine message still opens.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same
 * C code the device runs, loaded for the host JVM.
 */
class RatchetTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }

    /** Two sessions that have completed X3DH, ready to exchange messages. */
    private data class Sessions(val alice: RatchetSession, val bob: RatchetSession)

    private fun pair(): Sessions {
        val aliceIdentity = IdentitySecret.generate()
        val bobIdentity = IdentitySecret.generate()
        val bobSignedPrekey = KeyPair.generate()
        val bobOneTime = KeyPair.generate()

        val bundle = PrekeyBundle(
            bobIdentity.public(),
            SignedPrekey.create(bobIdentity, 1L, bobSignedPrekey),
            OneTimePrekey(2L, bobOneTime.public()),
        )

        val initiation = X3dh.initiate(aliceIdentity, bundle)
        val bobSeed = X3dh.respond(bobIdentity, bobSignedPrekey, bobOneTime, initiation.message)

        return Sessions(
            RatchetSession.initiator(initiation.seed, bundle.signedPrekey.publicKey),
            RatchetSession.responder(bobSeed, bobSignedPrekey),
        )
    }

    /**
     * A well-formed version-2 context, the same shape the Rust reference builds.
     *
     * Not a random byte string: a context that failed to build at all would make the two negative
     * tests below pass for the wrong reason, so this is produced by [Aad.context] — the same function
     * a client will call — over three fixed ids.
     */
    private fun aContext(): ByteArray = Aad.context(
        EnvelopeVersion.V2,
        SCHEME_DOUBLE_RATCHET,
        senderDevice = idFromBytes(ByteArray(16) { 0xa0.toByte() }),
        conversationId = idFromBytes(ByteArray(16) { 0xc0.toByte() }),
        messageId = idFromBytes(ByteArray(16) { 0xe0.toByte() }),
    )

    /** Asserts that [body] throws a [CryptoError], which is the only rejection this layer makes. */
    private fun throwsCryptoError(body: () -> Unit, why: String) {
        try {
            body()
        } catch (caught: CryptoError) {
            return
        }
        fail("$why: expected CryptoError, got none")
    }

    @Test
    fun `a forged frame does not advance the chain being tracked`() {
        // The ratchet key is public, so this frame needs no secret: the key the attacker read off
        // the wire, a message number at or past what Bob expects, and any ciphertext. Two shapes,
        // because the two advance paths fail differently — no gap at all, and a gap wide enough to
        // populate the skipped-key list.
        val (alice, bob) = pair()
        val first = alice.encrypt("satu".toByteArray(), Aad.NO_CONTEXT)
        assertArrayEquals(
            "the first genuine message opens",
            "satu".toByteArray(),
            bob.decrypt(first.header, first.ciphertext, Aad.NO_CONTEXT),
        )

        for (gap in listOf(0L, 3L)) {
            val forged = RatchetHeader.of(
                first.header.ratchetKey,
                0L,
                first.header.messageNumber + 1 + gap,
            )
            throwsCryptoError(
                { bob.decrypt(forged, first.ciphertext, Aad.NO_CONTEXT) },
                "a replay of the current key with gap $gap must not open",
            )
        }

        val second = alice.encrypt("dua".toByteArray(), Aad.NO_CONTEXT)
        assertArrayEquals(
            "the genuine message still opens after two forged frames",
            "dua".toByteArray(),
            bob.decrypt(second.header, second.ciphertext, Aad.NO_CONTEXT),
        )
    }

    @Test
    fun `a forged frame does not advance the chain being left behind`() {
        // The same corruption one chain over. A frame claiming a ratchet key Bob is not tracking
        // sends him down the DH-ratchet path, where he finishes the chain he is leaving so messages
        // still in flight from it stay readable — and that finishing is what must not happen until
        // the frame is proven. The attacker supplies a well-formed key of their own, so the
        // rejection comes from the tag rather than from a malformed point.
        val (alice, bob) = pair()

        val a0 = alice.encrypt("a0".toByteArray(), Aad.NO_CONTEXT)
        assertArrayEquals(
            "a0 opens",
            "a0".toByteArray(),
            bob.decrypt(a0.header, a0.ciphertext, Aad.NO_CONTEXT),
        )

        // Two more Alice sends that Bob never receives: in flight on the chain he is about to leave.
        val held0 = alice.encrypt("a1".toByteArray(), Aad.NO_CONTEXT)
        val held1 = alice.encrypt("a2".toByteArray(), Aad.NO_CONTEXT)

        // Bob replies, so Alice turns the DH ratchet and her next send opens a new chain whose
        // `previousChainLength` names all three she has sent.
        val b0 = bob.encryptNext("b0".toByteArray(), Aad.NO_CONTEXT)
        assertArrayEquals(
            "b0 opens",
            "b0".toByteArray(),
            alice.decrypt(b0.header, b0.ciphertext, Aad.NO_CONTEXT),
        )
        val a3 = alice.encryptNext("a3".toByteArray(), Aad.NO_CONTEXT)
        assertEquals("three sent on the old chain", 3L, a3.header.previousChainLength)

        val forged = RatchetHeader.of(KeyPair.generate().public(), 3L, 0L)
        throwsCryptoError(
            { bob.decrypt(forged, a3.ciphertext, Aad.NO_CONTEXT) },
            "a frame on an unknown chain must not open",
        )

        assertArrayEquals(
            "the in-flight message still opens after a forged frame",
            "a1".toByteArray(),
            bob.decrypt(held0.header, held0.ciphertext, Aad.NO_CONTEXT),
        )
        assertArrayEquals(
            "and the one after it",
            "a2".toByteArray(),
            bob.decrypt(held1.header, held1.ciphertext, Aad.NO_CONTEXT),
        )

        // The genuine new-chain message still opens too, so the forged frame cost the session
        // nothing in either direction.
        assertArrayEquals(
            "the new chain still opens",
            "a3".toByteArray(),
            bob.decrypt(a3.header, a3.ciphertext, Aad.NO_CONTEXT),
        )
    }

    @Test
    fun `a context is bound to the message`() {
        val (alice, bob) = pair()
        val context = aContext()
        val sealed = alice.encrypt("halo".toByteArray(), context)
        assertArrayEquals(
            "a context the receiver rebuilds identically opens the message",
            "halo".toByteArray(),
            bob.decrypt(sealed.header, sealed.ciphertext, context),
        )
    }

    @Test
    fun `a message does not open under a different context`() {
        // One byte apart, because a context that only matched on whole fields would pass a test that
        // swapped a field and fail in the field on a version byte. A fresh pair each time: a failed
        // open is not something this test then retries, since the message key it derived is gone.
        val context = aContext()
        val other = context.copyOf()
        other[other.size - 1] = (other[other.size - 1].toInt() xor 0x01).toByte()

        val (alice, bob) = pair()
        val sealed = alice.encrypt("halo".toByteArray(), context)
        throwsCryptoError(
            { bob.decrypt(sealed.header, sealed.ciphertext, other) },
            "a context changed by one bit must not authenticate",
        )

        // And the converse: an empty context is not a wildcard that opens a message sealed under a
        // real one.
        val (alice2, bob2) = pair()
        val sealed2 = alice2.encrypt("halo".toByteArray(), context)
        throwsCryptoError(
            { bob2.decrypt(sealed2.header, sealed2.ciphertext, Aad.NO_CONTEXT) },
            "an empty context must not open a message sealed under a real one",
        )
    }

    @Test
    fun `an empty context is the version one associated data`() {
        // The compatibility claim, at the layer that would break if it were false: two sessions that
        // both pass nothing agree, and an empty context adds no bytes to what a version-1 envelope
        // authenticated. The assembled length is asserted against [Aad.assemble] rather than against
        // the session, which keeps its associated data private — the session's own half of the claim
        // is the round trip above it.
        val (alice, bob) = pair()
        val sealed = alice.encrypt("halo".toByteArray(), Aad.NO_CONTEXT)
        assertArrayEquals(
            "two sessions that both pass no context still agree",
            "halo".toByteArray(),
            bob.decrypt(sealed.header, sealed.ciphertext, Aad.NO_CONTEXT),
        )

        val prefix = ByteArray(64) { it.toByte() }
        assertEquals(
            "an empty context must add no bytes to the associated data",
            prefix.size + RatchetHeader.ENCODED_LEN,
            Aad.assemble(prefix, sealed.header.toBytes(), Aad.NO_CONTEXT).size,
        )
        assertArrayEquals(
            "and the assembled bytes are the version-1 prefix and header, nothing else",
            prefix + sealed.header.toBytes(),
            Aad.assemble(prefix, sealed.header.toBytes(), Aad.NO_CONTEXT),
        )
    }

    @Test
    fun `a context survives the dh ratchet`() {
        // The context is per message and the ratchet turns between messages, so a binding that only
        // worked within one chain would look correct in the test above and fail on the second reply
        // of every real conversation.
        val (alice, bob) = pair()
        val first = aContext()
        val one = alice.encrypt("one".toByteArray(), first)
        assertArrayEquals(
            "the first chain carries its context",
            "one".toByteArray(),
            bob.decrypt(one.header, one.ciphertext, first),
        )

        val second = first.copyOf()
        second[second.size - 1] = 0xe1.toByte()

        val two = bob.encryptNext("two".toByteArray(), second)
        assertArrayEquals(
            "the next chain carries a different one",
            "two".toByteArray(),
            alice.decrypt(two.header, two.ciphertext, second),
        )

        // And the binding is per message rather than per session: the context the first chain used
        // must not open a message the next chain sealed. The second pair has to exchange one message
        // first, for the same reason the first pair above does — the responder cannot send until it
        // has received, because until then it holds no peer ratchet key to step against.
        val (carol, dave) = pair()
        val opening = carol.encrypt("tiga".toByteArray(), first)
        assertArrayEquals(
            "the second pair's opening message opens",
            "tiga".toByteArray(),
            dave.decrypt(opening.header, opening.ciphertext, first),
        )
        val d0 = dave.encryptNext("empat".toByteArray(), second)
        throwsCryptoError(
            { carol.decrypt(d0.header, d0.ciphertext, first) },
            "the previous chain's context must not open a message from the next chain",
        )
        assertArrayEquals(
            "and the context it was sealed under still does",
            "empat".toByteArray(),
            carol.decrypt(d0.header, d0.ciphertext, second),
        )
    }
}
