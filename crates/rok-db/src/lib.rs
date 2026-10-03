//! # rok-db
//!
//! An ergonomic, type-safe async ORM for PostgreSQL, built on [`sqlx`].
//!
//! ```no_run
//! use rok_db::prelude::*;
//!
//! #[derive(Debug, Clone, Model)]
//! struct User {
//!     #[rok(primary_key, generated)]
//!     id: i64,
//!     email: String,
//!     name: Option<String>,
//!     age: i32,
//! }
//!
//! # async fn run() -> rok_db::Result<()> {
//! let db = Db::connect("postgres://postgres@localhost/app").await?;
//!
//! // Create
//! let ann = User { id: 0, email: "ann@example.com".into(), name: None, age: 31 }
//!     .insert(&db)
//!     .await?;
//!
//! // Read with typed, model-scoped columns
//! let adults = User::filter(User::AGE.gte(18))
//!     .filter(User::EMAIL.ends_with("@example.com"))
//!     .order_by(User::AGE.desc())
//!     .limit(10)
//!     .all(&db)
//!     .await?;
//!
//! // Update
//! let mut ann = User::find_or_fail(&db, ann.id).await?;
//! ann.name = Some("Ann".into());
//! let ann = ann.save(&db).await?;
//!
//! // Bulk update & delete
//! User::filter(User::AGE.lt(18)).update().set(User::NAME, None::<String>).exec(&db).await?;
//! User::filter(User::EMAIL.is_null()).delete(&db).await?;
//!
//! // Pagination (one round trip)
//! let page = User::order_by(User::ID).paginate(&db, 1, 20).await?;
//! println!("{} of {} users", page.len(), page.total);
//!
//! // Transactions
//! db.transaction(|tx| Box::pin(async move {
//!     ann.delete(&mut *tx).await?;
//!     Ok::<_, rok_db::Error>(())
//! })).await?;
//! # Ok(()) }
//! ```
//!
//! ## Defining models
//!
//! `#[derive(Model)]` implements [`Model`] and `sqlx::FromRow`, and adds one
//! typed [`Column`] constant per field (`User::EMAIL`, …). See
//! [`derive@Model`] for every attribute.
//!
//! ## Relations
//!
//! `#[rok(belongs_to = User)]`, `#[rok(has_many(posts = Post::USER_ID))]` and
//! `#[rok(has_one(..))]` generate relation constants ([`BelongsTo`],
//! [`HasMany`], [`HasOne`]) and lazy query methods. Eager-load related
//! records for many parents with one extra query: see [`relation`].
//!
//! ## More features
//!
//! - **Streaming**: [`Select::stream`] yields rows one at a time.
//! - **Aggregates & projections**: [`Select::sum`], [`Select::avg`],
//!   [`Select::group_by`], [`Select::having`] and [`Select::select`] with
//!   [`Projection`]s, decoded into tuples or [`derive@FromRow`] structs.
//! - **Timestamps**: `#[rok(timestamps)]` manages `created_at`/`updated_at`.
//! - **Keyset pagination**: [`Select::cursor_paginate`] with opaque
//!   [`Cursor`]s.
//! - **Soft deletes**: `#[rok(soft_delete)]`, [`Select::with_trashed`],
//!   [`Model::restore`], [`Model::force_delete`].
//! - **Optimistic locking**: `#[rok(version)]`; stale writes fail with
//!   [`Error::Conflict`].
//! - **Upserts**: [`Model::upsert_on`], [`Model::insert_many`] and
//!   [`Insert::on_conflict`] with `do_nothing`/`do_update`/`do_update_all`.
//! - **Subqueries**: [`Column::in_subquery`], [`Expr::exists`] and
//!   [`Column::eq_outer`] for correlation.
//! - **Custom column types**: [`derive@DbEnum`], [`derive@DbNewtype`] and
//!   [`impl_value!`].
//! - **Validation & hooks**: `#[rok(validate(…))]` rules ([`validate`]) and
//!   the [`Hooks`] trait.
//! - **Scopes**: `#[rok(default_scope = …)]`, [`Select::scope`],
//!   [`Select::unscoped`].
//! - **Retrying transactions**: [`Db::transaction_with`] with [`TxOptions`].
//! - **Memoization**: [`Select::memoize`] caches results in the pool's
//!   [`QueryCache`] with automatic invalidation on writes.
//! - **Query logging**: every statement is logged through `tracing`
//!   (`rok_db::query` at DEBUG, `rok_db::slow_query` at WARN; see
//!   [`DbBuilder::slow_query_threshold`]).
//!
//! ## Executors
//!
//! Every query method accepts any [`Executor`]: `&Db`, `&mut Tx` (write
//! `&mut *tx` inside a transaction closure), or a plain sqlx `&PgPool` /
//! `&mut PgConnection` — so rok-db slots into existing sqlx code.
//!
//! ## Raw SQL
//!
//! When the builder isn't enough, [`raw()`] runs any SQL with `?` placeholders
//! and decodes into models or any `FromRow` type; [`Expr::raw`] embeds a raw
//! condition in a builder query. Every builder has a `to_sql()` method for
//! inspecting the generated statement.
//!
//! ## Cargo features
//!
//! | feature   | enables                                              |
//! |-----------|------------------------------------------------------|
//! | `chrono`  | `chrono` date/time column types                      |
//! | `uuid`    | `uuid::Uuid` columns                                 |
//! | `json`    | `serde_json::Value` and `sqlx::types::Json<T>` columns |
//! | `migrate` | [`Db::migrate`] for running sqlx migrations          |
//! | `testing` | [`macro@test`] and [`testing::TestDb`]: a temporary database per test |
//! | `full`    | all of the above                                     |

pub use rok_db_core::*;

/// Derive [`Model`] for a struct. See the [crate docs](crate) for an example.
pub use rok_db_macros::Model;

/// Derive `sqlx::FromRow` for a plain struct, without depending on sqlx.
pub use rok_db_macros::FromRow;

/// Derive a column type for a field-less enum (stored as `TEXT` or a
/// PostgreSQL enum).
pub use rok_db_macros::DbEnum;

/// Derive a column type for a single-field tuple struct.
pub use rok_db_macros::DbNewtype;

/// Run an async test against a fresh, temporary database (feature `testing`).
#[cfg(feature = "testing")]
pub use rok_db_macros::test;

/// Everything you need for day-to-day use: `use rok_db::prelude::*;`
pub mod prelude {
    pub use crate::{
        Column, Db, DbEnum, DbNewtype, Executor, Expr, Model, Page, Projection, Select, Tx,
    };
    pub use futures_util_reexports::*;

    mod futures_util_reexports {
        pub use crate::{StreamExt as _, TryStreamExt as _};
    }
}
