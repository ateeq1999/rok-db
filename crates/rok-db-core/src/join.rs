//! Typed joins (RFC 0001).
//!
//! ```ignore
//! // Join through a relation, filter and order by the joined model, select columns of both.
//! let rows: Vec<(String, String)> = Post::query()
//!     .join(Post::AUTHOR)                       // INNER JOIN "users" ON "users"."id" = "posts"."author_id"
//!     .filter(User::ROLE.eq("admin"))           // columns of joined models are allowed
//!     .order_by(User::NAME.asc())
//!     .select((Post::TITLE, User::NAME))
//!     .fetch_all(&db)
//!     .await?;
//!
//! // Ad-hoc joins on any two columns; LEFT JOIN keeps rows without a match.
//! let per_category: Vec<(Option<String>, i64)> = Post::query()
//!     .left_join(Category::ID.on(Post::CATEGORY_ID))
//!     .group_by(Category::NAME)
//!     .select((Category::NAME, Post::ID.count()))
//!     .fetch_all(&db)
//!     .await?;
//!
//! // Fetching root models: one row per post even through `has_many` joins.
//! let users_with_hits: Vec<User> = User::query()
//!     .join(User::POSTS)
//!     .filter(Post::VIEWS.gt(1000))
//!     .all(&db)
//!     .await?;
//! ```
//!
//! The compiler checks that every column used belongs to the root model or
//! a joined one. Joined models' tenant, soft-delete and default scopes are
//! applied in the `ON` clause. Joins that may produce several rows per root
//! row (`has_many` and ad-hoc joins) are de-duplicated with `DISTINCT ON`
//! the root key when fetching root models; counts use `COUNT(DISTINCT …)`.
//!
//! Projections that don't name a model, like `Projection::count_all()`,
//! need one in a joined `select`: write `Projection::<Post>::count_all()`
//! or a column aggregate such as `Post::ID.count()`.
//!
//! Through joins you can also `cursor_paginate`, `memoize` (invalidated by
//! writes to any joined table), bulk `update` / `delete` / `restore`, and
//! fetch whole models as tuples with [`Joined::all_with`].
//!
//! Not supported: self-joins (the same table twice).

use std::fmt;
use std::marker::PhantomData;

use futures_core::stream::BoxStream;
use sqlx::postgres::PgRow;

use crate::expr::Cond;
use crate::query::{JoinClause, join_scope};
use crate::relation::{BelongsTo, HasMany, HasOne};
use crate::{
    Column, Executor, Expr, Model, Order, Page, Projected, Projection, Result, Select, Sql, Update,
};

/// Index markers proving a model is part of a joined query. Inferred by the
/// compiler; you never write them.
#[doc(hidden)]
pub mod scope {
    use std::marker::PhantomData;

    /// The root model.
    #[derive(Debug)]
    pub struct Here;
    /// The most recently joined model.
    #[derive(Debug)]
    pub struct Last;
    /// A model joined earlier.
    #[derive(Debug)]
    pub struct There<I>(PhantomData<I>);
}

use scope::{Here, Last, There};

/// `Self` is the root or one of the joined models of the query scope `S`
/// (`(Root, Joins)`, with `Joins` = `()` or `(EarlierJoins, Model)`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not part of this query",
    label = "join `{Self}` first (`.join(…)` / `.left_join(…)`)"
)]
pub trait InScope<S, I> {}

// The root is found by walking back to the empty join list, so every model
// has exactly one proof (self-joins, which would make it ambiguous, are not
// supported).
impl<M> InScope<(M, ()), Here> for M {}
impl<M, Rest, N> InScope<(M, (Rest, N)), Last> for N {}
impl<X, M, Rest, N, I> InScope<(M, (Rest, N)), There<I>> for X where X: InScope<(M, Rest), I> {}

/// The `ON` condition of a join (opaque).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct JoinCondition(pub(crate) Cond);

/// Something that can be joined: a relation constant or `ColumnA.on(ColumnB)`.
pub trait IntoJoin {
    /// The model already in the query.
    type From: Model;
    /// The model being joined.
    type To: Model;
    #[doc(hidden)]
    fn condition(&self) -> JoinCondition;
    #[doc(hidden)]
    fn multiplies(&self) -> bool;
}

fn eq_columns(
    left: (&'static str, &'static str),
    right: (&'static str, &'static str),
) -> JoinCondition {
    JoinCondition(Cond::Columns {
        left,
        op: "=",
        right,
    })
}

impl<C: Model, P: Model> IntoJoin for BelongsTo<C, P> {
    type From = C;
    type To = P;
    fn condition(&self) -> JoinCondition {
        eq_columns(
            (P::TABLE, P::PRIMARY_KEY),
            (C::TABLE, self.foreign_key().name()),
        )
    }
    fn multiplies(&self) -> bool {
        false
    }
}

impl<P: Model, C: Model> IntoJoin for HasMany<P, C> {
    type From = P;
    type To = C;
    fn condition(&self) -> JoinCondition {
        eq_columns(
            (C::TABLE, self.foreign_key().name()),
            (P::TABLE, P::PRIMARY_KEY),
        )
    }
    fn multiplies(&self) -> bool {
        true
    }
}

impl<P: Model, C: Model> IntoJoin for HasOne<P, C> {
    type From = P;
    type To = C;
    fn condition(&self) -> JoinCondition {
        eq_columns(
            (C::TABLE, self.foreign_key().name()),
            (P::TABLE, P::PRIMARY_KEY),
        )
    }
    fn multiplies(&self) -> bool {
        false
    }
}

/// An ad-hoc join condition `to = from`, created with [`Column::on`].
pub struct JoinOn<From, To> {
    to: &'static str,
    from: &'static str,
    _models: PhantomData<fn() -> (From, To)>,
}

impl<From: Model, To: Model> IntoJoin for JoinOn<From, To> {
    type From = From;
    type To = To;
    fn condition(&self) -> JoinCondition {
        eq_columns((To::TABLE, self.to), (From::TABLE, self.from))
    }
    /// Unknown cardinality: assume it may multiply rows.
    fn multiplies(&self) -> bool {
        true
    }
}

impl<From, To> fmt::Debug for JoinOn<From, To> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinOn")
            .field("to", &self.to)
            .field("from", &self.from)
            .finish()
    }
}

impl<To: Model> Column<To> {
    /// An ad-hoc join condition: join `To` where this column equals `other`
    /// (a column of a model already in the query).
    ///
    /// ```ignore
    /// Post::query().join(Category::ID.on(Post::CATEGORY_ID))
    /// ```
    pub fn on<From: Model>(self, other: Column<From>) -> JoinOn<From, To> {
        JoinOn {
            to: self.name(),
            from: other.name(),
            _models: PhantomData,
        }
    }
}

/// A value usable in a joined `select(…)`: a column or projection of some
/// model in the query.
pub trait ScopedItem {
    /// The model the item belongs to.
    type Model;
    #[doc(hidden)]
    fn into_projection(self) -> Projection<Self::Model>;
}

impl<X: Model> ScopedItem for Column<X> {
    type Model = X;
    fn into_projection(self) -> Projection<X> {
        self.into()
    }
}

impl<X> ScopedItem for Projection<X> {
    type Model = X;
    fn into_projection(self) -> Projection<X> {
        self
    }
}

/// The select list of a joined query: one [`ScopedItem`] or a tuple of up
/// to 8, each from a model in the query scope `S`.
pub trait ScopedProjections<S, I> {
    #[doc(hidden)]
    fn into_items<R>(self) -> Vec<Projection<R>>;
}

impl<S, I, A> ScopedProjections<S, (I,)> for A
where
    A: ScopedItem,
    A::Model: InScope<S, I>,
{
    fn into_items<R>(self) -> Vec<Projection<R>> {
        vec![self.into_projection().retag()]
    }
}

macro_rules! scoped_tuples {
    ($(($($t:ident $i:ident),+)),+ $(,)?) => {$(
        #[allow(non_snake_case)]
        impl<S, $($i,)+ $($t),+> ScopedProjections<S, ($($i,)+)> for ($($t,)+)
        where
            $($t: ScopedItem, $t::Model: InScope<S, $i>,)+
        {
            fn into_items<R>(self) -> Vec<Projection<R>> {
                let ($($t,)+) = self;
                vec![$($t.into_projection().retag()),+]
            }
        }
    )+};
}

scoped_tuples! {
    (A IA, B IB),
    (A IA, B IB, C IC),
    (A IA, B IB, C IC, D ID),
    (A IA, B IB, C IC, D ID, E IE),
    (A IA, B IB, C IC, D ID, E IE, F IF),
    (A IA, B IB, C IC, D ID, E IE, F IF, G IG),
    (A IA, B IB, C IC, D ID, E IE, F IF, G IG, H IH),
}

/// Joined models fetched next to the root model with [`Joined::all_with`]:
/// a model `N`, `Option<N>` (`None` when a `LEFT JOIN` found no match), or
/// a tuple of up to 8 of those, each part of the query scope `S`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't be fetched from this query",
    label = "fetch joined models, `Option`s of them or tuples of those"
)]
pub trait JoinedModels<S, I> {
    /// What each row decodes to.
    type Output;
    #[doc(hidden)]
    fn write_columns(sql: &mut Sql);
    #[doc(hidden)]
    fn decode(row: &PgRow, offset: &mut usize) -> Result<Self::Output>;
}

impl<S, I, N: Model + InScope<S, I>> JoinedModels<S, I> for N {
    type Output = N;
    fn write_columns(sql: &mut Sql) {
        sql.push(", ");
        crate::model::push_columns::<N>(sql);
    }
    fn decode(row: &PgRow, offset: &mut usize) -> Result<N> {
        let model = N::from_row_at(row, *offset)?;
        *offset += N::COLUMNS.len();
        Ok(model)
    }
}

impl<S, I, N: Model + InScope<S, I>> JoinedModels<S, I> for Option<N> {
    type Output = Option<N>;
    fn write_columns(sql: &mut Sql) {
        <N as JoinedModels<S, I>>::write_columns(sql);
    }
    fn decode(row: &PgRow, offset: &mut usize) -> Result<Option<N>> {
        use sqlx::{Row, ValueRef};
        let mut missing = true;
        for key in N::PRIMARY_KEYS {
            let at = N::COLUMNS.iter().position(|c| c == key).unwrap_or(0);
            missing &= row.try_get_raw(*offset + at)?.is_null();
        }
        if missing {
            *offset += N::COLUMNS.len();
            return Ok(None);
        }
        <N as JoinedModels<S, I>>::decode(row, offset).map(Some)
    }
}

macro_rules! joined_model_tuples {
    ($(($($t:ident $i:ident),+)),+ $(,)?) => {$(
        impl<S, $($i,)+ $($t),+> JoinedModels<S, ($($i,)+)> for ($($t,)+)
        where
            $($t: JoinedModels<S, $i>,)+
        {
            type Output = ($($t::Output,)+);
            fn write_columns(sql: &mut Sql) {
                $($t::write_columns(sql);)+
            }
            fn decode(row: &PgRow, offset: &mut usize) -> Result<Self::Output> {
                Ok(($($t::decode(row, offset)?,)+))
            }
        }
    )+};
}

joined_model_tuples! {
    (A IA, B IB),
    (A IA, B IB, C IC),
    (A IA, B IB, C IC, D ID),
    (A IA, B IB, C IC, D ID, E IE),
    (A IA, B IB, C IC, D ID, E IE, F IF),
    (A IA, B IB, C IC, D ID, E IE, F IF, G IG),
    (A IA, B IB, C IC, D ID, E IE, F IF, G IG, H IH),
}

/// A `SELECT` over root model `M` with joined models `J`, created with
/// [`Select::join`] or [`Select::left_join`]. See the [module docs](self).
pub struct Joined<M, J> {
    select: Select<M>,
    _joins: PhantomData<fn() -> J>,
}

impl<M: Model> Select<M> {
    /// `INNER JOIN` a related model (see [`join`](crate::join)).
    pub fn join<R, I>(self, join: R) -> Joined<M, ((), R::To)>
    where
        R: IntoJoin,
        R::From: InScope<(M, ()), I>,
    {
        Joined::<M, ()>::root(self).push_join(join, false)
    }

    /// `LEFT JOIN` a related model: root rows without a match are kept,
    /// with `NULL`s for the joined model's columns.
    pub fn left_join<R, I>(self, join: R) -> Joined<M, ((), R::To)>
    where
        R: IntoJoin,
        R::From: InScope<(M, ()), I>,
    {
        Joined::<M, ()>::root(self).push_join(join, true)
    }
}

impl<M: Model, J> Joined<M, J> {
    fn from_select<J2>(select: Select<M>) -> Joined<M, J2> {
        Joined {
            select,
            _joins: PhantomData,
        }
    }

    fn root(select: Select<M>) -> Joined<M, ()> {
        Joined {
            select,
            _joins: PhantomData,
        }
    }

    fn push_join<R: IntoJoin, J2>(mut self, join: R, left: bool) -> Joined<M, J2> {
        self.select.joins.push(JoinClause {
            left,
            table: <R::To as Model>::TABLE,
            on: join.condition().0,
            scope: join_scope::<R::To>,
            multiplies: join.multiplies(),
        });
        Joined::<M, J>::from_select(self.select)
    }

    /// `INNER JOIN` another model related to one already in the query.
    pub fn join<R, I>(self, join: R) -> Joined<M, (J, R::To)>
    where
        R: IntoJoin,
        R::From: InScope<(M, J), I>,
    {
        self.push_join(join, false)
    }

    /// `LEFT JOIN` another model related to one already in the query.
    pub fn left_join<R, I>(self, join: R) -> Joined<M, (J, R::To)>
    where
        R: IntoJoin,
        R::From: InScope<(M, J), I>,
    {
        self.push_join(join, true)
    }

    /// Add a `WHERE` condition on any model in the query.
    pub fn filter<X, I>(mut self, expr: Expr<X>) -> Self
    where
        X: InScope<(M, J), I>,
    {
        self.select = self.select.filter(expr.retag());
        self
    }

    /// Append an `ORDER BY` term on any model in the query.
    pub fn order_by<X, I>(mut self, order: impl Into<Order<X>>) -> Self
    where
        X: InScope<(M, J), I>,
    {
        self.select = self.select.order_by(order.into().retag::<M>());
        self
    }

    /// Append a `GROUP BY` column of any model in the query.
    pub fn group_by<X, I>(mut self, column: Column<X>) -> Self
    where
        X: Model + InScope<(M, J), I>,
    {
        self.select.group_by.push((X::TABLE, column.name()));
        self
    }

    /// Add a `HAVING` condition.
    pub fn having<X, I>(mut self, expr: Expr<X>) -> Self
    where
        X: InScope<(M, J), I>,
    {
        self.select.having.push(expr.retag::<M>().cond);
        self
    }

    /// Limit the number of returned rows.
    pub fn limit(mut self, limit: u64) -> Self {
        self.select = self.select.limit(limit);
        self
    }

    /// Skip the first `offset` rows.
    pub fn offset(mut self, offset: u64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    /// Include soft-deleted root rows.
    pub fn with_trashed(mut self) -> Self {
        self.select = self.select.with_trashed();
        self
    }

    /// Run on the primary even when read replicas are configured.
    pub fn on_primary(mut self) -> Self {
        self.select = self.select.on_primary();
        self
    }

    /// Select columns or aggregates from any model in the query, decoded
    /// into tuples or a `FromRow` type with [`Projected::fetch_all`] & co.
    pub fn select<P, I>(self, items: P) -> Projected<M>
    where
        P: ScopedProjections<(M, J), I>,
    {
        Projected {
            select: self.select,
            items: items.into_items(),
        }
    }

    /// Turn into a bulk `UPDATE` of the matching root rows (`SET` root
    /// columns only):
    ///
    /// ```ignore
    /// Post::query().join(Post::AUTHOR).filter(User::BANNED.eq(true))
    ///     .update().set(Post::HIDDEN, true).exec(&db).await?;
    /// ```
    ///
    /// Rendered as `UPDATE posts SET … WHERE id IN (SELECT posts.id FROM
    /// posts JOIN … WHERE …)`.
    pub fn update(self) -> Update<M> {
        self.select.key_subselect().update()
    }

    /// Delete the matching root rows (soft delete for soft-delete models).
    pub async fn delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        self.select.key_subselect().delete(executor).await
    }

    /// Permanently delete the matching root rows.
    pub async fn force_delete<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        self.select.key_subselect().force_delete(executor).await
    }

    /// Restore the matching soft-deleted root rows (combine with
    /// [`with_trashed`](Self::with_trashed)).
    pub async fn restore<'e, E: Executor<'e>>(self, executor: E) -> Result<u64> {
        self.select.key_subselect().restore(executor).await
    }

    /// Cache this query's results (see [`Select::memoize`]); writes to the
    /// root **or any joined table** invalidate them.
    pub fn memoize(self, ttl: std::time::Duration) -> crate::Memoized<M> {
        self.select.memoize(ttl)
    }

    #[doc(hidden)]
    pub fn into_select(self) -> Select<M> {
        self.select
    }

    /// Render the `SELECT` of root models.
    pub fn to_sql(&self) -> Sql {
        self.select.to_sql()
    }

    /// Fetch the matching root models (one per root row).
    pub async fn all<'e, E: Executor<'e>>(self, executor: E) -> Result<Vec<M>> {
        self.select.all(executor).await
    }

    /// Fetch the first matching root model.
    pub async fn first<'e, E: Executor<'e>>(self, executor: E) -> Result<Option<M>> {
        self.select.first(executor).await
    }

    /// Fetch the first matching root model, failing with `NotFound`.
    pub async fn one<'e, E: Executor<'e>>(self, executor: E) -> Result<M> {
        self.select.one(executor).await
    }

    /// Count matching root rows (distinct root keys for multiplying joins).
    pub async fn count<'e, E: Executor<'e>>(self, executor: E) -> Result<i64> {
        self.select.count(executor).await
    }

    /// `true` if anything matches.
    pub async fn exists<'e, E: Executor<'e>>(self, executor: E) -> Result<bool> {
        self.select.exists(executor).await
    }

    /// One page of root models with the total count, in one round trip.
    pub async fn paginate<'e, E: Executor<'e>>(
        self,
        executor: E,
        page: u64,
        per_page: u64,
    ) -> Result<Page<M>> {
        self.select.paginate(executor, page, per_page).await
    }

    /// Fetch the root models together with joined ones (RFC 0004): one
    /// tuple per joined row, so a `has_many` join repeats the root model.
    ///
    /// ```ignore
    /// let rows: Vec<(Post, User)> = Post::query()
    ///     .join(Post::AUTHOR)
    ///     .all_with::<User, _>(&db)
    ///     .await?;
    /// let rows: Vec<(Post, (User, Option<Category>))> = Post::query()
    ///     .join(Post::AUTHOR)
    ///     .left_join(Category::ID.on(Post::CATEGORY_ID))
    ///     .all_with::<(User, Option<Category>), _>(&db)
    ///     .await?;
    /// ```
    pub async fn all_with<'e, T, I>(
        self,
        executor: impl Executor<'e>,
    ) -> Result<Vec<(M, T::Output)>>
    where
        T: JoinedModels<(M, J), I>,
    {
        let sql = self.with_sql::<T, I>();
        let rows = crate::exec::fetch_rows(executor, &sql, &[], self.select.use_replica()).await?;
        rows.iter()
            .map(|row| {
                let root = M::from_row_at(row, 0)?;
                let mut offset = M::COLUMNS.len();
                Ok((root, T::decode(row, &mut offset)?))
            })
            .collect()
    }

    /// Render the statement of [`all_with`](Self::all_with).
    pub fn with_sql<T, I>(&self) -> Sql
    where
        T: JoinedModels<(M, J), I>,
    {
        let mut sql = self.select.new_sql();
        sql.push("SELECT ");
        crate::model::push_columns::<M>(&mut sql);
        T::write_columns(&mut sql);
        self.select.write_tail(&mut sql);
        sql
    }

    /// Keyset pagination (see [`Select::cursor_paginate`]); sort keys may
    /// come from joined models, and must be `NOT NULL`.
    pub async fn cursor_paginate<'e, E: Executor<'e>>(
        self,
        executor: E,
        after: Option<&crate::Cursor>,
        limit: u64,
    ) -> Result<crate::CursorPage<M>> {
        self.select.cursor_paginate(executor, after, limit).await
    }

    /// Stream the matching root models.
    pub fn stream<'e, E: Executor<'e> + 'e>(self, executor: E) -> BoxStream<'e, Result<M>> {
        self.select.stream(executor)
    }
}

impl<M, J> Clone for Joined<M, J> {
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            _joins: PhantomData,
        }
    }
}

impl<M: Model, J> fmt::Debug for Joined<M, J> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Joined").field(&self.to_sql()).finish()
    }
}
