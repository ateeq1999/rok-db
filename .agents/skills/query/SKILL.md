---
name: query
description: Change rok-db query building, SQL rendering or execution (crates/rok-db-core: query.rs, expr.rs, sql.rs, join.rs, exec.rs, cache.rs).
---

# Queries and SQL

Pipeline: builders (`Select`, `Update`, `Insert`, `Joined`) collect `Cond`/`Term`/`Order`
values (`expr.rs`) -> render into `Sql` (`sql.rs`) -> run through `exec.rs`, which handles
logging, metrics, replica routing and cache invalidation in one place.

SQL safety (never negotiable):

- Identifiers go through `Sql::push_ident` / `push_column` (quoted, schema-aware). Values go
  through `Sql::bind`. Raw fragments only via `push_raw` with `?` placeholders bound to
  parameters.
- Never `format!` a value into SQL. Table and column names come from `Model` constants,
  never from runtime strings.

Rendering rules:

- Columns are table-qualified only when `Sql::qualify` is set (joined queries); `new_sql()`
  sets it. JSON, array and search helpers always qualify.
- Implicit filters are applied for every query: soft deletes, `default_scope`, tenancy
  (fail-closed when no tenant is set). Joined models apply theirs in the `ON` clause
  (`join_scope`). A new query shape must keep all three.
- Render inside the returned future (`async move { let sql = ...; }`), not when the future is
  created: the tenant and context are task-locals read at render time.
- Joins that can repeat root rows (`multiplies()`) de-duplicate with `DISTINCT ON` the root
  key; counts use `COUNT(DISTINCT ..)`. Bulk writes through joins use `key_subselect()`.

Execution and caching:

- Every write passes the tables it touches to `exec::execute` so the query cache
  invalidates them; memoized reads stamp every table they read (`Select::tables()`).
- Public futures are `Send`; streams are `BoxStream<'e, Result<T>>`.

Tests for every change:

- SQL shape: assert the exact `to_sql()` string and parameters (`tests/sql.rs`, or the
  feature's file such as `tests/joins.rs`).
- Behaviour against Postgres with `#[rok_db::test]` (a fresh database per test) and seeded
  rows. Cover soft-deleted rows, `LEFT JOIN` misses and empty results.
- Type-level guarantees as `compile_fail` doctests in `crates/rok-db/src/lib.rs`.
