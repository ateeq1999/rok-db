//! The single path every statement takes to the database: parameter
//! binding, logging, slow-query detection and cache invalidation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_core::stream::BoxStream;
use futures_util::TryStreamExt;
use sqlx::postgres::PgRow;
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
    start: Instant,
}

impl<'a> Probe<'a> {
    fn start<'e, E: Executor<'e>>(executor: &E, sql: &'a Sql, writes: Writes<'a>) -> Self {
        for table in writes {
            executor.__touch(table);
        }
        tracing::trace!(target: "rok_db::query", sql = sql.as_str(), params = ?sql.params(), "executing");
        Self {
            sql,
            ctx: executor.__context(),
            writes,
            start: Instant::now(),
        }
    }

    fn finish<T>(self, result: &std::result::Result<T, sqlx::Error>, rows: impl FnOnce(&T) -> u64) {
        let elapsed = self.start.elapsed();
        let ms = elapsed.as_secs_f64() * 1000.0;
        let sql = self.sql.as_str();
        match result {
            Ok(value) => {
                let rows = rows(value);
                tracing::debug!(target: "rok_db::query", sql, elapsed_ms = ms, rows, "query");
                let slow = self
                    .ctx
                    .as_ref()
                    .map_or(DEFAULT_SLOW_QUERY, |c| c.slow_query);
                if slow > Duration::ZERO && elapsed >= slow {
                    tracing::warn!(target: "rok_db::slow_query", sql, elapsed_ms = ms, rows, threshold_ms = slow.as_millis() as u64, "slow query");
                }
                if let Some(cache) = self.ctx.as_ref().and_then(|c| c.cache.as_ref()) {
                    for table in self.writes {
                        cache.invalidate(table);
                    }
                }
            }
            Err(error) => {
                tracing::debug!(target: "rok_db::query", sql, elapsed_ms = ms, %error, "query failed");
            }
        }
    }
}

pub(crate) async fn fetch_all<'e, T, E>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
) -> Result<Vec<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e>,
{
    let args = sql.arguments()?;
    let probe = Probe::start(&executor, sql, writes);
    let result = sqlx::query_as_with::<_, T, _>(sql.as_str(), args)
        .fetch_all(executor)
        .await;
    probe.finish(&result, |rows| rows.len() as u64);
    Ok(result?)
}

pub(crate) async fn fetch_optional<'e, T, E>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
) -> Result<Option<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e>,
{
    let args = sql.arguments()?;
    let probe = Probe::start(&executor, sql, writes);
    let result = sqlx::query_as_with::<_, T, _>(sql.as_str(), args)
        .fetch_optional(executor)
        .await;
    probe.finish(&result, |row| row.is_some() as u64);
    Ok(result?)
}

pub(crate) async fn fetch_rows<'e, E: Executor<'e>>(executor: E, sql: &Sql) -> Result<Vec<PgRow>> {
    let args = sql.arguments()?;
    let probe = Probe::start(&executor, sql, &[]);
    let result = sqlx::query_with(sql.as_str(), args)
        .fetch_all(executor)
        .await;
    probe.finish(&result, |rows| rows.len() as u64);
    Ok(result?)
}

pub(crate) async fn fetch_scalar<'e, T, E>(executor: E, sql: &Sql) -> Result<T>
where
    T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
    E: Executor<'e>,
{
    let args = sql.arguments()?;
    let probe = Probe::start(&executor, sql, &[]);
    let result = sqlx::query_scalar_with::<_, T, _>(sql.as_str(), args)
        .fetch_one(executor)
        .await;
    probe.finish(&result, |_| 1);
    Ok(result?)
}

pub(crate) async fn execute<'e, E: Executor<'e>>(
    executor: E,
    sql: &Sql,
    writes: Writes<'_>,
) -> Result<u64> {
    let args = sql.arguments()?;
    let probe = Probe::start(&executor, sql, writes);
    let result = sqlx::query_with(sql.as_str(), args)
        .execute(executor)
        .await
        .map(|r| r.rows_affected());
    probe.finish(&result, |n| *n);
    Ok(result?)
}

/// Stream rows one by one; the SQL is owned by the stream.
pub(crate) fn stream<'e, T, E>(executor: E, sql: Sql) -> BoxStream<'e, Result<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin + 'e,
    E: Executor<'e> + 'e,
{
    tracing::debug!(target: "rok_db::query", sql = sql.as_str(), "stream");
    Box::pin(async_stream::try_stream! {
        let args = sql.arguments()?;
        let mut rows = sqlx::query_as_with::<_, T, _>(sql.as_str(), args).fetch(executor);
        while let Some(row) = rows.try_next().await? {
            yield row;
        }
    })
}
