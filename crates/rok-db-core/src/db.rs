use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use sqlx::postgres::{
    PgConnectOptions, PgConnection, PgPool, PgPoolOptions, PgQueryResult, PgRow, PgStatement,
    PgTypeInfo,
};
use sqlx::{Describe, Either, Execute, Postgres};

use crate::context::Context;
use crate::{Error, QueryCache, Result};

/// A pooled PostgreSQL connection handle — the entry point of rok-db.
///
/// `Db` is cheap to clone (it wraps an `Arc`'d pool) and can be passed
/// wherever an [`Executor`](crate::Executor) is expected:
///
/// ```ignore
/// let db = Db::connect("postgres://localhost/app").await?;
/// let user = User::find(&db, 1).await?;
/// ```
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
    ctx: Arc<Context>,
}

impl Db {
    /// Connect with default pool settings.
    pub async fn connect(url: &str) -> Result<Self> {
        Self::builder().connect(url).await
    }

    /// Connect using the `DATABASE_URL` environment variable.
    pub async fn connect_env() -> Result<Self> {
        Self::builder().connect_env().await
    }

    /// Configure the connection pool before connecting.
    pub fn builder() -> DbBuilder {
        DbBuilder::default()
    }

    /// Wrap an existing sqlx pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            ctx: Arc::default(),
        }
    }

    /// A handle to the same pool and settings that never uses read replicas,
    /// for reads that must see the latest writes.
    pub fn primary(&self) -> Db {
        let mut ctx = (*self.ctx).clone();
        ctx.replicas.clear();
        Db {
            pool: self.pool.clone(),
            ctx: Arc::new(ctx),
        }
    }

    /// Number of configured read replicas.
    pub fn replica_count(&self) -> usize {
        self.ctx.replicas.len()
    }

    /// A snapshot of the connection pool's state.
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            size: self.pool.size(),
            idle: self.pool.num_idle(),
            max_connections: self.pool.options().get_max_connections(),
        }
    }

    /// Report [`stats`](Self::stats) as `rok_db_pool_connections` gauges
    /// (feature `metrics`). Call it periodically, e.g. from a background
    /// task or before each metrics scrape.
    #[cfg(feature = "metrics")]
    pub fn record_pool_metrics(&self) {
        crate::metrics::pool(self.stats());
    }

    /// The query cache, if enabled with [`DbBuilder::query_cache`].
    pub fn cache(&self) -> Option<&QueryCache> {
        self.ctx.cache.as_ref()
    }

    /// The underlying sqlx pool, for anything rok-db doesn't cover.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Consume the handle and return the underlying sqlx pool.
    pub fn into_pool(self) -> PgPool {
        self.pool
    }

    /// Start a transaction. It is rolled back on drop unless
    /// [`Tx::commit`] is called.
    pub async fn begin(&self) -> Result<Tx> {
        #[cfg_attr(not(feature = "json"), allow(unused_mut))]
        let mut tx = Tx {
            inner: self.pool.begin().await?,
            ctx: Some(self.ctx.clone()),
            touched: Mutex::default(),
        };
        #[cfg(feature = "json")]
        if let Some(actor) = crate::audit::current_actor() {
            tx.set_actor(&actor).await?;
        }
        Ok(tx)
    }

    /// Run `f` inside a transaction, committing if it returns `Ok` and
    /// rolling back if it returns `Err`.
    ///
    /// ```ignore
    /// let user = db.transaction(|tx| Box::pin(async move {
    ///     let user = new_user.insert(&mut *tx).await?;
    ///     Profile::create().set(Profile::USER_ID, user.id).exec(&mut *tx).await?;
    ///     Ok(user)
    /// })).await?;
    /// ```
    pub async fn transaction<T, E, F>(&self, f: F) -> std::result::Result<T, E>
    where
        F: for<'t> FnOnce(&'t mut Tx) -> BoxFuture<'t, std::result::Result<T, E>>,
        E: From<Error>,
    {
        let mut tx = self.begin().await?;
        match f(&mut tx).await {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(err) => {
                tx.rollback().await?;
                Err(err)
            }
        }
    }

    /// Run `f` in a transaction configured by `options`, retrying the whole
    /// closure when PostgreSQL aborts it with a serialization failure or a
    /// deadlock (SQLSTATE `40001` / `40P01`), up to `options.retries` times.
    ///
    /// The closure may run more than once, so it must be `Fn` and should not
    /// have side effects outside the database.
    ///
    /// ```ignore
    /// let opts = TxOptions::new().isolation(Isolation::Serializable).retries(5);
    /// db.transaction_with(opts, |tx| Box::pin(async move {
    ///     let mut from = Account::find_or_fail(&mut *tx, 1).await?;
    ///     let mut to = Account::find_or_fail(&mut *tx, 2).await?;
    ///     from.balance -= 10;
    ///     to.balance += 10;
    ///     from.save(&mut *tx).await?;
    ///     to.save(&mut *tx).await?;
    ///     Ok::<_, rok_db::Error>(())
    /// })).await?;
    /// ```
    pub async fn transaction_with<T, E, F>(
        &self,
        options: TxOptions,
        f: F,
    ) -> std::result::Result<T, E>
    where
        F: for<'t> Fn(&'t mut Tx) -> BoxFuture<'t, std::result::Result<T, E>>,
        E: From<Error> + Retryable,
    {
        let mut attempt = 0;
        loop {
            let result = self.try_transaction(&options, &f).await;
            match result {
                Err(err) if attempt < options.retries && err.is_retryable() => {
                    attempt += 1;
                    let delay = options.backoff(attempt);
                    tracing::debug!(target: "rok_db::query", attempt, delay_ms = delay.as_millis() as u64, "retrying transaction");
                    tokio::time::sleep(delay).await;
                }
                other => return other,
            }
        }
    }

    async fn try_transaction<T, E, F>(
        &self,
        options: &TxOptions,
        f: &F,
    ) -> std::result::Result<T, E>
    where
        F: for<'t> Fn(&'t mut Tx) -> BoxFuture<'t, std::result::Result<T, E>>,
        E: From<Error>,
    {
        let mut tx = self.begin().await?;
        if let Some(sql) = options.set_transaction_sql() {
            sqlx::query(&sql)
                .execute(&mut *tx.inner)
                .await
                .map_err(Error::from)?;
        }
        match f(&mut tx).await {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(err) => {
                // The transaction may already be aborted; a failed rollback
                // must not hide the original error.
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// Execute one or more raw SQL statements without parameters (e.g. a
    /// schema script) and return the total number of affected rows.
    pub async fn execute(&self, sql: &str) -> Result<u64> {
        Ok(sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    /// Check that the database is reachable.
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Run every pending migration found in the `dir` directory
    /// (sqlx migration format).
    #[cfg(feature = "migrate")]
    pub async fn migrate(&self, dir: impl AsRef<std::path::Path>) -> Result<()> {
        let migrator = sqlx::migrate::Migrator::new(dir.as_ref()).await?;
        migrator.run(&self.pool).await?;
        Ok(())
    }

    /// Close every connection in the pool and its replicas.
    pub async fn close(&self) {
        self.pool.close().await;
        for replica in &self.ctx.replicas {
            replica.close().await;
        }
    }
}

impl From<PgPool> for Db {
    fn from(pool: PgPool) -> Self {
        Self::from_pool(pool)
    }
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db")
            .field("size", &self.pool.size())
            .field("idle", &self.pool.num_idle())
            .field("cache", &self.ctx.cache)
            .finish()
    }
}

/// Connection pool statistics, from [`Db::stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Open connections (idle and in use).
    pub size: u32,
    /// Idle connections.
    pub idle: usize,
    /// Configured maximum.
    pub max_connections: u32,
}

/// Transaction isolation level, see [`TxOptions::isolation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// `READ COMMITTED` (PostgreSQL's default).
    ReadCommitted,
    /// `REPEATABLE READ`.
    RepeatableRead,
    /// `SERIALIZABLE`: transactions behave as if run one after another;
    /// conflicting ones fail with a serialization error (and are retried by
    /// [`Db::transaction_with`]).
    Serializable,
}

/// Settings for [`Db::transaction_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOptions {
    isolation: Option<Isolation>,
    read_only: bool,
    retries: u32,
    base_delay: Duration,
}

impl Default for TxOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl TxOptions {
    /// Default settings: the server's isolation level, read-write, no retries.
    pub fn new() -> Self {
        Self {
            isolation: None,
            read_only: false,
            retries: 0,
            base_delay: Duration::from_millis(10),
        }
    }

    /// Run with this isolation level.
    pub fn isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = Some(isolation);
        self
    }

    /// Run as `READ ONLY`.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Retry up to `retries` times on serialization failures and deadlocks.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Base delay of the exponential backoff between retries (default 10ms;
    /// attempt *n* waits `base * 2^(n-1)` plus up to 50% jitter, capped at 1s).
    pub fn backoff_base(mut self, delay: Duration) -> Self {
        self.base_delay = delay;
        self
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1 << attempt.saturating_sub(1).min(16))
            .min(Duration::from_secs(1));
        // Cheap jitter without a RNG dependency.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        exp + exp.mul_f64(f64::from(nanos % 500) / 1000.0)
    }

    fn set_transaction_sql(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(isolation) = self.isolation {
            parts.push(match isolation {
                Isolation::ReadCommitted => "ISOLATION LEVEL READ COMMITTED",
                Isolation::RepeatableRead => "ISOLATION LEVEL REPEATABLE READ",
                Isolation::Serializable => "ISOLATION LEVEL SERIALIZABLE",
            });
        }
        if self.read_only {
            parts.push("READ ONLY");
        }
        (!parts.is_empty()).then(|| format!("SET TRANSACTION {}", parts.join(", ")))
    }
}

/// Errors that can tell whether retrying the transaction might succeed.
/// Implemented for [`Error`]; implement it for your own error type (usually
/// by delegating to a wrapped `rok_db::Error`) to use it with
/// [`Db::transaction_with`].
pub trait Retryable {
    /// `true` for serialization failures and deadlocks.
    fn is_retryable(&self) -> bool;
}

impl Retryable for Error {
    fn is_retryable(&self) -> bool {
        self.is_serialization_failure()
    }
}

/// Builder for a [`Db`] connection pool, created with [`Db::builder`].
#[derive(Debug, Clone, Default)]
pub struct DbBuilder {
    options: PgPoolOptions,
    ctx: Context,
    replica_urls: Vec<String>,
    shared_invalidation: bool,
}

impl DbBuilder {
    /// Enable the [query cache](QueryCache) used by
    /// [`Select::memoize`](crate::Select::memoize), holding up to
    /// `capacity` results.
    pub fn query_cache(mut self, capacity: usize) -> Self {
        self.ctx.cache = Some(QueryCache::new(capacity));
        self
    }

    /// Use an existing [`QueryCache`], e.g. one shared by several pools.
    pub fn with_query_cache(mut self, cache: QueryCache) -> Self {
        self.ctx.cache = Some(cache);
        self
    }

    /// Add a read replica. Reads made through `&Db` by the query builder
    /// (`all`, `first`, `count`, `paginate`, relations, …) are spread across
    /// replicas round-robin; writes, row locks (`for_update`), transactions
    /// and raw SQL stay on the primary. A read that fails because a replica
    /// is unreachable is retried on the primary.
    ///
    /// Replicas lag behind the primary: use
    /// [`Select::on_primary`](crate::Select::on_primary) or
    /// [`Db::primary`] where a read must see the latest write.
    pub fn read_replica(mut self, url: impl Into<String>) -> Self {
        self.replica_urls.push(url.into());
        self
    }

    /// Add an existing pool as a read replica (see [`read_replica`](Self::read_replica)).
    pub fn replica_pool(mut self, pool: PgPool) -> Self {
        self.ctx.replicas.push(pool);
        self
    }

    /// Keep the query caches of every process using this database coherent:
    /// invalidations are broadcast with PostgreSQL `NOTIFY` and applied by
    /// all instances that enabled this (needs [`query_cache`](Self::query_cache)
    /// and a Tokio runtime). A background task holds one extra connection.
    ///
    /// Invalidation is asynchronous: other instances drop stale entries a few
    /// milliseconds after the writing transaction's statement. After a lost
    /// connection the local cache is cleared, since messages may have been
    /// missed.
    pub fn shared_cache_invalidation(mut self) -> Self {
        self.shared_invalidation = true;
        self
    }

    /// Log queries slower than `threshold` at `WARN` level on the
    /// `rok_db::slow_query` tracing target (default 1s, `Duration::ZERO`
    /// disables it).
    pub fn slow_query_threshold(mut self, threshold: Duration) -> Self {
        self.ctx.slow_query = threshold;
        self
    }

    fn build(self, pool: PgPool) -> Db {
        if self.shared_invalidation {
            match &self.ctx.cache {
                Some(cache) => crate::cache_sync::start(pool.clone(), cache),
                None => {
                    tracing::warn!(target: "rok_db::cache", "shared_cache_invalidation() without query_cache(); ignored")
                }
            }
        }
        Db {
            pool,
            ctx: Arc::new(self.ctx),
        }
    }

    /// Maximum number of pooled connections (default 10).
    pub fn max_connections(mut self, n: u32) -> Self {
        self.options = self.options.max_connections(n);
        self
    }

    /// Minimum number of idle connections kept open (default 0).
    pub fn min_connections(mut self, n: u32) -> Self {
        self.options = self.options.min_connections(n);
        self
    }

    /// How long to wait for a free connection (default 30s).
    pub fn acquire_timeout(mut self, timeout: Duration) -> Self {
        self.options = self.options.acquire_timeout(timeout);
        self
    }

    /// Close connections idle for longer than `timeout` (default 10min).
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.options = self.options.idle_timeout(timeout);
        self
    }

    /// Recycle connections older than `lifetime` (default 30min).
    pub fn max_lifetime(mut self, lifetime: Duration) -> Self {
        self.options = self.options.max_lifetime(lifetime);
        self
    }

    /// Connect to `url` and return the pool.
    pub async fn connect(self, url: &str) -> Result<Db> {
        let pool = self.options.clone().connect(url).await?;
        self.finish(pool).await
    }

    /// Connect the configured replica URLs, then build the handle.
    async fn finish(mut self, pool: PgPool) -> Result<Db> {
        for url in std::mem::take(&mut self.replica_urls) {
            let replica = self.options.clone().connect(&url).await?;
            self.ctx.replicas.push(replica);
        }
        Ok(self.build(pool))
    }

    /// Connect using the `DATABASE_URL` environment variable.
    pub async fn connect_env(self) -> Result<Db> {
        let url = std::env::var("DATABASE_URL").map_err(|_| {
            Error::Config("the `DATABASE_URL` environment variable is not set".into())
        })?;
        self.connect(&url).await
    }

    /// Wrap an existing sqlx pool, applying this builder's rok-db settings
    /// (pool options and replica URLs are ignored; use
    /// [`replica_pool`](Self::replica_pool) for replicas).
    pub fn build_with_pool(self, pool: PgPool) -> Db {
        self.build(pool)
    }

    /// Connect with fully custom connect options.
    pub async fn connect_with(self, options: PgConnectOptions) -> Result<Db> {
        let pool = self.options.clone().connect_with(options).await?;
        self.finish(pool).await
    }

    /// Create the pool without opening any connection until first use.
    pub fn connect_lazy(self, url: &str) -> Result<Db> {
        let pool = self.options.clone().connect_lazy(url)?;
        let mut builder = self;
        for url in std::mem::take(&mut builder.replica_urls) {
            let replica = builder.options.clone().connect_lazy(&url)?;
            builder.ctx.replicas.push(replica);
        }
        Ok(builder.build(pool))
    }
}

/// An open database transaction, created with [`Db::begin`].
///
/// Pass `&mut *tx` wherever an [`Executor`](crate::Executor) is expected.
/// A transaction that is dropped without [`commit`](Tx::commit) is rolled back.
pub struct Tx {
    pub(crate) inner: sqlx::Transaction<'static, Postgres>,
    ctx: Option<Arc<Context>>,
    touched: Mutex<Vec<&'static str>>,
}

impl Tx {
    /// Commit the transaction.
    pub async fn commit(self) -> Result<()> {
        self.inner.commit().await?;
        // Results cached by other connections while this transaction was
        // open still reflect the old data.
        if let Some(cache) = self.ctx.as_ref().and_then(|c| c.cache.as_ref()) {
            let touched = self.touched.lock().unwrap_or_else(|e| e.into_inner());
            for table in touched.iter() {
                cache.invalidate(table);
            }
        }
        Ok(())
    }

    /// Roll the transaction back.
    pub async fn rollback(self) -> Result<()> {
        Ok(self.inner.rollback().await?)
    }

    /// Record `actor` as the author of this transaction's changes in the
    /// audit log (`SET LOCAL rok.actor`, see [`audit`](crate::audit)).
    pub async fn set_actor(&mut self, actor: &str) -> Result<()> {
        sqlx::query("SELECT set_config('rok.actor', $1, true)")
            .bind(actor)
            .execute(&mut *self.inner)
            .await?;
        Ok(())
    }

    /// The underlying sqlx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, Postgres> {
        &mut self.inner
    }

    /// Record a write to `table` made outside the executor path (e.g. COPY).
    pub(crate) fn note_write(&self, table: &'static str) {
        let mut touched = self.touched.lock().unwrap_or_else(|e| e.into_inner());
        if !touched.contains(&table) {
            touched.push(table);
        }
        if let Some(cache) = self.ctx.as_ref().and_then(|c| c.cache.as_ref()) {
            cache.invalidate(table);
        }
    }
}

impl From<sqlx::Transaction<'static, Postgres>> for Tx {
    fn from(inner: sqlx::Transaction<'static, Postgres>) -> Self {
        Self {
            inner,
            ctx: None,
            touched: Mutex::default(),
        }
    }
}

impl Deref for Tx {
    type Target = PgConnection;

    fn deref(&self) -> &PgConnection {
        &self.inner
    }
}

impl DerefMut for Tx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.inner
    }
}

impl fmt::Debug for Tx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tx")
    }
}

impl<'c> crate::Executor<'c> for &'c Db {
    fn __context(&self) -> Option<Arc<Context>> {
        Some(self.ctx.clone())
    }

    fn __replica(&self) -> Option<PgPool> {
        self.ctx.replica()
    }
}

impl<'c> crate::Executor<'c> for &'c mut Tx {
    fn __context(&self) -> Option<Arc<Context>> {
        self.ctx.clone()
    }

    fn __touch(&self, table: &'static str) {
        let mut touched = self.touched.lock().unwrap_or_else(|e| e.into_inner());
        if !touched.contains(&table) {
            touched.push(table);
        }
    }
}

/// Implement sqlx's `Executor` by delegating to an inner executor.
macro_rules! delegate_executor {
    ($ty:ty, |$this:ident| $inner:expr) => {
        impl<'c> sqlx::Executor<'c> for $ty {
            type Database = Postgres;

            fn fetch_many<'e, 'q: 'e, E>(
                self,
                query: E,
            ) -> BoxStream<'e, std::result::Result<Either<PgQueryResult, PgRow>, sqlx::Error>>
            where
                'c: 'e,
                E: 'q + Execute<'q, Postgres>,
            {
                let $this = self;
                $inner.fetch_many(query)
            }

            fn fetch_optional<'e, 'q: 'e, E>(
                self,
                query: E,
            ) -> BoxFuture<'e, std::result::Result<Option<PgRow>, sqlx::Error>>
            where
                'c: 'e,
                E: 'q + Execute<'q, Postgres>,
            {
                let $this = self;
                $inner.fetch_optional(query)
            }

            fn prepare_with<'e, 'q: 'e>(
                self,
                sql: &'q str,
                parameters: &'e [PgTypeInfo],
            ) -> BoxFuture<'e, std::result::Result<PgStatement<'q>, sqlx::Error>>
            where
                'c: 'e,
            {
                let $this = self;
                $inner.prepare_with(sql, parameters)
            }

            fn describe<'e, 'q: 'e>(
                self,
                sql: &'q str,
            ) -> BoxFuture<'e, std::result::Result<Describe<Postgres>, sqlx::Error>>
            where
                'c: 'e,
            {
                let $this = self;
                $inner.describe(sql)
            }
        }
    };
}

delegate_executor!(&'c Db, |this| &this.pool);
delegate_executor!(&'c mut Tx, |this| &mut *this.inner);
