//! Core runtime of [rok-db](https://docs.rs/rok-db).
//!
//! Most users should depend on the `rok-db` crate, which re-exports
//! everything here together with the `#[derive(Model)]` macro.

mod db;
mod error;
mod expr;
mod model;
mod page;
mod query;
mod raw;
mod sql;
mod value;

pub use db::{Db, DbBuilder, Tx};
pub use error::{Error, Result};
pub use expr::{Column, Direction, Expr, Order};
pub use model::Model;
pub use page::Page;
pub use query::{Insert, Select, Update};
pub use raw::{Raw, raw};
pub use sql::Sql;
pub use value::Value;

/// Anything that can run a query: [`&Db`](Db), `&mut Tx`, `&PgPool` or
/// `&mut PgConnection`.
pub use sqlx::postgres::PgExecutor as Executor;

/// A boxed `Send` future, as returned by [`Db::transaction`] closures.
pub use futures_core::future::BoxFuture;

/// The sqlx crate rok-db is built on, re-exported for advanced use.
pub use sqlx;

#[doc(hidden)]
pub mod __private {
    pub use crate::model::__insert_sql;
    pub use crate::query::__paginate_sql;
    pub use sqlx::postgres::PgRow;
    pub use sqlx::{Error as SqlxError, FromRow, Row};
}
