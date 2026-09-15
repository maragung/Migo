package com.migo.core.crypto

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.wire.Id
import com.migo.core.wire.idFromBytes
import com.migo.core.wire.idToBytes
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The call's media key and its rotation — a port of the Rust crate's `call_key` suite
 * (`server/crates/migo-crypto/src/call_key.rs`), carried test for test because the sealed bytes
 * cross clients: a rotation this build seals must be adopted by the web and desktop builds, and a
 * derivation that drifted from theirs is a call that cannot re-key, not a style difference.
 *
 * The rules the suite pins, each one a silent regression under an innocent-looking change:
 *
 *   1. **Both seats derive the same key** from one session secret, and the derivation itself is
 *      pinned to a vector computed from RFC 5869 directly — a change to the construction, not just
 *      the dependency, fails here.
 *   2. **The key is bound to its call and to nothing else's purpose** — not another call's id, not
 *      another label's derivation (section 163: no key for two purposes).
 *   3. **Rotation seals, adoption opens, and refusal is honest** — a non-advancing epoch is the
 *      replay guard, and a refused update leaves the working key intact; tampered, truncated,
 *      cross-call and wrong-epoch updates are all refused.
 *   4. **The mid-call joiner's first key travels sealed under their own pairwise secret** — it does
 *      not open for the call's other seat, it does not open for another call, and the joiner rides
 *      the same rotations as everyone else after it.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same C
 * code the device runs, loaded for the host JVM.
 */
class CallKeyTest {
    companion object {
        /** A session secret both call participants hold and no server ever saw. */
        private val SESSION = ByteArray(32) { 0x0a }

        /**
         * The pairwise secret the caller shares with a *third* device joining mid-call — different
         * from the call's own session, because it belongs to a different pair of devices.
         */
        private val JOINER_SESSION = ByteArray(32) { 0x0b }

        private fun callId(): Id =
            idFromBytes(byteArrayOf(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))

        private fun otherCallId(): Id =
            idFromBytes(byteArrayOf(15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0))

        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }

    /** Asserts that [body] throws the one [CryptoErrorKind] [expected], and nothing of the input. */
    private fun throwsKind(expected: CryptoErrorKind, why: String, body: () -> Unit) {
        try {
            body()
        } catch (caught: CryptoError) {
            assertEquals(why, expected, caught.kind)
            return
        }
        fail("$why: expected CryptoError, got none")
    }

    @Test
    fun `both seats derive the same key from one session`() {
        val caller = CallKeyState.fromSession(SESSION, callId())
        val callee = CallKeyState.fromSession(SESSION, callId())
        val frame = caller.sealFrame("media bytes".toByteArray())
        assertArrayEquals(
            "the frame opens at the other seat",
            "media bytes".toByteArray(),
            callee.openFrame(frame),
        )
    }

    @Test
    fun `the derivation is pinned to an independent vector`() {
        // Not this implementation's own output copied back in: the expected bytes were computed
        // from RFC 5869 directly (HMAC-SHA256 extract with the call id as salt, one expand round
        // over the label), so a change to the construction — not just the dependency — fails here.
        val expected = byteArrayOf(
            0x3e, 0xc0.toByte(), 0xb5, 0xb2.toByte(), 0x95, 0x15, 0xed.toByte(), 0xc6.toByte(),
            0xb4.toByte(), 0xb5.toByte(), 0x1d, 0x92.toByte(), 0xc1.toByte(), 0x31,
            0xc7, 0x56, 0xd1, 0xef.toByte(), 0x49, 0x66, 0xdc.toByte(), 0xa0.toByte(), 0x54, 0x29,
            0xed.toByte(), 0x92.toByte(), 0x6d, 0xc0.toByte(), 0x12, 0xa9, 0xcf.toByte(), 0xa0.toByte(),
        )
        val state = CallKeyState.fromSession(SESSION, callId())
        // The key is never exposed, so prove the pin through behaviour: the state must seal a
        // frame the expected bytes can open, which is only true if the state's key *is* the
        // pinned derivation. The associated data is the binding the class itself uses — the call
        // id followed by the epoch as eight big-endian bytes.
        val frame = state.sealFrame("pin".toByteArray())
        val binding = idToBytes(callId()) + ByteArray(8)
        val opened = Aead.open(SymmetricKey.fromBytes(expected), binding, frame)
        assertArrayEquals("the state's key is the pinned derivation", "pin".toByteArray(), opened)
    }

    @Test
    fun `a call key is bound to its call`() {
        val thisCall = CallKeyState.fromSession(SESSION, callId())
        val otherCall = CallKeyState.fromSession(SESSION, otherCallId())
        val frame = thisCall.sealFrame("this call only".toByteArray())
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "another call's key must not open this call's frames",
        ) { otherCall.openFrame(frame) }
    }

    @Test
    fun `a call key is not a message key`() {
        // The label is the separation (section 163: no key for two purposes). Same secret, same
        // salt, different label — the derivations must not agree, checked here at the behaviour
        // level: a frame sealed under the call key does not open under an X3DH-labelled one.
        val state = CallKeyState.fromSession(SESSION, callId())
        val frame = state.sealFrame("media".toByteArray())
        val wrong = Kdf.derive(SESSION, idToBytes(callId()), Kdf.LABEL_X3DH, CALL_KEY_LEN)
        val binding = idToBytes(callId()) + ByteArray(8)
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "a frame sealed under the call key must not open under another label's key",
        ) { Aead.open(SymmetricKey.fromBytes(wrong), binding, frame) }
    }

    @Test
    fun `rotation seals and adoption opens`() {
        val caller = CallKeyState.fromSession(SESSION, callId())
        val callee = CallKeyState.fromSession(SESSION, callId())
        val sealed = caller.rotate()
        assertEquals(1L, caller.epoch())
        callee.adopt(1L, sealed)
        assertEquals(1L, callee.epoch())
        val frame = caller.sealFrame("new epoch".toByteArray())
        assertArrayEquals("the adopted key opens the new epoch", "new epoch".toByteArray(), callee.openFrame(frame))
    }

    @Test
    fun `media before a rotation is not readable after it`() {
        // Section 163: a participant who joins cannot read media from before.
        val caller = CallKeyState.fromSession(SESSION, callId())
        val old = caller.sealFrame("before the join".toByteArray())
        val sealed = caller.rotate()
        val joiner = CallKeyState.fromSession(SESSION, callId())
        joiner.adopt(1L, sealed)
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "the joiner read media from before they joined",
        ) { joiner.openFrame(old) }
    }

    @Test
    fun `an update that does not advance the epoch is refused`() {
        val state = CallKeyState.fromSession(SESSION, callId())
        val sealed = state.rotate()
        throwsKind(
            CryptoErrorKind.KeyAlreadyUsed,
            "an update repeating the held epoch is a replay",
        ) { state.adopt(1L, sealed) }
        throwsKind(
            CryptoErrorKind.KeyAlreadyUsed,
            "an update rolling the epoch back is a replay",
        ) { state.adopt(0L, sealed) }
        // A refused update leaves the working key intact: the peer that applied the rotation for
        // real still opens this state's frames.
        val peer = CallKeyState.fromSession(SESSION, callId())
        peer.adopt(1L, sealed)
        val frame = state.sealFrame("still the same key".toByteArray())
        assertArrayEquals(
            "the refused update did not damage the working key",
            "still the same key".toByteArray(),
            peer.openFrame(frame),
        )
    }

    @Test
    fun `a tampered update is refused`() {
        val state = CallKeyState.fromSession(SESSION, callId())
        val sealed = state.rotate()
        sealed[sealed.size - 1] = (sealed[sealed.size - 1].toInt() xor 0x01).toByte()
        val peer = CallKeyState.fromSession(SESSION, callId())
        throwsKind(CryptoErrorKind.DecryptionFailed, "an edited update must not adopt") {
            peer.adopt(1L, sealed)
        }
    }

    @Test
    fun `a truncated update is refused`() {
        val state = CallKeyState.fromSession(SESSION, callId())
        val sealed = state.rotate()
        val peer = CallKeyState.fromSession(SESSION, callId())
        try {
            peer.adopt(1L, sealed.copyOf(sealed.size - 1))
            fail("a truncated update must not adopt")
        } catch (_: CryptoError) {
            // The shape of the refusal (BadLength or DecryptionFailed) is the AEAD layer's word;
            // the rule here is only that a truncated update never installs a key.
        }
    }

    @Test
    fun `an update cannot be replayed onto another call`() {
        val state = CallKeyState.fromSession(SESSION, callId())
        val sealed = state.rotate()
        val other = CallKeyState.fromSession(SESSION, otherCallId())
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "an update is bound to the call it rotated",
        ) { other.adopt(1L, sealed) }
    }

    @Test
    fun `an update bound to a different epoch is refused`() {
        // The frame says epoch 2 but the bytes are bound to another epoch: the binding, not the
        // frame's claim, decides.
        val state = CallKeyState.fromSession(SESSION, callId())
        val sealed = state.rotate()
        val peer = CallKeyState.fromSession(SESSION, callId())
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "the binding, not the frame's epoch claim, decides",
        ) { peer.adopt(2L, sealed) }
    }

    @Test
    fun `a state does not print its key`() {
        val state = CallKeyState.fromSession(SESSION, callId())
        val rendered = state.toString()
        assertTrue("the rendering stars the key: $rendered", rendered.contains("***"))
    }

    @Test
    fun `a mid-call joiner receives the current key sealed for them`() {
        // The joiner's first key travels sealed under their own pairwise session with the caller,
        // through whatever relay already moves sealed blobs between devices. The rotation on join
        // is what keeps pre-join media sealed from them.
        val caller = CallKeyState.fromSession(SESSION, callId())
        caller.rotate()
        val preJoin = caller.sealFrame("while the joiner was outside".toByteArray())

        caller.rotate()
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        val joiner = CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), sealed)
        assertEquals(2L, joiner.epoch())

        val frame = caller.sealFrame("welcome in".toByteArray())
        assertArrayEquals("the joiner opens the running epoch", "welcome in".toByteArray(), joiner.openFrame(frame))
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "the joiner read media from before they joined",
        ) { joiner.openFrame(preJoin) }
    }

    @Test
    fun `a call that never rotated hands the joiner the epoch zero key`() {
        // The honest boundary of the mechanism: without a rotation there is no older epoch for
        // pre-join media to be stranded on, so the epoch-0 key the joiner receives opens it.
        // Pre-join secrecy comes from rotating on join, not from the distribution itself — a test
        // that pretended otherwise would be pretending it in the model.
        val caller = CallKeyState.fromSession(SESSION, callId())
        val earlier = caller.sealFrame("before anyone else arrived".toByteArray())
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        val joiner = CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), sealed)
        assertEquals(0L, joiner.epoch())
        assertArrayEquals(
            "epoch-0 media opens under the epoch-0 key; this is why callers rotate on join",
            "before anyone else arrived".toByteArray(),
            joiner.openFrame(earlier),
        )
    }

    @Test
    fun `a joiner rides the rotations after their first key`() {
        // The first key is a baseline, not a leash: the joiner applies the same sealed updates as
        // everyone else from the epoch they entered on.
        val caller = CallKeyState.fromSession(SESSION, callId())
        caller.rotate()
        caller.rotate()
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        val joiner = CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), sealed)
        assertEquals(2L, joiner.epoch())

        val update = caller.rotate()
        joiner.adopt(3L, update)
        assertEquals(3L, joiner.epoch())
        val frame = caller.sealFrame("third epoch".toByteArray())
        assertArrayEquals("the joiner adopts like any seat", "third epoch".toByteArray(), joiner.openFrame(frame))
    }

    @Test
    fun `a join distribution needs the joiners own session`() {
        // The blob is sealed to one pairwise session. The call's own other seat — or the server,
        // which holds none of these secrets — cannot open the joiner's copy.
        val caller = CallKeyState.fromSession(SESSION, callId())
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "the call's own session must not open the joiner's copy",
        ) { CallKeyState.fromJoinDistribution(SESSION, callId(), sealed) }
    }

    @Test
    fun `a join distribution cannot be replayed onto another call`() {
        val caller = CallKeyState.fromSession(SESSION, callId())
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "a join distribution is bound to the call it was sealed for",
        ) { CallKeyState.fromJoinDistribution(JOINER_SESSION, otherCallId(), sealed) }
    }

    @Test
    fun `a tampered join distribution is refused`() {
        val caller = CallKeyState.fromSession(SESSION, callId())
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        sealed[sealed.size - 1] = (sealed[sealed.size - 1].toInt() xor 0x01).toByte()
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "an edited join distribution must not install a key",
        ) { CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), sealed) }
    }

    @Test
    fun `a truncated join distribution is refused`() {
        val caller = CallKeyState.fromSession(SESSION, callId())
        val sealed = caller.sealedJoinDistribution(JOINER_SESSION)
        try {
            CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), sealed.copyOf(sealed.size - 1))
            fail("a truncated join distribution must not install a key")
        } catch (_: CryptoError) {
            // The shape of the refusal is the AEAD layer's word; the rule here is only that a
            // truncated blob never installs a key.
        }
    }

    @Test
    fun `the join wrapping key is not the call key`() {
        // Section 163: no key for two purposes. The wrapping key comes from the same secret and
        // the same salt but its own label; if the two derivations ever agreed, the call key would
        // be the wrapping key and the separation would be gone. Checked at behaviour level: a
        // genuine blob opens, one wrapped under the call-key label does not.
        val caller = CallKeyState.fromSession(SESSION, callId())
        val genuine = caller.sealedJoinDistribution(JOINER_SESSION)
        CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), genuine)

        val wrong = Kdf.derive(JOINER_SESSION, idToBytes(callId()), Kdf.LABEL_CALL_KEY, CALL_KEY_LEN)
        val forged = Aead.seal(
            SymmetricKey.fromBytes(wrong),
            idToBytes(callId()),
            ByteArray(CallKeyState.JOIN_DISTRIBUTION_LEN),
        )
        throwsKind(
            CryptoErrorKind.DecryptionFailed,
            "the wrapper must not be the call key's own derivation",
        ) { CallKeyState.fromJoinDistribution(JOINER_SESSION, callId(), forged) }
    }
}
