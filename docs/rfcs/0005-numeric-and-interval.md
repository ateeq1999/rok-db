- Feature name: `numeric_and_interval`
- Start date: 2026-10-09
- Status: **Accepted** (2026-10-09) by the maintainer, with the recommended answers below
- RFC PR: to be assigned when this RFC's pull request is opened
- Tracking issue: to be opened on acceptance

# Summary

Support PostgreSQL `NUMERIC` (`DECIMAL`) and `INTERVAL` columns in models, filters,
`set`, keyset pagination and `rok-db-gen`:

- `NUMERIC` maps to `rust_decimal::Decimal` behind a new Cargo feature, `decimal`.
- `INTERVAL` maps to sqlx's `PgInterval`, with no feature (sqlx always has it).

# Motivation

- Money, prices, quantities and rates are stored as `NUMERIC` because floats round.
  Today rok-db has no column type for it: a model field of type `Decimal` doesn't
  compile, because `Value` has no `From<Decimal>`. Users can't fix that themselves:
  `impl_value!(rust_decimal::Decimal)` breaks the orphan rule (both the trait and the
  type are foreign to their crate).
- `rok-db-gen` rejects `NUMERIC` and `INTERVAL` columns and asks users to change their
  schema to `DOUBLE PRECISION`, `BIGINT` cents or `TEXT`. That is the most likely
  blocker for adopting the generator on an existing schema.
- `INTERVAL` is common for durations (subscription periods, timeouts, retention), and
  has the same orphan-rule problem.

# Guide-level explanation

```toml
rok-db = { version = "0.5", features = ["decimal", "chrono"] }
```

```rust
use rok_db::prelude::*;
use rok_db::sqlx::types::Decimal;
use rok_db::sqlx::postgres::types::PgInterval;

#[derive(Debug, Clone, Model)]
#[rok(table = "plans")]
pub struct Plan {
    #[rok(primary_key, generated)]
    pub id: i64,
    pub name: String,
    pub price: Decimal,                 // NUMERIC(10, 2) NOT NULL
    pub discount: Option<Decimal>,      // NUMERIC(5, 4)
    pub billing_period: PgInterval,     // INTERVAL NOT NULL
}

let cheap = Plan::query()
    .filter(Plan::PRICE.lt(Decimal::new(1000, 2)))   // price < 10.00
    .order_by(Plan::PRICE.asc())
    .all(&db)
    .await?;

Plan::query()
    .filter(Plan::ID.eq(id))
    .set(Plan::DISCOUNT, Some(Decimal::new(15, 2)))  // 0.15
    .update(&db)
    .await?;
```

With `rok-db-gen`, `price NUMERIC(10, 2) NOT NULL` becomes `pub price:
rok_db::sqlx::types::Decimal` and the generated `Cargo.toml` enables `decimal`;
`INTERVAL` becomes `rok_db::sqlx::postgres::types::PgInterval`. Arrays (`NUMERIC[]`,
`INTERVAL[]`) become `Vec<_>`.

# Reference-level explanation

## rok-db-core and rok-db

- New feature `decimal = ["sqlx/rust_decimal"]` on `rok-db-core`, forwarded by `rok-db`
  and included in `full`. The type is reached as `rok_db::sqlx::types::Decimal`, which
  sqlx re-exports, so users don't add `rust_decimal` themselves unless they want its
  other APIs (same version, via `rok_db::sqlx::types::Decimal`).
- `Value` (already `#[non_exhaustive]`) gains two variants:
  - `Value::Decimal(Option<Decimal>)` under `decimal`;
  - `Value::Interval(Option<PgInterval>)`, always present.
- `From` impls for `T`, `&T`, `Option<T>`, `&Option<T>`, plus `Vec<T>` arrays, like the
  other built-in types. Binding, binary encoding (used by `COPY`) and `is_null` handle
  both variants.
- Keyset pagination decodes `NUMERIC` and `INTERVAL` sort keys
  (`Value::from_row_column`), so `cursor` works when ordering by price.
- Rendered SQL is unchanged: values are bound as parameters (`$1`), and PostgreSQL gets
  the parameter type from the encoder (`NUMERIC`, `INTERVAL`).
- Scopes, tenancy, soft deletes, caching and replicas are unaffected: they only see
  `Value`s.

## rok-db-gen

- `types.rs`: `NUMERIC`/`DECIMAL` (with or without precision and scale) map to
  `rok_db::sqlx::types::Decimal` and record the `decimal` feature. `INTERVAL`, including
  field-restricted spellings (`INTERVAL DAY TO SECOND`, `INTERVAL(3)`), canonicalises to
  `INTERVAL...` and maps to `PgInterval`. Arrays of both are allowed.
- Query parameters and result columns reported by PostgreSQL as `NUMERIC` / `INTERVAL`
  map the same way.
- Migrations and diffs already keep `NUMERIC(p,s)` exactly; changing precision, scale or
  interval fields is an `ALTER COLUMN ... TYPE` step, marked destructive like every type
  change (so `--allow-destructive` is needed, even to widen).

## Limits, documented

- `rust_decimal` holds 28 significant digits. Decoding a larger value, or `NaN`
  (PostgreSQL allows `'NaN'::numeric`), returns a decode error at runtime instead of a
  wrong value.
- `PgInterval` keeps months, days and microseconds separately, as PostgreSQL does. It has
  no arithmetic; convert with `chrono::Duration` / `std::time::Duration` (both encode as
  `INTERVAL`, but can't decode one with months, which is why the column type is
  `PgInterval`).

# Drawbacks

- One more optional dependency (`rust_decimal`, MIT, no `unsafe` beyond its own crate)
  and one more feature to test.
- Two `Value` variants. `Value` is `#[non_exhaustive]`, so this isn't breaking, but
  every `match` in rok-db-core grows.
- `PgInterval` is an sqlx type in our public column types; an sqlx major upgrade can
  change it. We already expose `sqlx::types::*` the same way for chrono, uuid and JSON.

# Rationale and alternatives

- **`bigdecimal` instead of `rust_decimal`.** Arbitrary precision, but heap-allocated,
  not `Copy`, and slower. `rust_decimal` covers money and almost every business use, and
  is SeaORM's default. `bigdecimal` can be added later as a second feature (see Future
  possibilities).
- **Map `NUMERIC` to `String` in the generator.** No dependency, but users would parse
  and format money by hand, and filters would compare text.
- **Only document `BIGINT` cents.** Works for new schemas, not for existing ones, which
  is what the generator is for.
- **A rok-db `Interval` type.** More ergonomic, but another type to maintain for little
  gain; `PgInterval` is exact and already supported by sqlx.

# Prior art

- SeaORM: `Decimal` (`rust_decimal`, feature `with-rust_decimal`) and `BigDecimal`.
- Diesel: `Numeric` to `bigdecimal::BigDecimal`; `Interval` to `PgInterval`.
- sqlx: `rust_decimal` and `bigdecimal` features; `PgInterval` built in.
- Prisma: `Decimal` (decimal.js). ActiveRecord: `BigDecimal`, `ActiveSupport::Duration`.

# Compatibility

Additive. New feature, new `Value` variants on a `#[non_exhaustive]` enum, new `From`
impls for types that had none. The generator now accepts schemas it used to reject.
MSRV stays 1.85 (`rust_decimal` 1.x supports it; the resolver picks a compatible
version).

# Unresolved questions

1. **Which decimal crate?** Recommended: `rust_decimal`, feature named `decimal`;
   `bigdecimal` later if someone needs more than 28 digits.
2. **Should `INTERVAL` need a feature?** Recommended: no. sqlx always compiles
   `PgInterval`, so a feature would only add a switch to remember.
3. **Should `decimal` be on by default?** Recommended: no, like `chrono` and `uuid`;
   `rok-db-gen` enables it in the generated crate when a schema needs it.

# Decisions

Accepted by the maintainer on 2026-10-09:

1. `rust_decimal`, behind a `decimal` feature; `bigdecimal` stays a future possibility.
2. `INTERVAL` needs no feature.
3. `decimal` is opt-in (and part of `full`); `rok-db-gen` enables it when a schema needs it.

Shipped as described, with one addition: `Value::Decimal` and `Value::Interval` also have
cursor encodings, so a keyset cursor over either survives a round trip through its string
form.

# Future possibilities

- A `bigdecimal` feature mapping `NUMERIC` to `BigDecimal` (the generator would choose by
  a `rok-db.toml` setting).
- `MONEY`, network types (`INET`, `CIDR`) and range types (`INT4RANGE`, `TSTZRANGE`) the
  same way.
- Numeric validation rules (`#[rok(validate(min = "0.00"))]`) for decimals.
