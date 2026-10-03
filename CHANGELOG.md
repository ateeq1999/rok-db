# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Keyset pagination: `Select::cursor_paginate` returning a `CursorPage`
  with an opaque, URL-safe `Cursor` (mixed sort directions, primary-key
  tiebreaker).
- Soft deletes: `#[rok(soft_delete)]` / `#[rok(deleted_at)]`; queries,
  counts, relations and subqueries skip deleted rows; `with_trashed`,
  `only_trashed`, `restore`, `force_delete` and `Model::is_trashed`.
- Optimistic locking: `#[rok(version)]`; `save` and `delete` detect stale
  records in one round trip and fail with `Error::Conflict`
  (`Error::is_conflict`); bulk updates and upserts increment the version.
- Configurable upserts: `Model::upsert_on`, `Model::insert_many` and
  `Insert::{on_conflict, on_constraint, do_nothing, do_update,
  do_update_all, exec_optional}`.
- Subqueries: `Column::in_subquery`, `Column::not_in_subquery`,
  `Expr::exists`, `Expr::not_exists` and `Column::eq_outer` for correlated
  subqueries.
- RFC 0001 proposing typed joins.

- Relations: `#[rok(belongs_to = …)]`, `#[rok(has_many(…))]` and
  `#[rok(has_one(…))]` generate `BelongsTo`/`HasMany`/`HasOne` constants
  and lazy query methods; `load`/`load_from` eager-load related records for
  many parents with one query.
- Streaming with `Select::stream`, `Projected::stream` and `Raw::stream`.
- Aggregates and projections: `Select::sum/avg/min/max`, `group_by`,
  `having`, `select(…)` with `Projection` (`count`, `count_distinct`, `sum`,
  `avg`, `min`, `max`, `cast`, `alias`, `raw`) into tuples or structs.
- `#[derive(FromRow)]` for decoding into plain structs without depending on
  sqlx.
- Automatic timestamps with `#[rok(timestamps)]`, `#[rok(created_at)]` and
  `#[rok(updated_at)]`.
- Memoized queries: `Select::memoize(ttl)` backed by a per-pool
  `QueryCache` (`DbBuilder::query_cache`), invalidated by writes made through
  rok-db, including transactions; `Raw::invalidates` for raw writes.
- Query logging through `tracing` (`rok_db::query`, `rok_db::slow_query`,
  `rok_db::cache`) and `DbBuilder::slow_query_threshold`.
- `Model::value_of`, `Insert::set_raw`, `DbBuilder::build_with_pool`.

- `#[derive(Model)]` with `table`, `primary_key`, `generated`, `column`,
  `skip`, `no_from_row` and `crate` attributes; generates `Model`, `FromRow`
  and typed column constants.
- `Model` CRUD: `find`, `find_or_fail`, `find_many`, `all`, `count`,
  `insert`, `insert_all`, `save`, `upsert`, `delete`, `reload`.
- Query builders: `Select` (filters, ordering, limit/offset, row locking,
  `all`/`first`/`one`/`count`/`exists`, single-round-trip `paginate`),
  bulk `Update` (`set`, `set_raw`, `increment`, `decrement`, `returning`),
  bulk delete and `Insert`.
- Typed, model-scoped `Column`s and composable `Expr`s, including
  `Expr::raw`.
- `Db` / `DbBuilder` / `Tx` connection handles usable as sqlx executors;
  `Db::transaction` with automatic commit and rollback.
- `raw()` queries with `?` or `$n` placeholders.
- `Error` helpers: `is_not_found`, `is_unique_violation`,
  `is_foreign_key_violation`, `constraint`.
- Optional `chrono`, `uuid`, `json` and `migrate` features.
- Open source project files: dual MIT/Apache-2.0 license, contributing guide,
  governance, security policy, Code of Conduct, RFC process, issue and pull
  request templates, CI, Dependabot and cargo-deny configuration.

### Changed

- `Executor` is now a rok-db trait (implemented for `&Db`, `&mut Tx`,
  `&PgPool`, `&mut PgConnection` and `&mut PgListener`) so executors can
  carry pool settings such as the query cache.
- Renamed the crates from `orm`, `orm-core` and `orm-macros` to `rok-db`,
  `rok-db-core` and `rok-db-macros`.

[Unreleased]: https://github.com/ateeq1999/rok-db/commits/main
