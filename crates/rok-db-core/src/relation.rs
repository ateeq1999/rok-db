//! Relations between models, declared with `#[derive(Model)]` attributes.
//! The parent model must have a single-column primary key.
//!
//! Every relation can be queried lazily for one record, or eager-loaded for
//! many records with a single extra query (avoiding the N+1 problem):
//!
//! ```ignore
//! #[derive(Model)]
//! #[rok(has_many(posts = Post::USER_ID))]
//! struct User { id: i64, name: String }
//!
//! #[derive(Model)]
//! struct Post { id: i64, #[rok(belongs_to = User)] user_id: i64, title: String }
//!
//! // Lazy: one query for one record.
//! let posts = user.posts().order_by(Post::ID).all(&db).await?;
//! let author = post.user().one(&db).await?;
//!
//! // Eager: one query for all records.
//! let users = User::all(&db).await?;
//! let posts = User::POSTS.load(&db, &users).await?;
//! for user in &users {
//!     println!("{} wrote {} posts", user.name, posts.get(user).len());
//! }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

use crate::{Column, Executor, Model, Result, Select, Value};

/// Hashable identity of a key value; `None` for SQL `NULL`.
fn key(value: Option<Value>) -> Option<String> {
    value.filter(|v| !v.is_null()).map(|v| v.to_string())
}

/// Relations join on a single primary key column.
fn require_single_key<M: Model>() -> Result<()> {
    if M::PRIMARY_KEYS.len() == 1 {
        Ok(())
    } else {
        Err(crate::Error::InvalidQuery(format!(
            "relations need a single-column primary key; `{}` has a composite key",
            M::TABLE
        )))
    }
}

/// Distinct, non-null values of `column` across `records`.
fn distinct_values<M: Model>(records: &[M], column: &str) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    records
        .iter()
        .filter_map(|r| r.value_of(column))
        .filter(|v| !v.is_null() && seen.insert(v.to_string()))
        .collect()
}

/// `C` belongs to `P` through the foreign key column `C.fk → P.pk`.
///
/// Generated as an associated constant by `#[rok(belongs_to = P)]` on the
/// foreign key field (`Post::USER` for a field `user_id`).
pub struct BelongsTo<C, P> {
    foreign_key: &'static str,
    _models: PhantomData<fn() -> (C, P)>,
}

impl<C: Model, P: Model> BelongsTo<C, P> {
    /// Declare the relation through `foreign_key`, a column of `C`.
    pub const fn new(foreign_key: Column<C>) -> Self {
        Self {
            foreign_key: foreign_key.name(),
            _models: PhantomData,
        }
    }

    /// The foreign key column on `C`.
    pub const fn foreign_key(&self) -> Column<C> {
        Column::new(self.foreign_key)
    }

    /// Query the parent of `child` (no rows if the foreign key is `NULL`).
    pub fn query(&self, child: &C) -> Select<P> {
        match child.value_of(self.foreign_key).filter(|v| !v.is_null()) {
            Some(fk) => P::filter(P::primary_key_column().eq(fk)),
            None => P::filter(crate::Expr::none()),
        }
    }

    /// Load the parents of every record in `children` with one query.
    pub async fn load<'e, E: Executor<'e>>(
        &self,
        executor: E,
        children: &[C],
    ) -> Result<One<C, P>> {
        require_single_key::<P>()?;
        let ids = distinct_values(children, self.foreign_key);
        let parents = if ids.is_empty() {
            Vec::new()
        } else {
            P::filter(P::primary_key_column().is_in(ids))
                .all(executor)
                .await?
        };
        let map = parents
            .into_iter()
            .filter_map(|p| key(Some(p.primary_key())).map(|k| (k, p)))
            .collect();
        Ok(One::new(map, self.foreign_key))
    }
}

/// `P` has many `C` through the foreign key column `C.fk → P.pk`.
///
/// Generated as an associated constant by
/// `#[rok(has_many(posts = Post::USER_ID))]` on `P` (`User::POSTS`).
pub struct HasMany<P, C> {
    foreign_key: &'static str,
    _models: PhantomData<fn() -> (P, C)>,
}

impl<P: Model, C: Model> HasMany<P, C> {
    /// Declare the relation through `foreign_key`, a column of `C`.
    pub const fn new(foreign_key: Column<C>) -> Self {
        Self {
            foreign_key: foreign_key.name(),
            _models: PhantomData,
        }
    }

    /// The foreign key column on `C`.
    pub const fn foreign_key(&self) -> Column<C> {
        Column::new(self.foreign_key)
    }

    /// Query the children of `parent`.
    pub fn query(&self, parent: &P) -> Select<C> {
        C::filter(self.foreign_key().eq(parent.primary_key()))
    }

    /// Load the children of every record in `parents` with one query.
    pub async fn load<'e, E: Executor<'e>>(
        &self,
        executor: E,
        parents: &[P],
    ) -> Result<Many<P, C>> {
        require_single_key::<P>()?;
        self.load_from(executor, parents, C::query()).await
    }

    /// Like [`load`](Self::load), starting from `base` to filter or order
    /// the children (`Post::order_by(Post::CREATED_AT.desc())`). Limits
    /// apply to the whole result, not per parent.
    pub async fn load_from<'e, E: Executor<'e>>(
        &self,
        executor: E,
        parents: &[P],
        base: Select<C>,
    ) -> Result<Many<P, C>> {
        require_single_key::<P>()?;
        let ids = distinct_values(parents, P::PRIMARY_KEY);
        let children = if ids.is_empty() {
            Vec::new()
        } else {
            base.filter(self.foreign_key().is_in(ids))
                .all(executor)
                .await?
        };
        let mut map: HashMap<String, Vec<C>> = HashMap::new();
        for child in children {
            if let Some(k) = key(child.value_of(self.foreign_key)) {
                map.entry(k).or_default().push(child);
            }
        }
        Ok(Many::new(map, P::PRIMARY_KEY))
    }
}

/// `P` has at most one `C` through the foreign key column `C.fk → P.pk`.
///
/// Generated as an associated constant by
/// `#[rok(has_one(profile = Profile::USER_ID))]` on `P` (`User::PROFILE`).
pub struct HasOne<P, C> {
    foreign_key: &'static str,
    _models: PhantomData<fn() -> (P, C)>,
}

impl<P: Model, C: Model> HasOne<P, C> {
    /// Declare the relation through `foreign_key`, a column of `C`.
    pub const fn new(foreign_key: Column<C>) -> Self {
        Self {
            foreign_key: foreign_key.name(),
            _models: PhantomData,
        }
    }

    /// The foreign key column on `C`.
    pub const fn foreign_key(&self) -> Column<C> {
        Column::new(self.foreign_key)
    }

    /// Query the child of `parent`.
    pub fn query(&self, parent: &P) -> Select<C> {
        C::filter(self.foreign_key().eq(parent.primary_key()))
    }

    /// Load the child of every record in `parents` with one query.
    pub async fn load<'e, E: Executor<'e>>(&self, executor: E, parents: &[P]) -> Result<One<P, C>> {
        require_single_key::<P>()?;
        let ids = distinct_values(parents, P::PRIMARY_KEY);
        let children = if ids.is_empty() {
            Vec::new()
        } else {
            C::filter(self.foreign_key().is_in(ids))
                .all(executor)
                .await?
        };
        let map = children
            .into_iter()
            .filter_map(|c| key(c.value_of(self.foreign_key)).map(|k| (k, c)))
            .collect();
        Ok(One::new(map, P::PRIMARY_KEY))
    }
}

/// Eager-loaded single related records, keyed by owner. Returned by
/// [`BelongsTo::load`] and [`HasOne::load`].
pub struct One<O, T> {
    map: HashMap<String, T>,
    owner_key: &'static str,
    _owner: PhantomData<fn() -> O>,
}

impl<O: Model, T> One<O, T> {
    fn new(map: HashMap<String, T>, owner_key: &'static str) -> Self {
        Self {
            map,
            owner_key,
            _owner: PhantomData,
        }
    }

    /// The record related to `owner`, if any.
    pub fn get(&self, owner: &O) -> Option<&T> {
        key(owner.value_of(self.owner_key)).and_then(|k| self.map.get(&k))
    }

    /// Remove and return the record related to `owner`.
    pub fn take(&mut self, owner: &O) -> Option<T> {
        key(owner.value_of(self.owner_key)).and_then(|k| self.map.remove(&k))
    }

    /// Number of loaded records.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// `true` if nothing was loaded.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate over the loaded records (in no particular order).
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.map.values()
    }
}

/// Eager-loaded related records grouped by owner. Returned by
/// [`HasMany::load`].
pub struct Many<O, T> {
    map: HashMap<String, Vec<T>>,
    owner_key: &'static str,
    _owner: PhantomData<fn() -> O>,
}

impl<O: Model, T> Many<O, T> {
    fn new(map: HashMap<String, Vec<T>>, owner_key: &'static str) -> Self {
        Self {
            map,
            owner_key,
            _owner: PhantomData,
        }
    }

    /// The records related to `owner` (empty if none).
    pub fn get(&self, owner: &O) -> &[T] {
        key(owner.value_of(self.owner_key))
            .and_then(|k| self.map.get(&k))
            .map_or(&[], Vec::as_slice)
    }

    /// Remove and return the records related to `owner`.
    pub fn take(&mut self, owner: &O) -> Vec<T> {
        key(owner.value_of(self.owner_key))
            .and_then(|k| self.map.remove(&k))
            .unwrap_or_default()
    }

    /// Total number of loaded records.
    pub fn len(&self) -> usize {
        self.map.values().map(Vec::len).sum()
    }

    /// `true` if nothing was loaded.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate over every loaded record (in no particular order).
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.map.values().flatten()
    }
}

macro_rules! relation_boilerplate {
    ($($ty:ident<$a:ident, $b:ident>),*) => {$(
        impl<$a, $b> Clone for $ty<$a, $b> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<$a, $b> Copy for $ty<$a, $b> {}
        impl<$a, $b> fmt::Debug for $ty<$a, $b> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("foreign_key", &self.foreign_key)
                    .finish()
            }
        }
    )*};
}

relation_boilerplate!(BelongsTo<C, P>, HasMany<P, C>, HasOne<P, C>);

impl<O, T: fmt::Debug> fmt::Debug for One<O, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.map.iter()).finish()
    }
}

impl<O, T: fmt::Debug> fmt::Debug for Many<O, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.map.iter()).finish()
    }
}
