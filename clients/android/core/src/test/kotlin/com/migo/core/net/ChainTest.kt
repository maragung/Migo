package com.migo.core.net

import com.goterl.lazysodium.LazySodiumJava
import com.goterl.lazysodium.SodiumJava
import com.migo.core.account.AccountError
import com.migo.core.account.AccountErrorKind
import com.migo.core.account.Eip1559Tx
import com.migo.core.account.EvmWallet
import com.migo.core.account.FUJI_TESTNET
import com.migo.core.account.MigoRoot
import com.migo.core.crypto.Sodium
import java.math.BigInteger
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test

/**
 * The chain client: the one conversation in this package that never talks to a Migo server, so its
 * double is not the gateway transport but a fake JSON-RPC endpoint scripted through the client's
 * [ChainClient] transport seam. The tests care about the parts of the conversation that carry
 * security weight, not the happy plumbing:
 *
 * * the session rule — `eth_chainId` is the first request, and a mismatched answer closes the
 *   session before a single balance, nonce, or transaction byte is asked for;
 * * `broadcast` re-verifies the chain at the moment value-carrying bytes leave, and refuses an
 *   endpoint that answers a foreign hash — the hash is the handle the user tracks the send by;
 * * `track` never turns "the RPC accepted it" into confirmed: only a receipt with `status: 1`
 *   does that, a `status: 0` receipt is reverted, a vanished transaction is dropped, and a
 *   deadline is expired — an unresolved ending, never a quiet success.
 */
class ChainTest {
    companion object {
        @BeforeClass
        @JvmStatic
        fun loadDesktopLibsodium() {
            // The broadcast test signs with a real wallet, whose derivation runs HKDF through the
            // same C code the device runs; the desktop handle is injected as in the account suites.
            Sodium.overrideForTesting(LazySodiumJava(SodiumJava()))
        }
    }
    /** A whole-response wrapper: a handler that wants to answer a JSON-RPC *error* returns this. */
    private class Raw(val text: String)

    /**
     * A JSON-RPC endpoint double: routes by method, records every request, and answers from a
     * script the test mutates between calls (a poll loop must see different answers on later
     * rounds). A handler's return value is the `result` element — a [JsonNull] for "not found", a
     * [Raw] for a full error response.
     */
    private class FakeChain {
        val requests = mutableListOf<Pair<String, JsonArray>>()
        private val handlers = mutableMapOf<String, (JsonArray) -> Any>()

        fun on(method: String, handler: (JsonArray) -> Any) {
            handlers[method] = handler
        }

        fun callsTo(method: String): Int = requests.count { it.first == method }

        val transport: suspend (String) -> String = { body ->
            val parsed = Json.parseToJsonElement(body).jsonObject
            val method = parsed["method"]!!.jsonPrimitive.content
            val params = parsed["params"]!!.jsonArray
            requests.add(method to params)
            val result = handlers[method]?.invoke(params)
                ?: throw IllegalStateException("no handler: $method")
            when (result) {
                is Raw -> result.text
                is JsonElement -> """{"jsonrpc":"2.0","id":1,"result":$result}"""
                else -> """{"jsonrpc":"2.0","id":1,"result":${JsonPrimitive(result.toString())}}"""
            }
        }
    }

    /** A Fuji client over a fresh double, with the chain id answered correctly by default. */
    private fun fujiClient(): Pair<ChainClient, FakeChain> {
        val fake = FakeChain()
        fake.on("eth_chainId") { JsonPrimitive("0xa869") } // 43113, Fuji
        return ChainClient(FUJI_TESTNET, null, fake.transport) to fake
    }

    private fun quantity(value: Long): JsonPrimitive = JsonPrimitive("0x" + value.toString(16))

    private fun receipt(status: String, block: String, gas: String): JsonObject =
        buildJsonObject {
            put("status", status)
            put("blockNumber", block)
            put("gasUsed", gas)
        }

    @Test
    fun theSessionOpensWithEthChainIdAndRefusesAMismatchedNetwork() = runBlocking {
        val (chain, fake) = fujiClient()
        fake.on("eth_getBalance") { quantity(1) }

        // The first request the endpoint ever sees is the chain id check.
        chain.getBalance(ByteArray(20))
        assertEquals("eth_chainId", fake.requests[0].first)
        assertEquals("eth_getBalance", fake.requests[1].first)

        // A session whose chain id disagrees is closed before any other request: no balance was
        // asked for, and the refusal names both ids rather than picking one.
        val wrong = FakeChain()
        wrong.on("eth_chainId") { JsonPrimitive("0xa86a") } // 43114 — mainnet, not configured Fuji
        val confused = ChainClient(FUJI_TESTNET, null, wrong.transport)
        try {
            confused.getBalance(ByteArray(20))
            fail("a mismatched chain id must close the session")
        } catch (error: AccountError) {
            assertEquals(AccountErrorKind.ChainMismatch, error.kind)
        }
        assertEquals("the mismatched session asked nothing else", 1, wrong.requests.size)
    }

    @Test
    fun balancesNoncesGasAndFeesAreParsedFromHexQuantities() = runBlocking {
        val (chain, fake) = fujiClient()
        val address = ByteArray(20) { 0xab.toByte() }
        val oneAvax = BigInteger("de0b6b3a7640000", 16)

        fake.on("eth_getBalance") { JsonPrimitive("0xde0b6b3a7640000") }
        assertEquals(oneAvax, chain.getBalance(address))

        fake.on("eth_getTransactionCount") { JsonPrimitive("0x2a") }
        assertEquals(42L, chain.getNonce(address))

        fake.on("eth_estimateGas") { JsonPrimitive("0x5208") }
        assertEquals(
            21000L,
            chain.estimateGas(address, oneAvax, ByteArray(0)),
        )

        fake.on("eth_maxPriorityFeePerGas") { JsonPrimitive("0x77359400") } // 2 gwei
        fake.on("eth_gasPrice") { JsonPrimitive("0x6fc23ac00") } // 30 gwei
        val fees = chain.getFees()
        assertEquals(BigInteger("77359400", 16), fees.maxPriorityFeePerGas)
        assertEquals(BigInteger("6fc23ac00", 16).add(BigInteger("77359400", 16)), fees.maxFeePerGas)

        // The address travels as 0x-prefixed lowercase hex, and the balance read is against
        // "latest" — the mempool is for nonces, not balances.
        val balanceCall = fake.requests.first { it.first == "eth_getBalance" }.second
        assertEquals("0x" + "ab".repeat(20), balanceCall[0].jsonPrimitive.content)
        assertEquals("latest", balanceCall[1].jsonPrimitive.content)
    }

    @Test
    fun broadcastReVerifiesTheChainAndRefusesAForeignAnsweredHash() = runBlocking {
        val (chain, fake) = fujiClient()
        val wallet = EvmWallet.fromRoot(MigoRoot.fromBytes(ByteArray(32) { 0x5a.toByte() }), 0)
        val tx = Eip1559Tx(
            chainId = 43113,
            nonce = 0,
            maxPriorityFeePerGas = BigInteger.valueOf(2_000_000_000),
            maxFeePerGas = BigInteger.valueOf(30_000_000_000),
            gasLimit = 21000,
            to = ByteArray(20) { 0xcd.toByte() },
            value = BigInteger.ONE,
            data = ByteArray(0),
        )
        val signed = tx.sign(wallet)

        // The session was already verified by a read; broadcast checks the chain id *again*, at
        // the one moment value-carrying bytes leave.
        fake.on("eth_getBalance") { quantity(1) }
        chain.getBalance(wallet.address())
        val before = fake.callsTo("eth_chainId")

        fake.on("eth_sendRawTransaction") { JsonPrimitive(signed.txHashHex()) }
        assertEquals(signed.txHashHex(), chain.broadcast(signed))
        assertEquals(
            "broadcast re-verifies the chain id after the session was already verified",
            before + 1,
            fake.callsTo("eth_chainId"),
        )
        val sent = fake.requests.first { it.first == "eth_sendRawTransaction" }.second[0]
            .jsonPrimitive.content
        assertTrue("the raw transaction is type-2", sent.startsWith("0x02"))
        assertEquals(
            "the raw transaction travels hex-encoded, type byte first",
            2 + signed.raw.size * 2,
            sent.length,
        )

        // An endpoint that answers a different hash than Keccak-256(raw) is refused: the tracker
        // would follow someone else's transaction to its ending.
        fake.on("eth_sendRawTransaction") { JsonPrimitive("0x" + "00".repeat(32)) }
        try {
            chain.broadcast(signed)
            fail("a foreign answered hash must be refused")
        } catch (error: ChainError) {
            assertTrue(error.message!!.contains("foreign hash"))
        }
    }

    @Test
    fun aChainErrorFromTheEndpointCarriesTheJsonRpcCode() = runBlocking {
        val (chain, fake) = fujiClient()
        fake.on("eth_getBalance") {
            Raw("""{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"insufficient funds for gas"}}""")
        }
        try {
            chain.getBalance(ByteArray(20))
            fail("the endpoint's error must surface")
        } catch (error: ChainError) {
            assertEquals(java.lang.Long.valueOf(-32000L), error.code)
        }
    }

    @Test
    fun trackConfirmsOnlyThroughAReceiptWithStatus1() = runBlocking {
        val (chain, fake) = fujiClient()
        val txHash = "0x" + "11".repeat(32)
        val states = mutableListOf<String>()

        // The receipt arrives on the second poll, so the tracker first sees the transaction in the
        // mempool (PENDING) and only then in a block (CONFIRMED) — the two states spec #41 keeps
        // apart. Flipping the flag from the mempool handler makes the sequencing deterministic.
        var mined = false
        fake.on("eth_getTransactionReceipt") {
            if (mined) receipt("0x1", "0x2a", "0x5208") else JsonNull
        }
        fake.on("eth_getTransactionByHash") {
            mined = true
            buildJsonObject { put("hash", txHash) }
        }

        val result = chain.track(
            txHash,
            TrackOptions(initialIntervalMs = 1, onState = { states.add(it) }),
        )
        assertEquals(TrackOutcome.Confirmed, result.outcome)
        assertEquals(java.lang.Long.valueOf(42), result.blockNumber)
        assertEquals(BigInteger("5208", 16), result.gasUsed)
        assertEquals(listOf("PENDING", "CONFIRMED"), states)
    }

    @Test
    fun trackReportsAStatus0ReceiptAsRevertedNeverAsConfirmed() = runBlocking {
        val (chain, fake) = fujiClient()
        fake.on("eth_getTransactionReceipt") { receipt("0x0", "0x2a", "0x5208") }
        fake.on("eth_getTransactionByHash") { buildJsonObject { } }
        val result = chain.track("0x" + "22".repeat(32), TrackOptions(initialIntervalMs = 1))
        assertEquals(TrackOutcome.Reverted, result.outcome)
    }

    @Test
    fun trackReportsAVanishedTransactionAsDropped() = runBlocking {
        val (chain, fake) = fujiClient()
        val txHash = "0x" + "33".repeat(32)
        // In the mempool on the first look, gone by the second.
        var seenOnce = false
        fake.on("eth_getTransactionReceipt") { JsonNull }
        fake.on("eth_getTransactionByHash") {
            if (!seenOnce) {
                seenOnce = true
                buildJsonObject { put("hash", txHash) }
            } else {
                JsonNull
            }
        }
        val result = chain.track(txHash, TrackOptions(initialIntervalMs = 1, maxIntervalMs = 1))
        assertEquals(TrackOutcome.Dropped, result.outcome)
    }

    @Test
    fun trackReportsADeadlineAsExpiredAnUnresolvedEnding() = runBlocking {
        val (chain, fake) = fujiClient()
        fake.on("eth_getTransactionReceipt") { JsonNull }
        fake.on("eth_getTransactionByHash") { buildJsonObject { } } // still in the mempool, never mined
        val result = chain.track(
            "0x" + "44".repeat(32),
            TrackOptions(initialIntervalMs = 1, maxIntervalMs = 1, timeoutMs = 5),
        )
        assertEquals(TrackOutcome.Expired, result.outcome)
    }
}
