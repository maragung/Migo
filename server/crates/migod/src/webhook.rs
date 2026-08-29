//! The webhook transport a bot's command is delivered over.
//!
//! [`crate::webhook::ReqwestWebhook`] is the composition root's answer to
//! [`migo_bots::Webhook`](migo_bots::traits::Webhook): the crate defines what a delivery
//! must do and when it may fail; this type is the one that actually speaks HTTPS. It fails
//! closed — every transport failure maps to one opaque internal error, because the
//! commanding user has no use for the difference between a refused connection and a 500,
//! and the bot owner's own logs are where the detail belongs.

use std::time::Duration;

use async_trait::async_trait;
use migo_bots::traits::Webhook;
use migo_core::Result;
use migo_protocol::fault;

/// How long a bot backend gets to answer. A webhook is on the command path — a user is
/// waiting — and a backend that needs longer is down for the purposes of this exchange.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Delivers bot commands to the webhooks their owners registered, over HTTPS.
pub(crate) struct ReqwestWebhook {
    client: reqwest::Client,
}

impl ReqwestWebhook {
    /// Builds the shared client.
    ///
    /// # Errors
    ///
    /// Propagates the TLS backend's own startup failure, which is a process-level
    /// misconfiguration rather than a per-delivery one.
    pub(crate) fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(DELIVERY_TIMEOUT)
            .build()
            .map_err(|error| {
                fault::internal(format!("could not build the webhook client: {error}"))
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Webhook for ReqwestWebhook {
    async fn deliver(&self, url: &str, payload: &[u8]) -> Result<()> {
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_vec())
            .send()
            .await
            .map_err(|_| fault::internal("the bot webhook could not be reached"))?;
        // A backend that answers at all has received the command; anything outside 2xx is
        // it saying no, which is still a delivery, still opaque to the commander, and still
        // the bot owner's problem to read about in their own logs.
        if response.status().is_success() {
            Ok(())
        } else {
            Err(fault::internal("the bot webhook refused the delivery"))
        }
    }
}
