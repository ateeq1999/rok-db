use std::future::Future;

use sqlx::FromRow;
use sqlx::postgres::PgRow;

use crate::exec;
use crate::query::{Insert, Select, Update};
use crate::sql::Sql;
use crate::{Column, Error, Executor, Expr, Order, Result, Value};

/// A database-backed record.
///
/// Implement it with `#[derive(Model)]` rather than by hand: the derive
/// generates the metadata below, a [`FromRow`] implementation and one typed
/// [`Column`] constant per field.
///
/// All query and persistence methods take any [`Executor`]: a [`Db`](crate::Db),
/// a `&mut Tx` transaction, a raw `PgPool` or a `&mut PgConnection`.
///
/// ```ignore
/// let user = User::find_or_fail(&db, 1).await?;
/// let admins = User::filter(User::ROLE.eq("admin"))
///     .order_by(User::NAME.asc())
///     .all(&db)
///     .await?;
/// ```
pub trait Model: for<'r> FromRow<'r, PgRow> + Send + Sync + Unpin + Sized + 'static {
    /// Table name, optionally schema-qualified (`"auth.users"`).
    const TABLE: &'static str;
    /// Primary key column.
    const PRIMARY_KEY: &'static str;
    /// Every column read from and written to the database, in field order.
    const COLUMNS: &'static [&'static str];
    /// Columns filled in by the database (serial ids, defaults, triggers).
    /// They are read but never written by [`insert`](Model::insert) or [`save`](Model::save).
    const GENERATED: &'static [&'static str] = &[];
    /// Column set to `now()` on insert and never changed afterwards.
    const CREATED_AT_COLUMN: Option<&'static str> = None;
    /// Column set to `now()` on insert and on every update.
    const UPDATED_AT_COLUMN: Option<&'static str> = None;

    /// The value of this record's primary key.
    fn primary_key(&self) -> Value;

    /// Every column with its current value, in [`COLUMNS`](Model::COLUMNS) order.
    fn values(&self) -> Vec<(&'static str, Value)>;

    /// The current value of a single column, or `None` if `column` isn't one
    /// of [`COLUMNS`](Model::COLUMNS). The derive generates an efficient
    /// implementation.
    fn value_of(&self, column: &str) -> Option<Value> {
        self.values()
            .into_iter()
            .find(|(c, _)| *c == column)
            .map(|(_, v)| v)
    }

    // ----- query entry points ---------------------------------------------

    /// Start a `SELECT` over every row of the table.
    fn query() -> Select<Self> {
        Select::new()
    }

    /// Start a `SELECT` filtered by `expr`.
    fn filter(expr: Expr<Self>) -> Select<Self> {
        Select::new().filter(expr)
    }

    /// Start a `SELECT` ordered by `order`.
    fn order_by(order: impl Into<Order<Self>>) -> Select<Self> {
        Select::new().order_by(order)
    }

    /// Start an `INSERT` built column by column.
    fn create() -> Insert<Self> {
        Insert::new()
    }

    /// Start a bulk `UPDATE`; narrow it with [`Update::filter`].
    fn update_all() -> Update<Self> {
        Update::new()
    }

    /// The primary key as a typed column.
    fn primary_key_column() -> Column<Self> {
        Column::new(Self::PRIMARY_KEY)
    }

    // ----- reads ----------------------------------------------------------

    /// Fetch a record by primary key, or `None`.
    fn find<'e, E>(
        executor: E,
        id: impl Into<Value>,
    ) -> impl Future<Output = Result<Option<Self>>> + Send
    where
        E: Executor<'e>,
    {
        let select = Self::filter(Self::primary_key_column().eq(id));
        async move { select.first(executor).await }
    }

    /// Fetch a record by primary key, failing with [`Error::NotFound`].
    fn find_or_fail<'e, E>(
        executor: E,
        id: impl Into<Value>,
    ) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let id = id.into();
        let key = id.to_string();
        let select = Self::filter(Self::primary_key_column().eq(id));
        async move {
            select
                .first(executor)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(Some(key)))
        }
    }

    /// Fetch every record whose primary key is in `ids`.
    fn find_many<'e, E, V>(
        executor: E,
        ids: impl IntoIterator<Item = V>,
    ) -> impl Future<Output = Result<Vec<Self>>> + Send
    where
        E: Executor<'e>,
        V: Into<Value>,
    {
        let select = Self::filter(Self::primary_key_column().is_in(ids));
        async move { select.all(executor).await }
    }

    /// Fetch every record in the table.
    fn all<'e, E>(executor: E) -> impl Future<Output = Result<Vec<Self>>> + Send
    where
        E: Executor<'e>,
    {
        Self::query().all(executor)
    }

    /// Count every record in the table.
    fn count<'e, E>(executor: E) -> impl Future<Output = Result<i64>> + Send
    where
        E: Executor<'e>,
    {
        Self::query().count(executor)
    }

    /// Re-read this record from the database.
    fn reload<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        Self::find_or_fail(executor, self.primary_key())
    }

    // ----- writes ---------------------------------------------------------

    /// `INSERT` this record (skipping generated columns) and return the stored row.
    fn insert<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let sql = insert_sql::<Self>(std::slice::from_ref(self), false);
        async move {
            exec::fetch_optional::<Self, _>(executor, &sql, &[Self::TABLE])
                .await?
                .ok_or_else(|| Error::not_found::<Self>(None))
        }
    }

    /// `INSERT` many records in a single statement and return the stored rows.
    fn insert_all<'e, E>(
        executor: E,
        records: &[Self],
    ) -> impl Future<Output = Result<Vec<Self>>> + Send
    where
        E: Executor<'e>,
    {
        let sql = (!records.is_empty()).then(|| insert_sql::<Self>(records, false));
        async move {
            match sql {
                Some(sql) => exec::fetch_all(executor, &sql, &[Self::TABLE]).await,
                None => Ok(Vec::new()),
            }
        }
    }

    /// `INSERT` this record, or update every column if its primary key
    /// already exists (`ON CONFLICT (pk) DO UPDATE`). The primary key is
    /// always written, even when it is a generated column.
    fn upsert<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let sql = insert_sql::<Self>(std::slice::from_ref(self), true);
        async move {
            exec::fetch_optional::<Self, _>(executor, &sql, &[Self::TABLE])
                .await?
                .ok_or_else(|| Error::not_found::<Self>(None))
        }
    }

    /// `UPDATE` every non-generated column of this record by primary key and
    /// return the stored row. Fails with [`Error::NotFound`] if the row is gone.
    fn save<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let pk = self.primary_key();
        let key = pk.to_string();
        let sets: Vec<_> = writable::<Self>(self.values())
            .filter(|(c, _)| *c != Self::PRIMARY_KEY && !is_timestamp::<Self>(c))
            .collect();
        let mut update = Self::update_all().filter(Self::primary_key_column().eq(pk));
        for (column, value) in sets {
            update = update.set(Column::new(column), value);
        }
        async move {
            let row = if update.has_sets() || Self::UPDATED_AT_COLUMN.is_some() {
                update.returning_one(executor).await?
            } else {
                update.into_select().first(executor).await?
            };
            row.ok_or_else(|| Error::not_found::<Self>(Some(key)))
        }
    }

    /// `DELETE` this record by primary key. Fails with [`Error::NotFound`]
    /// if no row was deleted.
    fn delete<'e, E>(&self, executor: E) -> impl Future<Output = Result<()>> + Send
    where
        E: Executor<'e>,
    {
        let pk = self.primary_key();
        let key = pk.to_string();
        let select = Self::filter(Self::primary_key_column().eq(pk));
        async move {
            match select.delete(executor).await? {
                0 => Err(Error::not_found::<Self>(Some(key))),
                _ => Ok(()),
            }
        }
    }
}

fn writable<M: Model>(
    values: Vec<(&'static str, Value)>,
) -> impl Iterator<Item = (&'static str, Value)> {
    values
        .into_iter()
        .filter(|(c, _)| !M::GENERATED.contains(c))
}

fn is_timestamp<M: Model>(column: &str) -> bool {
    M::CREATED_AT_COLUMN == Some(column) || M::UPDATED_AT_COLUMN == Some(column)
}

/// Build `INSERT … VALUES (…), (…) [ON CONFLICT …] RETURNING …`.
fn insert_sql<M: Model>(records: &[M], upsert: bool) -> Sql {
    let include = |c: &str| !M::GENERATED.contains(&c) || (upsert && c == M::PRIMARY_KEY);
    let columns: Vec<&'static str> = M::COLUMNS.iter().copied().filter(|c| include(c)).collect();

    let mut sql = Sql::new();
    sql.push("INSERT INTO ").push_ident(M::TABLE);
    if columns.is_empty() {
        // Every column is generated: rely on defaults (single row only).
        sql.push(" DEFAULT VALUES");
    } else {
        sql.push(" (")
            .push_list(&columns, ", ", |sql, c| {
                sql.push_ident(c);
            })
            .push(") VALUES ");
        sql.push_list(records, ", ", |sql, record| {
            let values = record.values().into_iter().filter(|(c, _)| include(c));
            sql.push("(")
                .push_list(values, ", ", |sql, (c, v)| {
                    if is_timestamp::<M>(c) {
                        sql.push("now()");
                    } else {
                        sql.bind(v);
                    }
                })
                .push(")");
        });
    }
    if upsert {
        sql.push(" ON CONFLICT (")
            .push_ident(M::PRIMARY_KEY)
            .push(")");
        let updates: Vec<_> = columns
            .iter()
            .filter(|c| **c != M::PRIMARY_KEY && M::CREATED_AT_COLUMN != Some(**c))
            .collect();
        if updates.is_empty() {
            // Still return the existing row.
            sql.push(" DO UPDATE SET ")
                .push_ident(M::PRIMARY_KEY)
                .push(" = EXCLUDED.")
                .push_ident(M::PRIMARY_KEY);
        } else {
            sql.push(" DO UPDATE SET ")
                .push_list(updates, ", ", |sql, c| {
                    sql.push_ident(c).push(" = EXCLUDED.").push_ident(c);
                });
        }
    }
    push_returning::<M>(&mut sql);
    sql
}

pub(crate) fn push_columns<M: Model>(sql: &mut Sql) {
    sql.push_list(M::COLUMNS, ", ", |sql, c| {
        sql.push_ident(c);
    });
}

pub(crate) fn push_returning<M: Model>(sql: &mut Sql) {
    sql.push(" RETURNING ");
    push_columns::<M>(sql);
}

#[doc(hidden)]
pub fn __insert_sql<M: Model>(records: &[M], upsert: bool) -> Sql {
    insert_sql(records, upsert)
}
