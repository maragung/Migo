package com.migo.core.crypto

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The ratchet's state discipline: what a frame that fails to authenticate is allowed to change.
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
 * previous chain on a DH step. The two tests below are the two shapes, and each fails on the old
 * order for its own reason rather than as a side effect of the other. They are ports of the Rust
 * pair in `server/crates/migo-crypto/src/ratchet.rs` and the TypeScript pair in
 * `packages/crypto/test/ratchet.test.ts`, because all four builds carry the same receive path and
 * none of them ever calls another.
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
    private class Pair(val alice: RatchetSession, val bob: RatchetSession)

    private fun pair(): Pair {
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

        return Pair(
            RatchetSession.initiator(initiation.seed, bundle.signedPrekey.publicKey),
            RatchetSession.responder(bobSeed, bobSignedPrekey),
        )
    }

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
        val first = alice.encrypt("satu".toByteArray())
        assertArrayEquals(
            "the first genuine message opens",
            "satu".toByteArray(),
            bob.decrypt(first.header, first.ciphertext),
        )

        for (gap in listOf(0L, 3L)) {
            val forged = RatchetHeader.of(
                first.header.ratchetKey,
                0L,
                first.header.messageNumber + 1 + gap,
            )
            throwsCryptoError(
                { bob.decrypt(forged, first.ciphertext) },
                "a replay of the current key with gap $gap must not open",
            )
        }

        val second = alice.encrypt("dua".toByteArray())
        assertArrayEquals(
            "the genuine message still opens after two forged frames",
            "dua".toByteArray(),
            bob.decrypt(second.header, second.ciphertext),
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

        val a0 = alice.encrypt("a0".toByteArray())
        assertArrayEquals(
            "a0 opens",
            "a0".toByteArray(),
            bob.decrypt(a0.header, a0.ciphertext),
        )

        // Two more Alice sends that Bob never receives: in flight on the chain he is about to leave.
        val held0 = alice.encrypt("a1".toByteArray())
        val held1 = alice.encrypt("a2".toByteArray())

        // Bob replies, so Alice turns the DH ratchet and her next send opens a new chain whose
        // `previousChainLength` names all three she has sent.
        val b0 = bob.encryptNext("b0".toByteArray())
        assertArrayEquals(
            "b0 opens",
            "b0".toByteArray(),
            alice.decrypt(b0.header, b0.ciphertext),
        )
        val a3 = alice.encryptNext("a3".toByteArray())
        assertEquals("three sent on the old chain", 3L, a3.header.previousChainLength)

        val forged = RatchetHeader.of(KeyPair.generate().public(), 3L, 0L)
        throwsCryptoError(
            { bob.decrypt(forged, a3.ciphertext) },
            "a frame on an unknown chain must not open",
        )

        assertArrayEquals(
            "the in-flight message still opens after a forged frame",
            "a1".toByteArray(),
            bob.decrypt(held0.header, held0.ciphertext),
        )
        assertArrayEquals(
            "and the one after it",
            "a2".toByteArray(),
            bob.decrypt(held1.header, held1.ciphertext),
        )

        // The genuine new-chain message still opens too, so the forged frame cost the session
        // nothing in either direction.
        assertArrayEquals(
            "the new chain still opens",
            "a3".toByteArray(),
            bob.decrypt(a3.header, a3.ciphertext),
        )
    }
}
