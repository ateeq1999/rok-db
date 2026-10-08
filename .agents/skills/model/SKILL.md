---
name: model
description: Change rok-db's `#[derive(Model)]` macro (crates/rok-db-macros) or the `Model` trait (crates/rok-db-core/src/model.rs).
---

# Models and the derive

Where things live:

- `crates/rok-db-macros/src/lib.rs`: `#[derive(Model)]`, `#[derive(FromRow)]`, `#[rok_db::test]`
  (`expand` parses `#[rok(..)]` and generates the impls).
- `crates/rok-db-core/src/model.rs`: the `Model` trait, `Hooks`, record operations.

Rules for the derive:

1. Generated code uses the resolved crate path (`#krate`: `#[rok(crate = "..")]`, else
   `crate_path()` finds `rok-db` or `rok-db-core` in the user's manifest) and absolute paths
   (`::core::option::Option`), so it works in any module and when re-exported (rok-ui uses
   `crate = "rok_ui::db::rok_db"`).
2. Reach sqlx and other dependencies through `#krate::__private`, never by name: users don't
   depend on sqlx directly.
3. Unknown or misplaced attributes are `syn::Error`s with the span of the attribute and a list
   of what is expected. Turn them into `to_compile_error()` at the entry point.
4. Keep `COLUMNS` in field order without `skip` fields: `FromRow`, `from_row_at` (positional
   decoding for joined tuples), `values()` and inserts all rely on it.
5. A new `Model` trait item gets a default in the trait when possible, so hand-written impls
   keep compiling; the derive overrides it.
6. Generated public items have doc comments (users compile with `missing_docs`).

New attribute checklist:

- Parse it in `expand` and reject invalid combinations (e.g. `generated` on `skip`).
- Document it in the derive's rustdoc and in the README attribute table.
- Tests: generated constants and SQL in `crates/rok-db/tests/sql.rs`, behaviour against
  Postgres, and a `compile_fail` doctest for each new compile error.
- Check that it composes with composite keys, soft deletes, timestamps, versions, tenants and
  joins (columns are table-qualified in joined queries).
