use std::fmt;
use std::marker::PhantomData;
use std::time::Duration;

use futures_core::stream::BoxStream;
use sqlx::postgres::PgRow;
use sqlx::{Decode, FromRow, Postgres, Row, Type};

use crate::cursor::{Cursor, CursorPage};
use crate::exec;
use crate::expr::Direction;
use crate::expr::{Cond, IntoProjections, Projection};
use crate::model::{push_columns, push_returning};
use crate::sql::Sql;
use crate::{Column, Error, Executor, Expr, Model, Order, Page, Result, Value};

fn push_where<'a>(sql: &mut Sql, keyword: &str, filters: impl IntoIterator<Item = &'a Cond>) {
    let mut filters = filters.into_iter().peekable();
    if filters.peek().is_none() {
        return;
    }
    sql.push(keyword);
    // Groups render their own parentheses, so a top-level AND chain is safe.
    sql.push_list(filters, " AND ", |sql, cond| cond.write(sql));
}

/// Which soft-deleted rows a query sees (only relevant for models with a
/// `deleted_at` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trashed {
    Exclude,
    Include,
    Only,
}

impl Trashed {
    /// The implicit soft-delete condition for model `M`, if any.
    fn cond<M: Model>(self) -> Option<Cond> {
        let column = M::DELETED_AT_COLUMN?;
        match self {
            Trashed::Exclude => Some(Column::<M>::new(column).is_null().cond),
            Trashed::Only => Some(Column::<M>::new(column).is_not_null().cond),
            Trashed::Include => None,
        }
    }
}

/// Implicit conditions: the soft-delete scope and, unless `unscoped`, the
/// model's default scope.
fn implicit<M: Model>(trashed: Trashed, scoped: bool) -> Vec<Cond> {
    let default = scoped.then(M::default_scope).flatten().map(|e| e.cond);
    trashed.cond::<M>().into_iter().chain(default).collect()
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
    trashed: Trashed,
    scoped: bool,
    primary: bool,
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
            trashed: Trashed::Exclude,
            scoped: true,
            primary: false,
            _model: PhantomData,
        }
    }

    /// Add a `WHERE` condition. Multiple calls are combined with `AND`.
    pub fn filter(mut self, expr: Expr<M>) -> Self {
        self.filters.push(expr.cond);
        self
    }

    /// Run on the primary even when read replicas are configured, e.g. to
    /// read your own writes without replication lag.
    pub fn on_primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Whether this read may go to a replica: not forced to the primary and
    /// not locking rows.
    pub(crate) fn use_replica(&self) -> bool {
        !self.primary && self.lock.is_none()
    }

    /// Apply a reusable query fragment ("named scope"):
    ///
    /// ```ignore
    /// fn active(q: Select<User>) -> Select<User> { q.filter(User::ACTIVE.eq(true)) }
    /// fn newest(q: Select<User>) -> Select<User> { q.order_by(User::ID.desc()) }
    ///
    /// User::query().scope(active).scope(newest).limit(10)
    /// ```
    pub fn scope(self, scope: impl FnOnce(Self) -> Self) -> Self {
        scope(self)
    }

    /// Drop the model's default scope (`#[rok(default_scope = …)]`) for this
    /// query. Soft-delete filtering is controlled separately with
    /// [`with_trashed`](Self::with_trashed).
    pub fn unscoped(mut self) -> Self {
        self.scoped = false;
        self
    }

    /// Include soft-deleted rows (models with `#[rok(soft_delete)]` exclude
    /// them by default). No effect on other models.
    pub fn with_trashed(mut self) -> Self {
        self.trashed = Trashed::Include;
        self
    }

    /// Only soft-deleted rows. No effect on models without soft deletes.
    pub fn only_trashed(mut self) -> Self {
        self.trashed = Trashed::Only;
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
            trashed: self.trashed,
            scoped: self.scoped,
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
        let implicit = implicit::<M>(self.trashed, self.scoped);
        push_where(sql, " WHERE ", self.filters.iter().chain(&implicit));
    }

    /// `SELECT 1 FROM … WHERE …`, the body of an `EXISTS` subquery.
    pub(crate) fn write_exists_body(&self, sql: &mut Sql) {
        sql.push("SELECT 1");
        self.write_tail(sql);
    }

    /// `ORDER BY …`; with a `prefix` (the outer query of `paginate`),
    /// expression orders refer to their `__rok_o{i}` aliases.
    fn write_order(&self, sql: &mut Sql, prefix: &str) {
        if !self.order.is_empty() {
            sql.push(" ORDER BY ")
                .push_list(self.order.iter().enumerate(), ", ", |sql, (i, o)| {
                    let alias = (!prefix.is_empty()).then(|| format!("__rok_o{i}"));
                    o.write_key(sql, prefix, alias.as_deref());
                    o.write_direction(sql);
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
        exec::fetch_all(executor, &self.to_sql(), &[], self.use_replica()).await
    }

    /// Fetch the first matching row, or `None`.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let replica = self.use_replica();
        exec::fetch_optional(executor, &self.limit(1).to_sql(), &[], replica).await
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
        let replica = self.use_replica();
        exec::stream(executor, self.to_sql(), replica)
    }

    /// Count the matching rows (ignores ordering, limit and offset).
    pub async fn count<'e, E: Executor<'e>>(self, executor: E) -> Result<i64> {
        exec::fetch_scalar(executor, &self.count_sql(), self.use_replica()).await
    }

    /// `true` if at least one row matches.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        exec::fetch_scalar(executor, &self.exists_sql(), self.use_replica()).await
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
        exec::fetch_scalar(executor, &sql, self.use_replica()).await
    }

    /// `AVG(column)` over the matching rows as `DOUBLE PRECISION`.
    pub async fn avg<'e, E: Executor<'e>>(
        self,
        executor: E,
        column: Column<M>,
    ) -> Result<Option<f64>> {
        let sql = self.aggregate_sql(column.avg().cast("DOUBLE PRECISION"));
        exec::fetch_scalar(executor, &sql, self.use_replica()).await
    }

    /// `MIN(column)` over the matching rows.
    pub async fn min<'e, T, E>(self, executor: E, column: Column<M>) -> Result<Option<T>>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(
            executor,
            &self.aggregate_sql(column.min()),
            self.use_replica(),
        )
        .await
    }

    /// `MAX(column)` over the matching rows.
    pub async fn max<'e, T, E>(self, executor: E, column: Column<M>) -> Result<Option<T>>
    where
        T: Type<Postgres> + for<'r> Decode<'r, Postgres> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_scalar(
            executor,
            &self.aggregate_sql(column.max()),
            self.use_replica(),
        )
        .await
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
        let rows = exec::fetch_rows(
            executor,
            &self.paginate_sql(page, per_page),
            &[],
            self.use_replica(),
        )
        .await?;

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
        // Expression orders are selected so the outer query can sort by them.
        for (i, o) in self.order.iter().enumerate() {
            if o.column().is_none() {
                sql.push(", ");
                o.write_key(&mut sql, "", None);
                sql.push(" AS ").push_ident(&format!("__rok_o{i}"));
            }
        }
        self.write_from_where(&mut sql);
        self.write_order(&mut sql, "");
        let offset = (page - 1).saturating_mul(per_page);
        sql.push(&format!(" LIMIT {per_page} OFFSET {offset}) p ON TRUE"));
        self.write_order(&mut sql, "p.");
        sql
    }

    /// Delete every matching row and return how many were deleted.
    ///
    /// For models with soft deletes this sets `deleted_at = now()` instead;
    /// use [`force_delete`](Self::force_delete) to remove rows for good.
    pub async fn delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        exec::execute(executor, &self.delete_sql(), &[M::TABLE]).await
    }

    /// Render the statement [`delete`](Self::delete) would run.
    pub fn delete_sql(&self) -> Sql {
        match M::DELETED_AT_COLUMN {
            Some(deleted_at) => self
                .clone()
                .update()
                .set_raw(Column::new(deleted_at), "now()", [] as [Value; 0])
                .to_sql(),
            None => self.force_delete_sql(),
        }
    }

    /// `DELETE` every matching row, even for models with soft deletes.
    /// Soft-deleted rows are only matched with
    /// [`with_trashed`](Self::with_trashed) or [`only_trashed`](Self::only_trashed).
    pub async fn force_delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        exec::execute(executor, &self.force_delete_sql(), &[M::TABLE]).await
    }

    pub(crate) fn force_delete_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("DELETE");
        self.write_from_where(&mut sql);
        sql
    }

    /// Restore every matching soft-deleted row (`deleted_at = NULL`) and
    /// return how many were restored.
    pub async fn restore<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        let Some(deleted_at) = M::DELETED_AT_COLUMN else {
            return Err(Error::InvalidQuery(format!(
                "`{}` has no soft-delete column",
                M::TABLE
            )));
        };
        self.only_trashed()
            .update()
            .set_raw(Column::new(deleted_at), "NULL", [] as [Value; 0])
            .exec(executor)
            .await
    }

    /// The ordering used for keyset pagination: the query's `ORDER BY`
    /// columns plus the primary key as a tiebreaker.
    fn keyset_order(&self) -> Vec<(&'static str, Direction)> {
        let mut order: Vec<_> = self
            .order
            .iter()
            .filter_map(|o| o.column().map(|c| (c, o.direction)))
            .collect();
        if !order.iter().any(|(c, _)| *c == M::PRIMARY_KEY) {
            order.push((M::PRIMARY_KEY, Direction::Asc));
        }
        order
    }

    /// Keyset ("cursor") pagination: fetch up to `limit` rows after
    /// `cursor`, following the query's `ORDER BY` (the primary key is added
    /// as a tiebreaker). Unlike [`paginate`](Self::paginate), the cost
    /// doesn't grow with the page number and rows aren't skipped or repeated
    /// when data changes between requests.
    ///
    /// Ordering columns must be `NOT NULL`. Pass `None` for the first page,
    /// then `page.next` for the following ones:
    ///
    /// ```ignore
    /// let page = Post::order_by(Post::CREATED_AT.desc())
    ///     .cursor_paginate(&db, None, 20)
    ///     .await?;
    /// let token = page.next.map(|c| c.to_string()); // hand to the client
    ///
    /// let cursor: Cursor = token.unwrap().parse()?;
    /// let page2 = Post::order_by(Post::CREATED_AT.desc())
    ///     .cursor_paginate(&db, Some(&cursor), 20)
    ///     .await?;
    /// ```
    pub async fn cursor_paginate<'e, E: Executor<'e>>(
        self,
        executor: E,
        after: Option<&Cursor>,
        limit: u64,
    ) -> Result<CursorPage<M>> {
        if limit == 0 {
            return Err(Error::InvalidQuery("`limit` must be greater than 0".into()));
        }
        let select = self.cursor_select(after, limit)?;
        let order = select.keyset_order();
        let mut items = select.all(executor).await?;
        let next = if items.len() as u64 > limit {
            items.truncate(limit as usize);
            let last = items.last().expect("limit > 0");
            let values = order
                .iter()
                .map(|(c, _)| {
                    last.value_of(c).ok_or_else(|| {
                        Error::InvalidQuery(format!("`{c}` is not a column of `{}`", M::TABLE))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Some(Cursor::new(values))
        } else {
            None
        };
        Ok(CursorPage { items, next })
    }

    fn cursor_select(self, after: Option<&Cursor>, limit: u64) -> Result<Self> {
        if self.order.iter().any(|o| o.column().is_none()) {
            return Err(Error::InvalidQuery(
                "keyset pagination can only order by columns, not expressions".into(),
            ));
        }
        let order = self.keyset_order();
        let mut select = self;
        select.order = order
            .iter()
            .map(|(c, d)| match d {
                Direction::Asc => Column::new(c).asc(),
                Direction::Desc => Column::new(c).desc(),
            })
            .collect();
        select.offset = None;
        select.limit = Some(limit + 1);
        if let Some(cursor) = after {
            if cursor.values.len() != order.len() {
                return Err(Error::InvalidCursor(format!(
                    "expected {} values, got {}",
                    order.len(),
                    cursor.values.len()
                )));
            }
            // (a > x) OR (a = x AND b > y) OR … — works for mixed directions.
            let branches = (0..order.len()).map(|i| {
                let eqs = (0..i).map(|j| Column::<M>::new(order[j].0).eq(cursor.values[j].clone()));
                let (column, direction) = order[i];
                let value = cursor.values[i].clone();
                let step = match direction {
                    Direction::Asc => Column::<M>::new(column).gt(value),
                    Direction::Desc => Column::<M>::new(column).lt(value),
                };
                Expr::all_of(eqs.chain([step]))
            });
            select = select.filter(Expr::any_of(branches));
        }
        Ok(select)
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
            trashed: self.trashed,
            scoped: self.scoped,
            primary: self.primary,
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
        self.write_into(&mut sql);
        sql
    }

    pub(crate) fn write_into(&self, sql: &mut Sql) {
        sql.push("SELECT ");
        sql.push_list(&self.items, ", ", |sql, p| p.write(sql));
        self.select.write_tail(sql);
    }

    /// Fetch every row, decoded into `T` (a tuple or a `FromRow` type).
    pub async fn fetch_all<'e, T, E>(self, executor: E) -> Result<Vec<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
        E: Executor<'e>,
    {
        exec::fetch_all(executor, &self.to_sql(), &[], self.select.use_replica()).await
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
        let replica = projected.select.use_replica();
        exec::fetch_optional(executor, &projected.to_sql(), &[], replica).await
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
        exec::fetch_scalar(executor, &self.to_sql(), self.select.use_replica()).await
    }

    /// Stream rows one at a time.
    pub fn stream<'e, T, E>(self, executor: E) -> BoxStream<'e, Result<T>>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin + 'e,
        E: Executor<'e> + 'e,
    {
        let replica = self.select.use_replica();
        exec::stream(executor, self.to_sql(), replica)
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
        let replica = self.select.use_replica();
        self.cached(executor, "all", &sql, |e| {
            exec::fetch_all(e, &sql, &[], replica)
        })
        .await
    }

    /// Fetch the first matching row, from the cache when possible.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let sql = self.select.clone().limit(1).to_sql();
        self.cached(executor, "first", &sql, |e| {
            exec::fetch_optional(e, &sql, &[], self.select.use_replica())
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
        let replica = self.select.use_replica();
        self.cached(executor, "count", &sql, |e| {
            exec::fetch_scalar(e, &sql, replica)
        })
        .await
    }

    /// `true` if at least one row matches, from the cache when possible.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        let sql = self.select.exists_sql();
        let replica = self.select.use_replica();
        self.cached(executor, "exists", &sql, |e| {
            exec::fetch_scalar(e, &sql, replica)
        })
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
/// Managed columns are maintained automatically unless you set them
/// yourself: `updated_at = now()` for models with timestamps and
/// `version = version + 1` for models with optimistic locking. Soft-deleted
/// rows are skipped unless the query used `with_trashed`/`only_trashed`.
pub struct Update<M> {
    filters: Vec<Cond>,
    sets: Vec<(&'static str, Assign)>,
    trashed: Trashed,
    scoped: bool,
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
            trashed: Trashed::Exclude,
            scoped: true,
            _model: PhantomData,
        }
    }

    /// Ignore the model's default scope.
    pub fn unscoped(mut self) -> Self {
        self.scoped = false;
        self
    }

    /// Add a `WHERE` condition. Multiple calls are combined with `AND`.
    pub fn filter(mut self, expr: Expr<M>) -> Self {
        self.filters.push(expr.cond);
        self
    }

    /// Also update soft-deleted rows.
    pub fn with_trashed(mut self) -> Self {
        self.trashed = Trashed::Include;
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
            trashed: self.trashed,
            scoped: self.scoped,
            ..Select::new()
        }
    }

    fn is_set(&self, column: &str) -> bool {
        self.sets.iter().any(|(c, _)| *c == column)
    }

    /// Render the `UPDATE` statement.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        self.write_into(&mut sql);
        sql
    }

    pub(crate) fn write_into(&self, sql: &mut Sql) {
        sql.push("UPDATE ").push_ident(M::TABLE).push(" SET ");
        sql.push_list(&self.sets, ", ", |sql, (column, assign)| {
            sql.push_ident(column).push(" = ");
            match assign {
                Assign::Value(v) => sql.bind(v.clone()),
                Assign::Raw(raw, params) => sql.push_raw(raw, params),
            };
        });
        if let Some(updated_at) = M::UPDATED_AT_COLUMN {
            if !self.is_set(updated_at) {
                sql.push(", ").push_ident(updated_at).push(" = now()");
            }
        }
        if let Some(version) = M::VERSION_COLUMN {
            if !self.is_set(version) {
                sql.push(", ")
                    .push_ident(version)
                    .push(" = ")
                    .push_ident(version)
                    .push(" + 1");
            }
        }
        let implicit = implicit::<M>(self.trashed, self.scoped);
        push_where(sql, " WHERE ", self.filters.iter().chain(&implicit));
    }

    fn check(&self) -> Result<()> {
        if self.sets.is_empty() {
            return Err(Error::InvalidQuery(format!(
                "UPDATE on `{}` has no columns to set",
                M::TABLE
            )));
        }
        Ok(())
    }

    /// Run the update and return the number of affected rows.
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        self.check()?;
        exec::execute(executor, &self.to_sql(), &[M::TABLE]).await
    }

    /// Run the update and return the updated rows.
    pub async fn returning<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        self.check()?;
        let mut sql = self.to_sql();
        push_returning::<M>(&mut sql);
        exec::fetch_all(executor, &sql, &[M::TABLE], false).await
    }

    pub(crate) async fn returning_one<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let mut sql = self.to_sql();
        push_returning::<M>(&mut sql);
        exec::fetch_optional(executor, &sql, &[M::TABLE], false).await
    }
}

impl<M> Clone for Update<M> {
    fn clone(&self) -> Self {
        Self {
            filters: self.filters.clone(),
            sets: self.sets.clone(),
            trashed: self.trashed,
            scoped: self.scoped,
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

// ----- ON CONFLICT -------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) enum ConflictTarget {
    None,
    Columns(Vec<&'static str>),
    Constraint(&'static str),
}

#[derive(Debug, Clone)]
pub(crate) enum ConflictAction {
    Nothing,
    Update(Vec<&'static str>),
    UpdateAll,
}

/// An `ON CONFLICT` clause.
#[derive(Debug, Clone)]
pub(crate) struct Conflict {
    pub(crate) target: ConflictTarget,
    pub(crate) action: ConflictAction,
}

impl Conflict {
    /// Write ` ON CONFLICT … DO …` for an insert of `inserted` columns.
    pub(crate) fn write<M: Model>(&self, sql: &mut Sql, inserted: &[&'static str]) -> Result<()> {
        sql.push(" ON CONFLICT");
        let target_columns: &[&str] = match &self.target {
            ConflictTarget::None => &[],
            ConflictTarget::Columns(columns) => {
                sql.push(" (")
                    .push_list(columns, ", ", |sql, c| {
                        sql.push_ident(c);
                    })
                    .push(")");
                columns
            }
            ConflictTarget::Constraint(name) => {
                sql.push(" ON CONSTRAINT ").push_ident(name);
                &[]
            }
        };
        let mut updates: Vec<&'static str> = match &self.action {
            ConflictAction::Nothing => {
                sql.push(" DO NOTHING");
                return Ok(());
            }
            ConflictAction::Update(columns) => columns.clone(),
            ConflictAction::UpdateAll => inserted
                .iter()
                .copied()
                .filter(|c| {
                    !target_columns.contains(c)
                        && *c != M::PRIMARY_KEY
                        && M::CREATED_AT_COLUMN != Some(*c)
                })
                .collect(),
        };
        if matches!(self.target, ConflictTarget::None) {
            return Err(Error::InvalidQuery(
                "ON CONFLICT DO UPDATE needs conflict columns or a constraint".into(),
            ));
        }
        // Managed columns are maintained, never copied from the new row.
        updates.retain(|c| M::VERSION_COLUMN != Some(*c) && M::UPDATED_AT_COLUMN != Some(*c));
        sql.push(" DO UPDATE SET ");
        let mut first = true;
        let mut sep = |sql: &mut Sql| {
            if !std::mem::take(&mut first) {
                sql.push(", ");
            }
        };
        for column in &updates {
            sep(sql);
            sql.push_ident(column)
                .push(" = EXCLUDED.")
                .push_ident(column);
        }
        if let Some(updated_at) = M::UPDATED_AT_COLUMN {
            sep(sql);
            sql.push_ident(updated_at).push(" = now()");
        }
        if let Some(version) = M::VERSION_COLUMN {
            sep(sql);
            sql.push_ident(version)
                .push(" = ")
                .push_ident(M::TABLE)
                .push(".")
                .push_ident(version)
                .push(" + 1");
        }
        if first {
            // Nothing to change, but `DO UPDATE` still returns the row.
            sql.push_ident(M::PRIMARY_KEY)
                .push(" = ")
                .push_ident(M::TABLE)
                .push(".")
                .push_ident(M::PRIMARY_KEY);
        }
        Ok(())
    }
}

macro_rules! on_conflict_methods {
    () => {
        /// Handle conflicts on these columns (a unique index or the primary
        /// key). Follow with [`do_nothing`](Self::do_nothing),
        /// [`do_update`](Self::do_update) or
        /// [`do_update_all`](Self::do_update_all).
        pub fn on_conflict(mut self, columns: impl IntoIterator<Item = Column<M>>) -> Self {
            self.conflict_target = Some(ConflictTarget::Columns(
                columns.into_iter().map(|c| c.name()).collect(),
            ));
            self
        }

        /// Handle conflicts on the named constraint.
        pub fn on_constraint(mut self, name: &'static str) -> Self {
            self.conflict_target = Some(ConflictTarget::Constraint(name));
            self
        }

        /// `ON CONFLICT … DO NOTHING`: skip conflicting rows. Without
        /// [`on_conflict`](Self::on_conflict), any conflict is ignored.
        pub fn do_nothing(mut self) -> Self {
            self.conflict_action = Some(ConflictAction::Nothing);
            self
        }

        /// `ON CONFLICT … DO UPDATE` setting these columns from the new row.
        pub fn do_update(mut self, columns: impl IntoIterator<Item = Column<M>>) -> Self {
            self.conflict_action = Some(ConflictAction::Update(
                columns.into_iter().map(|c| c.name()).collect(),
            ));
            self
        }

        /// `ON CONFLICT … DO UPDATE` setting every inserted column except the
        /// conflict columns, the primary key and `created_at`. With
        /// [`on_constraint`](Self::on_constraint) the constraint's columns
        /// aren't known, so they are rewritten with their (equal) new values.
        pub fn do_update_all(mut self) -> Self {
            self.conflict_action = Some(ConflictAction::UpdateAll);
            self
        }

        fn conflict(&self) -> Result<Option<Conflict>> {
            match (&self.conflict_target, &self.conflict_action) {
                (None, None) => Ok(None),
                (target, Some(action)) => Ok(Some(Conflict {
                    target: target.clone().unwrap_or(ConflictTarget::None),
                    action: action.clone(),
                })),
                (Some(_), None) => Err(Error::InvalidQuery(
                    "`on_conflict` needs `do_nothing`, `do_update` or `do_update_all`".into(),
                )),
            }
        }
    };
}

/// An `INSERT` of a single row built column by column, created with
/// [`Model::create`]. Columns you don't set get their database default;
/// timestamp columns default to `now()`.
pub struct Insert<M> {
    /// `(column, raw sql, params)`; plain values are stored as `"?"`.
    values: Vec<(&'static str, String, Vec<Value>)>,
    conflict_target: Option<ConflictTarget>,
    conflict_action: Option<ConflictAction>,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Insert<M> {
    pub(crate) fn new() -> Self {
        Self {
            values: Vec::new(),
            conflict_target: None,
            conflict_action: None,
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

    on_conflict_methods!();

    /// Render the `INSERT` statement. An incomplete `ON CONFLICT`
    /// configuration is left out here and reported by `exec`.
    pub fn to_sql(&self) -> Sql {
        self.build(false).expect("lenient rendering never fails")
    }

    fn build(&self, strict: bool) -> Result<Sql> {
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
        let conflict = match self.conflict() {
            Ok(conflict) => conflict,
            Err(e) if strict => return Err(e),
            Err(_) => None,
        };
        if let Some(conflict) = conflict {
            let inserted: Vec<_> = values.iter().map(|(c, _, _)| *c).collect();
            let mut with_conflict = sql.clone();
            match conflict.write::<M>(&mut with_conflict, &inserted) {
                Ok(()) => sql = with_conflict,
                Err(e) if strict => return Err(e),
                Err(_) => {}
            }
        }
        push_returning::<M>(&mut sql);
        Ok(sql)
    }

    /// Run the insert and return the stored row. With
    /// [`do_nothing`](Self::do_nothing), a skipped row is an
    /// [`Error::NotFound`]; use [`exec_optional`](Self::exec_optional).
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        self.exec_optional(executor)
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }

    /// Run the insert and return the stored row, or `None` if it was
    /// skipped by `ON CONFLICT … DO NOTHING`.
    pub async fn exec_optional<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        exec::fetch_optional(executor, &self.build(true)?, &[M::TABLE], false).await
    }
}

impl<M> Clone for Insert<M> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            conflict_target: self.conflict_target.clone(),
            conflict_action: self.conflict_action.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: Model> fmt::Debug for Insert<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Insert").field(&self.to_sql()).finish()
    }
}

/// An `INSERT` of whole records, created with [`Model::insert_many`].
/// Generated columns are skipped and timestamps are set to `now()`.
pub struct InsertMany<'a, M> {
    records: &'a [M],
    conflict_target: Option<ConflictTarget>,
    conflict_action: Option<ConflictAction>,
}

impl<'a, M: Model> InsertMany<'a, M> {
    pub(crate) fn new(records: &'a [M]) -> Self {
        Self {
            records,
            conflict_target: None,
            conflict_action: None,
        }
    }

    on_conflict_methods!();

    /// Render the `INSERT` statement. An incomplete `ON CONFLICT`
    /// configuration is left out here and reported by `exec`.
    pub fn to_sql(&self) -> Sql {
        let conflict = self.conflict().ok().flatten();
        crate::model::insert_sql(self.records, false, conflict.as_ref())
            .or_else(|_| crate::model::insert_sql(self.records, false, None))
            .expect("rendering without ON CONFLICT never fails")
    }

    fn build(&self) -> Result<Sql> {
        crate::model::insert_sql(self.records, false, self.conflict()?.as_ref())
    }

    /// Run the insert and return the stored rows. Rows skipped by
    /// `ON CONFLICT … DO NOTHING` are not returned.
    ///
    /// Every record is validated and passed to `before_insert` first; each
    /// returned row is passed to `after_insert`.
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        if self.records.is_empty() {
            return Ok(Vec::new());
        }
        for record in self.records {
            crate::model::pre_insert(record)?;
        }
        let rows: Vec<M> = exec::fetch_all(executor, &self.build()?, &[M::TABLE], false).await?;
        for row in &rows {
            row.after_insert()?;
        }
        Ok(rows)
    }
}

impl<M> Clone for InsertMany<'_, M> {
    fn clone(&self) -> Self {
        Self {
            records: self.records,
            conflict_target: self.conflict_target.clone(),
            conflict_action: self.conflict_action.clone(),
        }
    }
}

impl<M: Model> fmt::Debug for InsertMany<'_, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("InsertMany").field(&self.to_sql()).finish()
    }
}

#[doc(hidden)]
pub fn __paginate_sql<M: Model>(select: &Select<M>, page: u64, per_page: u64) -> Sql {
    select.paginate_sql(page, per_page)
}

#[doc(hidden)]
pub fn __cursor_sql<M: Model>(
    select: Select<M>,
    after: Option<&Cursor>,
    limit: u64,
) -> Result<Sql> {
    Ok(select.cursor_select(after, limit)?.to_sql())
}
