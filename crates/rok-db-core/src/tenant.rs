//! Row-level multi-tenancy for models with a `#[rok(tenant)]` column.
//!
//! ```ignore
//! #[derive(Model)]
//! struct Invoice { id: i64, #[rok(tenant)] org_id: i64, total: i64 }
//!
//! rok_db::tenant::with_tenant(42_i64, async {
//!     Invoice::all(&db).await?;                 // … WHERE "org_id" = 42
//!     new_invoice.insert(&db).await?;           // org_id is written as 42
//!     Ok::<_, rok_db::Error>(())
//! }).await?;
//! ```
//!
//! Inside [`with_tenant`], every query, count, update, delete and record
//! operation on a tenant model is restricted to the current tenant, and
//! inserts (including `upsert`, `insert_many` and `copy_in`) write it.
//!
//! Outside a tenant scope, tenant models **fail closed**: queries match no
//! rows (with a warning) unless you opt out explicitly with
//! [`Select::all_tenants`](crate::Select::all_tenants) /
//! [`Update::all_tenants`](crate::Update::all_tenants), e.g. for admin
//! tools. Inserts outside a scope write the record's own value.
//!
//! The tenant is task-local: futures spawned with `tokio::spawn` don't
//! inherit it — wrap them in [`with_tenant`] too. Raw SQL is never filtered.

use std::future::Future;

use crate::Value;

tokio::task_local! {
    static TENANT: Value;
}

/// Run `future` with `tenant` as the current tenant.
pub async fn with_tenant<F: Future>(tenant: impl Into<Value>, future: F) -> F::Output {
    TENANT.scope(tenant.into(), future).await
}

/// The current tenant, if inside [`with_tenant`].
pub fn current() -> Option<Value> {
    TENANT.try_with(Value::clone).ok()
}
