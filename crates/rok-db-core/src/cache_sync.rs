//! Keeps the query caches of several processes coherent by broadcasting
//! invalidations over PostgreSQL `LISTEN`/`NOTIFY`.

use std::sync::{Mutex, Weak};
use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cache::{Inner, QueryCache, apply_remote};

const CHANNEL: &str = "rok_db_cache";

/// Start the background task for `cache` (no-op outside a Tokio runtime).
pub(crate) fn start(pool: PgPool, cache: &QueryCache) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(target: "rok_db::cache", "shared cache invalidation needs a Tokio runtime; disabled");
        return;
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    cache.set_broadcast(tx);
    // A random-enough id so an instance ignores its own messages.
    let instance = format!(
        "{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    handle.spawn(run(pool, cache.downgrade(), rx, instance));
}

async fn run(
    pool: PgPool,
    cache: Weak<Mutex<Inner>>,
    mut outbox: UnboundedReceiver<String>,
    instance: String,
) {
    let mut first = true;
    loop {
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(target: "rok_db::cache", %error, "cache sync: connect failed, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if let Err(error) = listener.listen(CHANNEL).await {
            tracing::warn!(target: "rok_db::cache", %error, "cache sync: LISTEN failed, retrying");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        // Invalidations may have been missed while disconnected.
        if !std::mem::take(&mut first) {
            match cache.upgrade() {
                Some(inner) => apply_remote(&inner, ""),
                None => return,
            }
        }
        loop {
            tokio::select! {
                received = listener.recv() => match received {
                    Ok(n) => {
                        let Some((from, table)) = n.payload().split_once(':') else { continue };
                        if from == instance {
                            continue;
                        }
                        match cache.upgrade() {
                            Some(inner) => apply_remote(&inner, table),
                            None => return,
                        }
                    }
                    Err(error) => {
                        tracing::warn!(target: "rok_db::cache", %error, "cache sync: connection lost, reconnecting");
                        break;
                    }
                },
                outgoing = outbox.recv() => {
                    let Some(table) = outgoing else { return }; // every cache handle dropped
                    let mut tables = vec![table];
                    while let Ok(more) = outbox.try_recv() {
                        if !tables.contains(&more) {
                            tables.push(more);
                        }
                    }
                    for table in tables {
                        let payload = format!("{instance}:{table}");
                        if let Err(error) = sqlx::query("SELECT pg_notify($1, $2)")
                            .bind(CHANNEL)
                            .bind(&payload)
                            .execute(&pool)
                            .await
                        {
                            tracing::warn!(target: "rok_db::cache", %error, "cache sync: publish failed");
                        }
                    }
                }
            }
        }
    }
}
