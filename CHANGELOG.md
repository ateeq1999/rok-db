# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

- Renamed the crates from `orm`, `orm-core` and `orm-macros` to `rok-db`,
  `rok-db-core` and `rok-db-macros`.

[Unreleased]: https://github.com/ateeq1999/rok-db/commits/main
