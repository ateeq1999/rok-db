use sqlx::postgres::PgRow;
use sqlx::{Decode, FromRow, Postgres, Type};

use crate::sql::Sql;
use crate::{Executor, Result, Value};

/// Start a raw SQL query — the escape hatch for anything the query builder
/// doesn't express.
///
/// Placeholders are either `?` (numbered for you, in bind order) or
/// PostgreSQL's native `$1, $2, …` — use one style per query. Use `$n`
/// when your SQL contains the JSONB `?` operator.
///
/// ```ignore
/// let rows: Vec<User> = rok_db::raw("SELECT * FROM users WHERE age > ? AND name ILIKE ?")
///     .bind(18)
///     .bind("a%")
///     .fetch_all(&db)
///     .await?;
/// ```
pub fn raw(sql: impl Into<String>) -> Raw {
    Raw {
        sql: sql.into(),
        params: Vec::new(),
    }
}

/// A raw SQL query with bound parameters, created with [`raw`].
#[derive(Debug, Clone)]
pub struct Raw {
    sql: String,
    params: Vec<Value>,
}

impl Raw {
    /// Bind the next parameter.
    pub fn bind(mut self, value: impl Into<Value>) -> Self {
        self.params.push(value.into());
        self
    }

    /// Render the statement with numbered placeholders.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push_raw_params(&self.sql, &self.params);
        sql
    }

    /// Fetch every row, decoded into `T` (any `FromRow` type, including models).
    pub async fn fetch_all<'e, T, E>(self, executor: E) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        let sql = self.to_sql();
        let args = sql.arguments()?;
        Ok(sqlx::query_as_with(sql.as_str(), args)
            .fetch_all(executor)
            .await?)
    }

    /// Fetch at most one row.
    pub async fn fetch_optional<'e, T, E>(self, executor: E) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        let sql = self.to_sql();
        let args = sql.arguments()?;
        Ok(sqlx::query_as_with(sql.as_str(), args)
            .fetch_optional(executor)
            .await?)
    }

    /// Fetch exactly one row.
    pub async fn fetch_one<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        let sql = self.to_sql();
        let args = sql.arguments()?;
        Ok(sqlx::query_as_with(sql.as_str(), args)
            .fetch_one(executor)
            .await?)
    }

    /// Fetch the first column of the first row, e.g. `SELECT COUNT(*) …`.
    pub async fn scalar<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        let sql = self.to_sql();
        let args = sql.arguments()?;
        Ok(sqlx::query_scalar_with(sql.as_str(), args)
            .fetch_one(executor)
            .await?)
    }

    /// Execute the statement and return the number of affected rows.
    pub async fn execute<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        let sql = self.to_sql();
        let args = sql.arguments()?;
        Ok(sqlx::query_with(sql.as_str(), args)
            .execute(executor)
            .await?
            .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_styles() {
        let q = raw("SELECT * FROM t WHERE a = ? AND b = ?")
            .bind(1)
            .bind("x");
        assert_eq!(
            q.to_sql().as_str(),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
        let q = raw("SELECT * FROM t WHERE a = $1").bind(1);
        assert_eq!(q.to_sql().as_str(), "SELECT * FROM t WHERE a = $1");
        assert_eq!(q.to_sql().params().len(), 1);
    }
}
