//! The single path every statement takes to the database: parameter
//! binding, replica routing, logging, metrics, slow-query detection and
//! cache invalidation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_core::stream::BoxStream;
use futures_util::TryStreamExt;
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Decode, FromRow, Postgres, Type};

use crate::context::{Context, DEFAULT_SLOW_QUERY};
use crate::sql::Sql;
use crate::{Executor, Result};

/// Tables a statement writes to, invalidated in the query cache.
pub(crate) type Writes<'a> = &'a [&'static str];

struct Probe<'a> {
    sql: &'a Sql,
    ctx: Option<Arc<Context>>,
    writes: Writes<'a>,
    replica: bool,
    start: Instant,
}

impl<'a> Probe<'a> {
    fn start(ctx: Option<Arc<Context>>, sql: &'a Sql, writes: Writes<'a>, replica: bool) -> Self {
        tracing::trace!(target: "rok_db::query", sql = sql.as_str(), params = ?sql.params(), replica, "executing");
        Self {
            sql,
            ctx,
            writes,
            replica,
            start: Instant::now(),
        }
    }

    fn finish<T>(self, result: &std::result::Result<T, sqlx::Error>, rows: impl FnOnce(&T) -> u64) {
        let elapsed = self.start.elapsed();
        let ms = elapsed.as_secs_f64() * 1000.0;
        let sql = self.sql.as_str();
        let replica = self.replica;
        let slow = self
            .ctx
            .as_ref()
            .map_or(DEFAULT_SLOW_QUERY, |c| c.slow_query);
        let is_slow = slow > Duration::ZERO && elapsed >= slow;
        match result {
            Ok(value) => {
                let rows = rows(value);
                crate::metrics::query(sql, elapsed, Some(rows), is_slow);
                tracing::debug!(target: "rok_db::query", sql, elapsed_ms = ms, rows, replica, "query");
                if is_slow {
                    tracing::warn!(target: "rok_db::slow_query", sql, elapsed_ms = ms, rows, replica, threshold_ms = slow.as_millis() as u64, "slow query");
                }
                if let Some(cache) = self.ctx.as_ref().and_then(|c| c.cache.as_ref()) {
                    for table in self.writes {
                        cache.invalidate(table);
                    }
                }
            }
            Err(error) => {
                crate::metrics::query(sql, elapsed, None, is_slow);
                tracing::debug!(target: "rok_db::query", sql, elapsed_ms = ms, replica, %error, "query failed");
            }
        }
    }
}

/// Connection-level failures after which a read is retried on the primary.
fn is_connection_error(e: &sqlx::Error) -> bool {
    matches!(
        e,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
    )
}

/// Run a read on a replica when routing allows it, falling back to the
/// primary executor on connection errors. `$run` is invoked with either a
/// `&PgPool` or the original executor, plus whether it is the replica.
macro_rules! routed {
    ($executor:ident, $replica:expr, $writes:expr, |$x:ident, $on_replica:ident, $ctx:ident| $run:expr) => {{
        let $ctx = $executor.__context();
        for table in $writes {
            $executor.__touch(table);
        }
        let replica_pool: Option<PgPool> = if $replica && $writes.is_empty() {
            $executor.__replica()
        } else {
            None
        };
        match replica_pool {
            Some(pool) => {
                let $x = &pool;
                let $on_replica = true;
                match $run {
                    Err(e) if is_connection_error(&e) => {
                        tracing::warn!(target: "rok_db::query", error = %e, "replica unavailable; retrying on the primary");
                        let $x = $executor;
                        let $on_replica = false;
                        $run
                    }
                    other => other,
                }
            }
            None => {
                let $x = $executor;
                let $on_replica = false;
                $run
            }
        }
        .map_err(crate::Error::from)
    }};
}

async fn all_on<'c, X, T>(
    x: X,
    sql: &Sql,
    probe: Probe<'_>,
) -> std::result::Result<Vec<T>, sqlx::Error>
where
    X: sqlx::Executor<'c, Database = Postgres>,
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let args = sql
        .arguments()
        .map_err(|e| sqlx::Error::Encode(e.to_string().into()))?;
    let result = sqlx::query_as_with::<_, T, _>(sql.as_str(), args)
        .fetch_all(x)
        .await;
    probe.finish(&result, |rows| rows.len() as u64);
    result
}

async fn optional_on<'c, X, T>(
    x: X,
    sql: &Sql,
    probe: Probe<'_>,
) -> std::result::Result<Option<T>, sqlx::Error>
where
    X: sqlx::Executor<'c, Database = Postgres>,
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let args = sql
        .arguments()
        .map_err(|e| sqlx::Error::Encode(e.to_string().into()))?;
    let result = sqlx::query_as_with::<_, T, _>(sql.as_str(), args)
        .fetch_optional(x)
        .await;
    probe.finish(&result, |row| row.is_some() as u64);
    result
}

async fn rows_on<'c, X>(
    x: X,
    sql: &Sql,
    probe: Probe<'_>,
) -> std::result::Result<Vec<PgRow>, sqlx::Error>
where
    X: sqlx::Executor<'c, Database = Postgres>,
{
    let args = sql
        .arguments()
        .map_err(|e| sqlx::Error::Encode(e.to_string().into()))?;
    let result = sqlx::query_with(sql.as_str(), args).fetch_all(x).await;
    probe.finish(&result, |rows| rows.len() as u64);
    result
}

async fn scalar_on<'c, X, T>(
    x: X,
    sql: &Sql,
    probe: Probe<'_>,
) -> std::result::Result<T, sqlx::Error>
where
    X: sqlx::Executor<'c, Database = Postgres>,
    T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
{
    let args = sql
        .arguments()
        .map_err(|e| sqlx::Error::Encode(e.to_string().into()))?;
    let result = sqlx::query_scalar_with::<_, T, _>(sql.as_str(), args)
        .fetch_one(x)
        .await;
    probe.finish(&result, |_| 1);
    result
}

/// Bind errors are reported as `Error::Encode`, not wrapped in sqlx errors.
fn check_args(sql: &Sql) -> Result<()> {
    sql.arguments().map(|_| ())
}

pub(crate) async fn fetch_all<'e, T, E>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
    replica: bool,
) -> Result<Vec<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e>,
{
    check_args(sql)?;
    routed!(executor, replica, writes, |x, on_replica, ctx| {
        all_on(x, sql, Probe::start(ctx.clone(), sql, writes, on_replica)).await
    })
}

pub(crate) async fn fetch_optional<'e, T, E>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
    replica: bool,
) -> Result<Option<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e>,
{
    check_args(sql)?;
    routed!(executor, replica, writes, |x, on_replica, ctx| {
        optional_on(x, sql, Probe::start(ctx.clone(), sql, writes, on_replica)).await
    })
}

pub(crate) async fn fetch_rows<'e, E: Executor<'e>>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
    replica: bool,
) -> Result<Vec<PgRow>> {
    check_args(sql)?;
    routed!(executor, replica, writes, |x, on_replica, ctx| {
        rows_on(x, sql, Probe::start(ctx.clone(), sql, writes, on_replica)).await
    })
}

pub(crate) async fn fetch_scalar<'e, T, E>(executor: E, sql: &Sql, replica: bool) -> Result<T>
where
    T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
    E: Executor<'e>,
{
    check_args(sql)?;
    let writes: Writes<'_> = &[];
    routed!(executor, replica, writes, |x, on_replica, ctx| {
        scalar_on(x, sql, Probe::start(ctx.clone(), sql, writes, on_replica)).await
    })
}

pub(crate) async fn execute<'e, E: Executor<'e>>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
) -> Result<u64> {
    let args = sql.arguments()?;
    for table in writes {
        executor.__touch(table);
    }
    let probe = Probe::start(executor.__context(), sql, writes, false);
    let result = sqlx::query_with(sql.as_str(), args)
        .execute(executor)
        .await
        .map(|r| r.rows_affected());
    probe.finish(&result, |n| *n);
    Ok(result?)
}

/// Stream rows one by one; the SQL is owned by the stream. Replica routing
/// applies, without fallback (a stream can't be restarted transparently).
pub(crate) fn stream<'e, T, E>(executor: E, sql: Sql, replica: bool) -> BoxStream<'e, Result<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin + 'e,
    E: Executor<'e> + 'e,
{
    let pool = if replica { executor.__replica() } else { None };
    tracing::debug!(target: "rok_db::query", sql = sql.as_str(), replica = pool.is_some(), "stream");
    match pool {
        Some(pool) => Box::pin(async_stream::try_stream! {
            let args = sql.arguments()?;
            let mut rows = sqlx::query_as_with::<_, T, _>(sql.as_str(), args).fetch(&pool);
            while let Some(row) = rows.try_next().await? {
                yield row;
            }
        }),
        None => Box::pin(async_stream::try_stream! {
            let args = sql.arguments()?;
            let mut rows = sqlx::query_as_with::<_, T, _>(sql.as_str(), args).fetch(executor);
            while let Some(row) = rows.try_next().await? {
                yield row;
            }
        }),
    }
}
