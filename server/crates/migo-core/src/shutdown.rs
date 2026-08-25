//! Cooperative shutdown.
//!
//! Every long-lived task in `migod` holds a [`Shutdown`] handle and races its
//! work against [`Shutdown::cancelled`]. On `SIGTERM` the process stops
//! accepting new connections, tells open sessions to reconnect elsewhere, drains
//! in-flight work, and only then exits.
//!
//! This matters more than it looks. A gateway that exits instantly during a
//! deploy converts a rolling restart into a thundering herd: thousands of
//! clients reconnect at the same instant, every one of them asking for a resume
//! window. Draining spreads that cost out — see
//! `docs/09-observability-ops.md`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// A clonable shutdown signal.
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    triggered: AtomicBool,
    notify: Notify,
}

impl Shutdown {
    /// A fresh, untriggered signal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests shutdown. Idempotent: calling it twice is not an error, which
    /// matters because an operator may send a second `SIGTERM` when the first
    /// looks slow.
    pub fn trigger(&self) {
        if !self.inner.triggered.swap(true, Ordering::SeqCst) {
            tracing::info!("shutdown requested");
        }
        // Woken unconditionally so late waiters are not left hanging.
        self.inner.notify.notify_waiters();
    }

    /// True once shutdown has been requested.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.inner.triggered.load(Ordering::SeqCst)
    }

    /// Resolves as soon as shutdown is requested, immediately if it already was.
    pub async fn cancelled(&self) {
        if self.is_triggered() {
            return;
        }
        let waiting = self.inner.notify.notified();
        // Re-check after arming the waiter to close the race where trigger()
        // fired between the check above and the subscription.
        if self.is_triggered() {
            return;
        }
        waiting.await;
    }

    /// Triggers on `SIGTERM` or `SIGINT`. Spawned once by the composition root.
    pub fn install_signal_handler(&self) {
        let signal = self.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal as unix_signal, SignalKind};
                let mut term = match unix_signal(SignalKind::terminate()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        tracing::error!(%error, "cannot listen for SIGTERM");
                        return;
                    }
                };
                let mut interrupt = match unix_signal(SignalKind::interrupt()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        tracing::error!(%error, "cannot listen for SIGINT");
                        return;
                    }
                };
                tokio::select! {
                    _ = term.recv() => tracing::info!(signal = "SIGTERM", "signal received"),
                    _ = interrupt.recv() => tracing::info!(signal = "SIGINT", "signal received"),
                }
            }
            #[cfg(not(unix))]
            {
                if tokio::signal::ctrl_c().await.is_err() {
                    tracing::error!("cannot listen for ctrl-c");
                    return;
                }
                tracing::info!(signal = "ctrl-c", "signal received");
            }
            signal.trigger();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_resolves_after_trigger() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_triggered());
        let waiter = shutdown.clone();
        let handle = tokio::spawn(async move { waiter.cancelled().await });
        shutdown.trigger();
        handle.await.expect("waiter completes");
        assert!(shutdown.is_triggered());
    }

    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_triggered() {
        let shutdown = Shutdown::new();
        shutdown.trigger();
        shutdown.cancelled().await;
    }

    #[tokio::test]
    async fn trigger_is_idempotent() {
        let shutdown = Shutdown::new();
        shutdown.trigger();
        shutdown.trigger();
        assert!(shutdown.is_triggered());
    }
}
