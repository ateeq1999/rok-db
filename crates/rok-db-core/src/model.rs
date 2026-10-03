use std::future::Future;

use sqlx::FromRow;
use sqlx::postgres::PgRow;

use sqlx::Row;

use crate::exec;
use crate::query::{Conflict, ConflictAction, ConflictTarget, Insert, InsertMany, Select, Update};
use crate::sql::Sql;
use crate::validate::ValidationErrors;
use crate::{Column, Error, Executor, Expr, Order, Result, Value};

/// Lifecycle hooks, called around the record-level writes of [`Model`]
/// (`insert`, `insert_all`/`insert_many`, `upsert`, `save`, `delete`,
/// `force_delete`). Bulk query operations (`Select::update`,
/// `Select::delete`, …) don't call hooks.
///
/// `#[derive(Model)]` implements this trait with no-op hooks; add
/// `#[rok(hooks)]` to implement it yourself:
///
/// ```ignore
/// #[derive(Model)]
/// #[rok(hooks)]
/// struct User { id: i64, email: String }
///
/// impl Hooks for User {
///     fn before_insert(&self) -> rok_db::Result<()> {
///         if self.email.ends_with("@blocked.example") {
///             return Err(rok_db::Error::hook("this domain is blocked"));
///         }
///         Ok(())
///     }
/// }
/// ```
///
/// A `before_*` error aborts the operation before anything is written.
/// An `after_*` error is returned to the caller *after* the write happened;
/// run the operation in a transaction if it should be rolled back.
pub trait Hooks {
    /// Before inserting (also upserts), after validation.
    fn before_insert(&self) -> Result<()> {
        Ok(())
    }
    /// After inserting, with the stored row.
    fn after_insert(&self) -> Result<()> {
        Ok(())
    }
    /// Before `save`, after validation.
    fn before_save(&self) -> Result<()> {
        Ok(())
    }
    /// After `save`, with the stored row.
    fn after_save(&self) -> Result<()> {
        Ok(())
    }
    /// Before `delete` / `force_delete`.
    fn before_delete(&self) -> Result<()> {
        Ok(())
    }
    /// After `delete` / `force_delete`.
    fn after_delete(&self) -> Result<()> {
        Ok(())
    }
}

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
pub trait Model:
    for<'r> FromRow<'r, PgRow> + Hooks + Send + Sync + Unpin + Sized + 'static
{
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
    /// Soft-delete column: `delete` sets it to `now()` and queries skip rows
    /// where it isn't `NULL`.
    const DELETED_AT_COLUMN: Option<&'static str> = None;
    /// Optimistic-locking column: incremented on every update, and `save`/
    /// `delete` fail with [`Error::Conflict`] if it changed since loading.
    const VERSION_COLUMN: Option<&'static str> = None;

    /// The value of this record's primary key.
    fn primary_key(&self) -> Value;

    /// Every column with its current value, in [`COLUMNS`](Model::COLUMNS) order.
    fn values(&self) -> Vec<(&'static str, Value)>;

    /// Check the record's `#[rok(validate(…))]` rules. Called automatically
    /// before every insert and save.
    fn validate(&self) -> std::result::Result<(), ValidationErrors> {
        Ok(())
    }

    /// A condition applied to every query of this model (like soft deletes),
    /// set with `#[rok(default_scope = path::to::fn)]`. Remove it per query
    /// with [`Select::unscoped`].
    fn default_scope() -> Option<Expr<Self>> {
        None
    }

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

    /// Start an `INSERT` of whole records, e.g. to configure `ON CONFLICT`:
    ///
    /// ```ignore
    /// User::insert_many(&users)
    ///     .on_conflict([User::EMAIL])
    ///     .do_update([User::NAME])
    ///     .exec(&db)
    ///     .await?;
    /// ```
    fn insert_many(records: &[Self]) -> InsertMany<'_, Self> {
        InsertMany::new(records)
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
        // Reloading a soft-deleted record still works.
        let key = self.primary_key();
        let missing = key.to_string();
        let select = Self::filter(Self::primary_key_column().eq(key))
            .with_trashed()
            .unscoped();
        async move {
            select
                .first(executor)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(Some(missing)))
        }
    }

    /// Track changes to this record so [`Tracked::save`](crate::Tracked::save)
    /// writes only modified columns.
    fn track(self) -> crate::Tracked<Self> {
        crate::Tracked::new(self)
    }

    /// Install (or replace) an `AFTER INSERT OR UPDATE OR DELETE` trigger
    /// that announces every row change of this table, for
    /// [`changes`](Model::changes). Run it once, e.g. from a migration or at
    /// startup (PostgreSQL 11+).
    fn install_change_notifications(db: &crate::Db) -> impl Future<Output = Result<()>> + Send {
        let sql = crate::notify::install_sql::<Self>();
        async move {
            db.execute(&sql?).await?;
            Ok(())
        }
    }

    /// Remove the trigger installed by
    /// [`install_change_notifications`](Model::install_change_notifications).
    fn uninstall_change_notifications(db: &crate::Db) -> impl Future<Output = Result<()>> + Send {
        let sql = crate::notify::uninstall_sql::<Self>();
        async move {
            db.execute(&sql).await?;
            Ok(())
        }
    }

    /// Subscribe to row changes of this table (see
    /// [`install_change_notifications`](Model::install_change_notifications)).
    fn changes(
        db: &crate::Db,
    ) -> impl Future<Output = Result<crate::notify::ChangeStream<Self>>> + Send {
        async move {
            let listener = db.listen(&[&crate::notify::channel::<Self>()]).await?;
            Ok(crate::notify::ChangeStream::new(listener))
        }
    }

    /// `true` if this record has been soft-deleted.
    fn is_trashed(&self) -> bool {
        Self::DELETED_AT_COLUMN
            .and_then(|c| self.value_of(c))
            .is_some_and(|v| !v.is_null())
    }

    // ----- writes ---------------------------------------------------------

    /// `INSERT` this record (skipping generated columns) and return the stored row.
    fn insert<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let pre = pre_insert(self);
        let sql = insert_sql::<Self>(std::slice::from_ref(self), false, None);
        async move {
            pre?;
            let row = exec::fetch_optional::<Self, _>(executor, &sql?, &[Self::TABLE], false)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(None))?;
            row.after_insert()?;
            Ok(row)
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
        Self::insert_many(records).exec(executor)
    }

    /// Bulk-load `records` with PostgreSQL's binary `COPY`, typically 5–20×
    /// faster than `INSERT` for large batches. Returns the number of rows
    /// written.
    ///
    /// ```ignore
    /// User::copy_in(&db, &users).await?;          // borrows a pooled connection
    /// User::copy_in(&mut *tx, &users).await?;     // inside a transaction
    /// ```
    ///
    /// Records are validated and passed to `before_insert` first;
    /// `after_insert` is not called and nothing is returned, because `COPY`
    /// doesn't return rows. Generated columns are skipped; every other value
    /// (timestamps and `version` included) is written as-is. Binary `COPY`
    /// needs each field's Rust type to match its column type exactly (e.g.
    /// `i64` for `BIGINT`). The table's cached query results are invalidated
    /// (except when copying through a plain `PgConnection`).
    fn copy_in<'a>(
        target: impl Into<crate::copy::CopyTarget<'a>>,
        records: &[Self],
    ) -> impl Future<Output = Result<u64>> + Send {
        let target = target.into();
        async move { crate::copy::copy_in(target, records).await }
    }

    /// `INSERT` this record, or update every column if its primary key
    /// already exists (`ON CONFLICT (pk) DO UPDATE`). The primary key is
    /// always written, even when it is a generated column.
    fn upsert<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let conflict = Conflict {
            target: ConflictTarget::Columns(vec![Self::PRIMARY_KEY]),
            action: ConflictAction::UpdateAll,
        };
        let pre = pre_insert(self);
        let sql = insert_sql::<Self>(std::slice::from_ref(self), true, Some(&conflict));
        async move {
            pre?;
            let row = exec::fetch_optional::<Self, _>(executor, &sql?, &[Self::TABLE], false)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(None))?;
            row.after_insert()?;
            Ok(row)
        }
    }

    /// `INSERT` this record, or update it if it conflicts on `columns` (a
    /// unique index), e.g. upsert by email:
    ///
    /// ```ignore
    /// let user = new_user.upsert_on(&db, [User::EMAIL]).await?;
    /// ```
    ///
    /// Every inserted column except `columns`, the primary key and
    /// `created_at` is overwritten. For other policies use
    /// [`insert_many`](Model::insert_many) or [`create`](Model::create).
    fn upsert_on<'e, E>(
        &self,
        executor: E,
        columns: impl IntoIterator<Item = Column<Self>>,
    ) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let conflict = Conflict {
            target: ConflictTarget::Columns(columns.into_iter().map(|c| c.name()).collect()),
            action: ConflictAction::UpdateAll,
        };
        let pre = pre_insert(self);
        let sql = insert_sql::<Self>(std::slice::from_ref(self), false, Some(&conflict));
        async move {
            pre?;
            let row = exec::fetch_optional::<Self, _>(executor, &sql?, &[Self::TABLE], false)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(None))?;
            row.after_insert()?;
            Ok(row)
        }
    }

    /// `UPDATE` every non-generated column of this record by primary key and
    /// return the stored row. Fails with [`Error::NotFound`] if the row is
    /// gone, and with [`Error::Conflict`] if its `#[rok(version)]` changed
    /// since it was loaded.
    fn save<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        self.save_only(executor, None)
    }

    /// Like [`save`](Model::save), but only write `columns` (managed columns
    /// such as `updated_at` and `version` are still maintained). Passing
    /// `None` writes every column.
    ///
    /// ```ignore
    /// user.save_only(&db, Some(vec![User::EMAIL])).await?;
    /// ```
    ///
    /// [`Tracked`](crate::Tracked) uses this to write only changed fields.
    fn save_only<'e, E>(
        &self,
        executor: E,
        columns: Option<Vec<Column<Self>>>,
    ) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let only: Option<Vec<&'static str>> = columns.map(|c| c.iter().map(|c| c.name()).collect());
        let pre = self
            .validate()
            .map_err(Error::Validation)
            .and_then(|()| self.before_save());
        let pk = self.primary_key();
        let key = pk.to_string();
        let managed = |c: &str| {
            c == Self::PRIMARY_KEY || is_timestamp::<Self>(c) || Self::VERSION_COLUMN == Some(c)
        };
        let sets: Vec<_> = writable::<Self>(self.values())
            .filter(|(c, _)| !managed(c))
            .filter(|(c, _)| only.as_ref().is_none_or(|only| only.contains(c)))
            .collect();
        let mut update = Self::update_all()
            .with_trashed()
            .unscoped()
            .filter(Self::primary_key_column().eq(pk.clone()));
        let version = Self::VERSION_COLUMN.and_then(|c| self.value_of(c).map(|v| (c, v)));
        if let Some((column, current)) = version.clone() {
            update = update.filter(Column::new(column).eq(current));
        }
        for (column, value) in sets {
            update = update.set(Column::new(column), value);
        }
        let changes = update.has_sets()
            || Self::UPDATED_AT_COLUMN.is_some()
            || Self::VERSION_COLUMN.is_some();
        async move {
            pre?;
            let row = if !changes {
                update
                    .into_select()
                    .first(executor)
                    .await?
                    .ok_or_else(|| Error::not_found::<Self>(Some(key)))?
            } else if version.is_some() {
                let mut sql = Sql::new();
                update.write_into(&mut sql);
                let exists = Self::filter(Self::primary_key_column().eq(pk.clone()))
                    .with_trashed()
                    .unscoped();
                checked_write::<Self, _>(executor, sql, pk, exists).await?
            } else {
                update
                    .returning_one(executor)
                    .await?
                    .ok_or_else(|| Error::not_found::<Self>(Some(key)))?
            };
            row.after_save()?;
            Ok(row)
        }
    }

    /// Delete this record by primary key. Models with soft deletes are
    /// marked deleted (`deleted_at = now()`); use
    /// [`force_delete`](Model::force_delete) to remove the row.
    ///
    /// Fails with [`Error::NotFound`] if no row was deleted, and with
    /// [`Error::Conflict`] if its `#[rok(version)]` changed since it was
    /// loaded.
    fn delete<'e, E>(&self, executor: E) -> impl Future<Output = Result<()>> + Send
    where
        E: Executor<'e>,
    {
        self.delete_by_key(executor, false)
    }

    /// Permanently `DELETE` this record, even for models with soft deletes.
    fn force_delete<'e, E>(&self, executor: E) -> impl Future<Output = Result<()>> + Send
    where
        E: Executor<'e>,
    {
        self.delete_by_key(executor, true)
    }

    #[doc(hidden)]
    fn delete_by_key<'e, E>(
        &self,
        executor: E,
        force: bool,
    ) -> impl Future<Output = Result<()>> + Send
    where
        E: Executor<'e>,
    {
        let pre = self.before_delete();
        let pk = self.primary_key();
        let key = pk.to_string();
        let soft = Self::DELETED_AT_COLUMN.filter(|_| !force);
        let version = Self::VERSION_COLUMN.and_then(|c| self.value_of(c).map(|v| (c, v)));
        let mut select = Self::filter(Self::primary_key_column().eq(pk.clone())).unscoped();
        if force {
            select = select.with_trashed();
        }
        // "Does the row exist?" uses the same soft-delete scope as the write.
        let exists = select.clone();
        if let Some((column, current)) = version.clone() {
            select = select.filter(Column::new(column).eq(current));
        }
        let mut sql = Sql::new();
        match soft {
            Some(deleted_at) => select
                .update()
                .set_raw(Column::new(deleted_at), "now()", [] as [Value; 0])
                .write_into(&mut sql),
            None => {
                let delete = if force {
                    select.force_delete_sql()
                } else {
                    select.delete_sql()
                };
                sql = delete;
            }
        }
        async move {
            pre?;
            if version.is_some() {
                checked_write::<Self, _>(executor, sql, pk, exists).await?;
            } else if exec::execute(executor, &sql, &[Self::TABLE]).await? == 0 {
                return Err(Error::not_found::<Self>(Some(key)));
            }
            self.after_delete()
        }
    }

    /// Restore this soft-deleted record (`deleted_at = NULL`) and return the
    /// stored row.
    fn restore<'e, E>(&self, executor: E) -> impl Future<Output = Result<Self>> + Send
    where
        E: Executor<'e>,
    {
        let pk = self.primary_key();
        let key = pk.to_string();
        let update = Self::DELETED_AT_COLUMN.map(|deleted_at| {
            Self::filter(Self::primary_key_column().eq(pk))
                .only_trashed()
                .unscoped()
                .update()
                .set_raw(Column::new(deleted_at), "NULL", [] as [Value; 0])
        });
        async move {
            let Some(update) = update else {
                return Err(Error::InvalidQuery(format!(
                    "`{}` has no soft-delete column",
                    Self::TABLE
                )));
            };
            update
                .returning_one(executor)
                .await?
                .ok_or_else(|| Error::not_found::<Self>(Some(key)))
        }
    }
}

/// Run a single-row write (`UPDATE`/`DELETE` by primary key with a version
/// check) and tell apart success, a version conflict and a missing row in
/// one round trip:
///
/// ```sql
/// WITH w AS (<write> RETURNING TRUE AS __rok_present, <columns>)
/// SELECT EXISTS(<exists>) AS __rok_exists, w.*
/// FROM (SELECT 1) d LEFT JOIN w ON TRUE
/// ```
///
/// `exists` selects the record by primary key (without the version check).
async fn checked_write<'e, M: Model, E: Executor<'e>>(
    executor: E,
    write: Sql,
    pk: Value,
    exists: Select<M>,
) -> Result<M> {
    let key = pk.to_string();
    let mut sql = Sql::new();
    sql.push(r#"WITH "__rok_w" AS ("#);
    sql.append(write);
    sql.push(r#" RETURNING TRUE AS "__rok_present", "#);
    push_columns::<M>(&mut sql);
    sql.push(") SELECT EXISTS(");
    exists.write_exists_body(&mut sql);
    sql.push(r#") AS "__rok_exists", "w".* FROM (SELECT 1) AS "__rok_d" LEFT JOIN "__rok_w" AS "w" ON TRUE"#);

    let rows = exec::fetch_rows(executor, &sql, &[M::TABLE], false).await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::not_found::<M>(Some(key.clone())))?;
    if row.try_get::<Option<bool>, _>("__rok_present")?.is_some() {
        Ok(M::from_row(row)?)
    } else if row.try_get::<bool, _>("__rok_exists")? {
        Err(Error::Conflict {
            table: M::TABLE,
            key,
        })
    } else {
        Err(Error::not_found::<M>(Some(key)))
    }
}

/// Validation and `before_insert`, run before any insert of `record`.
pub(crate) fn pre_insert<M: Model>(record: &M) -> Result<()> {
    record.validate().map_err(Error::Validation)?;
    record.before_insert()
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
pub(crate) fn insert_sql<M: Model>(
    records: &[M],
    include_pk: bool,
    conflict: Option<&Conflict>,
) -> Result<Sql> {
    let include = |c: &str| !M::GENERATED.contains(&c) || (include_pk && c == M::PRIMARY_KEY);
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
    if let Some(conflict) = conflict {
        conflict.write::<M>(&mut sql, &columns)?;
    }
    push_returning::<M>(&mut sql);
    Ok(sql)
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
    let conflict = upsert.then(|| Conflict {
        target: ConflictTarget::Columns(vec![M::PRIMARY_KEY]),
        action: ConflictAction::UpdateAll,
    });
    insert_sql(records, upsert, conflict.as_ref()).expect("valid insert")
}
