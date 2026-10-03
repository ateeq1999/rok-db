---
name: check
description: Run the same checks CI runs on rok-db before committing or opening a pull request.
---

# Local checks

Start PostgreSQL once (the `DATABASE_URL` user needs `CREATEDB` for `#[rok_db::test]`):

```sh
docker run -d --name rok-db-pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=rok_db_test \
    -p 5432:5432 postgres:18
export DATABASE_URL=postgres://postgres:postgres@localhost/rok_db_test
```

Then, from the repository root (fast to slow):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings          # default features
cargo test --workspace --all-features
cargo test --workspace --no-default-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo +1.85 check --workspace --all-targets --all-features      # MSRV
cargo deny check                                                # licenses and advisories
cargo run -p rok-db --example blog                              # needs DATABASE_URL
```

Notes:

- Without `DATABASE_URL` the database tests return early and pass. A test binary finishing in
  0.00s did not touch Postgres: export the variable and run again.
- Feature-gated code (`chrono`, `uuid`, `json`, `migrate`, `serde`, `axum`, `metrics`,
  `testing`) must build alone: `cargo clippy -p rok-db-core --no-default-features --features
  <feature> -- -D warnings` for each feature you touched.
- Compile-time guarantees are `compile_fail` doctests in `crates/rok-db/src/lib.rs`
  (`__compile_checks`). Run them with `cargo test -p rok-db --doc --all-features`. Pair each
  `compile_fail` with a passing example so it fails for the right reason.
- A killed test run can leave `rok_test_*` databases behind. List them with
  `psql "$DATABASE_URL" -Atc "select datname from pg_database where datname like 'rok_test_%'"`
  and drop them.
- Then review the diff with the `quality` skill.
