- Feature name: `joined-tuples`
- Start date: 2026-10-03
- Status: **Accepted** (2026-10-03): approved by the maintainer as part of lifting the
  join limitations (follow-up to [RFC 0001](0001-joins.md), decision 2)
- RFC PR: to be assigned when this RFC's pull request is opened
- Tracking issue: to be opened

# Summary

Fetch the root model of a joined query together with joined models in one query,
for example `Vec<(Post, User)>` or `Vec<(Post, (User, Option<Category>))>`.

# Motivation

Joins (RFC 0001) can filter, order and group across models, and can select individual
columns. But a common need is "each post with its author": today that takes two queries
(`all` and then a relation loader), or a hand-written tuple of columns that has to be kept
in sync with the structs.

# Guide-level explanation

```rust
let rows: Vec<(Post, User)> = Post::query()
    .join(Post::AUTHOR)
    .filter(User::ROLE.eq("admin"))
    .all_with::<User, _>(&db)
    .await?;

// Several models: a tuple. `Option<N>` is `None` when a LEFT JOIN found no match.
let rows: Vec<(Post, (User, Option<Category>))> = Post::query()
    .join(Post::AUTHOR)
    .left_join(Category::ID.on(Post::CATEGORY_ID))
    .all_with::<(User, Option<Category>), _>(&db)
    .await?;
```

The second type parameter (`_`) is the compiler's proof that every requested model has
been joined. Requesting a model that isn't part of the query is a compile error.

One tuple is returned per joined row. Unlike `all`, a `has_many` join *repeats* the root
model, once for each match. That is the point: `User::query().left_join(User::POSTS)`
returns `(User, Option<Post>)` for every post, plus `(user, None)` for users without posts.

# Reference-level explanation

- `JoinedModels<S, I>` is implemented for a model `N: InScope<S, I>` (output `N`), for
  `Option<N>` (output `Option<N>`), and for tuples of 2 to 8 of these. It writes the
  columns of each part and decodes them.
- The statement is `SELECT <root columns>, <part columns>… FROM … JOIN … WHERE …` followed
  by the query's `GROUP BY`, `HAVING`, `ORDER BY`, `LIMIT` and `OFFSET`. Columns are
  table-qualified. There is no de-duplication.
- Models are decoded **by position** with a new hidden `Model::from_row_at(row, offset)`,
  which the derive generates. Aliasing (`"users__name"`) was rejected because PostgreSQL
  truncates identifiers at 63 bytes, so long table plus column names would silently
  collide. Models with `#[rok(no_from_row)]` get a default that returns a decode error.
- `Option<N>` is `None` when all of `N`'s primary-key columns are `NULL` in the row.
- `with_sql::<T, _>()` renders the statement.

# Drawbacks

- The turbofish needs the inferred index parameter (`::<User, _>`).
- Without a primary key that is `NOT NULL`, a matched row with a `NULL` key would read as
  `None`. Keys can't be `NULL` in PostgreSQL, so this only affects views.

# Rationale and alternatives

- *Return `(M, J)` for the whole join list automatically.* This is less flexible, and it
  forces decoding of models the caller doesn't need.
- *A `.with::<T>()` builder step.* This adds a type but doesn't remove the index parameter.

# Prior art

Diesel's `.select((posts::all_columns, users::all_columns))` and SeaORM's
`find_also_related` / `SelectTwo`.

# Compatibility

The change is additive. `Model::from_row_at` has a default, so hand-written `Model`
impls keep compiling.

# Future possibilities

- `first_with` and `stream_with`.
- Grouping repeated roots into `(User, Vec<Post>)`.
