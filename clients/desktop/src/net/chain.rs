//! The chain client: JSON-RPC to a public EVM network, from this process directly.
//!
//! Every other module in `net` talks to a Migo server. This one talks to Avalanche's public
//! C-Chain RPC and deliberately skips the Migo server entirely — §184: the server is never a
//! blockchain proxy, never holds a nonce, and never sees a transaction, because the chain is
//! public and a proxy would only add a trusted party the network does not need. The RPC URL comes
//! from a [`Network`] constant the user picked by name, never as free input — a self-supplied RPC
//! is the classic way a wallet gets shown a fake chain.
//!
//! # What this client does and does not decide
//!
//! The read side (balance, nonce, gas, fees) and the write side (broadcast) are here; *signing*
//! is not. [`broadcast`](ChainClient::broadcast) takes a [`SignedTx`] from `migo-account` — the
//! private key never enters this module, and the only bytes this client hands to the network are
//! ones the user already confirmed against a full transaction display.
//!
//! # The transport seam
//!
//! The HTTP call is one boxed async closure from request body to response body, so the tests
//! script a fake endpoint instead of opening sockets — the same seam the TypeScript and Kotlin
//! ports carry, and the reason all four clients can be tested against the identical conversation.
//!
//! # The two confirmations that are never the same state
//!
//! `eth_sendRawTransaction` returning a hash means the RPC *accepted* the transaction, not that
//! the blockchain confirmed it. `broadcast` therefore reports acceptance and nothing more, and
//! the only road to CONFIRMED is [`track`](ChainClient::track): `eth_getTransactionReceipt`
//! answering `status: 1`. The tracker polls with exponential backoff and a deadline; a receipt
//! with `status: 0` is REVERTED, a transaction that vanishes from the mempool before a block is
//! DROPPED, and a deadline that runs out is EXPIRED — reported as an unresolved *ending*, never
//! as success.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use migo_account::{Network, SignedTx};

/// The states the tracker can end in; everything else is progress.
///
/// The full spec #41 ladder is the UI's vocabulary — this module only ever answers the four
/// states the chain itself decides, because "the RPC accepted it" is a state no method here
/// returns as an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutcome {
    /// A receipt with `status: 1`. The only happy ending.
    Confirmed,
    /// A receipt with `status: 0`: the transaction ran and the chain refused its effect.
    Reverted,
    /// The transaction vanished from the mempool without a block.
    Dropped,
    /// The tracking deadline ran out. Unresolved, and reported as exactly that.
    Expired,
}

impl TrackOutcome {
    /// Spec #41's own word, so every surface labels the ending identically.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "CONFIRMED",
            Self::Reverted => "REVERTED",
            Self::Dropped => "DROPPED",
            Self::Expired => "EXPIRED",
        }
    }
}

/// The result of tracking a transaction to an ending.
#[derive(Debug, Clone)]
pub struct TrackResult {
    pub outcome: TrackOutcome,
    /// The block that included the transaction, when it got into one.
    pub block_number: Option<u64>,
    /// The gas the transaction actually used, when it got into a block.
    pub gas_used: Option<u128>,
}

/// A fee ceiling pair for an EIP-1559 transaction, both in wei per gas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeEstimate {
    /// The priority fee ceiling, from `eth_maxPriorityFeePerGas`.
    pub max_priority_fee_per_gas: u128,
    /// The total fee ceiling: the observed gas price plus the priority fee. A ceiling, not a
    /// price — EIP-1559 refunds the difference between this and what the block actually cost.
    pub max_fee_per_gas: u128,
}

/// The chain refused or failed a call. Distinct from [`AccountError`], which this module raises
/// only for the chain-id mismatch; a [`ChainError`] is a fact about the endpoint, an
/// [`AccountError::ChainMismatch`] is a fact about the configured network.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ChainError {
    message: String,
    /// The JSON-RPC error code the endpoint answered with, when there was one.
    pub code: Option<i64>,
}

impl ChainError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    fn with_code(message: impl Into<String>, code: Option<i64>) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}

/// Options for [`ChainClient::track`]. Defaults match the other clients: two-minute deadline,
/// two-second first interval growing by half to a fifteen-second cap.
#[derive(Debug, Clone)]
pub struct TrackOptions {
    pub timeout: Duration,
    pub initial_interval: Duration,
    pub max_interval: Duration,
    /// How many consecutive absent transaction lookups to tolerate before declaring DROPPED. A
    /// transaction can sit unindexed for a poll or two right after broadcast; it cannot sit
    /// there forever.
    pub missing_tolerance: u32,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            initial_interval: Duration::from_secs(2),
            max_interval: Duration::from_secs(15),
            missing_tolerance: 6,
        }
    }
}

/// One request body in, one response body out — the whole HTTP conversation, so tests script the
/// endpoint and production pins the URL.
type Transport = Arc<dyn Fn(String) -> TransportFuture + Send + Sync>;
type TransportFuture = Pin<Box<dyn Future<Output = Result<String, ChainError>> + Send>>;

/// A JSON-RPC 2.0 conversation with one pinned EVM network.
///
/// One instance per network per client, and not part of the worker's session state: the chain
/// conversation is orthogonal to the Migo session (it needs no login and no trust), so a fresh
/// one is built per operation and dies with it — except in the tracker task, which keeps its own
/// for the length of one transaction's life.
pub struct ChainClient {
    network: Network,
    transport: Transport,
    next_id: u64,
    chain_verified: bool,
}

impl ChainClient {
    /// The production transport: one POST per request to the network's pinned RPC URL.
    #[must_use]
    pub fn connect(network: Network, http: reqwest::Client) -> Self {
        let url = network.rpc_url.to_owned();
        let transport: Transport = Arc::new(move |body| {
            let http = http.clone();
            let url = url.clone();
            Box::pin(async move {
                let response = http
                    .post(&url)
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .map_err(|error| {
                        ChainError::new(format!("the RPC endpoint could not be reached: {error}"))
                    })?;
                if !response.status().is_success() {
                    return Err(ChainError::new(format!(
                        "the RPC endpoint answered HTTP {}",
                        response.status()
                    )));
                }
                response.text().await.map_err(|error| {
                    ChainError::new(format!("the RPC answer was cut short: {error}"))
                })
            })
        });
        Self::with_transport(network, transport)
    }

    /// The test transport: whatever the caller scripts, no socket anywhere.
    #[must_use]
    pub fn with_transport(network: Network, transport: Transport) -> Self {
        Self {
            network,
            transport,
            next_id: 1,
            chain_verified: false,
        }
    }

    /// The session rule: asks `eth_chainId` and refuses to continue unless it matches. Called
    /// automatically before every operation (once per client, and again at every broadcast).
    ///
    /// # Errors
    ///
    /// [`AccountError::ChainMismatch`] naming both ids when the endpoint answers another chain,
    /// or a [`ChainError`] when it answers nothing sensible.
    pub async fn verify_chain(&mut self) -> Result<(), ChainError> {
        let observed = self.rpc("eth_chainId", serde_json::json!([])).await?;
        let text = result_string(&observed, "eth_chainId")?;
        let parsed = quantity_u64(&text, "chain id")?;
        // The refusal names both ids, because the honest response to a mismatch is "these are
        // different chains", never a silent pick of one of them.
        self.network
            .check_chain_id(parsed)
            .map_err(|error| ChainError::new(error.to_string()))?;
        self.chain_verified = true;
        Ok(())
    }

    /// The balance of an address, in wei, as of the latest block. Explicitly a pull: §184
    /// forbids silent polling, so callers refresh when the user asks.
    ///
    /// # Errors
    ///
    /// A [`ChainError`] from the endpoint, or [`AccountError::ChainMismatch`] as a string.
    pub async fn get_balance(&mut self, address: &[u8; 20]) -> Result<u128, ChainError> {
        self.ensure_session().await?;
        // JSON-RPC quantities are hex strings, whatever their magnitude — and a balance in wei
        // lives far above what a float or a u64 could hold straight.
        let balance = self
            .rpc(
                "eth_getBalance",
                serde_json::json!([address_hex(address), "latest"]),
            )
            .await?;
        let text = result_string(&balance, "eth_getBalance")?;
        quantity_wei(&text, "balance")
    }

    /// The account's next nonce, from `eth_getTransactionCount` with `pending` — the count
    /// includes the account's in-flight transactions, so two sends composed in a row get
    /// distinct nonces rather than a second broadcast that quietly replaces the first.
    ///
    /// # Errors
    /// As [`ChainClient::get_balance`].
    pub async fn get_nonce(&mut self, address: &[u8; 20]) -> Result<u64, ChainError> {
        self.ensure_session().await?;
        let nonce = self
            .rpc(
                "eth_getTransactionCount",
                serde_json::json!([address_hex(address), "pending"]),
            )
            .await?;
        let text = result_string(&nonce, "eth_getTransactionCount")?;
        quantity_u64(&text, "nonce")
    }

    /// The gas a transaction needs, from `eth_estimateGas`. The estimate is for the current
    /// block; a caller that shows it to a user should add nothing to it silently — the ceiling
    /// the user confirms is the one signed.
    ///
    /// # Errors
    /// As [`ChainClient::get_balance`].
    pub async fn estimate_gas(
        &mut self,
        from: Option<&[u8; 20]>,
        to: &[u8; 20],
        value: u128,
    ) -> Result<u64, ChainError> {
        self.ensure_session().await?;
        let mut subject = serde_json::json!({
            "to": address_hex(to),
            "value": wei_hex(value),
            "data": "0x",
        });
        if let Some(from) = from {
            subject["from"] = serde_json::Value::String(address_hex(from));
        }
        let gas = self
            .rpc("eth_estimateGas", serde_json::json!([subject]))
            .await?;
        let text = result_string(&gas, "eth_estimateGas")?;
        quantity_u64(&text, "gas estimate")
    }

    /// The EIP-1559 fee ceilings for the current block: the priority fee the endpoint recommends
    /// and a total ceiling above it.
    ///
    /// # Errors
    /// As [`ChainClient::get_balance`].
    pub async fn get_fees(&mut self) -> Result<FeeEstimate, ChainError> {
        self.ensure_session().await?;
        let priority = self
            .rpc("eth_maxPriorityFeePerGas", serde_json::json!([]))
            .await?;
        let gas_price = self.rpc("eth_gasPrice", serde_json::json!([])).await?;
        let priority = quantity_wei(
            &result_string(&priority, "eth_maxPriorityFeePerGas")?,
            "priority fee",
        )?;
        let gas_price = quantity_wei(&result_string(&gas_price, "eth_gasPrice")?, "gas price")?;
        Ok(FeeEstimate {
            max_priority_fee_per_gas: priority,
            max_fee_per_gas: gas_price + priority,
        })
    }

    /// Broadcasts a signed transaction and reports *acceptance* — never confirmation. An RPC
    /// that answers a hash other than `Keccak-256(raw)` is refused: the hash is the only handle
    /// the user will track this transaction by, and a substituted one means the tracker would
    /// follow someone else's transaction to its ending.
    ///
    /// # Errors
    ///
    /// A [`ChainError`] if the endpoint refuses the transaction (FAILED in spec #41's terms) or
    /// answers a foreign hash.
    pub async fn broadcast(&mut self, signed: &SignedTx) -> Result<String, ChainError> {
        // The session rule, again, at the one moment value-carrying bytes leave. An endpoint
        // that verified a moment ago and answers a different chain now does not get the bytes.
        self.verify_chain().await?;
        let raw_hex = format!("0x{}", hex_of(signed.raw()));
        let answered = self
            .rpc("eth_sendRawTransaction", serde_json::json!([raw_hex]))
            .await?;
        let answered = result_string(&answered, "eth_sendRawTransaction")?;
        let expected = format!("0x{}", hex_of(signed.tx_hash()));
        if answered != expected {
            return Err(ChainError::new(format!(
                "eth_sendRawTransaction answered a foreign hash: {answered} (expected {expected})"
            )));
        }
        Ok(answered)
    }

    /// Follows a broadcast transaction to an honest ending: CONFIRMED only via a receipt with
    /// `status: 1`, REVERTED on `status: 0`, DROPPED when the transaction is gone from the
    /// mempool without a block, EXPIRED when the deadline runs out.
    ///
    /// The poll interval grows by half each round up to the cap, because a transaction that has
    /// waited a minute is not going to confirm in the next two seconds and polling like it will
    /// is noise. `on_state` fires on every state the tracker passes through (`PENDING` on first
    /// sight, then the ending), so a caller can show progress without owning the poll loop.
    ///
    /// # Errors
    ///
    /// A [`ChainError`] only when the endpoint itself cannot be asked; every on-chain answer is
    /// an [`TrackOutcome`], never an error — the chain's refusals are data.
    pub async fn track(
        &mut self,
        tx_hash: &str,
        options: &TrackOptions,
        mut on_state: impl FnMut(&str),
    ) -> Result<TrackResult, ChainError> {
        let deadline = tokio::time::Instant::now() + options.timeout;
        let mut interval = options.initial_interval;
        let mut missing: u32 = 0;
        let mut seen = false;
        loop {
            if let Some((status, block, gas_used)) = self.get_receipt(tx_hash).await? {
                let outcome = if status == 1 {
                    TrackOutcome::Confirmed
                } else {
                    TrackOutcome::Reverted
                };
                on_state(outcome.label());
                return Ok(TrackResult {
                    outcome,
                    block_number: Some(block),
                    gas_used: Some(gas_used),
                });
            }
            // No receipt. The transaction may simply not be in a block yet — or it may be gone:
            // look for it in the mempool and count consecutive absences.
            if self.transaction_exists(tx_hash).await? {
                missing = 0;
                if !seen {
                    seen = true;
                    on_state("PENDING");
                }
            } else {
                missing += 1;
                // A transaction the mempool never indexed (right after broadcast) and one that
                // appeared then vanished are both gone as far as this client can tell. REPLACED
                // — a same-nonce sibling confirming instead — is indistinguishable without an
                // indexer, so a vanished transaction reports DROPPED and the Activity list lets
                // a later refresh correct it.
                if seen || missing >= options.missing_tolerance {
                    on_state(TrackOutcome::Dropped.label());
                    return Ok(TrackResult {
                        outcome: TrackOutcome::Dropped,
                        block_number: None,
                        gas_used: None,
                    });
                }
            }
            if tokio::time::Instant::now() + interval >= deadline {
                on_state(TrackOutcome::Expired.label());
                return Ok(TrackResult {
                    outcome: TrackOutcome::Expired,
                    block_number: None,
                    gas_used: None,
                });
            }
            tokio::time::sleep(interval).await;
            interval = (interval + interval / 2).min(options.max_interval);
        }
    }

    // --- plumbing --------------------------------------------------------------

    /// The session rule on first use: no RPC leaves this client before the chain id has been
    /// checked.
    async fn ensure_session(&mut self) -> Result<(), ChainError> {
        if !self.chain_verified {
            self.verify_chain().await?;
        }
        Ok(())
    }

    /// One JSON-RPC request/response over the transport.
    async fn rpc(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ChainError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let text = (self.transport)(body.to_string()).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            ChainError::new(format!("{method}: the endpoint is not speaking JSON-RPC"))
        })?;
        if let Some(error) = parsed.get("error") {
            let code = error.get("code").and_then(serde_json::Value::as_i64);
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(method);
            return Err(ChainError::with_code(format!("{method}: {message}"), code));
        }
        Ok(parsed
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// The receipt of a mined transaction, or `None` when there is not one yet: `(status,
    /// block_number, gas_used)`.
    async fn get_receipt(&mut self, tx_hash: &str) -> Result<Option<(u64, u64, u128)>, ChainError> {
        self.ensure_session().await?;
        let receipt = self
            .rpc("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        if receipt.is_null() {
            return Ok(None);
        }
        let status = receipt
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("0x0");
        let block = receipt
            .get("blockNumber")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("0x0");
        let gas = receipt
            .get("gasUsed")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("0x0");
        Ok(Some((
            quantity_u64(status, "receipt status")?,
            quantity_u64(block, "receipt block")?,
            quantity_wei(gas, "receipt gas used")?,
        )))
    }

    /// Whether the chain still knows the transaction at all — mempool or block.
    async fn transaction_exists(&mut self, tx_hash: &str) -> Result<bool, ChainError> {
        self.ensure_session().await?;
        let entry = self
            .rpc("eth_getTransactionByHash", serde_json::json!([tx_hash]))
            .await?;
        Ok(!entry.is_null())
    }
}

/// A JSON-RPC result as a string, refusing the shapes that are not one.
fn result_string(value: &serde_json::Value, method: &str) -> Result<String, ChainError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ChainError::new(format!("{method}: the endpoint answered a non-string")))
}

/// A JSON-RPC quantity string (`0x…`) as a `u128` — balances, fees and gas used live far above
/// what a `u64` could hold straight, and anything past `u128` is not a quantity anyone sent.
fn quantity_wei(value: &str, what: &str) -> Result<u128, ChainError> {
    let digits = value
        .strip_prefix("0x")
        .ok_or_else(|| ChainError::new(format!("{what} is not a quantity: {value}")))?;
    if digits.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(digits, 16)
        .map_err(|_| ChainError::new(format!("{what} is not a quantity: {value}")))
}

/// A JSON-RPC quantity string as a `u64`, for the small integers: nonce, gas, chain id, block.
fn quantity_u64(value: &str, what: &str) -> Result<u64, ChainError> {
    quantity_wei(value, what)?
        .try_into()
        .map_err(|_| ChainError::new(format!("{what} is not a small integer quantity: {value}")))
}

/// 20 bytes as the `0x`-prefixed lowercase hex every RPC method takes.
fn address_hex(address: &[u8; 20]) -> String {
    format!("0x{}", hex_of(address))
}

/// A wei amount as the `0x`-prefixed minimal hex JSON-RPC expects.
fn wei_hex(value: u128) -> String {
    format!("0x{value:x}")
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use migo_account::{Eip1559Tx, EvmWallet, MigoRoot, FUJI_TESTNET};

    /// A whole-response wrapper: a handler that wants to answer a JSON-RPC *error* answers
    /// [`Either::Raw`] with the whole response text instead.
    type Handler = Box<dyn Fn(&serde_json::Value) -> Either + Send>;

    /// A JSON-RPC endpoint double: routes by method, records every request, and answers from a
    /// script the test mutates between calls (a poll loop must see different answers on later
    /// rounds). A handler's return value is the `result` element — `Value::Null` for "not
    /// found", an [`Either::Raw`] for a full error response.
    struct FakeChain {
        requests: Mutex<Vec<(String, serde_json::Value)>>,
        handlers: Mutex<HashMap<String, Handler>>,
    }

    /// What a scripted handler answers.
    enum Either {
        Result(serde_json::Value),
        Raw(String),
    }

    impl FakeChain {
        fn new() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                handlers: Mutex::new(HashMap::new()),
            }
        }

        /// Scripts one method. Handlers take the params array and answer for it.
        fn on<F>(&self, method: &str, handler: F)
        where
            F: Fn(&serde_json::Value) -> Either + Send + 'static,
        {
            self.handlers
                .lock()
                .unwrap()
                .insert(method.to_owned(), Box::new(handler));
        }

        fn calls_to(&self, method: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(asked, _)| asked == method)
                .count()
        }

        fn transport(self: &Arc<Self>) -> Transport {
            let fake = Arc::clone(self);
            Arc::new(move |body| {
                let fake = Arc::clone(&fake);
                Box::pin(async move {
                    let parsed: serde_json::Value = serde_json::from_str(&body)
                        .map_err(|_| ChainError::new("the test body is not JSON"))?;
                    let method = parsed["method"]
                        .as_str()
                        .expect("a JSON-RPC body names a method")
                        .to_owned();
                    let params = parsed["params"].clone();
                    fake.requests
                        .lock()
                        .unwrap()
                        .push((method.clone(), params.clone()));
                    let answer = {
                        let handlers = fake.handlers.lock().unwrap();
                        let handler = handlers
                            .get(&method)
                            .unwrap_or_else(|| panic!("no handler: {method}"));
                        handler(&params)
                    };
                    match answer {
                        Either::Result(value) => Ok(
                            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": value})
                                .to_string(),
                        ),
                        Either::Raw(text) => Ok(text),
                    }
                })
            })
        }
    }

    /// A Fuji client over a fresh double, with the chain id answered correctly by default.
    fn fuji_client() -> (ChainClient, Arc<FakeChain>) {
        let fake = Arc::new(FakeChain::new());
        fake.on("eth_chainId", |_| {
            Either::Result(serde_json::json!("0xa869"))
        });
        (
            ChainClient::with_transport(FUJI_TESTNET, fake.transport()),
            fake,
        )
    }

    fn quantity(value: u64) -> serde_json::Value {
        serde_json::Value::String(format!("0x{value:x}"))
    }

    /// The first request the endpoint ever sees is the chain id check, and a session whose chain
    /// id disagrees is closed before any other request — no balance was asked for, and the
    /// refusal names both ids rather than picking one.
    #[tokio::test]
    async fn the_session_opens_with_eth_chain_id_and_refuses_a_mismatched_network() {
        let (mut chain, fake) = fuji_client();
        fake.on("eth_getBalance", |_| Either::Result(quantity(1)));

        chain.get_balance(&[0xab; 20]).await.expect("the balance");
        let first = fake.requests.lock().unwrap()[0].0.clone();
        assert_eq!("eth_chainId", first);

        let wrong = Arc::new(FakeChain::new());
        // 43114 — mainnet, not the configured Fuji.
        wrong.on("eth_chainId", |_| {
            Either::Result(serde_json::json!("0xa86a"))
        });
        let mut confused = ChainClient::with_transport(FUJI_TESTNET, wrong.transport());
        let outcome = confused.get_balance(&[0x00; 20]).await;
        assert!(
            outcome.is_err(),
            "a mismatched chain id must close the session"
        );
        let error = outcome.unwrap_err().to_string();
        assert!(
            error.contains("43113"),
            "the refusal names the configured id: {error}"
        );
        assert!(
            error.contains("43114"),
            "the refusal names the observed id: {error}"
        );
        assert_eq!(
            1,
            wrong.requests.lock().unwrap().len(),
            "the mismatched session asked nothing else"
        );
    }

    /// Balances, nonces, gas and fees are hex quantities, and the address travels as
    /// `0x`-prefixed lowercase hex against `latest` — the mempool is for nonces, not balances.
    #[tokio::test]
    async fn balances_nonces_gas_and_fees_are_parsed_from_hex_quantities() {
        let (mut chain, fake) = fuji_client();
        let one_avax: u128 = 0xde0b6b3a7640000;

        fake.on("eth_getBalance", |_| {
            Either::Result(serde_json::json!("0xde0b6b3a7640000"))
        });
        assert_eq!(one_avax, chain.get_balance(&[0xab; 20]).await.unwrap());

        fake.on("eth_getTransactionCount", |_| Either::Result(quantity(42)));
        assert_eq!(42, chain.get_nonce(&[0xab; 20]).await.unwrap());

        fake.on("eth_estimateGas", |_| Either::Result(quantity(21_000)));
        assert_eq!(
            21_000,
            chain
                .estimate_gas(None, &[0xcd; 20], one_avax)
                .await
                .unwrap()
        );

        // 2 gwei priority, 30 gwei base: the total ceiling is their sum.
        fake.on("eth_maxPriorityFeePerGas", |_| {
            Either::Result(serde_json::json!("0x77359400"))
        });
        fake.on("eth_gasPrice", |_| {
            Either::Result(serde_json::json!("0x6fc23ac00"))
        });
        let fees = chain.get_fees().await.unwrap();
        assert_eq!(0x77359400, fees.max_priority_fee_per_gas);
        assert_eq!(0x6fc23ac00 + 0x77359400, fees.max_fee_per_gas);

        let requests = fake.requests.lock().unwrap();
        let balance = requests
            .iter()
            .find(|(method, _)| method == "eth_getBalance")
            .map(|(_, params)| params.clone())
            .expect("a balance was asked for");
        assert_eq!(
            format!("0x{}", "ab".repeat(20)),
            balance[0].as_str().unwrap()
        );
        assert_eq!("latest", balance[1].as_str().unwrap());
    }

    /// Broadcast re-verifies the chain at the one moment value-carrying bytes leave, and an
    /// endpoint that answers a different hash than Keccak-256(raw) is refused: the tracker
    /// would follow someone else's transaction to its ending.
    #[tokio::test]
    async fn broadcast_re_verifies_the_chain_and_refuses_a_foreign_answered_hash() {
        let (mut chain, fake) = fuji_client();
        let wallet = EvmWallet::from_root(&MigoRoot::from_bytes(&[0x5a; 32]).unwrap(), 0).unwrap();
        let tx = Eip1559Tx {
            chain_id: 43113,
            nonce: 0,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            gas_limit: 21_000,
            to: [0xcd; 20],
            value: 1,
            data: Vec::new(),
        };
        let signed = tx.sign(&wallet).unwrap();

        // The session was already verified by a read; broadcast checks the chain id *again*.
        fake.on("eth_getBalance", |_| Either::Result(quantity(1)));
        chain.get_balance(wallet.address()).await.unwrap();
        let before = fake.calls_to("eth_chainId");

        let expected = format!("0x{}", hex_of(signed.tx_hash()));
        fake.on("eth_sendRawTransaction", move |_| {
            Either::Result(serde_json::Value::String(expected.clone()))
        });
        let answered = chain.broadcast(&signed).await.unwrap();
        assert_eq!(format!("0x{}", hex_of(signed.tx_hash())), answered);
        assert_eq!(
            before + 1,
            fake.calls_to("eth_chainId"),
            "broadcast re-verifies the chain id after the session was already verified"
        );

        let sent = {
            let requests = fake.requests.lock().unwrap();
            requests
                .iter()
                .find(|(method, _)| method == "eth_sendRawTransaction")
                .map(|(_, params)| params[0].as_str().unwrap().to_owned())
                .expect("a transaction was sent")
        };
        assert!(
            sent.starts_with("0x02"),
            "the raw transaction is type-2: {sent}"
        );
        assert_eq!(
            2 + signed.raw().len() * 2,
            sent.len(),
            "the raw transaction travels hex-encoded, type byte first"
        );

        // A foreign answered hash is refused, whatever else the endpoint did.
        fake.on("eth_sendRawTransaction", |_| {
            Either::Result(serde_json::json!(format!("0x{}", "00".repeat(32))))
        });
        let outcome = chain.broadcast(&signed).await;
        let error = outcome.unwrap_err().to_string();
        assert!(error.contains("foreign hash"), "{error}");
    }

    /// The endpoint's JSON-RPC error code surfaces with the message.
    #[tokio::test]
    async fn a_chain_error_from_the_endpoint_carries_the_json_rpc_code() {
        let (mut chain, fake) = fuji_client();
        fake.on("eth_getBalance", |_| {
            Either::Raw(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"insufficient funds for gas"}}"#
                    .to_owned(),
            )
        });
        let error = chain.get_balance(&[0x00; 20]).await.unwrap_err();
        assert_eq!(Some(-32000), error.code);
    }

    /// Only a receipt with `status: 1` is CONFIRMED: the receipt arrives on the second poll, so
    /// the tracker first sees the transaction in the mempool (PENDING) and only then in a block
    /// — the two states spec #41 keeps apart.
    #[tokio::test]
    async fn track_confirms_only_through_a_receipt_with_status_1() {
        let (mut chain, fake) = fuji_client();
        let tx_hash = format!("0x{}", "11".repeat(32));
        let states: Mutex<Vec<String>> = Mutex::new(Vec::new());

        let mined = std::sync::Arc::new(Mutex::new(false));
        let receipt_mined = mined.clone();
        fake.on("eth_getTransactionReceipt", move |_| {
            if *receipt_mined.lock().unwrap() {
                Either::Result(serde_json::json!({
                    "status": "0x1", "blockNumber": "0x2a", "gasUsed": "0x5208"
                }))
            } else {
                Either::Result(serde_json::Value::Null)
            }
        });
        let tx_hash_for_by_hash = tx_hash.clone();
        let by_hash_mined = mined.clone();
        fake.on("eth_getTransactionByHash", move |_| {
            // The by-hash answer flips the shared flag on its first call, so the receipt
            // handler and this one stay in step: the first poll sees a null receipt and a
            // mempool transaction (PENDING), the second sees both flipped and the receipt
            // answers — the two states spec #41 keeps apart.
            let mut mined = by_hash_mined.lock().unwrap();
            *mined = true;
            Either::Result(serde_json::json!({ "hash": tx_hash_for_by_hash.clone() }))
        });

        let options = TrackOptions {
            initial_interval: Duration::from_millis(1),
            ..TrackOptions::default()
        };
        let result = chain
            .track(&tx_hash, &options, |state| {
                states.lock().unwrap().push(state.to_owned())
            })
            .await
            .unwrap();
        assert_eq!(TrackOutcome::Confirmed, result.outcome);
        assert_eq!(Some(42), result.block_number);
        assert_eq!(Some(0x5208), result.gas_used);
        assert_eq!(vec!["PENDING", "CONFIRMED"], *states.lock().unwrap());
    }

    /// A `status: 0` receipt is REVERTED, never CONFIRMED.
    #[tokio::test]
    async fn track_reports_a_status_0_receipt_as_reverted() {
        let (mut chain, fake) = fuji_client();
        fake.on("eth_getTransactionReceipt", |_| {
            Either::Result(serde_json::json!({
                "status": "0x0", "blockNumber": "0x2a", "gasUsed": "0x5208"
            }))
        });
        fake.on("eth_getTransactionByHash", |_| {
            Either::Result(serde_json::json!({}))
        });
        let options = TrackOptions {
            initial_interval: Duration::from_millis(1),
            ..TrackOptions::default()
        };
        let result = chain
            .track(&format!("0x{}", "22".repeat(32)), &options, |_| ())
            .await
            .unwrap();
        assert_eq!(TrackOutcome::Reverted, result.outcome);
    }

    /// In the mempool on the first look, gone by the second: DROPPED.
    #[tokio::test]
    async fn track_reports_a_vanished_transaction_as_dropped() {
        let (mut chain, fake) = fuji_client();
        fake.on("eth_getTransactionReceipt", |_| {
            Either::Result(serde_json::Value::Null)
        });
        let seen_once = Mutex::new(false);
        fake.on("eth_getTransactionByHash", move |_| {
            let mut seen = seen_once.lock().unwrap();
            if !*seen {
                *seen = true;
                Either::Result(serde_json::json!({ "hash": "0x33" }))
            } else {
                Either::Result(serde_json::Value::Null)
            }
        });
        let options = TrackOptions {
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(1),
            ..TrackOptions::default()
        };
        let result = chain
            .track(&format!("0x{}", "33".repeat(32)), &options, |_| ())
            .await
            .unwrap();
        assert_eq!(TrackOutcome::Dropped, result.outcome);
    }

    /// A deadline that runs out is EXPIRED — an unresolved ending, never a quiet success.
    #[tokio::test]
    async fn track_reports_a_deadline_as_expired() {
        let (mut chain, fake) = fuji_client();
        fake.on("eth_getTransactionReceipt", |_| {
            Either::Result(serde_json::Value::Null)
        });
        // Still in the mempool, never mined.
        fake.on("eth_getTransactionByHash", |_| {
            Either::Result(serde_json::json!({}))
        });
        let options = TrackOptions {
            timeout: Duration::from_millis(5),
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(1),
            ..TrackOptions::default()
        };
        let result = chain
            .track(&format!("0x{}", "44".repeat(32)), &options, |_| ())
            .await
            .unwrap();
        assert_eq!(TrackOutcome::Expired, result.outcome);
    }
}
