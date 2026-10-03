use std::fmt;
use std::marker::PhantomData;
use std::time::Duration;

use futures_core::stream::BoxStream;
use sqlx::postgres::PgRow;
use sqlx::{Decode, FromRow, Postgres, Row, Type};

use crate::exec;
use crate::expr::{Cond, IntoProjections, Projection};
use crate::model::{push_columns, push_returning};
use crate::sql::Sql;
use crate::{Column, Error, Executor, Expr, Model, Order, Page, Result, Value};

fn push_where(sql: &mut Sql, keyword: &str, filters: &[Cond]) {
    if filters.is_empty() {
        return;
    }
    sql.push(keyword);
    // Groups render their own parentheses, so a top-level AND chain is safe.
    sql.push_list(filters, " AND ", |sql, cond| cond.write(sql));
}

/// Lock mode appended to a `SELECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lock {
    Update,
    Share,
}

/// A `SELECT` query over model `M`, created with [`Model::query`] or [`Model::filter`].
///
/// Builders are cheap, cloneable values: nothing touches the database until
/// one of the async terminal methods (`all`, `first`, `count`, …) is awaited.
pub struct Select<M> {
    filters: Vec<Cond>,
    order: Vec<Order<M>>,
    group_by: Vec<&'static str>,
    having: Vec<Cond>,
    limit: Option<u64>,
    offset: Option<u64>,
    lock: Option<Lock>,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Default for Select<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Model> Select<M> {
    /// A `SELECT` over every row of `M`'s table.
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
            order: Vec::new(),
            group_by: Vec::new(),
            having: Vec::new(),
            limit: None,
            offset: None,
            lock: None,
            _model: PhantomData,
        }
    }

    /// Add a `WHERE` condition. Multiple calls are combined with `AND`.
    pub fn filter(mut self, expr: Expr<M>) -> Self {
        self.filters.push(expr.cond);
        self
    }

    /// Add a condition only when `condition` is true.
    pub fn filter_if(self, condition: bool, expr: impl FnOnce() -> Expr<M>) -> Self {
        if condition { self.filter(expr()) } else { self }
    }

    /// Add a condition built from an optional value, e.g. a search parameter.
    ///
    /// ```ignore
    /// User::query().filter_opt(params.role, |r| User::ROLE.eq(r))
    /// ```
    pub fn filter_opt<T>(self, value: Option<T>, expr: impl FnOnce(T) -> Expr<M>) -> Self {
        match value {
            Some(v) => self.filter(expr(v)),
            None => self,
        }
    }

    /// Append an `ORDER BY` term. Pass a column for ascending order, or
    /// `column.desc()`.
    pub fn order_by(mut self, order: impl Into<Order<M>>) -> Self {
        self.order.push(order.into());
        self
    }

    /// Append a `GROUP BY` column; combine with [`select`](Self::select).
    pub fn group_by(mut self, column: Column<M>) -> Self {
        self.group_by.push(column.name());
        self
    }

    /// Add a `HAVING` condition, typically on an aggregate
    /// (`User::AGE.avg().gt(30)`). Multiple calls are combined with `AND`.
    pub fn having(mut self, expr: Expr<M>) -> Self {
        self.having.push(expr.cond);
        self
    }

    /// Limit the number of returned rows.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skip the first `offset` rows.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Lock selected rows with `FOR UPDATE` (use inside a transaction).
    pub fn for_update(mut self) -> Self {
        self.lock = Some(Lock::Update);
        self
    }

    /// Lock selected rows with `FOR SHARE` (use inside a transaction).
    pub fn for_share(mut self) -> Self {
        self.lock = Some(Lock::Share);
        self
    }

    /// Select specific columns or aggregates instead of whole models.
    ///
    /// ```ignore
    /// let emails: Vec<(i64, String)> = User::query()
    ///     .select((User::ID, User::EMAIL))
    ///     .fetch_all(&db)
    ///     .await?;
    /// ```
    pub fn select(self, projections: impl IntoProjections<M>) -> Projected<M> {
        Projected {
            select: self,
            items: projections.into_projections(),
        }
    }

    /// Cache this query's results for `ttl` in the pool's
    /// [`QueryCache`](crate::QueryCache).
    ///
    /// Writes made through rok-db invalidate the cached results of the
    /// table. When the executor has no cache (it wasn't enabled with
    /// [`DbBuilder::query_cache`](crate::DbBuilder::query_cache), or a plain
    /// sqlx pool or connection is used) the query simply runs every time.
    pub fn memoize(self, ttl: Duration) -> Memoized<M> {
        Memoized { select: self, ttl }
    }

    /// Turn this query into a bulk `UPDATE` with the same filters.
    pub fn update(self) -> Update<M> {
        Update {
            filters: self.filters,
            sets: Vec::new(),
            _model: PhantomData,
        }
    }

    /// Render the `SELECT` statement.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("SELECT ");
        push_columns::<M>(&mut sql);
        self.write_tail(&mut sql);
        sql
    }

    /// Everything after the select list.
    fn write_tail(&self, sql: &mut Sql) {
        self.write_from_where(sql);
        if !self.group_by.is_empty() {
            sql.push(" GROUP BY ")
                .push_list(&self.group_by, ", ", |sql, c| {
                    sql.push_ident(c);
                });
        }
        push_where(sql, " HAVING ", &self.having);
        self.write_order(sql, "");
        if let Some(limit) = self.limit {
            sql.push(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = self.offset {
            sql.push(&format!(" OFFSET {offset}"));
        }
        match self.lock {
            Some(Lock::Update) => sql.push(" FOR UPDATE"),
            Some(Lock::Share) => sql.push(" FOR SHARE"),
            None => sql,
        };
    }

    fn write_from_where(&self, sql: &mut Sql) {
        sql.push(" FROM ").push_ident(M::TABLE);
        push_where(sql, " WHERE ", &self.filters);
    }

    fn write_order(&self, sql: &mut Sql, prefix: &str) {
        if !self.order.is_empty() {
            sql.push(" ORDER BY ")
                .push_list(&self.order, ", ", |sql, o| {
                    sql.push(prefix);
                    o.write(sql);
                });
        }
    }

    fn count_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("SELECT COUNT(*)");
        self.write_from_where(&mut sql);
        sql
    }

    fn exists_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("SELECT EXISTS(SELECT 1");
        self.write_from_where(&mut sql);
        sql.push(")");
        sql
    }

    fn aggregate_sql(&self, projection: Projection<M>) -> Sql {
        let mut sql = Sql::new();
        sql.push("SELECT ");
        projection.write(&mut sql);
        self.write_from_where(&mut sql);
        sql
    }

    /// Fetch every matching row.
    pub async fn all<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        exec::fetch_all(executor, &self.to_sql(), &[]).await
    }

    /// Fetch the first matching row, or `None`.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        exec::fetch_optional(executor, &self.limit(1).to_sql(), &[]).await
    }

    /// Fetch the first matching row, failing with [`Error::NotFound`].
    pub async fn one<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        self.first(executor)
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }

    /// Stream matching rows one at a time instead of loading them all into
    /// memory. Consume it with `futures::TryStreamExt` (re-exported in the
    /// rok-db prelude):
    ///
    /// ```ignore
    /// let mut users = User::query().stream(&db);
    /// while let Some(user) = users.try_next().await? {
    ///     // …
    /// }
    /// ```
    pub fn stream<'e, E: Executor<'e> + 'e>(self, executor: E) -> BoxStream<'e, Result<M>> {
        exec::stream(executor, self.to_sql())
    }

    /// Count the matching rows (ignores ordering, limit and offset).
    pub async fn count<'e, E: Executor<'e>>(self, executor: E) -> Result<i64> {
        exec::fetch_scalar(executor, &self.count_sql()).await
    }

    /// `true` if at least one row matches.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        exec::fetch_scalar(executor, &self.exists_sql()).await
    }

    /// `SUM(column)` over the matching rows as `BIGINT` (`None` when no
    /// rows match). Use [`select`](Self::select) with
    /// [`Projection::cast`] for other result types.
    pub async fn sum<'e, E: Executor<'e>>(
        self,
        executor: E,
        column: Column<M>,
    ) -> Result<Option<i64>> {
        let sql = self.aggregate_sql(column.sum().cast("BIGINT"));
        exec::fetch_scalar(executor, &sql).await
    }

    /// `AVG(column)` over the matching rows as `DOUBLE PRECISION`.
    pub async fn avg<'e, E: Executor<'e>>(
        self,
        executor: E,
        column: Column<M>,
    ) -> Result<Option<f64>> {
        let sql = self.aggregate_sql(column.avg().cast("DOUBLE PRECISION"));
        exec::fetch_scalar(executor, &sql).await
    }

    /// `MIN(column)` over the matching rows.
    pub async fn min<'e, T, E>(self, executor: E, column: Column<M>) -> Result<Option<T>>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(executor, &self.aggregate_sql(column.min())).await
    }

    /// `MAX(column)` over the matching rows.
    pub async fn max<'e, T, E>(self, executor: E, column: Column<M>) -> Result<Option<T>>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(executor, &self.aggregate_sql(column.max())).await
    }

    /// Fetch one page of results together with the total number of matches,
    /// in a single round trip. `page` is 1-based.
    pub async fn paginate<'e, E: Executor<'e>>(
        self,
        executor: E,
        page: u64,
        per_page: u64,
    ) -> Result<Page<M>> {
        if per_page == 0 {
            return Err(Error::InvalidQuery(
                "`per_page` must be greater than 0".into(),
            ));
        }
        let page = page.max(1);
        let rows = exec::fetch_rows(executor, &self.paginate_sql(page, per_page)).await?;

        let mut total = 0;
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            total = row.try_get::<i64, _>("__rok_total")?;
            if row.try_get::<Option<bool>, _>("__rok_present")?.is_some() {
                items.push(M::from_row(row)?);
            }
        }
        Ok(Page::new(items, total.max(0) as u64, page, per_page))
    }

    fn paginate_sql(&self, page: u64, per_page: u64) -> Sql {
        // `count LEFT JOIN LATERAL (page)` always yields at least one row, so
        // the total is known even when the requested page is empty.
        let mut sql = Sql::new();
        sql.push(r#"SELECT c."__rok_total", p.* FROM (SELECT COUNT(*) AS "__rok_total""#);
        self.write_from_where(&mut sql);
        sql.push(r#") c LEFT JOIN LATERAL (SELECT TRUE AS "__rok_present", "#);
        push_columns::<M>(&mut sql);
        self.write_from_where(&mut sql);
        self.write_order(&mut sql, "");
        let offset = (page - 1).saturating_mul(per_page);
        sql.push(&format!(" LIMIT {per_page} OFFSET {offset}) p ON TRUE"));
        self.write_order(&mut sql, "p.");
        sql
    }

    /// `DELETE` every matching row and return how many were deleted.
    pub async fn delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        exec::execute(executor, &self.delete_sql(), &[M::TABLE]).await
    }

    /// Render the `DELETE` statement [`delete`](Self::delete) would run.
    pub fn delete_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("DELETE");
        self.write_from_where(&mut sql);
        sql
    }
}

impl<M> Clone for Select<M> {
    fn clone(&self) -> Self {
        Self {
            filters: self.filters.clone(),
            order: self.order.clone(),
            group_by: self.group_by.clone(),
            having: self.having.clone(),
            limit: self.limit,
            offset: self.offset,
            lock: self.lock,
            _model: PhantomData,
        }
    }
}

impl<M: Model> fmt::Debug for Select<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Select").field(&self.to_sql()).finish()
    }
}

/// A `SELECT` of specific columns or aggregates, created with
/// [`Select::select`]. Decode rows into tuples or any `sqlx::FromRow` type.
pub struct Projected<M> {
    select: Select<M>,
    items: Vec<Projection<M>>,
}

impl<M: Model> Projected<M> {
    /// Render the `SELECT` statement.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("SELECT ");
        sql.push_list(&self.items, ", ", |sql, p| p.write(sql));
        self.select.write_tail(&mut sql);
        sql
    }

    /// Fetch every row, decoded into `T` (a tuple or a `FromRow` type).
    pub async fn fetch_all<'e, T, E>(self, executor: E) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_all(executor, &self.to_sql(), &[]).await
    }

    /// Fetch the first row, or `None`.
    pub async fn fetch_optional<'e, T, E>(self, executor: E) -> Result<Option<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        let projected = Projected {
            select: self.select.limit(1),
            items: self.items,
        };
        exec::fetch_optional(executor, &projected.to_sql(), &[]).await
    }

    /// Fetch the first row, failing with [`Error::NotFound`].
    pub async fn fetch_one<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        self.fetch_optional(executor)
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }

    /// Fetch the first column of the first row.
    pub async fn scalar<'e, T, E>(self, executor: E) -> Result<T>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(executor, &self.to_sql()).await
    }

    /// Stream rows one at a time.
    pub fn stream<'e, T, E>(self, executor: E) -> BoxStream<'e, Result<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin + 'e,
        E: Executor<'e> + 'e,
    {
        exec::stream(executor, self.to_sql())
    }
}

impl<M> Clone for Projected<M> {
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            items: self.items.clone(),
        }
    }
}

impl<M: Model> fmt::Debug for Projected<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Projected").field(&self.to_sql()).finish()
    }
}

/// A [`Select`] whose results are cached, created with [`Select::memoize`].
///
/// Results are cloned out of the cache, so the model must be `Clone`.
pub struct Memoized<M> {
    select: Select<M>,
    ttl: Duration,
}

impl<M: Model + Clone> Memoized<M> {
    async fn cached<'e, T, E, F, Fut>(&self, executor: E, op: &str, sql: &Sql, run: F) -> Result<T>
    where
        T: Clone + Send + Sync + 'static,
        E: Executor<'e>,
        F: FnOnce(E) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let cache = executor.__context().and_then(|c| c.cache.clone());
        let Some(cache) = cache else {
            return run(executor).await;
        };
        let key = format!(
            "{op}\u{1f}{}\u{1f}{}\u{1f}{:?}",
            std::any::type_name::<M>(),
            sql.as_str(),
            sql.params()
        );
        if let Some(hit) = cache.get::<T>(&key) {
            tracing::debug!(target: "rok_db::cache", table = M::TABLE, op, "cache hit");
            return Ok(hit);
        }
        let generation = cache.generation(M::TABLE);
        let value = run(executor).await?;
        cache.put(key, M::TABLE, generation, self.ttl, value.clone());
        Ok(value)
    }

    /// Fetch every matching row, from the cache when possible.
    pub async fn all<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        let sql = self.select.to_sql();
        self.cached(executor, "all", &sql, |e| exec::fetch_all(e, &sql, &[]))
            .await
    }

    /// Fetch the first matching row, from the cache when possible.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let sql = self.select.clone().limit(1).to_sql();
        self.cached(executor, "first", &sql, |e| {
            exec::fetch_optional(e, &sql, &[])
        })
        .await
    }

    /// Fetch the first matching row, failing with [`Error::NotFound`].
    pub async fn one<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        self.first(executor)
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }

    /// Count the matching rows, from the cache when possible.
    pub async fn count<'e, E: Executor<'e>>(self, executor: E) -> Result<i64> {
        let sql = self.select.count_sql();
        self.cached(executor, "count", &sql, |e| exec::fetch_scalar(e, &sql))
            .await
    }

    /// `true` if at least one row matches, from the cache when possible.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        let sql = self.select.exists_sql();
        self.cached(executor, "exists", &sql, |e| exec::fetch_scalar(e, &sql))
            .await
    }

    /// Fetch one page, from the cache when possible.
    pub async fn paginate<'e, E: Executor<'e>>(
        self,
        executor: E,
        page: u64,
        per_page: u64,
    ) -> Result<Page<M>> {
        let sql = self.select.paginate_sql(page.max(1), per_page);
        let select = self.select.clone();
        self.cached(executor, "paginate", &sql, |e| {
            select.paginate(e, page, per_page)
        })
        .await
    }
}

impl<M> Clone for Memoized<M> {
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            ttl: self.ttl,
        }
    }
}

impl<M: Model> fmt::Debug for Memoized<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memoized")
            .field("sql", &self.select.to_sql())
            .field("ttl", &self.ttl)
            .finish()
    }
}

/// A bulk `UPDATE` over model `M`, created with [`Model::update_all`] or
/// [`Select::update`].
///
/// For models with an `updated_at` timestamp, `updated_at = now()` is added
/// automatically unless you set it yourself.
pub struct Update<M> {
    filters: Vec<Cond>,
    sets: Vec<(&'static str, Assign)>,
    _model: PhantomData<fn() -> M>,
}

#[derive(Debug, Clone)]
enum Assign {
    Value(Value),
    Raw(String, Vec<Value>),
}

impl<M: Model> Update<M> {
    pub(crate) fn new() -> Self {
        Self {
            filters: Vec::new(),
            sets: Vec::new(),
            _model: PhantomData,
        }
    }

    /// Add a `WHERE` condition. Multiple calls are combined with `AND`.
    pub fn filter(mut self, expr: Expr<M>) -> Self {
        self.filters.push(expr.cond);
        self
    }

    /// `SET column = value`
    pub fn set(mut self, column: Column<M>, value: impl Into<Value>) -> Self {
        self.sets.push((column.name(), Assign::Value(value.into())));
        self
    }

    /// `SET column = <raw sql>`, binding each `?` to the next parameter.
    ///
    /// ```ignore
    /// Post::update_all().set_raw(Post::PUBLISHED_AT, "now()", [] as [i32; 0])
    /// ```
    pub fn set_raw<V: Into<Value>>(
        mut self,
        column: Column<M>,
        sql: impl Into<String>,
        params: impl IntoIterator<Item = V>,
    ) -> Self {
        let params = params.into_iter().map(Into::into).collect();
        self.sets
            .push((column.name(), Assign::Raw(sql.into(), params)));
        self
    }

    /// `SET column = column + by`
    pub fn increment(self, column: Column<M>, by: impl Into<Value>) -> Self {
        let raw = format!("{} + ?", quoted(column.name()));
        self.set_raw(column, raw, [by.into()])
    }

    /// `SET column = column - by`
    pub fn decrement(self, column: Column<M>, by: impl Into<Value>) -> Self {
        let raw = format!("{} - ?", quoted(column.name()));
        self.set_raw(column, raw, [by.into()])
    }

    pub(crate) fn has_sets(&self) -> bool {
        !self.sets.is_empty()
    }

    pub(crate) fn into_select(self) -> Select<M> {
        Select {
            filters: self.filters,
            ..Select::new()
        }
    }

    /// Render the `UPDATE` statement.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("UPDATE ").push_ident(M::TABLE).push(" SET ");
        sql.push_list(&self.sets, ", ", |sql, (column, assign)| {
            sql.push_ident(column).push(" = ");
            match assign {
                Assign::Value(v) => sql.bind(v.clone()),
                Assign::Raw(raw, params) => sql.push_raw(raw, params),
            };
        });
        if let Some(updated_at) = M::UPDATED_AT_COLUMN {
            if !self.sets.iter().any(|(c, _)| *c == updated_at) {
                sql.push(", ").push_ident(updated_at).push(" = now()");
            }
        }
        push_where(&mut sql, " WHERE ", &self.filters);
        sql
    }

    fn checked_sql(&self) -> Result<Sql> {
        if self.sets.is_empty() {
            return Err(Error::InvalidQuery(format!(
                "UPDATE on `{}` has no columns to set",
                M::TABLE
            )));
        }
        Ok(self.to_sql())
    }

    /// Run the update and return the number of affected rows.
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        exec::execute(executor, &self.checked_sql()?, &[M::TABLE]).await
    }

    /// Run the update and return the updated rows.
    pub async fn returning<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        let mut sql = self.checked_sql()?;
        push_returning::<M>(&mut sql);
        exec::fetch_all(executor, &sql, &[M::TABLE]).await
    }

    pub(crate) async fn returning_one<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let mut sql = self.checked_sql()?;
        push_returning::<M>(&mut sql);
        exec::fetch_optional(executor, &sql, &[M::TABLE]).await
    }
}

impl<M> Clone for Update<M> {
    fn clone(&self) -> Self {
        Self {
            filters: self.filters.clone(),
            sets: self.sets.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: Model> fmt::Debug for Update<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Update").field(&self.to_sql()).finish()
    }
}

fn quoted(ident: &str) -> String {
    let mut s = String::new();
    crate::sql::push_ident(&mut s, ident);
    s
}

/// An `INSERT` of a single row built column by column, created with
/// [`Model::create`]. Columns you don't set get their database default;
/// timestamp columns default to `now()`.
pub struct Insert<M> {
    /// `(column, raw sql, params)`; plain values are stored as `"?"`.
    values: Vec<(&'static str, String, Vec<Value>)>,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Insert<M> {
    pub(crate) fn new() -> Self {
        Self {
            values: Vec::new(),
            _model: PhantomData,
        }
    }

    /// Set `column` to `value`.
    pub fn set(mut self, column: Column<M>, value: impl Into<Value>) -> Self {
        self.values
            .push((column.name(), "?".into(), vec![value.into()]));
        self
    }

    /// Set `column` to raw SQL, binding each `?` to the next parameter.
    pub fn set_raw<V: Into<Value>>(
        mut self,
        column: Column<M>,
        sql: impl Into<String>,
        params: impl IntoIterator<Item = V>,
    ) -> Self {
        let params = params.into_iter().map(Into::into).collect();
        self.values.push((column.name(), sql.into(), params));
        self
    }

    /// Render the `INSERT` statement.
    pub fn to_sql(&self) -> Sql {
        let mut values = self.values.clone();
        for ts in [M::CREATED_AT_COLUMN, M::UPDATED_AT_COLUMN]
            .into_iter()
            .flatten()
        {
            if !values.iter().any(|(c, _, _)| *c == ts) {
                values.push((ts, "now()".into(), Vec::new()));
            }
        }
        let mut sql = Sql::new();
        sql.push("INSERT INTO ").push_ident(M::TABLE);
        if values.is_empty() {
            sql.push(" DEFAULT VALUES");
        } else {
            sql.push(" (")
                .push_list(&values, ", ", |sql, (c, _, _)| {
                    sql.push_ident(c);
                })
                .push(") VALUES (")
                .push_list(&values, ", ", |sql, (_, raw, params)| {
                    sql.push_raw(raw, params);
                })
                .push(")");
        }
        push_returning::<M>(&mut sql);
        sql
    }

    /// Run the insert and return the stored row.
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        exec::fetch_optional(executor, &self.to_sql(), &[M::TABLE])
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }
}

impl<M> Clone for Insert<M> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: Model> fmt::Debug for Insert<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Insert").field(&self.to_sql()).finish()
    }
}

#[doc(hidden)]
pub fn __paginate_sql<M: Model>(select: &Select<M>, page: u64, per_page: u64) -> Sql {
    select.paginate_sql(page, per_page)
}
