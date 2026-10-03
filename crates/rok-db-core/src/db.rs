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
        Ok(Tx {
            inner: self.pool.begin().await?,
            ctx: Some(self.ctx.clone()),
            touched: Mutex::default(),
        })
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

    /// Close every connection in the pool.
    pub async fn close(&self) {
        self.pool.close().await;
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

/// Builder for a [`Db`] connection pool, created with [`Db::builder`].
#[derive(Debug, Clone, Default)]
pub struct DbBuilder {
    options: PgPoolOptions,
    ctx: Context,
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

    /// Log queries slower than `threshold` at `WARN` level on the
    /// `rok_db::slow_query` tracing target (default 1s, `Duration::ZERO`
    /// disables it).
    pub fn slow_query_threshold(mut self, threshold: Duration) -> Self {
        self.ctx.slow_query = threshold;
        self
    }

    fn build(self, pool: PgPool) -> Db {
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
    /// (pool options are ignored).
    pub fn build_with_pool(self, pool: PgPool) -> Db {
        self.build(pool)
    }

    /// Connect with fully custom connect options.
    pub async fn connect_with(self, options: PgConnectOptions) -> Result<Db> {
        let pool = self.options.clone().connect_with(options).await?;
        Ok(self.build(pool))
    }

    /// Create the pool without opening any connection until first use.
    pub fn connect_lazy(self, url: &str) -> Result<Db> {
        let pool = self.options.clone().connect_lazy(url)?;
        Ok(self.build(pool))
    }
}

/// An open database transaction, created with [`Db::begin`].
///
/// Pass `&mut *tx` wherever an [`Executor`](crate::Executor) is expected.
/// A transaction that is dropped without [`commit`](Tx::commit) is rolled back.
pub struct Tx {
    inner: sqlx::Transaction<'static, Postgres>,
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

    /// The underlying sqlx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, Postgres> {
        &mut self.inner
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
