//! Test support: a fresh, isolated database per test (feature `testing`).
//!
//! Most tests use the [`#[rok_db::test]`](https://docs.rs/rok-db/latest/rok_db/attr.test.html)
//! attribute, which builds on [`TestDb`].

use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection};

use crate::{Db, Result};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temporary database created on the server named by `DATABASE_URL`
/// and dropped (with `DROP DATABASE … WITH (FORCE)`, PostgreSQL 13+) when
/// this value is dropped — also when the test panics.
///
/// The `DATABASE_URL` user needs the `CREATEDB` privilege.
pub struct TestDb {
    db: Option<Db>,
    name: String,
    admin: PgConnectOptions,
}

impl TestDb {
    /// Create a database, or `None` when `DATABASE_URL` is not set.
    pub async fn create() -> Result<Option<Self>> {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return Ok(None);
        };
        let admin = PgConnectOptions::from_str(&url)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let name = format!(
            "rok_test_{}_{}_{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut conn = PgConnection::connect_with(&admin).await?;
        sqlx::query(&format!(r#"CREATE DATABASE "{name}""#))
            .execute(&mut conn)
            .await?;
        conn.close().await?;
        let db = Db::builder()
            .max_connections(5)
            .connect_with(admin.clone().database(&name))
            .await?;
        Ok(Some(Self {
            db: Some(db),
            name,
            admin,
        }))
    }

    /// The connection pool of the test database.
    pub fn db(&self) -> &Db {
        self.db.as_ref().expect("test database is alive")
    }

    /// The generated database name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run the migrations in `dir` (requires the `migrate` feature).
    #[cfg(feature = "migrate")]
    pub async fn migrate(&self, dir: impl AsRef<std::path::Path>) -> Result<()> {
        self.db().migrate(dir).await
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Dropping the pool without awaiting `close` avoids depending on the
        // test's runtime; `WITH (FORCE)` terminates any leftover connections.
        drop(self.db.take());
        let name = std::mem::take(&mut self.name);
        let admin = self.admin.clone();
        let cleanup = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let mut conn = PgConnection::connect_with(&admin).await?;
                sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#))
                    .execute(&mut conn)
                    .await?;
                conn.close().await?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            })
        });
        match cleanup.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("rok-db: failed to drop test database: {e}"),
            Err(_) => eprintln!("rok-db: test database cleanup panicked"),
        }
    }
}

impl std::fmt::Debug for TestDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestDb").field("name", &self.name).finish()
    }
}
