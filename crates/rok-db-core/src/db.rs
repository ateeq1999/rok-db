use std::fmt;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use sqlx::postgres::{
    PgConnectOptions, PgConnection, PgPool, PgPoolOptions, PgQueryResult, PgRow, PgStatement,
    PgTypeInfo,
};
use sqlx::{Describe, Either, Execute, Postgres};

use crate::{Error, Result};

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
        Self { pool }
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
        Ok(Tx(self.pool.begin().await?))
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
            .finish()
    }
}

/// Builder for a [`Db`] connection pool, created with [`Db::builder`].
#[derive(Debug, Clone, Default)]
pub struct DbBuilder {
    options: PgPoolOptions,
}

impl DbBuilder {
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
        Ok(Db::from_pool(self.options.connect(url).await?))
    }

    /// Connect using the `DATABASE_URL` environment variable.
    pub async fn connect_env(self) -> Result<Db> {
        let url = std::env::var("DATABASE_URL").map_err(|_| {
            Error::Config("the `DATABASE_URL` environment variable is not set".into())
        })?;
        self.connect(&url).await
    }

    /// Connect with fully custom connect options.
    pub async fn connect_with(self, options: PgConnectOptions) -> Result<Db> {
        Ok(Db::from_pool(self.options.connect_with(options).await?))
    }

    /// Create the pool without opening any connection until first use.
    pub fn connect_lazy(self, url: &str) -> Result<Db> {
        Ok(Db::from_pool(self.options.connect_lazy(url)?))
    }
}

/// An open database transaction, created with [`Db::begin`].
///
/// Pass `&mut *tx` wherever an [`Executor`](crate::Executor) is expected.
/// A transaction that is dropped without [`commit`](Tx::commit) is rolled back.
pub struct Tx(sqlx::Transaction<'static, Postgres>);

impl Tx {
    /// Commit the transaction.
    pub async fn commit(self) -> Result<()> {
        Ok(self.0.commit().await?)
    }

    /// Roll the transaction back.
    pub async fn rollback(self) -> Result<()> {
        Ok(self.0.rollback().await?)
    }

    /// The underlying sqlx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, Postgres> {
        &mut self.0
    }
}

impl From<sqlx::Transaction<'static, Postgres>> for Tx {
    fn from(tx: sqlx::Transaction<'static, Postgres>) -> Self {
        Self(tx)
    }
}

impl Deref for Tx {
    type Target = PgConnection;

    fn deref(&self) -> &PgConnection {
        &self.0
    }
}

impl DerefMut for Tx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.0
    }
}

impl fmt::Debug for Tx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tx")
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
delegate_executor!(&'c mut Tx, |this| &mut *this.0);
