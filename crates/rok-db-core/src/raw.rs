use sqlx::postgres::PgRow;
use sqlx::{Decode, FromRow, Postgres, Type};

use futures_core::stream::BoxStream;

use crate::exec;
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
        writes: Vec::new(),
        replica: false,
    }
}

/// A raw SQL query with bound parameters, created with [`raw`].
#[derive(Debug, Clone)]
pub struct Raw {
    sql: String,
    params: Vec<Value>,
    writes: Vec<&'static str>,
    replica: bool,
}

impl Raw {
    /// Bind the next parameter.
    pub fn bind(mut self, value: impl Into<Value>) -> Self {
        self.params.push(value.into());
        self
    }

    /// Allow this read-only statement to run on a read replica. Raw SQL
    /// runs on the primary by default, since rok-db can't tell whether it
    /// writes.
    pub fn on_replica(mut self) -> Self {
        self.replica = true;
        self
    }

    /// Declare that this statement modifies `table`, so cached results of
    /// that table are invalidated when it succeeds (see
    /// [`QueryCache`](crate::QueryCache)).
    pub fn invalidates(mut self, table: &'static str) -> Self {
        self.writes.push(table);
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
        exec::fetch_all(executor, &self.to_sql(), &self.writes, self.replica).await
    }

    /// Fetch at most one row.
    pub async fn fetch_optional<'e, T, E>(self, executor: E) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_optional(executor, &self.to_sql(), &self.writes, self.replica).await
    }

    /// Fetch exactly one row.
    pub async fn fetch_one<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_optional(executor, &self.to_sql(), &self.writes, self.replica)
            .await?
            .ok_or_else(|| sqlx::Error::RowNotFound.into())
    }

    /// Fetch the first column of the first row, e.g. `SELECT COUNT(*) …`.
    pub async fn scalar<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(executor, &self.to_sql(), self.replica).await
    }

    /// Stream rows one at a time.
    pub fn stream<'e, T, E>(self, executor: E) -> BoxStream<'e, Result<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin + 'e,
        E: Executor<'e> + 'e,
    {
        exec::stream(executor, self.to_sql(), self.replica)
    }

    /// Execute the statement and return the number of affected rows.
    pub async fn execute<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        exec::execute(executor, &self.to_sql(), &self.writes).await
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
