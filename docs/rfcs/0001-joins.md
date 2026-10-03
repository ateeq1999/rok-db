- Feature name: `joins`
- Start date: 2026-10-03
- RFC PR: to be assigned when this RFC's pull request is opened
- Tracking issue: to be opened on acceptance

# Summary

Add typed `JOIN`s to the query builder, so a query can filter, order and
select across related tables in one statement while keeping rok-db's
guarantees: columns are tied to their model, identifiers are quoted and
values are always bound parameters.

# Motivation

Today a query reads one table. Cross-table needs are covered by:

- relations (`User::POSTS.load(..)`): one extra query per relation, great for
  loading object graphs, but no filtering or ordering *by* the related table;
- subqueries (`in_subquery`, `Expr::exists` + `eq_outer`): filtering by a
  related table, but no columns from it in the result;
- raw SQL: anything, without type safety.

Common cases that still need raw SQL:

1. "Posts with their author's name, ordered by author name" — select and order
   by columns of another table.
2. Reporting: `GROUP BY` a column of one table while aggregating another
   (posts per category name).
3. Filtering by several related tables at once, where nested `EXISTS`
   subqueries become hard to read.

# Guide-level explanation

A join is declared from a relation or from two columns:

```rust
// From a relation declared with #[rok(belongs_to = User)] on Post::author_id
let rows: Vec<(String, String)> = Post::query()
    .join(Post::AUTHOR)                      // INNER JOIN "users" ON "users"."id" = "posts"."author_id"
    .filter(User::ROLE.eq("admin"))          // columns of joined models are allowed
    .order_by(User::NAME.asc())
    .select((Post::TITLE, User::NAME))
    .fetch_all(&db)
    .await?;

// Ad-hoc join on any two columns
Post::query()
    .left_join(Category::ID.on(Post::CATEGORY_ID))
    .group_by(Category::NAME)
    .select((Category::NAME, Projection::count_all()))
    .fetch_all(&db)
    .await?;
```

Generated SQL qualifies every column once a query has joins:

```sql
SELECT "posts"."title", "users"."name"
FROM "posts"
INNER JOIN "users" ON "users"."id" = "posts"."author_id"
WHERE "users"."role" = $1 AND "users"."deleted_at" IS NULL
ORDER BY "users"."name" ASC
```

Fetching whole models (`.all()`) on a joined query still returns the root
model (`Vec<Post>`), so joins can be used purely for filtering and ordering.

# Reference-level explanation

## Type-level tracking of joined models

`Select<M>` gains a second type parameter listing the joined models:
`Select<M, J = ()>`. `join` returns `Select<M, (J, N)>`. Methods that accept
expressions take `Expr<X>` where `X: InScope<(M, J)>`, a trait implemented for
`M` and for every model in the `J` list:

```rust
pub trait InScope<Scope, Index> {}
impl<M, J> InScope<(M, J), Here> for M {}
impl<M, N, J, I> InScope<(M, (J, N)), There<I>> for X where X: InScope<(M, J), I> {}
```

The `Index` parameter is inferred, the same technique `frunk` uses for
`HList` lookups. It is invisible to users and keeps error messages
reasonable ("`Comment` is not part of this query").

Because the default `J = ()` keeps today's `Select<M>` unchanged, existing code
and signatures compile as before.

## Rendering

- Every `Column`, `Order`, `Projection` and `Cond` already carries its column
  name; with joins the renderer prefixes the model's table
  (`"posts"."title"`). The table comes from `M::TABLE` captured when the
  expression is built (as `eq_outer` does today).
- Self-joins (the same table twice) need aliases and are **out of scope** for
  this RFC; they remain possible with raw SQL.
- Soft-delete scopes of joined models are added to the `ON` clause, so
  `LEFT JOIN` keeps parent rows without live children.

## API

| Method | SQL |
|---|---|
| `join(relation)` / `join(a.on(b))` | `INNER JOIN` |
| `left_join(..)` | `LEFT JOIN`; joined columns may be `NULL` (decode into `Option<T>`) |
| `ColumnA.on(ColumnB)` | builds an ad-hoc join condition |

`BelongsTo`, `HasMany` and `HasOne` implement `IntoJoin<M>`. A `HasMany` join
multiplies rows, which is expected for filtering; `.all()` on such a query adds
`DISTINCT` on the root model's primary key to avoid duplicate models.

## Interaction with existing features

- **Pagination**: `paginate` and `cursor_paginate` work on joined queries;
  ordering columns may come from joined models (the cursor stores their values).
- **Memoization**: the cache key already includes the SQL. Invalidation must
  cover every joined table, so `Memoized` records all of them.
- **Updates/deletes**: `update()`/`delete()` on joined queries render
  `UPDATE … FROM` / `DELETE … USING`. Proposed as a follow-up, not part of
  the first implementation.

# Drawbacks

- A second type parameter on `Select` makes signatures and errors more complex
  for users who write generic code over queries.
- Qualified rendering touches every SQL generator; risk of regressions (mitigated
  by the existing SQL-shape tests, which must stay byte-for-byte identical for
  join-free queries).

# Rationale and alternatives

- **Untyped joins** (`join_raw("JOIN users ON …")`): trivial to build, but
  loses column checking, the main reason to use rok-db. Could still be offered
  as an escape hatch.
- **Runtime scope checking** (validate columns at `to_sql`): simpler types,
  but turns compile errors into runtime errors.
- **Only relation joins**: simpler, but reporting queries often join on columns
  that aren't modelled as relations.

# Prior art

- Diesel: fully typed joins via `joinable!`/`allow_tables_to_appear_in_same_query!`;
  very safe, but famously complex error messages.
- SeaORM: typed `JoinType` + `RelationDef`, results via `find_also_related`.
- ActiveRecord / Ecto: `joins(:author)` / `join(:inner, [p], u in assoc(p, :author))`,
  untyped or loosely typed.

The proposal aims for Diesel-level safety with ActiveRecord-level ergonomics by
building on the relations rok-db already generates.

# Compatibility

Additive. `Select<M>` keeps its meaning through the defaulted parameter. No
change to SQL generated for existing queries. No MSRV impact.

# Unresolved questions

1. Should `.all()` on a `has_many` join add `DISTINCT` automatically, or
   require an explicit `.distinct()`?
2. How should joined models be returned when users want them, e.g. `Vec<(Post, User)>`?
   This needs prefixed column aliases, so it could be split into its own RFC.
3. Naming: `left_join` versus `join_optional`.

# Future possibilities

- Self-joins with aliases (`User::alias("manager")`).
- `UPDATE … FROM` / `DELETE … USING`.
- Eager loading built on joins for `belongs_to` (one query instead of two).
