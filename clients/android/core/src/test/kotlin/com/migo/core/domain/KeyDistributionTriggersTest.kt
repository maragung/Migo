package com.migo.core.domain

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.crypto.CALL_KEY_LEN
import com.migo.core.crypto.CallKeyState
import com.migo.core.crypto.Content
import com.migo.core.crypto.CryptoError
import com.migo.core.crypto.IdentityPublic
import com.migo.core.crypto.OneTimePrekey
import com.migo.core.crypto.PrekeyBundle
import com.migo.core.crypto.SignedPrekey
import com.migo.core.crypto.Sodium
import com.migo.core.net.Inbound
import com.migo.core.net.RealtimeTransport
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.CallKeyUpdate
import com.migo.core.protocol.CallRenegotiate
import com.migo.core.protocol.CallSdp
import com.migo.core.protocol.CallSfuParticipant
import com.migo.core.protocol.GroupKeyDistribution
import com.migo.core.protocol.Op
import com.migo.core.session.GroupCrypto
import com.migo.core.session.PeerBundleSource
import com.migo.core.session.SessionCrypto
import com.migo.core.wire.Frame
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.frameHeader
import com.migo.core.wire.idFromBytes
import com.migo.core.wire.idToBytes
import com.migo.core.wire.parseId
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The section 163 key-distribution triggers, over the real domains and the real crypto: what each
 * trigger puts on the wire, and what the peer's trigger does with it when it lands.
 *
 * Three triggers, each pinned end to end — sender, wire frame, receiver — because each one is a
 * *cross-client* contract in at least one direction (the web and desktop builds run the same
 * section's triggers against these same frames):
 *
 *   1. **Membership movement re-keys the conversation.** A member event stamped with a group key
 *      generation rotates the sender's own chain to exactly that generation, then re-distributes
 *      it over the dedicated `GROUP_KEY_DISTRIBUTE` relay — one sealed copy per member device, the
 *      raw distribution bytes under the pairwise Double Ratchet, adopted straight into the peer's
 *      receiver state. At-least-once delivery is the rule the wire promises, so a redelivered or
 *      stale event must send nothing the second time.
 *   2. **Roster movement re-keys a group call.** A seat arriving or departing rotates the frame
 *      key and announces it as one `CALL_KEY_UPDATE` sealed under the *running* key; a seat that
 *      never held the key drops the announcement quietly (that is the mid-call joiner's
 *      pre-answer window), and a replayed announcement is the no-op the replay guard makes it.
 *   3. **A mid-call joiner asks for the running key.** The ask rides a `CALL_RENEGOTIATE` that the
 *      relay projects to a `CALL_SDP` for the asked seat; that seat rotates, announces the
 *      rotation to the roster, and answers with the new key sealed under the wrapper the pairwise
 *      session's X3DH secret derives; the joiner installs it as a baseline and rides the next
 *      rotation like any other seat.
 *
 * The fixture is two whole devices — key stores, pairwise ratchets, group and call crypto —
 * bridged by an in-memory bundle source that serves the other side's published material, so every
 * seal and open in these tests is the real X3DH and the real Double Ratchet, not a stub standing
 * in for them. What is scripted is only the transport, the same seam [GroupCallTest] drives.
 *
 * The AEAD half needs real libsodium, and the Android artifact's native library only loads on a
 * device, so the suite injects the desktop handle through [Sodium.overrideForTesting] — the same
 * C code the device runs, loaded for the host JVM.
 */
class KeyDistributionTriggersTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }

    /** Encodes one wire struct, for frames a test composes rather than captures. */
    private fun encodeOf(encode: (Writer) -> Unit): ByteArray {
        val writer = Writer()
        encode(writer)
        return writer.finish()
    }

    /**
     * Pushes the frames and drains them through the device's pump.
     *
     * The pump is launched and then yielded to rather than run to a closed channel, because these
     * tests deliver to the same device more than once (a joiner receives its answer and then a
     * later rotation). Every handler the domains register runs inline to completion — the
     * transports never really suspend — so one yield is the whole drain, deterministically, before
     * the pump is cancelled off its empty receive.
     */
    private fun deliver(device: TestDevice, vararg frames: Pair<Long, ByteArray>) {
        for ((opcode, payload) in frames) {
            device.transport.event(opcode, payload)
        }
        runBlocking {
            val pump = launch { device.rpc.deliver() }
            yield()
            pump.cancel()
        }
    }

    private fun framesOf(device: TestDevice, opcode: Long): List<ByteArray> =
        device.transport.sent.filter { it.first == opcode }.map { it.second }

    // --- trigger 1: membership movement re-keys the conversation ---

    @Test
    fun `a member event rotates the chain to its generation and re-distributes over the relay`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)

        runBlocking { f.me.messaging.redistributeOnMembership(CONVERSATION, groupKeyEpoch = 2L) }

        assertEquals("the chain rotated to the event's generation", 2L, f.me.groupCrypto.currentEpoch(CONVERSATION))
        val relayed = framesOf(f.me, Op.GROUP_KEY_DISTRIBUTE)
        assertEquals("one sealed copy per member device", 1, relayed.size)
        val frame = GroupKeyDistribution.decode(Reader(relayed[0]))
        assertEquals(CONVERSATION, frame.conversationId)
        assertEquals(ME_DEVICE, frame.fromDevice)
        assertEquals(ADA, frame.toAccount)
        assertEquals(ADA_LAPTOP, frame.toDevice)

        // The payload is the *raw* distribution, not a control-event body: it opens at the peer
        // and adopts straight into the receiver state, no message-layer unwrap anywhere.
        val raw = runBlocking {
            f.ada.sessionCrypto.open(CONVERSATION, ME, ME_DEVICE, frame.sealedDistribution)
        }
        f.ada.groupCrypto.acceptDistribution(CONVERSATION, ME_DEVICE, raw)
        val sealed = f.me.groupCrypto.sealContent(CONVERSATION, "after the movement".toByteArray())
        assertEquals(
            "the redistributed key is the chain that now seals",
            "after the movement",
            String(f.ada.groupCrypto.open(CONVERSATION, ME_DEVICE, sealed.envelope)),
        )
        assertTrue("nothing reached either error sink", f.me.sink.isEmpty() && f.ada.sink.isEmpty())
    }

    @Test
    fun `a redelivered or stale member event sends nothing the second time`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)

        runBlocking {
            f.me.messaging.redistributeOnMembership(CONVERSATION, groupKeyEpoch = 2L)
            val firstPass = framesOf(f.me, Op.GROUP_KEY_DISTRIBUTE).size
            // The same event delivered again, and then an older one arriving late: at-least-once
            // delivery means both happen, and neither may mint a second chain.
            f.me.messaging.redistributeOnMembership(CONVERSATION, groupKeyEpoch = 2L)
            f.me.messaging.redistributeOnMembership(CONVERSATION, groupKeyEpoch = 1L)
            assertEquals("a redelivered or stale event is a no-op", firstPass, framesOf(f.me, Op.GROUP_KEY_DISTRIBUTE).size)
        }
        assertEquals("the epoch never rewinds", 2L, f.me.groupCrypto.currentEpoch(CONVERSATION))
    }

    @Test
    fun `a relayed distribution is opened by its addressee and nobody else`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)

        runBlocking { f.me.messaging.redistributeOnMembership(CONVERSATION, groupKeyEpoch = 2L) }
        val relayed = framesOf(f.me, Op.GROUP_KEY_DISTRIBUTE).single()
        val sealed = f.me.groupCrypto.sealContent(CONVERSATION, "the re-keyed conversation".toByteArray())
        try {
            f.ada.groupCrypto.open(CONVERSATION, ME_DEVICE, sealed.envelope)
            fail("the peer opened content before its distribution arrived")
        } catch (_: CryptoError) {
            // No receiver state yet: the honest answer until the key lands.
        }

        f.ada.messaging.start()
        // A copy addressed to a sibling device of the peer's is not the addressee's to open, and a
        // distribution naming a device the directory does not know is dropped with the open
        // refused — both quietly, because the relay promises neither is an error.
        val sibling = encodeOf {
            GroupKeyDistribution(
                conversationId = CONVERSATION,
                fromDevice = ME_DEVICE,
                toAccount = ADA,
                toDevice = ADA_PHONE,
                sealedDistribution = GroupKeyDistribution.decode(Reader(relayed)).sealedDistribution,
            ).encode(it)
        }
        val foreign = encodeOf {
            GroupKeyDistribution(
                conversationId = CONVERSATION,
                fromDevice = BEN_PHONE,
                toAccount = ADA,
                toDevice = ADA_LAPTOP,
                sealedDistribution = byteArrayOf(1, 2, 3),
            ).encode(it)
        }
        deliver(
            f.ada,
            Op.GROUP_KEY_DISTRIBUTE to sibling,
            Op.GROUP_KEY_DISTRIBUTE to foreign,
            Op.GROUP_KEY_DISTRIBUTE to relayed,
        )

        assertEquals(
            "the addressed copy adopted and the content opened",
            "the re-keyed conversation",
            String(f.ada.groupCrypto.open(CONVERSATION, ME_DEVICE, sealed.envelope)),
        )
        assertTrue("neither the sibling's copy nor the foreign one is an error", f.ada.sink.isEmpty())
    }

    // --- trigger 2: roster movement re-keys a group call ---

    @Test
    fun `membership movement rotates the frame key and announces it under the running key`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)
        // Both seats derived the same epoch-0 key; the announcement of the rotation is sealed
        // under exactly that key, which is what keeps a departure from reading what follows.
        f.me.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x11 })
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x11 })

        // Two movements, no echo of our own in between: the rotating device advances its state
        // before the frame leaves and never waits to hear its own update back.
        runBlocking {
            f.me.groupCallKeys.rotateForMembership(CALL)
            f.me.groupCallKeys.rotateForMembership(CALL)
        }
        val updates = framesOf(f.me, Op.CALL_KEY_UPDATE)
        assertEquals(
            "one announcement per movement, each one epoch up",
            listOf(1L, 2L),
            updates.map { CallKeyUpdate.decode(Reader(it)).epoch },
        )

        f.ada.groupCallKeys.start()
        deliver(
            f.ada,
            Op.CALL_KEY_UPDATE to updates[0],
            Op.CALL_KEY_UPDATE to updates[1],
            // A replay of the first announcement after the second landed: the replay guard
            // refuses the non-advancing epoch and that refusal is not an error.
            Op.CALL_KEY_UPDATE to updates[0],
            // An announcement for a call this device holds no key for is the joiner's
            // pre-answer window or another call's news: dropped quietly either way.
            Op.CALL_KEY_UPDATE to encodeOf {
                CallKeyUpdate(callId = OTHER_CALL, epoch = 5L, sealedKeyMaterial = byteArrayOf(9)).encode(it)
            },
        )
        assertTrue(
            "the seated device adopted both rotations in order, and refused nothing else loudly",
            f.ada.sink.isEmpty(),
        )
    }

    @Test
    fun `a rotation for a call this device holds no key for sends nothing`() {
        val f = Fixture()
        runBlocking { f.me.groupCallKeys.rotateForMembership(CALL) }
        assertTrue("an un-seated screen's roster news is not this device's to re-key", f.me.transport.sent.isEmpty())
    }

    // --- trigger 3: the mid-call joiner's first key ---

    @Test
    fun `a mid-call joiner asks the first seated participant and installs the answer`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x22 })
        f.ada.groupCallKeys.start()
        f.me.groupCallKeys.start()

        // The roster as the joiner received it: the joiner's own account seated on another device
        // first, the holder second — the ask must skip the sibling and name the holder.
        val roster = joinerRoster()
        runBlocking {
            f.me.groupCallKeys.requestJoinKey(roster)
            // A second ask while the first stands: the joiner sends no second ask.
            f.me.groupCallKeys.requestJoinKey(roster)
        }
        val asks = framesOf(f.me, Op.CALL_RENEGOTIATE)
        assertEquals("one ask, not one per attempt", 1, asks.size)
        val ask = CallRenegotiate.decode(Reader(asks[0]))
        assertEquals(CALL, ask.callId)
        assertEquals(ME_DEVICE, ask.fromDevice)
        assertEquals(
            "the ask names the first seated participant whose account is not the joiner's own",
            ADA_LAPTOP,
            ask.toDevice,
        )
        // The ask's payload is *not* opened here: the pairwise ratchet spends a message key on
        // decrypt, so a test-side open would leave the holder's own open a replay it must refuse.
        // The payload's shape is pinned by the test below; here the holder opens it for real.

        // The relay projects the renegotiation to a CALL_SDP for the asked seat.
        deliver(f.ada, Op.CALL_SDP to asks[0])
        val holderUpdate = framesOf(f.ada, Op.CALL_KEY_UPDATE).single()
        assertEquals("the holder rotated on the join before answering", 1L, CallKeyUpdate.decode(Reader(holderUpdate)).epoch)
        val replies = framesOf(f.ada, Op.CALL_SDP)
        assertEquals("one answer, addressed to the joiner", 1, replies.size)
        val reply = CallSdp.decode(Reader(replies[0]))
        assertEquals(CALL, reply.callId)
        assertEquals(ADA_LAPTOP, reply.fromDevice)
        assertEquals(ME_DEVICE, reply.toDevice)
        assertTrue("the holder's rotation and answer drew no error", f.ada.sink.isEmpty())

        // The joiner installs the answer as its baseline, and rides the next rotation like any seat.
        deliver(f.me, Op.CALL_SDP to replies[0])
        assertTrue("the joiner holds the call's key now", f.me.groupCallKeys.holdsKey(CALL))
        runBlocking { f.ada.groupCallKeys.rotateForMembership(CALL) }
        val laterUpdates = framesOf(f.ada, Op.CALL_KEY_UPDATE)
        assertEquals(2, laterUpdates.size)
        deliver(f.me, Op.CALL_KEY_UPDATE to laterUpdates[1])
        assertTrue(
            "the joiner adopted the post-join rotation, which only opens under the key the answer carried",
            f.me.sink.isEmpty(),
        )

        // With a key held, the joiner is no longer a joiner: the same roster asks nothing.
        runBlocking { f.me.groupCallKeys.requestJoinKey(roster) }
        assertEquals("a seated device does not ask for a key it holds", 1, framesOf(f.me, Op.CALL_RENEGOTIATE).size)
    }

    @Test
    fun `the ask is the unified content event carrying the joiner's account id`() {
        // Its own fixture, because an envelope opens exactly once: the ratchet deletes the message
        // key on use, so the open that pins the ask's shape cannot also be the open the holder's
        // answer path performs. Here the test *is* the holder's one open.
        val f = Fixture()
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x22 })
        f.ada.groupCallKeys.start()
        f.me.groupCallKeys.start()

        runBlocking { f.me.groupCallKeys.requestJoinKey(joinerRoster()) }
        val ask = CallRenegotiate.decode(Reader(framesOf(f.me, Op.CALL_RENEGOTIATE).single()))
        val plaintext = runBlocking {
            f.ada.sessionCrypto.open(CONVERSATION, ME, ME_DEVICE, ask.sealedSdp)
        }
        // Byte for byte what the SDK and desktop encode: the `call-key-ask` control event, the
        // joiner's account id as `data`, and the content codec's default bucket padding. Pinning
        // the whole payload rather than its shape is what makes a padding or field-order drift on
        // any side a failing test here rather than a mixed-client call that cannot re-key.
        val expected = Content.ControlEvent("call-key-ask", idToBytes(ME)).encode()
        assertTrue("the ask is the unified content event, bytes pinned", plaintext.contentEquals(expected))
        val decoded = Content.decode(plaintext)
        assertTrue("the ask decodes as a control event", decoded is Content.ControlEvent)
        if (decoded !is Content.ControlEvent) return
        assertEquals("call-key-ask", decoded.event)
        assertEquals("the ask names the joiner's account", ME, idFromBytes(decoded.data!!))
        assertTrue("the holder's open drew no error", f.ada.sink.isEmpty())
    }

    @Test
    fun `a holder answers a unified ask under the session-derived wrapper and the joiner opens it`() {
        // The end-to-end test above drives both halves through the domains; this one pins the
        // *wrapper's* input: the answer must open under the X3DH secret of the pairwise session
        // the ask itself established, which is the unified contract the SDK and desktop answer by.
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x44 })
        f.ada.groupCallKeys.start()

        runBlocking { f.me.groupCallKeys.requestJoinKey(joinerRoster()) }
        deliver(f.ada, Op.CALL_SDP to framesOf(f.me, Op.CALL_RENEGOTIATE).single())

        val reply = CallSdp.decode(Reader(framesOf(f.ada, Op.CALL_SDP).single()))
        assertEquals("the answer is addressed to the joiner", ME_DEVICE, reply.toDevice)
        // The joiner's own view of the session secret -- the copy its SessionCrypto retained when
        // it sealed the ask -- is what the sealed distribution must open under.
        val joinerSecret = runBlocking { f.me.sessionCrypto.sessionSecret(CONVERSATION, ADA_LAPTOP) }
        val holderSecret = runBlocking { f.ada.sessionCrypto.sessionSecret(CONVERSATION, ME_DEVICE) }
        assertTrue("both halves retained the session secret", joinerSecret != null && holderSecret != null)
        assertTrue(
            "initiator and responder hold the same X3DH secret",
            joinerSecret!!.contentEquals(holderSecret!!),
        )
        val installed = CallKeyState.fromJoinDistribution(joinerSecret!!, CALL, reply.sealedSdp)
        assertEquals("the joiner receives the post-rotation epoch", 1L, installed.epoch())
        // And a stranger's secret -- a device that never ran the handshake -- cannot open it.
        try {
            CallKeyState.fromJoinDistribution(ByteArray(CALL_KEY_LEN) { 0x41 }, CALL, reply.sealedSdp)
            fail("the join distribution opened under a stranger's secret")
        } catch (_: CryptoError) {
            // The wrapper is anchored to the pairwise session, which is its whole point.
        }
        assertTrue("the unified answer drew no error", f.ada.sink.isEmpty())
    }

    @Test
    fun `a legacy raw-secret ask is still answered under those bytes`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x55 })
        f.ada.groupCallKeys.start()

        // An older Android build sealed a freshly minted 32-byte secret as the whole ask payload,
        // riding the same pairwise envelope. Bytes chosen with a first byte no content type uses,
        // so this is unambiguously the legacy shape.
        val legacySecret = ByteArray(CALL_KEY_LEN) { 0x7F }
        val sealed = runBlocking {
            f.me.sessionCrypto.seal(CONVERSATION, ADA, ADA_LAPTOP, legacySecret)
        }
        val ask = encodeOf {
            CallRenegotiate(
                callId = CALL,
                fromDevice = ME_DEVICE,
                toDevice = ADA_LAPTOP,
                sealedSdp = sealed.envelope,
            ).encode(it)
        }
        deliver(f.ada, Op.CALL_SDP to ask)

        val reply = CallSdp.decode(Reader(framesOf(f.ada, Op.CALL_SDP).single()))
        assertEquals(ME_DEVICE, reply.toDevice)
        assertEquals("the holder still rotated on the join", 1L, CallKeyUpdate.decode(Reader(framesOf(f.ada, Op.CALL_KEY_UPDATE).single())).epoch)
        // The join distribution opens under the very bytes the legacy ask carried: that joiner
        // holds no session secret to derive a wrapper from, so those bytes are its only key.
        val installed = CallKeyState.fromJoinDistribution(legacySecret, CALL, reply.sealedSdp)
        assertEquals("the legacy joiner receives the running epoch", 1L, installed.epoch())
        assertTrue("the legacy answer drew no error", f.ada.sink.isEmpty())
    }

    @Test
    fun `a joiner with no retained session secret declines the answer without touching state`() {
        val f = Fixture()
        f.ada.directory.devices += DeviceAddress(ME, ME_DEVICE)
        f.me.directory.devices += DeviceAddress(ADA, ADA_LAPTOP)
        f.ada.groupCallKeys.seated(CALL, CONVERSATION, ByteArray(CALL_KEY_LEN) { 0x66 })
        f.ada.groupCallKeys.start()
        f.me.groupCallKeys.start()

        runBlocking { f.me.groupCallKeys.requestJoinKey(joinerRoster()) }
        deliver(f.ada, Op.CALL_SDP to framesOf(f.me, Op.CALL_RENEGOTIATE).single())
        val reply = framesOf(f.ada, Op.CALL_SDP).single()

        // The joiner's session dies (a peer identity change forgets it, for instance): the answer
        // that comes back finds no secret to open under, and must decline quietly -- no key
        // installed, no error surfaced, the ask standing as it was.
        runBlocking { f.me.sessionCrypto.forget(CONVERSATION) }
        deliver(f.me, Op.CALL_SDP to reply)
        assertTrue("a declined answer installs no key", !f.me.groupCallKeys.holdsKey(CALL))
        assertTrue("the decline was quiet", f.me.sink.isEmpty())
    }

    /**
     * The roster as the mid-call joiner received it: the joiner's own account seated on another
     * device first, the holder second — the ask must skip the sibling and name the holder.
     */
    private fun joinerRoster(): GroupCallRoster = GroupCallRoster(
        callId = CALL,
        conversationId = CONVERSATION,
        userId = ME,
        deviceId = ME_DEVICE,
        participantCount = 3L,
        participants = listOf(
            CallSfuParticipant(ME, ME_LAPTOP, NOW - 60_000, byteArrayOf(1)),
            CallSfuParticipant(ADA, ADA_LAPTOP, NOW - 30_000, byteArrayOf(2)),
            CallSfuParticipant(ME, ME_DEVICE, NOW, byteArrayOf(3)),
        ),
    )
}

// The same fixture names GroupCallTest uses, so the files read side by side.

private val ME: Id = parseId("0123456789ABCDEFGHJKMNPQRV")
private val ME_DEVICE: Id = parseId("0123456789ABCDEFGHJKMNPQRW")
private val ME_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRX")
private val ADA: Id = parseId("0123456789ABCDEFGHJKMNPQRY")
private val ADA_LAPTOP: Id = parseId("0123456789ABCDEFGHJKMNPQRZ")
private val ADA_PHONE: Id = parseId("0123456789ABCDEFGHJKMNPQ22")
private val BEN_PHONE: Id = parseId("0123456789ABCDEFGHJKMNPQ24")
private val CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRS")
private val OTHER_CALL: Id = parseId("0123456789ABCDEFGHJKMNPQRT")
private val CONVERSATION: Id = parseId("0123456789ABCDEFGHJKMNPQ25")
private val NOW: Long = 1_800_000_000_000

/**
 * One device's whole stack, wired the way [com.migo.core.MigoClient] wires it: the once-built
 * crypto (`SessionCrypto`, `GroupCrypto`, `CallKeyStore`) and the per-connection domains over one
 * scripted transport. The sink collects everything either layer would report, because half of
 * these tests' assertions are that nothing was ever an error.
 */
private class TestDevice(
    val account: Id,
    val device: Id,
    val transport: ScriptedTransport,
    val rpc: Rpc,
    val keys: KeyStore,
    val sessionCrypto: SessionCrypto,
    val groupCrypto: GroupCrypto,
    val directory: FakeDirectory,
    val messaging: MessagingDomain,
    val callKeys: CallKeyStore,
    val groupCallKeys: GroupCallKeysDomain,
    val sink: ArrayList<Pair<Long, Throwable>>,
)

/**
 * Two devices and the bridge between them. The bridge is the whole "server" of these tests: it
 * serves each side's published prekey bundles to the other, the same public material the real
 * key directory would, so the X3DH handshakes run for real in both directions.
 */
private class Fixture {
    private val bridge = BundleBridge()
    val me = device(ME, ME_DEVICE)
    val ada = device(ADA, ADA_LAPTOP)

    private fun device(account: Id, device: Id): TestDevice {
        val transport = ScriptedTransport()
        val sink = ArrayList<Pair<Long, Throwable>>()
        val rpc = Rpc(transport) { opcode, cause -> sink += opcode to cause }
        val keys = KeyStore.create(oneTimePrekeyCount = 4)
        bridge.publish(device, keys)
        val sessionCrypto = SessionCrypto(keys, bridge)
        val groupCrypto = GroupCrypto(keys)
        val directory = FakeDirectory()
        val scope = CoroutineScope(Dispatchers.Unconfined)
        val watermarks = WatermarkTracker(scope, null)
        val messaging = MessagingDomain(
            rpc,
            scope,
            device,
            sessionCrypto,
            groupCrypto,
            directory,
            watermarks,
        ) { opcode, cause -> sink += opcode to cause }
        val callKeys = CallKeyStore()
        val groupCallKeys = GroupCallKeysDomain(
            rpc,
            account,
            device,
            scope,
            callKeys,
            sessionCrypto,
            directory,
        ) { opcode, cause -> sink += opcode to cause }
        return TestDevice(
            account,
            device,
            transport,
            rpc,
            keys,
            sessionCrypto,
            groupCrypto,
            directory,
            messaging,
            callKeys,
            groupCallKeys,
            sink,
        )
    }
}

/** The membership and device list a real client caches behind [DeviceDirectory]. */
private class FakeDirectory : DeviceDirectory {
    val devices = ArrayList<DeviceAddress>()

    override suspend fun recipientDevices(conversationId: Id): List<DeviceAddress> = devices.toList()

    override suspend fun accountOfDevice(conversationId: Id, deviceId: Id): Id? =
        devices.firstOrNull { it.deviceId == deviceId }?.userId
}

/**
 * Serves each device's own published material back to whoever asks, as the key directory would.
 *
 * Deliberately not a full directory simulation: the first one-time prekey of the publication is
 * served for every fetch, which is exactly enough for these suites — a session is established
 * once per conversation and device, and the store caches it after that.
 */
private class BundleBridge : PeerBundleSource {
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

/**
 * A [RealtimeTransport] that sends nothing anywhere: every request is answered with an
 * acknowledgement, every event is pushed by the test, and the sent frames are kept for asserting
 * on — the same seam [GroupCallTest] drives, minus that suite's scripted replies.
 */
private class ScriptedTransport : RealtimeTransport {
    /** What went out, in order: the opcode and its encoded payload. */
    val sent = ArrayList<Pair<Long, ByteArray>>()

    override val inbound = Channel<Inbound>(Channel.UNLIMITED)

    override val sessionId: Id = NIL_ID
    override val lastFrameSeq: Long = 0L

    private var nextCorrelation = 1L

    override fun correlate(): Long = nextCorrelation++

    override suspend fun send(opcode: Long, correlation: Long, encode: (Writer) -> Unit) {
        sent += opcode to encodeToPayload(encode)
    }

    override suspend fun request(opcode: Long, encode: (Writer) -> Unit): Frame {
        sent += opcode to encodeToPayload(encode)
        return Frame(frameHeader(opcode), encodeToPayload { w -> Acknowledged(ok = true).encode(w) })
    }

    override suspend fun acknowledge() {}

    override fun close() {}

    /** Pushes one server event, as the gateway would hand it to the Rpc pump. */
    fun event(opcode: Long, payload: ByteArray) {
        inbound.trySend(Inbound(Frame(frameHeader(opcode), payload)))
    }

    private fun encodeToPayload(encode: (Writer) -> Unit): ByteArray {
        val writer = Writer()
        encode(writer)
        return writer.finish()
    }
}
