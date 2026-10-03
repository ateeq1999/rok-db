# rok-db

[![CI](https://github.com/ateeq1999/rok-db/actions/workflows/ci.yml/badge.svg)](https://github.com/ateeq1999/rok-db/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange.svg)](CONTRIBUTING.md#compatibility-policy)

An ergonomic, type-safe async ORM for PostgreSQL, built on [sqlx](https://github.com/launchbadge/sqlx).

- **One derive** — `#[derive(Model)]` gives you CRUD, a `FromRow` impl and typed column constants.
- **Typed, model-scoped columns** — `User::EMAIL.eq(..)` can't be used to filter `Post`s, and typos are compile errors.
- **One executor story** — every method accepts `&Db`, `&mut Tx`, `&PgPool` or `&mut PgConnection`.
- **`Send` futures everywhere** — works in `tokio::spawn`, axum handlers, etc.
- **Relations without N+1** — `belongs_to`, `has_many`, `has_one` with batched eager loading.
- **Batteries included** — streaming, aggregates and projections, automatic timestamps,
  memoized (cached) queries and structured query logging.
- **Escape hatches** — `Expr::raw`, `rok_db::raw(..)`, `to_sql()` on every builder, and full access to sqlx.

## Install

```toml
[dependencies]
rok-db = { git = "https://github.com/ateeq1999/rok-db", features = ["chrono", "uuid", "json"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| feature   | enables                                                  |
|-----------|----------------------------------------------------------|
| `chrono`  | `DateTime<Utc>`, `NaiveDateTime`, `NaiveDate`, `NaiveTime` |
| `uuid`    | `Uuid` columns                                           |
| `json`    | `serde_json::Value` and `Json<T>` columns                 |
| `migrate` | `Db::migrate("./migrations")`                            |
| `testing` | `#[rok_db::test]`: a temporary database per test        |
| `serde`   | `Serialize` for `Page`, `CursorPage`, `Cursor`, `ValidationErrors` |
| `axum`    | `rok_db::Error` as an HTTP response (implies `serde`)    |
| `metrics` | query, cache and pool metrics via the `metrics` crate    |
| `full`    | all of the above except `testing`                        |

## Quick start

```rust
use rok_db::prelude::*;

#[derive(Debug, Clone, Model)]          // table: "users"
struct User {
    #[rok(primary_key, generated)]      // BIGSERIAL, filled in by Postgres
    id: i64,
    email: String,
    name: Option<String>,
    age: i32,
}

#[tokio::main]
async fn main() -> rok_db::Result<()> {
    let db = Db::connect_env().await?;  // reads DATABASE_URL

    // Create
    let ann = User { id: 0, email: "ann@example.com".into(), name: None, age: 31 }
        .insert(&db)
        .await?;

    // Read
    let user = User::find_or_fail(&db, ann.id).await?;
    let adults = User::filter(User::AGE.gte(18))
        .filter(User::NAME.is_not_null().or(User::EMAIL.ends_with("@example.com")))
        .order_by(User::AGE.desc())
        .limit(10)
        .all(&db)
        .await?;

    // Update
    let mut user = user;
    user.name = Some("Ann".into());
    let user = user.save(&db).await?;

    // Delete
    user.delete(&db).await?;
    Ok(())
}
```

## Defining models

```rust
#[derive(Model)]
#[rok(table = "app.blog_posts")]        // default: snake_case plural of the struct name
struct Post {
    #[rok(primary_key)]                 // default: the field named `id`
    slug: String,
    #[rok(column = "author_id")]        // column name differs from the field
    author: i64,
    #[rok(generated)]                   // DB default / trigger: read but never written
    created_at: chrono::DateTime<chrono::Utc>,
    #[rok(skip)]                        // not a column; Default::default() on load
    rendered_html: String,
}
```

Every column gets a constant named after its field: `Post::SLUG`, `Post::AUTHOR`, `Post::CREATED_AT`.

| Attribute | On | Meaning |
|---|---|---|
| `table = "name"` | struct | table name (default: snake_case plural) |
| `timestamps` | struct | manage `created_at` / `updated_at` automatically |
| `soft_delete` | struct | `deleted_at` marks rows deleted; queries skip them |
| `hooks` | struct | you implement `rok_db::Hooks` (lifecycle hooks) |
| `validate_with = path` | struct | record-level validation `fn(&Self) -> Result<(), ValidationErrors>` |
| `default_scope = path` | struct | `fn() -> Expr<Self>` applied to every query (remove with `.unscoped()`) |
| `has_many(posts = Post::USER_ID)` | struct | one-to-many relation → `User::POSTS`, `user.posts()` |
| `has_one(profile = Profile::USER_ID)` | struct | one-to-one relation → `User::PROFILE`, `user.profile()` |
| `primary_key` | field | primary key (default: `id`) |
| `generated` | field | filled in by the database; never written |
| `column = "name"` | field | column name differs from the field |
| `skip` | field | not a column |
| `created_at` / `updated_at` / `deleted_at` | field | managed column with a custom name |
| `version` | field | optimistic-locking counter |
| `tenant` | field | tenant column for row-level multi-tenancy |
| `validate(length(..), range(..), email, non_empty, custom = f)` | field | validation rules |
| `belongs_to = User` | field | `user_id` → `Post::USER`, `post.user()` (or `belongs_to(author = User)`) |

## API overview

| On the model (`User::…`) | |
|---|---|
| `query()`, `filter(expr)`, `order_by(col)` | start a `SELECT` |
| `find(db, id)` / `find_or_fail(db, id)` / `find_many(db, ids)` | by primary key |
| `all(db)`, `count(db)` | whole table |
| `create().set(col, v)….exec(db)` | insert column by column |
| `insert_all(db, &records)` | multi-row insert, returns stored rows |
| `update_all().filter(..).set(..).exec(db)` | bulk update |

| On a record (`user.…`) | |
|---|---|
| `insert(db)` | insert (skipping `generated` columns), returns stored row |
| `save(db)` | update by primary key, returns stored row |
| `save_only(db, Some(vec![User::EMAIL]))` | update only some columns |
| `track()` | `Tracked<M>`: save only what changed |
| `Model::copy_in(db, &records)` | bulk load with binary `COPY` |
| `upsert(db)` | `INSERT … ON CONFLICT (pk) DO UPDATE` |
| `upsert_on(db, [User::EMAIL])` | upsert on a unique column |
| `force_delete(db)`, `restore(db)`, `is_trashed()` | soft deletes |
| `delete(db)` | delete by primary key |
| `reload(db)` | re-read from the database |

| On a query (`Select`) | |
|---|---|
| `filter`, `filter_opt`, `filter_if` | `WHERE` (combined with `AND`) |
| `order_by`, `limit`, `offset`, `for_update`, `for_share` | |
| `all`, `first`, `one`, `count`, `exists` | run it |
| `cursor_paginate(db, after, limit)` | keyset pagination with an opaque cursor |
| `with_trashed()`, `only_trashed()`, `force_delete(db)`, `restore(db)` | soft deletes |
| `paginate(db, page, per_page)` | `Page<T>` with `total`, `total_pages()`, `has_next()` — one round trip |
| `update().set(..)/increment(..)/set_raw(..)` | turn into a bulk `UPDATE` |
| `delete(db)` | bulk `DELETE` |
| `stream(db)` | rows one at a time |
| `sum`, `avg`, `min`, `max` | aggregates over the matching rows |
| `group_by`, `having`, `select(..)` | projections and grouped aggregates |
| `memoize(ttl)` | cache the result (see below) |
| `to_sql()` | inspect the generated SQL and parameters |

Column operators: `eq ne gt gte lt lte like not_like ilike contains starts_with ends_with is_in not_in in_subquery not_in_subquery eq_outer between is_null is_not_null asc desc`.
Combine expressions with `.and(..)`, `.or(..)`, `!expr`, `Expr::all_of(..)`, `Expr::any_of(..)`, `Expr::exists(..)`, `Expr::not_exists(..)`, or `Expr::raw("lower(email) = ?", [v])`.

### Composite primary keys

```rust
#[derive(Model)]
struct Membership {
    #[rok(primary_key)] org_id: i64,
    #[rok(primary_key)] user_id: i64,
    role: String,
}

Membership::find(&db, (org_id, user_id)).await?;            // tuple in key-field order
Membership::find_many(&db, [(1, 2), (1, 3)]).await?;
membership.save(&db).await?;                                // WHERE org_id = $ AND user_id = $
```

Everything that identifies a row — `save`, `delete`, `reload`, `upsert`, optimistic locking, keyset
pagination tiebreakers, change feeds and the audit log — uses the whole key. Relations still need a
single-column key on the parent side.

### Relations

```rust
#[derive(Model)]
#[rok(has_many(posts = Post::USER_ID))]
struct User { id: i64, name: String }

#[derive(Model)]
struct Post {
    id: i64,
    #[rok(belongs_to = User)]
    user_id: i64,
    title: String,
}

// Lazy: a query you can refine
let recent = user.posts().order_by(Post::ID.desc()).limit(5).all(&db).await?;
let author = post.user().one(&db).await?;

// Eager: one extra query for any number of records (no N+1)
let users = User::all(&db).await?;
let posts = User::POSTS.load(&db, &users).await?;
for user in &users {
    println!("{}: {} posts", user.name, posts.get(user).len());
}
let authors = Post::USER.load(&db, &recent).await?;   // `authors.get(&post)`
```

### Streaming

```rust
use rok_db::prelude::*; // brings `try_next`, `try_collect`, … into scope

let mut users = User::query().stream(&db);
while let Some(user) = users.try_next().await? {
    // rows arrive one at a time; memory stays flat
}
```

### Aggregates and projections

```rust
let total: Option<i64> = Post::query().sum(&db, Post::VIEWS).await?;
let avg_age: Option<f64> = User::query().avg(&db, User::AGE).await?;
let oldest: Option<i32> = User::query().max(&db, User::AGE).await?;

// GROUP BY / HAVING into tuples…
let per_author: Vec<(i64, i64)> = Post::query()
    .group_by(Post::USER_ID)
    .having(Projection::count_all().gte(10))
    .select((Post::USER_ID, Projection::count_all()))
    .fetch_all(&db)
    .await?;

// …or into your own structs
#[derive(rok_db::FromRow)]
struct RoleStats { role: String, users: i64 }

let stats: Vec<RoleStats> = User::query()
    .group_by(User::ROLE)
    .select((User::ROLE, Projection::count_all().alias("users")))
    .fetch_all(&db)
    .await?;
```

Aggregates: `count`, `count_distinct`, `sum`, `avg`, `min`, `max`, plus `.cast("BIGINT")` and
`Projection::raw(..)`.

### Timestamps

```rust
#[derive(Model)]
#[rok(timestamps)]
struct Note {
    id: i64,
    body: String,
    created_at: chrono::DateTime<chrono::Utc>,   // set to now() on insert
    updated_at: chrono::DateTime<chrono::Utc>,   // set to now() on insert and every update
}
```

The database clock is used (`now()`), so values are consistent across servers. Bulk updates
set `updated_at` too, unless you set it yourself.

### Memoized queries

Cache read results in memory, with automatic invalidation:

```rust
let db = Db::builder().query_cache(10_000).connect(url).await?;

let admins = User::filter(User::ROLE.eq("admin"))
    .memoize(Duration::from_secs(60))
    .all(&db)                       // also: first, one, count, exists, paginate
    .await?;

db.cache().unwrap().stats();        // hits, misses, entries
```

- Entries expire after their TTL and are keyed by the exact SQL and parameters.
- Any write rok-db makes to a table (insert, save, upsert, delete, bulk update/delete — in
  transactions too, and again on commit) invalidates that table's cached results.
- Writes rok-db can't see (other services, raw SQL, cascades and triggers on other tables) need `raw(..).invalidates("users")`,
  `cache.invalidate("users")`, or a short TTL.
- Without a configured cache (or with a plain sqlx pool), `memoize` simply runs the query.

Running several app servers? Add `.shared_cache_invalidation()` to the builder: every instance
broadcasts its invalidations over PostgreSQL `NOTIFY` and drops stale entries from the others,
with no extra infrastructure. (If a listener reconnects, its local cache is cleared, since
messages may have been missed.)

### Query logging

Every statement is logged through [`tracing`](https://docs.rs/tracing):

| target | level | content |
|---|---|---|
| `rok_db::query` | `DEBUG` | SQL, elapsed time, row count; failures with the error |
| `rok_db::query` | `TRACE` | bound parameter values (may contain sensitive data) |
| `rok_db::slow_query` | `WARN` | queries slower than the threshold (default 1s) |
| `rok_db::cache` | `DEBUG` | memoized cache hits |

```rust
let db = Db::builder().slow_query_threshold(Duration::from_millis(200)).connect(url).await?;
// e.g. RUST_LOG=rok_db=debug with tracing-subscriber's EnvFilter
```

### Keyset pagination

Offset pagination (`paginate`) gets slower with every page and can skip or repeat rows when
data changes between requests. Keyset pagination doesn't:

```rust
let page = Post::order_by(Post::CREATED_AT.desc())
    .cursor_paginate(&db, None, 20)          // first page
    .await?;
let token: Option<String> = page.next.map(|c| c.to_string());   // opaque, URL-safe

// next request
let cursor: rok_db::Cursor = token.unwrap().parse()?;
let page = Post::order_by(Post::CREATED_AT.desc())
    .cursor_paginate(&db, Some(&cursor), 20)
    .await?;
```

The primary key is added as a tiebreaker automatically; ordering columns must be `NOT NULL`.

### Soft deletes

```rust
#[derive(Model)]
#[rok(soft_delete)]
struct Doc { id: i64, title: String, deleted_at: Option<DateTime<Utc>> }

doc.delete(&db).await?;                    // UPDATE … SET deleted_at = now()
Doc::all(&db).await?;                       // skips deleted rows (also count, find, relations, subqueries)
Doc::query().with_trashed().all(&db).await?;
Doc::query().only_trashed().restore(&db).await?;
doc.force_delete(&db).await?;              // real DELETE
```

### Optimistic locking

```rust
#[derive(Model)]
struct Account { id: i64, balance: i64, #[rok(version)] version: i32 }

let mut account = Account::find_or_fail(&db, 1).await?;
account.balance += 10;
match account.save(&db).await {
    Ok(saved) => { /* saved.version was incremented */ }
    Err(e) if e.is_conflict() => { /* someone else saved first: reload and retry */ }
    Err(e) => return Err(e),
}
```

`save` and `delete` check the version in the same statement (no extra round trip), and bulk
updates and upserts increment it too.

### Upserts

```rust
user.upsert(&db).await?;                                   // ON CONFLICT (id) DO UPDATE …
user.upsert_on(&db, [User::EMAIL]).await?;                 // ON CONFLICT (email) DO UPDATE …

User::insert_many(&users)
    .on_conflict([User::EMAIL])
    .do_update([User::NAME])                               // or .do_update_all() / .do_nothing()
    .exec(&db)
    .await?;                                               // returns inserted + updated rows

let created: Option<User> = User::create()
    .set(User::EMAIL, "ann@example.com")
    .on_constraint("users_email_key")
    .do_nothing()
    .exec_optional(&db)                                    // None if it already existed
    .await?;
```

### Subqueries

```rust
// IN (subquery)
let authors = User::filter(User::ID.in_subquery(
    Post::filter(Post::VIEWS.gt(100)).select(Post::AUTHOR_ID),
)).all(&db).await?;

// Correlated EXISTS / NOT EXISTS
let without_posts = User::filter(Expr::not_exists(
    Post::filter(Post::AUTHOR_ID.eq_outer(User::ID)),
)).all(&db).await?;
```

### Joins

```rust
// Join through a relation; filter, order and select across both models.
let rows: Vec<(String, String)> = Post::query()
    .join(Post::AUTHOR)                          // belongs_to: INNER JOIN users ON users.id = posts.author_id
    .filter(User::ROLE.eq("admin"))
    .order_by(User::NAME.asc())
    .select((Post::TITLE, User::NAME))
    .fetch_all(&db)
    .await?;

// Root models through has_many joins come back once each, in your order.
let authors: Vec<User> = User::query()
    .join(User::POSTS)
    .filter(Post::VIEWS.gt(1000))
    .order_by(Post::VIEWS.desc())               // each user ranked by their best post
    .paginate(&db, 1, 20)
    .await?
    .items;

// LEFT JOIN, ad-hoc conditions, grouping, chains.
let per_user: Vec<(String, i64)> = User::query()
    .left_join(User::POSTS)
    .group_by(User::NAME)
    .select((User::NAME, Post::ID.count()))      // 0 for users without posts
    .fetch_all(&db)
    .await?;
Post::query().left_join(Category::ID.on(Post::CATEGORY_ID));
Comment::query().join(Comment::POST).join(Post::AUTHOR).filter(User::ROLE.eq("admin"));
```

Using a column of a model that isn't part of the query is a compile error. Joined models'
tenant, soft-delete and default scopes apply in the `ON` clause. See
[RFC 0001](docs/rfcs/0001-joins.md) for the design; keyset pagination, memoization, bulk
update/delete through joins and fetching `(Post, User)` tuples are not supported yet.

### Custom column types

```rust
#[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
enum Role { Admin, Member, #[rok(rename = "ro")] ReadOnly }    // stored as TEXT: "admin", "member", "ro"

#[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
#[rok(type_name = "mood")]                                        // a native `CREATE TYPE mood AS ENUM (…)`
enum Mood { Happy, Sad }

#[derive(Debug, Clone, PartialEq, DbNewtype)]
struct Email(String);                                              // stored like its inner type

#[derive(Model)]
struct Member { id: i64, email: Email, role: Role, mood: Option<Mood> }

Member::filter(Member::ROLE.eq(Role::Admin)).all(&db).await?;
```

Any other type that implements sqlx's `Type` + `Encode` can be registered with `rok_db::impl_value!(MyType)`.

### Validation and hooks

```rust
#[derive(Model)]
#[rok(hooks)]
struct User {
    id: i64,
    #[rok(validate(length(min = 1, max = 50)))]
    name: String,
    #[rok(validate(email))]
    email: String,
    #[rok(validate(range(min = 13, max = 150)))]
    age: i32,
}

impl Hooks for User {
    fn before_insert(&self) -> rok_db::Result<()> {
        if self.email.ends_with("@blocked.example") {
            return Err(rok_db::Error::hook("domain is blocked"));
        }
        Ok(())
    }
}

match user.insert(&db).await {
    Err(rok_db::Error::Validation(errors)) => { /* errors.field("email"), errors.to_string() */ }
    other => { /* … */ }
}
```

Validation runs before every `insert`, `insert_all`/`insert_many`, `upsert` and `save`, collecting
every failure. Hooks: `before_/after_` × `insert`, `save`, `delete`. Bulk query operations
(`update()`, `delete()` on a query) skip both, by design.

### Scopes

```rust
#[derive(Model)]
#[rok(default_scope = published)]
struct Article { id: i64, title: String, published: bool }

fn published() -> Expr<Article> { Article::PUBLISHED.eq(true) }
fn popular(q: Select<Article>) -> Select<Article> { q.filter(Article::VIEWS.gte(100)) }

Article::all(&db).await?;                                // only published
Article::query().scope(popular).all(&db).await?;         // named scope
Article::query().unscoped().count(&db).await?;           // everything
```

### Change tracking

```rust
let mut user = User::find_or_fail(&db, 1).await?.track();
user.name = "Ann".into();                  // Tracked<User> derefs to User
user.changes();                            // [("name", "ann", "Ann")]
user.save(&db).await?;                     // UPDATE users SET name = $1 … — only changed columns
user.save(&db).await?;                     // nothing changed: no query
```

Writing only changed columns avoids clobbering concurrent edits to other columns.

### Bulk loading with COPY

```rust
let rows: u64 = Event::copy_in(&db, &events).await?;      // or (&mut tx, …)
```

Uses PostgreSQL's binary `COPY`, typically 5–20× faster than `INSERT` for large batches. Records
are validated first; generated columns are skipped; field types must match column types
exactly (`i64` ↔ `BIGINT`). Nothing is returned (no `after_insert` hooks).

### Web integration (axum)

With the `axum` feature, rok-db errors are HTTP responses, so handlers can just use `?`:

```rust
async fn show(State(db): State<Db>, Path(id): Path<i64>) -> rok_db::Result<Json<User>> {
    Ok(Json(User::find_or_fail(&db, id).await?))         // 404 {"error":"not_found",…}
}

async fn list(State(db): State<Db>, Query(q): Query<ListQuery>) -> rok_db::Result<Json<CursorPage<User>>> {
    // `ListQuery { after: Option<Cursor> }` — cursors (de)serialize as strings
    Ok(Json(User::order_by(User::ID).cursor_paginate(&db, q.after.as_ref(), 50).await?))
}
```

| error | status |
|---|---|
| not found | 404 |
| validation (with `fields`), hook rejection, foreign-key violation | 422 |
| optimistic-lock conflict, unique violation | 409 |
| invalid cursor | 400 |
| anything else | 500 (details logged, never returned) |

### Metrics

With the `metrics` feature, install any [`metrics`](https://docs.rs/metrics) recorder (Prometheus,
StatsD, OpenTelemetry…) and rok-db reports `rok_db_queries_total{kind,outcome}`,
`rok_db_query_duration_seconds{kind}`, `rok_db_slow_queries_total`, `rok_db_rows_total`,
`rok_db_cache_requests_total{result}` and, via `db.record_pool_metrics()`,
`rok_db_pool_connections{state}`. `db.stats()` gives a pool snapshot without the feature.

### PostgreSQL arrays, JSONB and full-text search

```rust
#[derive(Model)]
struct Doc { id: i64, body: String, tags: Vec<String>, prefs: serde_json::Value }

Doc::filter(Doc::TAGS.array_has("rust"));                         // $1 = ANY(tags)
Doc::filter(Doc::TAGS.array_overlaps(vec!["db".to_string()]));    // tags && $1
Doc::filter(Doc::ID.eq_any(ids));                                  // id = ANY($1): one parameter for any number of ids

Doc::filter(Doc::PREFS.json_has_key("theme"));                     // prefs ? $1
Doc::filter(Doc::PREFS.json_contains(json!({"theme": "dark"})));   // prefs @> $1   (feature `json`)
Doc::filter(Doc::PREFS.json_text("lang").eq("en"));                // prefs ->> 'lang' = $1
Doc::filter(Doc::PREFS.json_path_text(["notify", "email"]).eq("true"));

Doc::filter(Doc::BODY.search_in("english", "\"query builder\" -java"))   // websearch syntax
    .order_by(Doc::BODY.search_rank("query builder").desc())               // order by an expression
    .paginate(&db, 1, 20)
    .await?;
```

`Vec<T>` fields map to PostgreSQL arrays for `String`, `bool`, integers, floats and (with the
features) `Uuid` and dates.

### Read replicas

```rust
let db = Db::builder()
    .read_replica("postgres://replica-1/app")
    .read_replica("postgres://replica-2/app")
    .connect("postgres://primary/app")
    .await?;

User::all(&db).await?;                          // round-robin over replicas
User::query().on_primary().all(&db).await?;     // read your own writes
User::all(&db.primary()).await?;                // a handle that never uses replicas
```

Only the query builder's plain reads use replicas. Writes, `for_update`/`for_share`, transactions and
raw SQL (unless `.on_replica()`) go to the primary, and a read whose replica is unreachable is retried
on the primary.

### Change notifications (LISTEN/NOTIFY)

```rust
let mut listener = db.listen(&["jobs"]).await?;
db.notify("jobs", "resize:42").await?;
let n = listener.recv().await?;                 // n.channel, n.payload

User::install_change_notifications(&db).await?; // once: an AFTER trigger on `users`
let mut changes = User::changes(&db).await?;
while let Ok(change) = changes.recv().await {
    match change.op {
        ChangeOp::Insert | ChangeOp::Update => { let user = change.fetch(&db).await?; }
        ChangeOp::Delete => { /* change.key is the primary key, as text */ }
    }
}
```

Notifications arrive on commit, at most once, and only to connected listeners: great for cache
busting, websockets and waking workers, not a durable event log.

### Multi-tenancy

```rust
#[derive(Model)]
struct Invoice { id: i64, #[rok(tenant)] org_id: i64, total: i64 }

rok_db::tenant::with_tenant(org.id, async {
    Invoice::all(&db).await?;                 // … WHERE org_id = $1
    invoice.insert(&db).await?;               // org_id is always the current tenant
    Ok::<_, rok_db::Error>(())
}).await?;

Invoice::all(&db).await?;                     // outside a scope: matches nothing (fails closed)
Invoice::query().all_tenants().all(&db).await?; // explicit opt-out for admin jobs
```

Every query, count, bulk update/delete and record operation is restricted to the current tenant;
upserts can't take over another tenant's row and saves can't move a row to another tenant. The
scope is task-local (`tokio::spawn`ed tasks need their own `with_tenant`), and raw SQL is not
filtered.

### Audit log

```rust
use rok_db::audit;

audit::install(&db).await?;                                   // table + trigger function, once
audit::enable::<User>(&db, &[User::PASSWORD_HASH]).await?;    // audit `users`, minus secrets

audit::with_actor("user:42", async {                          // or tx.set_actor("user:42")
    db.transaction(|tx| Box::pin(async move { user.save(&mut *tx).await })).await
}).await?;

for entry in audit::history::<User>(&db, user.id).await? {
    println!("{} by {:?}: {:?}", entry.op, entry.actor, entry.changed);
}
```

Trigger-based (feature `json`): it records every insert, update and delete of the table, including bulk
and raw SQL, with old/new rows as JSONB and the changed columns. No-op updates are skipped.
`AuditEntry` is a regular model you can query.

### Retrying transactions

```rust
use rok_db::{Isolation, TxOptions};

// Retry serialization failures and deadlocks with backoff.
let opts = TxOptions::new().isolation(Isolation::Serializable).retries(5);
db.transaction_with(opts, |tx| Box::pin(async move {
    let mut a = Account::find_or_fail(&mut *tx, 1).await?;
    a.balance -= 10;
    a.save(&mut *tx).await?;
    Ok::<_, rok_db::Error>(())
})).await?;
```

### Testing

With the `testing` feature, `#[rok_db::test]` gives each test a fresh database on the server
named by `DATABASE_URL`, dropped afterwards (even on panic). Tests are skipped when
`DATABASE_URL` isn't set.

```rust
#[rok_db::test(migrations = "migrations", sql = "tests/fixtures/seed.sql")]
async fn lists_admins(db: Db) {
    assert_eq!(User::filter(User::ROLE.eq(Role::Admin)).count(&db).await.unwrap(), 1);
}
```

Needs `tokio` (with `macros`, `rt`) as a dev-dependency and a database user allowed to create
databases.

### Transactions

```rust
let user = db.transaction(|tx| Box::pin(async move {
    let user = new_user.insert(&mut *tx).await?;
    Profile::create().set(Profile::USER_ID, user.id).exec(&mut *tx).await?;
    Ok::<_, rok_db::Error>(user)
})).await?;                              // commits on Ok, rolls back on Err

let mut tx = db.begin().await?;          // or manage it yourself
User::filter(User::ID.eq(1)).for_update().one(&mut *tx).await?;
tx.commit().await?;
```

### Raw SQL

```rust
let users: Vec<User> = rok_db::raw("SELECT * FROM users WHERE age > ? AND email ILIKE ?")
    .bind(18)
    .bind("%@example.com")
    .fetch_all(&db)
    .await?;

let total: i64 = rok_db::raw("SELECT COUNT(*) FROM users").scalar(&db).await?;
```

### Errors

`rok_db::Error` has helpers for the common cases: `is_not_found()`, `is_conflict()`, `validation_errors()`, `is_serialization_failure()`, `is_unique_violation()`, `is_foreign_key_violation()` and `constraint()`.

## Crates

| crate | |
|---|---|
| `rok-db` | the crate to depend on: re-exports everything plus the derive |
| `rok-db-core` | runtime: `Db`, `Model`, query builders, `Value` |
| `rok-db-macros` | `#[derive(Model)]` |

## Contributing

Contributions are welcome! Start with [CONTRIBUTING.md](CONTRIBUTING.md) for
setup, the change protocol and commit conventions. Larger changes go through
the [RFC process](docs/rfcs/README.md); see [GOVERNANCE.md](GOVERNANCE.md) for
how decisions are made. Please report security issues privately as described
in [SECURITY.md](SECURITY.md). Everyone is expected to follow the
[Code of Conduct](CODE_OF_CONDUCT.md). Notable changes are listed in the
[changelog](CHANGELOG.md).

## Development

```sh
cargo test --workspace --all-features          # SQL-generation tests run everywhere
DATABASE_URL=postgres://postgres:postgres@localhost/rok_db_test \
  cargo test --workspace --all-features        # plus end-to-end tests against Postgres
cargo run -p rok-db --example blog             # needs DATABASE_URL
```

Database tests use per-connection `TEMP` tables, so they never touch existing data.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
