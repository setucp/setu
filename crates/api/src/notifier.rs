//! Command-queue wake-ups (spec §5, plan Task 8): one `PgListener` on
//! the `setu_commands` channel (the queue's `enqueue` notifies with the
//! server id on commit) fans each payload out to the long polls.
//!
//! A long poll [`Notifier::subscribe`]s BEFORE it looks at the queue, so
//! an enqueue that commits between its check and its wait still wakes it.
//! The listener reconnects with backoff; after every (re)connect, and
//! whenever the connection drops, it wakes every subscriber (`*`) since
//! notifications may have been lost meanwhile. Consumers re-check the
//! queue on a fallback tick regardless ([`FALLBACK_TICK`]).

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::broadcast;

/// Consumers re-check the queue at least this often.
pub const FALLBACK_TICK: Duration = Duration::from_secs(5);
/// Payload that wakes every subscriber.
const WAKE_ALL: &str = "*";
const CAPACITY: usize = 1024;
const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// Fan-out of `setu_commands` notifications. Cheap to clone.
#[derive(Clone)]
pub struct Notifier {
    tx: broadcast::Sender<Arc<str>>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notifier")
    }
}

impl Notifier {
    /// Start the listener task on its own connection from `pool`'s
    /// connect options. Must be called inside a tokio runtime.
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        let task_tx = tx.clone();
        tokio::spawn(async move { listen_forever(pool, task_tx).await });
        Self { tx }
    }

    /// A notifier without a listener (wakes only through [`Self::wake`]).
    pub fn detached() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Subscribe to wake-ups for `server_id`. Only notifications sent
    /// after this call are seen: subscribe before checking the queue.
    pub fn subscribe(&self, server_id: &str) -> Subscription {
        Subscription {
            rx: self.tx.subscribe(),
            server_id: server_id.to_string(),
        }
    }

    /// Wake the subscribers of `server_id` (`*`: everyone).
    pub fn wake(&self, server_id: &str) {
        let _ = self.tx.send(Arc::from(server_id));
    }
}

/// One long poll's view of the notifications.
pub struct Subscription {
    rx: broadcast::Receiver<Arc<str>>,
    server_id: String,
}

impl Subscription {
    /// Resolves at the next notification for this server (or a wake-all,
    /// or after missing messages: the caller re-checks either way). Never
    /// resolves once the notifier is gone; pair it with a timeout.
    pub async fn notified(&mut self) {
        loop {
            match self.rx.recv().await {
                Ok(id) if *id == *self.server_id || &*id == WAKE_ALL => return,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return,
                Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
            }
        }
    }
}

async fn listen_forever(pool: PgPool, tx: broadcast::Sender<Arc<str>>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        match PgListener::connect_with(&pool).await {
            Ok(mut listener) => match listener.listen(setu_db::NOTIFY_CHANNEL).await {
                Ok(()) => {
                    backoff = BACKOFF_MIN;
                    let _ = tx.send(Arc::from(WAKE_ALL));
                    loop {
                        match listener.try_recv().await {
                            Ok(Some(n)) => {
                                let _ = tx.send(Arc::from(n.payload()));
                            }
                            // Connection lost: sqlx reconnects on the next
                            // call; anything sent meanwhile is lost.
                            Ok(None) => {
                                let _ = tx.send(Arc::from(WAKE_ALL));
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "command notifier lost its listener");
                                break;
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "command notifier LISTEN failed"),
            },
            Err(e) => tracing::warn!(error = %e, "command notifier cannot connect"),
        }
        let _ = tx.send(Arc::from(WAKE_ALL));
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscription_wakes_for_its_server_and_wake_all_only() {
        let n = Notifier::detached();
        let mut sub = n.subscribe("srv-a");
        n.wake("srv-b");
        let other = tokio::time::timeout(Duration::from_millis(50), sub.notified()).await;
        assert!(
            other.is_err(),
            "another server's notification must not wake"
        );
        n.wake("srv-a");
        tokio::time::timeout(Duration::from_millis(50), sub.notified())
            .await
            .expect("own server wakes");
        n.wake(WAKE_ALL);
        tokio::time::timeout(Duration::from_millis(50), sub.notified())
            .await
            .expect("wake-all wakes");
    }
}
