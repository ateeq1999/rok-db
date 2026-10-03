use std::fmt;
use std::marker::PhantomData;

use sqlx::Row;

use crate::expr::Cond;
use crate::model::{push_columns, push_returning};
use crate::sql::Sql;
use crate::{Column, Error, Executor, Expr, Model, Order, Page, Result, Value};

pub(crate) async fn fetch_all<'e, M: Model, E: Executor<'e>>(
    executor: E,
    sql: Sql,
) -> Result<Vec<M>> {
    let args = sql.arguments()?;
    Ok(sqlx::query_as_with::<_, M, _>(sql.as_str(), args)
        .fetch_all(executor)
        .await?)
}

pub(crate) async fn fetch_optional<'e, M: Model, E: Executor<'e>>(
    executor: E,
    sql: Sql,
) -> Result<Option<M>> {
    let args = sql.arguments()?;
    Ok(sqlx::query_as_with::<_, M, _>(sql.as_str(), args)
        .fetch_optional(executor)
        .await?)
}

async fn execute<'e, E: Executor<'e>>(executor: E, sql: Sql) -> Result<u64> {
    let args = sql.arguments()?;
    Ok(sqlx::query_with(sql.as_str(), args)
        .execute(executor)
        .await?
        .rows_affected())
}

fn push_where(sql: &mut Sql, filters: &[Cond]) {
    if filters.is_empty() {
        return;
    }
    sql.push(" WHERE ");
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
        self.write_select(&mut sql);
        sql
    }

    fn write_select(&self, sql: &mut Sql) {
        sql.push("SELECT ");
        push_columns::<M>(sql);
        sql.push(" FROM ").push_ident(M::TABLE);
        push_where(sql, &self.filters);
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
        push_where(sql, &self.filters);
    }

    /// Fetch every matching row.
    pub async fn all<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        fetch_all(executor, self.to_sql()).await
    }

    /// Fetch the first matching row, or `None`.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        fetch_optional(executor, self.limit(1).to_sql()).await
    }

    /// Fetch the first matching row, failing with [`Error::NotFound`].
    pub async fn one<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        self.first(executor)
            .await?
            .ok_or_else(|| Error::not_found::<M>(None))
    }

    /// Count the matching rows (ignores ordering, limit and offset).
    pub async fn count<'e, E: Executor<'e>>(self, executor: E) -> Result<i64> {
        let mut sql = Sql::new();
        sql.push("SELECT COUNT(*)");
        self.write_from_where(&mut sql);
        let args = sql.arguments()?;
        Ok(sqlx::query_scalar_with(sql.as_str(), args)
            .fetch_one(executor)
            .await?)
    }

    /// `true` if at least one row matches.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        let mut sql = Sql::new();
        sql.push("SELECT EXISTS(SELECT 1");
        self.write_from_where(&mut sql);
        sql.push(")");
        let args = sql.arguments()?;
        Ok(sqlx::query_scalar_with(sql.as_str(), args)
            .fetch_one(executor)
            .await?)
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
        let sql = self.paginate_sql(page, per_page);
        let args = sql.arguments()?;
        let rows = sqlx::query_with(sql.as_str(), args)
            .fetch_all(executor)
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
        self.write_from_where(&mut sql);
        self.write_order(&mut sql, "");
        let offset = (page - 1).saturating_mul(per_page);
        sql.push(&format!(" LIMIT {per_page} OFFSET {offset}) p ON TRUE"));
        self.write_order(&mut sql, "p.");
        sql
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

    /// `DELETE` every matching row and return how many were deleted.
    pub async fn delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        execute(executor, self.delete_sql()).await
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

/// A bulk `UPDATE` over model `M`, created with [`Model::update_all`] or
/// [`Select::update`].
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
    /// Post::update_all().set_raw(Post::UPDATED_AT, "now()", [] as [i32; 0])
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
        push_where(&mut sql, &self.filters);
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
        execute(executor, self.checked_sql()?).await
    }

    /// Run the update and return the updated rows.
    pub async fn returning<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        let mut sql = self.checked_sql()?;
        push_returning::<M>(&mut sql);
        fetch_all(executor, sql).await
    }

    pub(crate) async fn returning_one<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        let mut sql = self.checked_sql()?;
        push_returning::<M>(&mut sql);
        fetch_optional(executor, sql).await
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
/// [`Model::create`]. Columns you don't set get their database default.
pub struct Insert<M> {
    values: Vec<(&'static str, Value)>,
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
        self.values.push((column.name(), value.into()));
        self
    }

    /// Render the `INSERT` statement.
    pub fn to_sql(&self) -> Sql {
        let mut sql = Sql::new();
        sql.push("INSERT INTO ").push_ident(M::TABLE);
        if self.values.is_empty() {
            sql.push(" DEFAULT VALUES");
        } else {
            sql.push(" (")
                .push_list(&self.values, ", ", |sql, (c, _)| {
                    sql.push_ident(c);
                })
                .push(") VALUES (")
                .push_list(&self.values, ", ", |sql, (_, v)| {
                    sql.bind(v.clone());
                })
                .push(")");
        }
        push_returning::<M>(&mut sql);
        sql
    }

    /// Run the insert and return the stored row.
    pub async fn exec<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        fetch_optional(executor, self.to_sql())
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
