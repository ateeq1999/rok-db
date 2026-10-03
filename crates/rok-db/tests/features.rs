//! Relations, streaming, aggregates, timestamps and memoization.
//!
//! SQL-shape tests always run; database tests need `DATABASE_URL` (see
//! `tests/postgres.rs`).

use std::time::Duration;

use rok_db::prelude::*;
use rok_db::raw;

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(has_many(posts = Post::AUTHOR_ID), has_one(profile = Profile::USER_ID))]
struct User {
    #[rok(generated)]
    id: i64,
    name: String,
    role: String,
    age: i32,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Post {
    #[rok(generated)]
    id: i64,
    #[rok(belongs_to(author = User))]
    author_id: i64,
    #[rok(belongs_to = Category)]
    category_id: Option<i64>,
    title: String,
    views: i32,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(table = "categories")]
struct Category {
    #[rok(generated)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Profile {
    #[rok(generated)]
    id: i64,
    user_id: i64,
    bio: String,
}

const SCHEMA: &str = r#"
CREATE TEMP TABLE users (
    id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL, age INT NOT NULL
);
CREATE TEMP TABLE categories (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
CREATE TEMP TABLE posts (
    id BIGSERIAL PRIMARY KEY,
    author_id BIGINT NOT NULL REFERENCES users (id),
    category_id BIGINT REFERENCES categories (id),
    title TEXT NOT NULL,
    views INT NOT NULL DEFAULT 0
);
CREATE TEMP TABLE profiles (
    id BIGSERIAL PRIMARY KEY, user_id BIGINT NOT NULL REFERENCES users (id), bio TEXT NOT NULL
);
"#;

async fn setup_with(builder: rok_db::DbBuilder) -> Option<Db> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return None;
    };
    let db = builder
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    db.execute(SCHEMA).await.expect("schema");
    Some(db)
}

async fn setup() -> Option<Db> {
    setup_with(Db::builder()).await
}

/// 3 users, 2 categories, 6 posts, 1 profile.
async fn seed(db: &Db) -> (Vec<User>, Vec<Post>) {
    let users = User::insert_all(
        db,
        &[
            ("ann", "admin", 30),
            ("bob", "member", 20),
            ("cid", "member", 40),
        ]
        .map(|(name, role, age)| User {
            id: 0,
            name: name.into(),
            role: role.into(),
            age,
        }),
    )
    .await
    .unwrap();
    let rust = Category::create()
        .set(Category::NAME, "rust")
        .exec(db)
        .await
        .unwrap();
    let sql = Category::create()
        .set(Category::NAME, "sql")
        .exec(db)
        .await
        .unwrap();
    let post = |author: &User, category: Option<&Category>, title: &str, views| Post {
        id: 0,
        author_id: author.id,
        category_id: category.map(|c| c.id),
        title: title.into(),
        views,
    };
    let posts = Post::insert_all(
        db,
        &[
            post(&users[0], Some(&rust), "a1", 10),
            post(&users[0], Some(&sql), "a2", 20),
            post(&users[0], None, "a3", 30),
            post(&users[1], Some(&rust), "b1", 5),
            post(&users[1], Some(&rust), "b2", 15),
            post(&users[2], Some(&sql), "c1", 100),
        ],
    )
    .await
    .unwrap();
    Profile::create()
        .set(Profile::USER_ID, users[0].id)
        .set(Profile::BIO, "hi")
        .exec(db)
        .await
        .unwrap();
    (users, posts)
}

// ----- SQL shape -------------------------------------------------------------

#[test]
fn relation_queries_sql() {
    let user = User {
        id: 7,
        name: "x".into(),
        role: "r".into(),
        age: 1,
    };
    assert_eq!(
        user.posts().order_by(Post::ID).to_sql().as_str(),
        r#"SELECT "id", "author_id", "category_id", "title", "views" FROM "posts" WHERE "author_id" = $1 ORDER BY "id" ASC"#
    );
    let post = Post {
        id: 1,
        author_id: 7,
        category_id: None,
        title: "t".into(),
        views: 0,
    };
    assert!(
        post.author()
            .to_sql()
            .as_str()
            .ends_with(r#"FROM "users" WHERE "id" = $1"#)
    );
    assert!(
        post.category().to_sql().as_str().ends_with("WHERE FALSE"),
        "NULL foreign key"
    );
    assert_eq!(Post::AUTHOR.foreign_key().name(), "author_id");
    assert_eq!(User::POSTS.foreign_key().name(), "author_id");
    assert_eq!(User::PROFILE.foreign_key().name(), "user_id");
    assert_eq!(post.value_of("title"), Some(rok_db::Value::from("t")));
    assert_eq!(post.value_of("nope"), None);
}

#[test]
fn projection_sql() {
    let sql = Post::filter(Post::VIEWS.gt(1))
        .group_by(Post::AUTHOR_ID)
        .having(Projection::count_all().gte(2))
        .order_by(Post::AUTHOR_ID)
        .select((
            Post::AUTHOR_ID,
            Projection::count_all(),
            Post::VIEWS.sum().cast("BIGINT").alias("total"),
        ))
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "author_id", COUNT(*), CAST(SUM("views") AS BIGINT) AS "total" FROM "posts" WHERE "views" > $1 GROUP BY "author_id" HAVING COUNT(*) >= $2 ORDER BY "author_id" ASC"#
    );
}

#[cfg(feature = "chrono")]
mod timestamps_sql {
    use super::*;
    use rok_db::sqlx::types::chrono::{DateTime, Utc};

    #[derive(Debug, Clone, Model)]
    #[rok(timestamps)]
    pub(super) struct Note {
        #[rok(generated)]
        pub id: i64,
        pub body: String,
        pub created_at: DateTime<Utc>,
        pub updated_at: DateTime<Utc>,
    }

    #[test]
    fn timestamps_are_written_by_the_database() {
        assert_eq!(Note::CREATED_AT_COLUMN, Some("created_at"));
        assert_eq!(Note::UPDATED_AT_COLUMN, Some("updated_at"));
        let note = Note {
            id: 0,
            body: "b".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let sql = rok_db::__private::__insert_sql(&[note], false);
        assert_eq!(
            sql.as_str(),
            r#"INSERT INTO "notes" ("body", "created_at", "updated_at") VALUES ($1, now(), now()) RETURNING "id", "body", "created_at", "updated_at""#
        );
        assert_eq!(
            Note::update_all()
                .set(Note::BODY, "x")
                .filter(Note::ID.eq(1))
                .to_sql()
                .as_str(),
            r#"UPDATE "notes" SET "body" = $1, "updated_at" = now() WHERE "id" = $2"#
        );
        assert_eq!(
            Note::create().set(Note::BODY, "x").to_sql().as_str(),
            r#"INSERT INTO "notes" ("body", "created_at", "updated_at") VALUES ($1, now(), now()) RETURNING "id", "body", "created_at", "updated_at""#
        );
    }
}

// ----- database --------------------------------------------------------------

#[tokio::test]
async fn relations_lazy_and_eager() {
    let Some(db) = setup().await else { return };
    let (users, posts) = seed(&db).await;

    // Lazy
    let ann_posts = users[0]
        .posts()
        .order_by(Post::TITLE)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        ann_posts
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["a1", "a2", "a3"]
    );
    assert_eq!(posts[5].author().one(&db).await.unwrap().name, "cid");
    assert!(posts[2].category().first(&db).await.unwrap().is_none());
    assert_eq!(users[0].profile().one(&db).await.unwrap().bio, "hi");

    // Eager: has_many
    let by_author = User::POSTS.load(&db, &users).await.unwrap();
    assert_eq!(by_author.len(), 6);
    assert_eq!(
        users
            .iter()
            .map(|u| by_author.get(u).len())
            .collect::<Vec<_>>(),
        [3, 2, 1]
    );
    let top = User::POSTS
        .load_from(
            &db,
            &users,
            Post::filter(Post::VIEWS.gte(15)).order_by(Post::VIEWS.desc()),
        )
        .await
        .unwrap();
    assert_eq!(
        top.get(&users[0])
            .iter()
            .map(|p| p.views)
            .collect::<Vec<_>>(),
        [30, 20]
    );

    // Eager: belongs_to, with NULL and shared foreign keys
    let mut categories = Post::CATEGORY.load(&db, &posts).await.unwrap();
    assert_eq!(categories.len(), 2);
    assert_eq!(categories.get(&posts[0]).unwrap().name, "rust");
    assert!(categories.get(&posts[2]).is_none());
    assert_eq!(categories.take(&posts[5]).unwrap().name, "sql");
    let authors = Post::AUTHOR.load(&db, &posts).await.unwrap();
    assert!(
        posts
            .iter()
            .all(|p| authors.get(p).unwrap().id == p.author_id)
    );

    // Eager: has_one
    let profiles = User::PROFILE.load(&db, &users).await.unwrap();
    assert!(profiles.get(&users[0]).is_some() && profiles.get(&users[1]).is_none());

    // Empty input runs no query.
    assert!(User::POSTS.load(&db, &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn streaming() {
    let Some(db) = setup().await else { return };
    seed(&db).await;

    let mut stream = Post::order_by(Post::VIEWS.desc()).stream(&db);
    let mut seen = Vec::new();
    while let Some(post) = stream.try_next().await.unwrap() {
        seen.push(post.views);
    }
    assert_eq!(seen, [100, 30, 20, 15, 10, 5]);
    drop(stream);

    let titles: Vec<(String,)> = Post::filter(Post::VIEWS.lt(15))
        .order_by(Post::TITLE)
        .select(Post::TITLE)
        .stream(&db)
        .try_collect()
        .await
        .unwrap();
    assert_eq!(titles, [("a1".to_string(),), ("b1".to_string(),)]);

    let n = raw("SELECT * FROM users")
        .stream::<User, _>(&db)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(n.len(), 3);

    // Streams work inside transactions too.
    let mut tx = db.begin().await.unwrap();
    let count = User::query()
        .stream(&mut *tx)
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .len();
    assert_eq!(count, 3);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn aggregates_and_projections() {
    let Some(db) = setup().await else { return };
    seed(&db).await;

    assert_eq!(
        Post::query().sum(&db, Post::VIEWS).await.unwrap(),
        Some(180)
    );
    assert_eq!(
        Post::filter(Post::VIEWS.gt(1000))
            .sum(&db, Post::VIEWS)
            .await
            .unwrap(),
        None
    );
    assert_eq!(User::query().avg(&db, User::AGE).await.unwrap(), Some(30.0));
    assert_eq!(
        User::query().min::<i32, _>(&db, User::AGE).await.unwrap(),
        Some(20)
    );
    assert_eq!(
        User::query()
            .max::<String, _>(&db, User::NAME)
            .await
            .unwrap(),
        Some("cid".into())
    );

    let per_author: Vec<(i64, i64, i64)> = Post::query()
        .group_by(Post::AUTHOR_ID)
        .having(Projection::count_all().gte(2))
        .order_by(Post::AUTHOR_ID)
        .select((
            Post::AUTHOR_ID,
            Projection::count_all(),
            Post::VIEWS.sum().cast("BIGINT"),
        ))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(per_author.len(), 2);
    assert_eq!((per_author[0].1, per_author[0].2), (3, 60));
    assert_eq!((per_author[1].1, per_author[1].2), (2, 20));

    #[derive(rok_db::FromRow)]
    struct RoleStats {
        role: String,
        users: i64,
        avg_age: f64,
    }
    let stats: Vec<RoleStats> = User::query()
        .group_by(User::ROLE)
        .order_by(User::ROLE)
        .select((
            User::ROLE,
            Projection::count_all().alias("users"),
            User::AGE.avg().cast("DOUBLE PRECISION").alias("avg_age"),
        ))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(
        (stats[1].role.as_str(), stats[1].users, stats[1].avg_age),
        ("member", 2, 30.0)
    );

    let names: i64 = User::query()
        .select(User::NAME.count_distinct())
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(names, 3);
    let (name, age): (String, i32) = User::order_by(User::AGE.desc())
        .select((User::NAME, User::AGE))
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!((name.as_str(), age), ("cid", 40));
}

#[cfg(feature = "chrono")]
#[tokio::test]
async fn timestamps() {
    use rok_db::sqlx::types::chrono::{TimeZone, Utc};
    use timestamps_sql::Note;

    let Some(db) = setup().await else { return };
    db.execute(
        "CREATE TEMP TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL,
         created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL)",
    )
    .await
    .unwrap();
    let epoch = Utc.timestamp_opt(0, 0).unwrap();

    let note = Note {
        id: 0,
        body: "a".into(),
        created_at: epoch,
        updated_at: epoch,
    }
    .insert(&db)
    .await
    .unwrap();
    assert!(
        note.created_at > epoch,
        "set by the database, not the struct"
    );
    assert_eq!(note.created_at, note.updated_at);

    // `now()` is the transaction start time, so use separate statements.
    let mut edited = note.clone();
    edited.body = "b".into();
    edited.created_at = epoch;
    let saved = edited.save(&db).await.unwrap();
    assert_eq!(
        saved.created_at, note.created_at,
        "created_at is never updated"
    );
    assert!(saved.updated_at > note.updated_at);

    Note::filter(Note::ID.eq(note.id))
        .update()
        .set(Note::BODY, "c")
        .exec(&db)
        .await
        .unwrap();
    let reloaded = note.reload(&db).await.unwrap();
    assert!(reloaded.updated_at > saved.updated_at);

    let upserted = Note {
        body: "d".into(),
        ..reloaded.clone()
    }
    .upsert(&db)
    .await
    .unwrap();
    assert_eq!(upserted.created_at, note.created_at);
    assert!(upserted.updated_at > reloaded.updated_at);

    let built = Note::create().set(Note::BODY, "e").exec(&db).await.unwrap();
    assert!(built.created_at > epoch);
}

#[tokio::test]
async fn memoized_queries() {
    let Some(db) = setup_with(Db::builder().query_cache(100)).await else {
        return;
    };
    seed(&db).await;
    let cache = db.cache().expect("cache enabled").clone();
    let ttl = Duration::from_secs(60);
    let admins = || User::filter(User::ROLE.eq("admin")).memoize(ttl);

    assert_eq!(admins().all(&db).await.unwrap().len(), 1);
    assert_eq!(admins().all(&db).await.unwrap().len(), 1);
    assert_eq!(admins().count(&db).await.unwrap(), 1);
    assert_eq!(admins().count(&db).await.unwrap(), 1);
    let stats = cache.stats();
    assert_eq!((stats.hits, stats.misses), (2, 2));

    // Different parameters are different entries.
    assert_eq!(
        User::filter(User::ROLE.eq("member"))
            .memoize(ttl)
            .count(&db)
            .await
            .unwrap(),
        2
    );
    assert_eq!(cache.stats().misses, 3);

    // A write through rok-db invalidates the table.
    User::create()
        .set(User::NAME, "dee")
        .set(User::ROLE, "admin")
        .set(User::AGE, 50)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(admins().count(&db).await.unwrap(), 2);
    assert_eq!(admins().all(&db).await.unwrap().len(), 2);

    // ...but only that table.
    let posts = || Post::query().memoize(ttl);
    assert_eq!(posts().count(&db).await.unwrap(), 6);
    User::filter(User::NAME.eq("dee"))
        .delete(&db)
        .await
        .unwrap();
    let before = cache.stats().hits;
    assert_eq!(posts().count(&db).await.unwrap(), 6);
    assert_eq!(cache.stats().hits, before + 1);

    // Writes in a transaction invalidate (again on commit).
    admins().all(&db).await.unwrap();
    let mut tx = db.begin().await.unwrap();
    User::filter(User::ROLE.eq("admin"))
        .update()
        .set(User::ROLE, "owner")
        .exec(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(admins().count(&db).await.unwrap(), 0);

    // Raw writes are invisible unless declared.
    let owners = || User::filter(User::ROLE.eq("owner")).memoize(ttl);
    assert_eq!(owners().count(&db).await.unwrap(), 1);
    raw("UPDATE users SET role = 'admin' WHERE role = 'owner'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        owners().count(&db).await.unwrap(),
        1,
        "stale until invalidated"
    );
    raw("UPDATE users SET role = 'admin' WHERE role = 'owner'")
        .invalidates("users")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(owners().count(&db).await.unwrap(), 0);
    cache.clear();
    assert_eq!(cache.stats().entries, 0);

    // Pagination and first/exists are memoized too.
    let page = User::order_by(User::ID)
        .memoize(ttl)
        .paginate(&db, 1, 2)
        .await
        .unwrap();
    let again = User::order_by(User::ID)
        .memoize(ttl)
        .paginate(&db, 1, 2)
        .await
        .unwrap();
    assert_eq!(page, again);
    assert!(User::query().memoize(ttl).exists(&db).await.unwrap());
    assert!(
        User::order_by(User::ID)
            .memoize(ttl)
            .first(&db)
            .await
            .unwrap()
            .is_some()
    );

    // Without a cache, memoize is a no-op.
    let before = cache.stats();
    let plain = db.pool();
    assert_eq!(admins().count(plain).await.unwrap(), 1);
    assert_eq!(admins().count(plain).await.unwrap(), 1);
    let after = cache.stats();
    assert_eq!((after.hits, after.misses), (before.hits, before.misses));
}

#[tokio::test]
async fn memoize_expires() {
    let Some(db) = setup_with(Db::builder().query_cache(10)).await else {
        return;
    };
    seed(&db).await;
    let q = || User::query().memoize(Duration::from_millis(50));
    q().count(&db).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    q().count(&db).await.unwrap();
    assert_eq!(db.cache().unwrap().stats().hits, 0);
}
