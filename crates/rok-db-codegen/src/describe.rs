//! Parameter and column types of queries, as PostgreSQL reports them.
//!
//! Queries are prepared in a temporary database that has every migration
//! applied. Results are cached in `queries.json` (committed next to the
//! generated code), so later runs and CI need no database until a query or
//! the schema changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::{Column as _, Connection, Executor as _, PgConnection, TypeInfo as _};

use crate::Error;
use crate::ir::Query;

/// What PostgreSQL says about one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Description {
    /// Parameter type names (`INT8`, `TEXT[]`, `user_role`).
    pub(crate) params: Vec<String>,
    /// Result columns.
    pub(crate) columns: Vec<DescribedColumn>,
}

/// A result column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DescribedColumn {
    pub(crate) name: String,
    pub(crate) type_name: String,
    /// `Some(false)` when PostgreSQL knows it is never NULL.
    pub(crate) nullable: Option<bool>,
}

/// The cache file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Cache {
    /// Keyed by a hash of the schema and the query text.
    pub(crate) queries: BTreeMap<String, CachedQuery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedQuery {
    /// Where the query is, to make the file readable.
    pub(crate) query: String,
    #[serde(flatten)]
    pub(crate) description: Description,
}

pub(crate) fn key(migrations_sql: &str, query_sql: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in migrations_sql.bytes().chain([0]).chain(query_sql.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Describe `queries`, using `cache` where possible. Returns the
/// descriptions and the new cache (only entries still in use).
pub(crate) fn describe_all(
    queries: &[Query],
    migrations_sql: &[String],
    cache: &Cache,
    database_url: Option<&str>,
) -> Result<(Vec<Description>, Cache), Error> {
    let schema_text = migrations_sql.join("\n");
    let keys: Vec<String> = queries.iter().map(|q| key(&schema_text, &q.sql)).collect();
    let missing: Vec<usize> = (0..queries.len())
        .filter(|&i| !cache.queries.contains_key(&keys[i]))
        .collect();
    let mut fresh: BTreeMap<usize, Description> = BTreeMap::new();
    if !missing.is_empty() {
        let Some(url) = database_url else {
            let names: Vec<String> = missing
                .iter()
                .map(|&i| format!("`{}` ({})", queries[i].name, queries[i].location))
                .collect();
            return Err(Error::new(format!(
                "these queries are new or the schema changed, so PostgreSQL has to check them: {}.\n\
                 Set DATABASE_URL to a server where the user may create databases; rok-db-gen \
                 creates a temporary database, applies the migrations, prepares the queries and \
                 drops it. The results are saved in queries.json for later runs.",
                names.join(", ")
            )));
        };
        let to_check: Vec<&Query> = missing.iter().map(|&i| &queries[i]).collect();
        // A thread of its own, so this also works when called from async code.
        let described = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| Error::new(format!("can't start an async runtime: {e}")))?
                        .block_on(describe_in_temp_db(url, migrations_sql, &to_check))
                })
                .join()
                .unwrap_or_else(|_| Err(Error::new("checking the queries panicked")))
        })?;
        fresh = missing.into_iter().zip(described).collect();
    }
    let mut out = Cache::default();
    let mut descriptions = Vec::new();
    for (i, query) in queries.iter().enumerate() {
        let description = match fresh.remove(&i) {
            Some(d) => d,
            None => cache.queries[&keys[i]].description.clone(),
        };
        out.queries.insert(
            keys[i].clone(),
            CachedQuery {
                query: format!("{}::{}", query.module, query.name),
                description: description.clone(),
            },
        );
        descriptions.push(description);
    }
    Ok((descriptions, out))
}

async fn describe_in_temp_db(
    url: &str,
    migrations_sql: &[String],
    queries: &[&Query],
) -> Result<Vec<Description>, Error> {
    use std::str::FromStr;
    let db_error = |what: &str, e: sqlx::Error| Error::new(format!("{what}: {e}"));
    let admin_options = sqlx::postgres::PgConnectOptions::from_str(url)
        .map_err(|e| db_error("DATABASE_URL is not a valid PostgreSQL URL", e))?;
    let mut admin = PgConnection::connect_with(&admin_options)
        .await
        .map_err(|e| db_error("can't connect to DATABASE_URL", e))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let name = format!("rok_gen_{}_{nanos}", std::process::id());
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .map_err(|e| {
            db_error(
                "can't create a temporary database (the user needs CREATEDB)",
                e,
            )
        })?;
    let result = async {
        let mut conn = PgConnection::connect_with(&admin_options.clone().database(&name))
            .await
            .map_err(|e| db_error("can't connect to the temporary database", e))?;
        for (i, sql) in migrations_sql.iter().enumerate() {
            conn.execute(sql.as_str()).await.map_err(|e| {
                db_error(
                    &format!("migration {} failed in the temporary database", i + 1),
                    e,
                )
            })?;
        }
        let mut out = Vec::new();
        for query in queries {
            let described = conn.describe(query.sql.as_str()).await.map_err(|e| {
                Error::at(
                    &query.location,
                    format!("PostgreSQL rejected `{}`: {e}", query.name),
                )
            })?;
            let params = match described.parameters() {
                Some(sqlx::Either::Left(types)) => {
                    types.iter().map(|t| t.name().to_owned()).collect()
                }
                Some(sqlx::Either::Right(0)) | None => Vec::new(),
                Some(sqlx::Either::Right(_)) => {
                    return Err(Error::at(
                        &query.location,
                        "PostgreSQL didn't report the parameter types",
                    ));
                }
            };
            let columns = described
                .columns()
                .iter()
                .enumerate()
                .map(|(i, c)| DescribedColumn {
                    name: c.name().to_owned(),
                    type_name: c.type_info().name().to_owned(),
                    nullable: described.nullable(i),
                })
                .collect();
            out.push(Description { params, columns });
        }
        conn.close().await.ok();
        Ok(out)
    }
    .await;
    // Drop the temporary database even when something failed.
    let dropped = admin
        .execute(format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str())
        .await;
    let out = result?;
    dropped.map_err(|e| db_error("can't drop the temporary database", e))?;
    Ok(out)
}
