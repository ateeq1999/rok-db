use std::sync::Arc;
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
}

impl Default for Context {
    fn default() -> Self {
        Self {
            cache: None,
            slow_query: DEFAULT_SLOW_QUERY,
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

    /// Called before a write to `table` runs through this executor.
    #[doc(hidden)]
    fn __touch(&self, _table: &'static str) {}
}

impl<'c> Executor<'c> for &'c PgPool {}
impl<'c> Executor<'c> for &'c mut PgConnection {}
impl<'c> Executor<'c> for &'c mut PgListener {}
