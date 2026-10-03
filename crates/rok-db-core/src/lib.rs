//! Core runtime of [rok-db](https://docs.rs/rok-db).
//!
//! Most users should depend on the `rok-db` crate, which re-exports
//! everything here together with the `#[derive(Model)]` macro.

mod cache;
mod context;
mod cursor;
mod db;
mod error;
mod exec;
mod expr;
mod model;
mod page;
mod query;
mod raw;
pub mod relation;
mod sql;
#[cfg(feature = "testing")]
pub mod testing;
pub mod validate;
mod value;

pub use cache::{CacheStats, QueryCache};
pub use context::Executor;
pub use cursor::{Cursor, CursorPage};
pub use db::{Db, DbBuilder, Isolation, Retryable, Tx, TxOptions};
pub use error::{Error, Result};
pub use expr::{Column, Direction, Expr, IntoProjections, Order, Projection};
pub use model::{Hooks, Model};
pub use page::Page;
pub use query::{Insert, InsertMany, Memoized, Projected, Select, Update};
pub use raw::{Raw, raw};
pub use relation::{BelongsTo, HasMany, HasOne};
pub use sql::Sql;
pub use validate::ValidationErrors;
pub use value::{CustomType, CustomValue, Value};

/// A boxed `Send` future, as returned by [`Db::transaction`] closures.
pub use futures_core::future::BoxFuture;
/// A boxed `Send` stream, as returned by [`Select::stream`].
pub use futures_core::stream::BoxStream;
/// Extension traits for consuming streams (`try_next`, `try_collect`, …).
pub use futures_util::{StreamExt, TryStreamExt};

/// The sqlx crate rok-db is built on, re-exported for advanced use.
pub use sqlx;

#[doc(hidden)]
pub mod __private {
    pub use crate::context::Context;
    pub use crate::model::__insert_sql;
    pub use crate::query::{__cursor_sql, __paginate_sql};
    pub use sqlx::encode::IsNull;
    pub use sqlx::error::BoxDynError;
    pub use sqlx::postgres::{PgArgumentBuffer, PgRow, PgTypeInfo, PgValueRef};
    pub use sqlx::{Decode, Encode, Error as SqlxError, FromRow, Postgres, Row, Type};
}
