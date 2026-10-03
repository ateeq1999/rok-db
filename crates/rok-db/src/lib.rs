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
//! | `full`    | all of the above                                     |

pub use rok_db_core::*;

/// Derive [`Model`] for a struct. See the [crate docs](crate) for an example.
pub use rok_db_macros::Model;

/// Everything you need for day-to-day use: `use rok_db::prelude::*;`
pub mod prelude {
    pub use crate::{Column, Db, Executor, Expr, Model, Page, Tx};
}
