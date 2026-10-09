//! Versioned migrations embedded in Rust code.
//!
//! The SQL code generator (`rok-db-gen`) writes one [`Migration`] per schema
//! change and an `up` / `down` pair that call [`apply`] and [`revert`]. They
//! can also be written by hand:
//!
//! ```ignore
//! use rok_db::migration::{self, Migration};
//!
//! const MIGRATIONS: &[Migration] = &[Migration {
//!     version: 1,
//!     name: "init",
//!     up: "CREATE TABLE users (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL)",
//!     down: "DROP TABLE users",
//! }];
//!
//! migration::apply(&db, MIGRATIONS).await?;     // runs pending migrations
//! migration::revert(&db, MIGRATIONS, 1).await?; // undoes the latest one
//! ```
//!
//! Applied migrations are recorded in the `_rok_db_migrations` table with a
//! checksum of their `up` SQL. Each migration runs in its own transaction,
//! which holds a PostgreSQL advisory lock so two processes never migrate at
//! once.
//! Running is refused when an applied migration was edited, when the
//! database has a migration the code doesn't know, or when a pending
//! migration is older than the latest applied one.

use futures_core::future::BoxFuture;

use crate::{Db, Error, Result, Tx};

/// One schema change: SQL to apply it and SQL to undo it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Ordering key, unique and increasing (`1`, `2`, ...).
    pub version: i64,
    /// Short description (`"init"`, `"add_post_slug"`).
    pub name: &'static str,
    /// SQL that applies the change; may contain several statements.
    pub up: &'static str,
    /// SQL that undoes the change; empty when it can't be undone.
    pub down: &'static str,
}

impl Migration {
    /// A stable checksum of the `up` SQL (64-bit FNV-1a, hex), stored when
    /// the migration is applied to detect later edits.
    pub fn checksum(&self) -> String {
        checksum(self.up)
    }
}

/// A row of the `_rok_db_migrations` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// The migration's version.
    pub version: i64,
    /// The migration's name.
    pub name: String,
    /// Checksum of the `up` SQL that was applied.
    pub checksum: String,
}

const TABLE_SQL: &str = r#"CREATE TABLE IF NOT EXISTS "_rok_db_migrations" (
    "version" BIGINT PRIMARY KEY,
    "name" TEXT NOT NULL,
    "checksum" TEXT NOT NULL,
    "applied_at" TIMESTAMPTZ NOT NULL DEFAULT now()
)"#;

/// Advisory lock key: the bytes of "rok_mig".
const LOCK_KEY: i64 = 0x0072_6f6b_5f6d_6967;

/// Apply every pending migration in version order, each in its own
/// transaction. Returns the versions applied (empty when up to date).
pub async fn apply(db: &Db, migrations: &[Migration]) -> Result<Vec<i64>> {
    check_order(migrations)?;
    let mut done = Vec::new();
    loop {
        // One transaction per migration, holding the migration lock, so a
        // concurrent run waits and then sees what this one applied.
        let mut tx = locked_tx(db).await?;
        let applied = read_applied(&mut tx).await?;
        verify(migrations, &applied)?;
        let Some(migration) = migrations
            .iter()
            .find(|m| !applied.iter().any(|a| a.version == m.version))
        else {
            tx.commit().await?;
            break;
        };
        if let Some(latest) = applied
            .iter()
            .map(|a| a.version)
            .max()
            .filter(|latest| migration.version < *latest)
        {
            return Err(Error::Config(format!(
                "migration {} `{}` is older than the latest applied migration {latest}; \
                 give it a higher version",
                migration.version, migration.name
            )));
        }
        run_script(&mut tx, migration.up)
            .await
            .map_err(|e| failed(migration, "applying", &e))?;
        crate::raw(
            r#"INSERT INTO "_rok_db_migrations" ("version", "name", "checksum") VALUES (?, ?, ?)"#,
        )
        .bind(migration.version)
        .bind(migration.name)
        .bind(migration.checksum())
        .execute(&mut tx)
        .await?;
        tx.commit().await?;
        done.push(migration.version);
    }
    clear_cache(db, &done);
    Ok(done)
}

/// Undo the latest `steps` applied migrations, newest first. Returns the
/// versions reverted.
pub async fn revert(db: &Db, migrations: &[Migration], steps: usize) -> Result<Vec<i64>> {
    check_order(migrations)?;
    let mut done = Vec::new();
    while done.len() < steps {
        let mut tx = locked_tx(db).await?;
        let applied = read_applied(&mut tx).await?;
        verify(migrations, &applied)?;
        let Some(version) = applied.iter().map(|a| a.version).max() else {
            tx.commit().await?;
            break;
        };
        let Some(migration) = migrations.iter().find(|m| m.version == version) else {
            break; // unreachable: `verify` checked every applied version
        };
        if migration.down.trim().is_empty() {
            return Err(Error::Config(format!(
                "migration {} `{}` can't be reverted (it has no down SQL)",
                migration.version, migration.name
            )));
        }
        run_script(&mut tx, migration.down)
            .await
            .map_err(|e| failed(migration, "reverting", &e))?;
        crate::raw(r#"DELETE FROM "_rok_db_migrations" WHERE "version" = ?"#)
            .bind(version)
            .execute(&mut tx)
            .await?;
        tx.commit().await?;
        done.push(version);
    }
    clear_cache(db, &done);
    Ok(done)
}

/// A transaction holding the migration advisory lock until it ends.
async fn locked_tx(db: &Db) -> Result<Tx> {
    let mut tx = db.begin().await?;
    crate::raw("SELECT pg_advisory_xact_lock(?)")
        .bind(LOCK_KEY)
        .execute(&mut tx)
        .await?;
    Ok(tx)
}

fn clear_cache(db: &Db, done: &[i64]) {
    if let (false, Some(cache)) = (done.is_empty(), db.cache()) {
        // Cached results may describe the old schema.
        cache.clear();
    }
}

/// The migrations recorded as applied, oldest first.
pub async fn applied(db: &Db) -> Result<Vec<AppliedMigration>> {
    let mut tx = db.begin().await?;
    let applied = read_applied(&mut tx).await?;
    tx.commit().await?;
    Ok(applied)
}

fn check_order(migrations: &[Migration]) -> Result<()> {
    for pair in migrations.windows(2) {
        if pair[0].version >= pair[1].version {
            return Err(Error::Config(format!(
                "migration versions must increase: {} `{}` is followed by {} `{}`",
                pair[0].version, pair[0].name, pair[1].version, pair[1].name
            )));
        }
    }
    Ok(())
}

fn verify(migrations: &[Migration], applied: &[AppliedMigration]) -> Result<()> {
    for row in applied {
        match migrations.iter().find(|m| m.version == row.version) {
            None => {
                return Err(Error::Config(format!(
                    "the database has migration {} `{}`, which this code doesn't know; \
                     is the code older than the database?",
                    row.version, row.name
                )));
            }
            Some(m) if m.checksum() != row.checksum => {
                return Err(Error::Config(format!(
                    "migration {} `{}` was edited after it was applied; \
                     add a new migration instead",
                    m.version, m.name
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

async fn read_applied(tx: &mut Tx) -> Result<Vec<AppliedMigration>> {
    run_script(tx, TABLE_SQL).await?;
    let rows: Vec<(i64, String, String)> = crate::raw(
        r#"SELECT "version", "name", "checksum" FROM "_rok_db_migrations" ORDER BY "version""#,
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(version, name, checksum)| AppliedMigration {
            version,
            name,
            checksum,
        })
        .collect())
}

/// Run a script of one or more statements without parameters. Boxed at a
/// concrete lifetime so the calling futures stay `Send`.
fn run_script<'a>(
    tx: &'a mut Tx,
    sql: &'a str,
) -> BoxFuture<'a, std::result::Result<(), sqlx::Error>> {
    // A plain `&str` runs over the simple query protocol, which allows
    // several statements (this is what sqlx's own migrator does).
    Box::pin(async move {
        sqlx::Executor::execute(&mut *tx.inner, sql)
            .await
            .map(|_| ())
    })
}

fn failed(migration: &Migration, action: &str, error: &sqlx::Error) -> Error {
    Error::Config(format!(
        "{action} migration {} `{}` failed: {error}",
        migration.version, migration.name
    ))
}

fn checksum(sql: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in sql.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn m(version: i64, up: &'static str) -> Migration {
        Migration {
            version,
            name: "m",
            up,
            down: "",
        }
    }

    #[test]
    fn checksums_are_stable_and_sensitive() {
        assert_eq!(checksum(""), "cbf29ce484222325");
        assert_eq!(m(1, "a").checksum(), m(2, "a").checksum());
        assert_ne!(m(1, "a").checksum(), m(1, "b").checksum());
    }

    #[test]
    fn versions_must_increase() {
        assert!(check_order(&[m(1, ""), m(2, "")]).is_ok());
        assert!(check_order(&[m(2, ""), m(1, "")]).is_err());
        assert!(check_order(&[m(1, ""), m(1, "")]).is_err());
    }

    #[test]
    fn edited_and_unknown_migrations_are_rejected() {
        let row = |version: i64, up: &str| AppliedMigration {
            version,
            name: "m".into(),
            checksum: checksum(up),
        };
        let code = [m(1, "a"), m(2, "b")];
        assert!(verify(&code, &[row(1, "a")]).is_ok());
        assert!(verify(&code, &[row(1, "edited")]).is_err());
        assert!(verify(&code, &[row(3, "c")]).is_err());
    }
}
