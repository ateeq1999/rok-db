use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::Postgres;
use sqlx::postgres::{PgConnection, PgListener, PgPool};

use crate::cache::QueryCache;

/// Default threshold above which a query is logged as slow.
pub(crate) const DEFAULT_SLOW_QUERY: Duration = Duration::from_secs(1);

/// Per-[`Db`](crate::Db) settings shared with every query run through it.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) cache: Option<QueryCache>,
    pub(crate) slow_query: Duration,
    pub(crate) replicas: Vec<PgPool>,
    pub(crate) next_replica: Arc<AtomicUsize>,
}

impl Context {
    /// The next replica in round-robin order, if any.
    pub(crate) fn replica(&self) -> Option<PgPool> {
        if self.replicas.is_empty() {
            return None;
        }
        let i = self.next_replica.fetch_add(1, Ordering::Relaxed) % self.replicas.len();
        Some(self.replicas[i].clone())
    }
}

impl Default for Context {
    fn default() -> Self {
        Self {
            cache: None,
            slow_query: DEFAULT_SLOW_QUERY,
            replicas: Vec::new(),
            next_replica: Arc::default(),
        }
    }
}

/// Anything that can run a query: [`&Db`](crate::Db), `&mut Tx`, a plain
/// sqlx `&PgPool` or `&mut PgConnection`.
///
/// Executors created by rok-db (`&Db`, `&mut Tx`) also carry the pool's
/// settings — the [query cache](crate::QueryCache) and the slow-query
/// threshold. Plain sqlx executors run queries uncached with the default
/// settings.
pub trait Executor<'c>: sqlx::Executor<'c, Database = Postgres> {
    #[doc(hidden)]
    fn __context(&self) -> Option<Arc<Context>> {
        None
    }

    /// A read replica to run a read-only query on, if configured.
    #[doc(hidden)]
    fn __replica(&self) -> Option<PgPool> {
        None
    }

    /// Called before a write to `table` runs through this executor.
    #[doc(hidden)]
    fn __touch(&self, _table: &'static str) {}
}

impl<'c> Executor<'c> for &'c PgPool {}
impl<'c> Executor<'c> for &'c mut PgConnection {}
impl<'c> Executor<'c> for &'c mut PgListener {}
