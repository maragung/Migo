package com.migo.core.net

import com.migo.core.account.Network
import com.migo.core.account.SignedTx
import com.migo.core.account.checkChainId
import com.migo.core.crypto.hexOf
import java.io.IOException
import java.math.BigInteger
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * The chain client: JSON-RPC to a pinned public EVM network, straight from the device.
 *
 * Every other conversation in this package talks to a Migo server; this one talks to Avalanche's
 * public C-Chain RPC and deliberately skips the Migo server entirely — §184: the server is never a
 * blockchain proxy, never holds a nonce, and never sees a transaction, because the chain is public
 * and a proxy would only add a trusted party the network does not need. The RPC URL comes from a
 * [Network] constant the user picked by name, never as free input — a self-supplied RPC is the
 * classic way a wallet gets shown a fake chain (spec #44).
 *
 * # What this class does and does not decide
 *
 * The read side (balance, nonce, gas, fees) and the write side (broadcast) are here; *signing* is
 * not. [broadcast] takes a [SignedTx] — the private key never enters this file, and the only bytes
 * this class hands to the network are ones the user already confirmed against a full transaction
 * display (spec #40).
 *
 * # The session rule (spec #44)
 *
 * The first RPC of a client's life is `eth_chainId`, and its answer must equal the configured
 * network before anything else is asked — a mismatch is the chain-confusion case, and the honest
 * response is to refuse, not to pick one of the two ids. [broadcast] re-verifies: it is the one
 * moment bytes that carry value leave, and an endpoint that answers differently mid-session must
 * not get them.
 *
 * # The two confirmations that are never the same state (spec #41)
 *
 * `eth_sendRawTransaction` returning a hash means the RPC *accepted* the transaction, not that the
 * blockchain confirmed it. [broadcast] therefore reports acceptance and nothing more, and the only
 * road to [TrackOutcome.Confirmed] is [track] seeing `eth_getTransactionReceipt` answer
 * `status: 1`; a `status: 0` receipt is [TrackOutcome.Reverted], a transaction gone from the
 * mempool without a block is [TrackOutcome.Dropped], and a deadline that runs out is
 * [TrackOutcome.Expired] — an unresolved ending, never a quiet success.
 */
class ChainError(message: String, val code: Long? = null) : Exception(message)

/** The endings [ChainClient.track] can reach; everything else is progress. */
sealed class TrackOutcome {
    /** A receipt with `status: 1`. The only confirmation there is. */
    object Confirmed : TrackOutcome()

    /** A receipt with `status: 0`: the chain ran the transaction and it failed. */
    object Reverted : TrackOutcome()

    /** Gone from the mempool without a block — evicted, or a same-nonce sibling took its place. */
    object Dropped : TrackOutcome()

    /** The tracking deadline ran out; the transaction's fate is unknown, not successful. */
    object Expired : TrackOutcome()
}

/** What [ChainClient.track] reports. */
data class TrackResult(
    val outcome: TrackOutcome,
    val blockNumber: Long? = null,
    val gasUsed: BigInteger? = null,
    val txHash: String,
)

/** A fee ceiling pair for an EIP-1559 transaction, both in wei per gas. */
data class FeeEstimate(
    val maxPriorityFeePerGas: BigInteger,
    /** The total ceiling — EIP-1559 refunds the difference between this and what the block cost. */
    val maxFeePerGas: BigInteger,
)

/** Options for [ChainClient.track]. */
class TrackOptions(
    /** How long to poll before [TrackOutcome.Expired]. Default two minutes. */
    val timeoutMs: Long = 120_000,
    /** The first poll interval; each later one grows by half, capped at [maxIntervalMs]. */
    val initialIntervalMs: Long = 2_000,
    val maxIntervalMs: Long = 15_000,
    /**
     * Consecutive `null` transaction lookups tolerated before [TrackOutcome.Dropped]: a
     * transaction can sit unindexed for a poll or two right after broadcast, not forever.
     */
    val missingTolerance: Int = 6,
    /** Called on each state (`PENDING` on first sight, then the ending). */
    val onState: ((String) -> Unit)? = null,
)

/**
 * A JSON-RPC 2.0 conversation with one pinned EVM network. One instance per network per client.
 *
 * [transport] is the seam the tests script: it receives the serialized JSON-RPC request body and
 * returns the response body. The default posts to the network's pinned URL over OkHttp.
 */
class ChainClient(
    val network: Network,
    httpClient: OkHttpClient? = null,
    private val transport: (suspend (String) -> String)? = null,
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val http: OkHttpClient =
        httpClient ?: OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(20, TimeUnit.SECONDS)
            .build()
    private var nextId = 1
    private var chainVerified = false

    /**
     * The session rule: asks `eth_chainId` and refuses to continue unless it matches. Called before
     * every operation (once per client, again at every broadcast); public so a wallet surface can
     * open the session explicitly and fail before rendering anything.
     *
     * @throws com.migo.core.account.AccountError (ChainMismatch) naming both ids.
     */
    suspend fun verifyChain() {
        val observed = quantityLong(resultString(rpc("eth_chainId", "[]"), "chain id"), "chain id")
        // checkChainId throws on mismatch — the caller's remedy is a different network, never a
        // transaction built against the mismatched one.
        checkChainId(network, observed)
        chainVerified = true
    }

    /** The balance of an address, in wei, as of the latest block. A pull: never polled silently. */
    suspend fun getBalance(address: ByteArray): BigInteger {
        ensureSession()
        // JSON-RPC quantities are hex strings, whatever their magnitude — and balances live far
        // above what a Long can hold (one AVAX is 10^18 wei).
        val balance = resultString(
            rpc("eth_getBalance", """["${addressHex(address)}","latest"]"""),
            "balance",
        )
        return quantityWei(balance, "balance")
    }

    /**
     * The account's next nonce, counted with `'pending'` so two sends composed in a row get
     * distinct nonces rather than a second broadcast that quietly replaces the first.
     */
    suspend fun getNonce(address: ByteArray): Long {
        ensureSession()
        return quantityLong(
            resultString(rpc("eth_getTransactionCount", """["${addressHex(address)}","pending"]"""), "nonce"),
            "nonce",
        )
    }

    /** The gas a transaction needs, from `eth_estimateGas` — for the current block, no padding. */
    suspend fun estimateGas(to: ByteArray, value: BigInteger, data: ByteArray): Long {
        ensureSession()
        return quantityLong(
            resultString(
                rpc(
                    "eth_estimateGas",
                    """[{"to":"${addressHex(to)}","value":"${weiHex(value)}","data":"0x${hexOf(data)}"}]""",
                ),
                "gas estimate",
            ),
            "gas estimate",
        )
    }

    /**
     * The EIP-1559 fee ceilings for the current block: the priority fee the endpoint recommends
     * and a total ceiling above the observed gas price. Both are *ceilings* — the chain charges
     * what the block costs and refunds the rest.
     */
    suspend fun getFees(): FeeEstimate {
        ensureSession()
        val priority = quantityWei(
            resultString(rpc("eth_maxPriorityFeePerGas", "[]"), "priority fee"),
            "priority fee",
        )
        val gasPrice = quantityWei(resultString(rpc("eth_gasPrice", "[]"), "gas price"), "gas price")
        return FeeEstimate(priority, gasPrice.add(priority))
    }

    /**
     * Broadcasts a signed transaction and reports *acceptance* — never confirmation. An endpoint
     * that answers a hash other than `Keccak-256(raw)` is refused: the hash is the only handle the
     * user will track this send by, and a substituted one means the tracker would follow someone
     * else's transaction to its ending.
     */
    suspend fun broadcast(signed: SignedTx): String {
        // The session rule, again, at the one moment value-carrying bytes leave.
        verifyChain()
        val answered = resultString(
            rpc("eth_sendRawTransaction", """["0x${hexOf(signed.raw)}"]"""),
            "transaction hash",
        )
        if (answered != signed.txHashHex()) {
            throw ChainError(
                "eth_sendRawTransaction answered a foreign hash: $answered (expected ${signed.txHashHex()})",
            )
        }
        return answered
    }

    /**
     * Follows a broadcast transaction to an honest ending: `CONFIRMED` only via
     * `eth_getTransactionReceipt` answering `status: 1`, `REVERTED` on `status: 0`, `DROPPED` when
     * the transaction is gone from the mempool without a block, `EXPIRED` when the deadline runs
     * out. "The RPC accepted it" is a state this method never returns.
     *
     * The poll interval starts at [TrackOptions.initialIntervalMs] and grows by half each round up
     * to [TrackOptions.maxIntervalMs], because a transaction that has waited a minute is not going
     * to confirm in the next two seconds and polling like it will is noise.
     */
    suspend fun track(txHash: String, options: TrackOptions = TrackOptions()): TrackResult {
        val deadline = System.currentTimeMillis() + options.timeoutMs
        var interval = options.initialIntervalMs
        var missing = 0
        var seen = false
        while (true) {
            val receipt = getReceipt(txHash)
            if (receipt != null) {
                val outcome =
                    if (receipt.status == BigInteger.ONE) TrackOutcome.Confirmed else TrackOutcome.Reverted
                options.onState?.invoke(outcomeLabel(outcome))
                return TrackResult(outcome, receipt.blockNumber, receipt.gasUsed, txHash)
            }
            // No receipt. The transaction may simply not be in a block yet — or it may be gone:
            // look for it in the mempool and count consecutive absences.
            val pending = transactionExists(txHash)
            if (pending) {
                missing = 0
                if (!seen) {
                    seen = true
                    options.onState?.invoke("PENDING")
                }
            } else {
                missing += 1
                // A transaction the mempool never indexed (right after broadcast) and one that
                // appeared then vanished are both gone as far as this client can tell. `REPLACED`
                // — a same-nonce sibling confirming instead — is indistinguishable without an
                // indexer, so a vanished transaction reports `DROPPED` and the Activity list lets
                // a refresh correct it.
                if (seen || missing >= options.missingTolerance) {
                    options.onState?.invoke("DROPPED")
                    return TrackResult(TrackOutcome.Dropped, txHash = txHash)
                }
            }
            if (System.currentTimeMillis() + interval >= deadline) {
                options.onState?.invoke("EXPIRED")
                return TrackResult(TrackOutcome.Expired, txHash = txHash)
            }
            delay(interval)
            interval = (interval + interval / 2).coerceAtMost(options.maxIntervalMs)
        }
    }

    // --- plumbing --------------------------------------------------------------

    /** The session rule on first use: no RPC leaves this class before the chain id is checked. */
    private suspend fun ensureSession() {
        if (!chainVerified) verifyChain()
    }

    /** One JSON-RPC request/response, through the injected transport or OkHttp against the pinned URL. */
    private suspend fun rpc(method: String, paramsJson: String): kotlinx.serialization.json.JsonObject {
        val body = """{"jsonrpc":"2.0","id":${nextId++},"method":"$method","params":$paramsJson}"""
        val text = transport?.invoke(body) ?: http(body)
        val parsed = json.parseToJsonElement(text).jsonObject
        val error = parsed["error"]
        if (error != null && error !is JsonNull) {
            val obj = error.jsonObject
            val code = obj["code"]?.jsonPrimitive?.content?.toLongOrNull()
            val message = obj["message"]?.jsonPrimitive?.content ?: method
            throw ChainError("$method: $message", code)
        }
        // The caller reads `result` itself; what comes back is the whole response object.
        return parsed
    }

    private suspend fun http(body: String): String = withContext(Dispatchers.IO) {
        val request = Request.Builder()
            .url(network.rpcUrl)
            .post(body.toRequestBody("application/json".toMediaType()))
            .build()
        try {
            http.newCall(request).execute().use { response ->
                if (!response.isSuccessful) throw ChainError("$network: HTTP ${response.code}")
                response.body?.string() ?: throw ChainError("empty response body")
            }
        } catch (error: IOException) {
            throw ChainError("cannot reach ${network.rpcUrl}: ${error.message}")
        }
    }

    /** A receipt, or null when the transaction is not in a block. */
    private suspend fun getReceipt(txHash: String): Receipt? {
        ensureSession()
        val result = rpc("eth_getTransactionReceipt", """["$txHash"]""")["result"] ?: return null
        if (result is JsonNull) return null
        val receipt = result.jsonObject
        return Receipt(
            status = quantityWei(receipt.field("status"), "receipt status"),
            blockNumber = quantityLong(receipt.field("blockNumber"), "receipt block"),
            gasUsed = quantityWei(receipt.field("gasUsed"), "receipt gas used"),
        )
    }

    private suspend fun transactionExists(txHash: String): Boolean {
        ensureSession()
        val result = rpc("eth_getTransactionByHash", """["$txHash"]""")["result"] ?: return false
        return result !is JsonNull
    }

    /** The `result` string of a response — an error-shaped success is refused, not passed on. */
    private fun resultString(response: kotlinx.serialization.json.JsonObject, what: String): String {
        val result = response["result"] ?: throw ChainError("$what: the endpoint answered no result")
        if (result is JsonNull) throw ChainError("$what: the endpoint answered null")
        return result.jsonPrimitive.content
    }

    /** A JSON-RPC quantity ("0x…") as a BigInteger — for balances and fees, which dwarf a Long. */
    private fun quantityWei(raw: String, what: String): BigInteger {
        if (!raw.startsWith("0x")) throw ChainError("$what is not a quantity: $raw")
        val digits = raw.substring(2)
        if (digits.isEmpty()) return BigInteger.ZERO
        val parsed = BigInteger(digits, 16)
        if (parsed.signum() < 0) throw ChainError("$what is negative: $raw")
        return parsed
    }

    /** A JSON-RPC quantity as a Long, refusing a non-integer or one past 2^63. */
    private fun quantityLong(raw: String, what: String): Long {
        if (!raw.startsWith("0x")) throw ChainError("$what is not a quantity: $raw")
        val parsed = raw.substring(2).toLongOrNull(16)
            ?: throw ChainError("$what is not a small integer quantity: $raw")
        return parsed
    }

    /** The label a tracker state reports, matching the other clients letter for letter. */
    private fun outcomeLabel(outcome: TrackOutcome): String = when (outcome) {
        TrackOutcome.Confirmed -> "CONFIRMED"
        TrackOutcome.Reverted -> "REVERTED"
        TrackOutcome.Dropped -> "DROPPED"
        TrackOutcome.Expired -> "EXPIRED"
    }

    /** A string field of a receipt object, defaulting to "0x0" the way the RPC never leaves it out. */
    private fun kotlinx.serialization.json.JsonObject.field(name: String): String =
        this[name]?.jsonPrimitive?.content ?: "0x0"

    private class Receipt(val status: BigInteger, val blockNumber: Long, val gasUsed: BigInteger)
}

/** 20 bytes as the `0x`-prefixed lowercase hex every RPC method takes. */
private fun addressHex(address: ByteArray): String = "0x" + hexOf(address)

/** A wei value as a `0x`-prefixed minimal hex quantity, the form the RPC takes. */
private fun weiHex(value: BigInteger): String = "0x" + value.toString(16)
