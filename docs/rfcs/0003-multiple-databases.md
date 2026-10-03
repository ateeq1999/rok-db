- Feature name: `multiple_databases`
- Start date: 2026-10-03
- Status: **Draft** — awaiting maintainer decisions on the open questions
- RFC PR: to be assigned when this RFC's pull request is opened
- Tracking issue: to be opened on acceptance

# Summary

Support SQLite and MySQL/MariaDB in addition to PostgreSQL, behind Cargo
features, without making PostgreSQL users pay for the abstraction.

# Motivation

SQLite is the natural choice for tests, CLIs, desktop and embedded apps;
MySQL/MariaDB remain common in existing systems. sqlx already supports both,
so most of the driver work exists. What doesn't exist is a rok-db core that
isn't hard-wired to PostgreSQL.

# Where rok-db is PostgreSQL-specific today

| Area | PostgreSQL construct | SQLite | MySQL |
|---|---|---|---|
| Binding / rows | `PgArguments`, `PgRow`, `$n` | `SqliteArguments`, `?` | `MySqlArguments`, `?` |
| Identifiers | `"name"` | `"name"` | `` `name` `` |
| `RETURNING` | yes | 3.35+ | MariaDB only |
| Upsert | `ON CONFLICT … DO UPDATE` | same syntax | `ON DUPLICATE KEY UPDATE` |
| Pagination total | `LEFT JOIN LATERAL` | — | 8.0.14+ |
| De-duplicated joins | `DISTINCT ON` | — | — |
| Version-checked writes | data-modifying CTE | — | — |
| `ILIKE`, arrays, JSONB operators, full-text search | yes | partial (JSON1) | partial (JSON) |
| `COPY`, `LISTEN`/`NOTIFY`, replicas, audit triggers | yes | no | no / different |

# Guide-level explanation

```toml
rok-db = { version = "0.2", features = ["sqlite"] }   # or "mysql"; "postgres" stays the default
```

```rust
let db = Db::connect("sqlite://app.db").await?;       // Db<Sqlite>
User::find(&db, 1).await?;                            // same model API
User::filter(User::EMAIL.ilike("%@x.io"));            // compile error on Sqlite: ILIKE is Postgres-only
```

# Reference-level explanation

## A `Backend` trait

```rust
pub trait Backend: sqlx::Database {
    const DIALECT: Dialect;                  // placeholders, quoting, feature flags
    fn bind(value: Value, args: &mut Self::Arguments<'_>) -> Result<()>;
}
```

- `Db<B: Backend = Postgres>`, `Tx<B>`, `Select<M>` rendering through a
  `Dialect` (placeholder style, identifier quoting, upsert syntax).
- `Model` gains a backend parameter with a default: `Model<B = Postgres>:
  for<'r> FromRow<'r, B::Row>`. `#[derive(Model)]` implements it for every
  enabled backend.
- PostgreSQL-only APIs (`ilike`, arrays, JSONB, search, `copy_in`,
  `listen`, replicas, `audit`, tenancy's upsert guard) get `B = Postgres`
  bounds, so misuse is a compile error rather than a runtime error.
- Features that need emulation pick a portable fallback per dialect:
  - `paginate`: two queries (count + page) where `LATERAL` is missing.
  - Version-checked `save`: `UPDATE … WHERE version = ?`, then an existence
    probe if 0 rows (two round trips, in a transaction).
  - `RETURNING` on MySQL: insert, then select by `LAST_INSERT_ID()` / key.
  - Joins that multiply rows: `WHERE pk IN (SELECT …)` instead of `DISTINCT ON`.

## Phasing

1. **Refactor** (no new backends): introduce `Backend`/`Dialect` with only
   PostgreSQL; the public API stays source compatible thanks to defaulted
   parameters. All existing tests must pass unchanged.
2. **SQLite**, run in CI against in-memory databases.
3. **MySQL/MariaDB**, run in CI against both servers.

# Drawbacks

- A generic parameter on `Db`, `Tx` and `Model` makes errors noisier and
  generic user code more verbose, even with defaults.
- Every new feature must state its backend support; the test matrix triples.
- Emulations (two-query pagination, MySQL `RETURNING`) have different
  performance and consistency characteristics that users must understand.

# Rationale and alternatives

- **`sqlx::Any`**: one runtime-selected driver, but it loses type
  information and PostgreSQL-specific types, the core of rok-db.
- **Runtime `Dialect` enum without generics**: simpler types, but turns
  "unsupported on this database" into runtime errors.
- **Separate crates per backend** (`rok-db-sqlite`): no generics, but
  duplicated model derives and APIs that drift apart.

# Prior art

Diesel (backend type parameter, per-backend features), SeaORM (runtime
`DatabaseBackend` plus `sea-query` dialects), sqlx (`Database` trait).

# Compatibility

Phase 1 aims to be source compatible but is a minor-version bump (0.y).
Behaviour on PostgreSQL must not change; SQL snapshots in tests guard this.
MSRV unchanged.

# Unresolved questions

1. Generic backend parameter (proposed) or a runtime dialect?
2. Should SQLite land first (most requested for testing), or MySQL?
3. Are emulated features acceptable, or should unsupported features simply
   not exist on that backend?

# Future possibilities

CockroachDB / YugabyteDB (PostgreSQL wire protocol with dialect quirks),
DuckDB for analytics.
