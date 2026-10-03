//! End-to-end tests against a real PostgreSQL server.
//!
//! Set `DATABASE_URL` to run them, e.g.
//! `DATABASE_URL=postgres://postgres:postgres@localhost/rok_db_test cargo test`.
//! Without it every test is skipped. Each test uses its own connection with
//! `TEMP` tables, so tests are isolated and leave nothing behind.

use rok_db::prelude::*;
use rok_db::{Error, raw};

#[derive(Debug, Clone, PartialEq, Model)]
struct User {
    #[rok(primary_key, generated)]
    id: i64,
    email: String,
    name: Option<String>,
    age: i32,
    #[rok(generated)]
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Post {
    #[rok(generated)]
    id: i32,
    user_id: i64,
    title: String,
    #[rok(column = "view_count")]
    views: i32,
    #[rok(skip)]
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(table = "settings")]
struct Setting {
    #[rok(primary_key)]
    key: String,
    value: String,
}

const SCHEMA: &str = r#"
CREATE TEMP TABLE users (
    id     BIGSERIAL PRIMARY KEY,
    email  TEXT NOT NULL UNIQUE,
    name   TEXT,
    age    INT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE
);
CREATE TEMP TABLE posts (
    id         SERIAL PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    view_count INT NOT NULL DEFAULT 0
);
CREATE TEMP TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// A single-connection pool with fresh temp tables, or `None` to skip.
async fn setup() -> Option<Db> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return None;
    };
    let db = Db::builder()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    db.execute(SCHEMA).await.expect("create schema");
    Some(db)
}

fn user(email: &str, age: i32) -> User {
    User {
        id: 0,
        email: email.into(),
        name: None,
        age,
        active: false,
    }
}

async fn seed(db: &Db) -> Vec<User> {
    let users = (1..=5)
        .map(|i| user(&format!("u{i}@example.com"), i * 10))
        .collect::<Vec<_>>();
    User::insert_all(db, &users).await.unwrap()
}

#[tokio::test]
async fn crud_roundtrip() {
    let Some(db) = setup().await else { return };
    db.ping().await.unwrap();

    let ann = user("ann@example.com", 31).insert(&db).await.unwrap();
    assert!(ann.id > 0, "generated id is returned");
    assert!(ann.active, "generated default is returned");

    let found = User::find(&db, ann.id).await.unwrap();
    assert_eq!(found.as_ref(), Some(&ann));
    assert!(User::find(&db, 9999_i64).await.unwrap().is_none());

    let mut ann = User::find_or_fail(&db, ann.id).await.unwrap();
    ann.name = Some("Ann".into());
    ann.age = 32;
    let ann = ann.save(&db).await.unwrap();
    assert_eq!(ann.name.as_deref(), Some("Ann"));
    assert_eq!(ann.reload(&db).await.unwrap().age, 32);

    assert_eq!(User::count(&db).await.unwrap(), 1);
    ann.delete(&db).await.unwrap();
    assert_eq!(User::count(&db).await.unwrap(), 0);

    let err = ann.delete(&db).await.unwrap_err();
    assert!(err.is_not_found(), "{err}");
    let err = User::find_or_fail(&db, ann.id).await.unwrap_err();
    assert!(err.is_not_found());
    assert!(err.to_string().contains("users"), "{err}");
}

#[tokio::test]
async fn query_builder() {
    let Some(db) = setup().await else { return };
    let users = seed(&db).await;
    assert_eq!(users.len(), 5);

    let older = User::filter(User::AGE.gt(25))
        .order_by(User::AGE.desc())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        older.iter().map(|u| u.age).collect::<Vec<_>>(),
        [50, 40, 30]
    );

    let some = User::filter(User::AGE.eq(10).or(User::AGE.eq(50)))
        .filter(User::EMAIL.ends_with("@example.com"))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(some, 2);

    let first = User::order_by(User::AGE).first(&db).await.unwrap().unwrap();
    assert_eq!(first.age, 10);
    assert!(User::filter(User::AGE.eq(10)).exists(&db).await.unwrap());
    assert!(!User::filter(User::AGE.eq(11)).exists(&db).await.unwrap());
    assert!(
        User::filter(User::AGE.eq(11))
            .one(&db)
            .await
            .unwrap_err()
            .is_not_found()
    );

    let ids = [users[0].id, users[2].id];
    assert_eq!(User::find_many(&db, ids).await.unwrap().len(), 2);
    assert_eq!(
        User::filter(User::ID.is_in(Vec::<i64>::new()))
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        User::filter(User::NAME.is_null()).count(&db).await.unwrap(),
        5
    );
    assert_eq!(
        User::filter(User::AGE.between(20, 40))
            .count(&db)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        User::filter(!User::AGE.between(20, 40))
            .count(&db)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        User::filter(Expr::raw("age % ? = 0", [20]))
            .count(&db)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        User::filter(User::EMAIL.ilike("U1%"))
            .count(&db)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn bulk_update_and_delete() {
    let Some(db) = setup().await else { return };
    seed(&db).await;

    let n = User::filter(User::AGE.lt(30))
        .update()
        .set(User::NAME, "young")
        .increment(User::AGE, 1)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let young = User::filter(User::NAME.eq("young"))
        .order_by(User::AGE)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(young.iter().map(|u| u.age).collect::<Vec<_>>(), [11, 21]);

    let updated = User::update_all()
        .filter(User::AGE.gte(40))
        .set(User::NAME, None::<String>)
        .set(User::AGE, 99)
        .returning(&db)
        .await
        .unwrap();
    assert!(updated.iter().all(|u| u.age == 99 && u.name.is_none()));
    assert_eq!(updated.len(), 2);

    let err = User::update_all().exec(&db).await.unwrap_err();
    assert!(matches!(err, Error::InvalidQuery(_)));

    assert_eq!(User::filter(User::AGE.eq(99)).delete(&db).await.unwrap(), 2);
    assert_eq!(User::count(&db).await.unwrap(), 3);
}

#[tokio::test]
async fn pagination() {
    let Some(db) = setup().await else { return };
    seed(&db).await;

    let page = User::order_by(User::AGE).paginate(&db, 2, 2).await.unwrap();
    assert_eq!(page.total, 5);
    assert_eq!(page.total_pages(), 3);
    assert_eq!(page.iter().map(|u| u.age).collect::<Vec<_>>(), [30, 40]);
    assert!(page.has_next() && page.has_prev());

    let last = User::order_by(User::AGE).paginate(&db, 3, 2).await.unwrap();
    assert_eq!(last.len(), 1);
    assert!(!last.has_next());

    let beyond = User::query().paginate(&db, 10, 2).await.unwrap();
    assert!(beyond.is_empty());
    assert_eq!(beyond.total, 5, "total is known even for an empty page");

    let filtered = User::filter(User::AGE.gt(100))
        .paginate(&db, 1, 2)
        .await
        .unwrap();
    assert_eq!((filtered.total, filtered.len()), (0, 0));

    assert!(User::query().paginate(&db, 1, 0).await.is_err());
}

#[tokio::test]
async fn create_builder_columns_and_skip() {
    let Some(db) = setup().await else { return };
    let bob = User::create()
        .set(User::EMAIL, "bob@example.com")
        .set(User::AGE, 40)
        .exec(&db)
        .await
        .unwrap();
    assert!(bob.active);

    let post = Post {
        id: 0,
        user_id: bob.id,
        title: "Hello".into(),
        views: 3,
        preview: "ignored".into(),
    }
    .insert(&db)
    .await
    .unwrap();
    assert_eq!(post.views, 3);
    assert_eq!(
        post.preview, "",
        "skipped fields are defaulted when loading"
    );

    Post::filter(Post::ID.eq(post.id))
        .update()
        .increment(Post::VIEWS, 10)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(post.reload(&db).await.unwrap().views, 13);

    // Unique violations are easy to detect.
    let err = user("bob@example.com", 1).insert(&db).await.unwrap_err();
    assert!(err.is_unique_violation(), "{err}");
    // So are foreign-key violations.
    let err = Post::create()
        .set(Post::USER_ID, 424242_i64)
        .set(Post::TITLE, "orphan")
        .exec(&db)
        .await
        .unwrap_err();
    assert!(err.is_foreign_key_violation(), "{err}");
}

#[tokio::test]
async fn upsert_natural_key() {
    let Some(db) = setup().await else { return };
    let theme = Setting {
        key: "theme".into(),
        value: "dark".into(),
    };
    theme.upsert(&db).await.unwrap();
    let updated = Setting {
        value: "light".into(),
        ..theme
    }
    .upsert(&db)
    .await
    .unwrap();
    assert_eq!(updated.value, "light");
    assert_eq!(Setting::count(&db).await.unwrap(), 1);
    assert_eq!(
        Setting::find_or_fail(&db, "theme").await.unwrap().value,
        "light"
    );
}

#[tokio::test]
async fn transactions() {
    let Some(db) = setup().await else { return };

    // Rolled back on error.
    let res: Result<(), Error> = db
        .transaction(|tx| {
            Box::pin(async move {
                user("tx@example.com", 1).insert(&mut *tx).await?;
                assert_eq!(User::count(&mut *tx).await?, 1);
                Err(Error::InvalidQuery("abort".into()))
            })
        })
        .await;
    assert!(res.is_err());
    assert_eq!(User::count(&db).await.unwrap(), 0);

    // Committed on success, with row locking.
    let saved = db
        .transaction(|tx| {
            Box::pin(async move {
                let u = user("tx@example.com", 1).insert(&mut *tx).await?;
                let locked = User::filter(User::ID.eq(u.id))
                    .for_update()
                    .one(&mut *tx)
                    .await?;
                Ok::<_, Error>(locked)
            })
        })
        .await
        .unwrap();
    assert_eq!(User::find_or_fail(&db, saved.id).await.unwrap(), saved);

    // Manual begin/rollback; dropping also rolls back.
    let mut tx = db.begin().await.unwrap();
    User::query().delete(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(User::count(&db).await.unwrap(), 1);
}

#[tokio::test]
async fn raw_queries_and_sqlx_interop() {
    let Some(db) = setup().await else { return };
    seed(&db).await;

    let users: Vec<User> = raw("SELECT * FROM users WHERE age > ? ORDER BY age")
        .bind(30)
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(users.len(), 2);

    let total: i64 = raw("SELECT SUM(age)::BIGINT FROM users WHERE age >= $1")
        .bind(10)
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(total, 150);

    let n = raw("UPDATE users SET name = ? WHERE age = ?")
        .bind("x")
        .bind(10)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // A plain sqlx pool works as an executor too.
    let pool = db.pool();
    assert_eq!(User::count(pool).await.unwrap(), 5);
}

#[tokio::test]
async fn futures_are_send() {
    let Some(db) = setup().await else { return };
    let handle = tokio::spawn(async move {
        let u = user("spawn@example.com", 5).insert(&db).await?;
        User::filter(User::ID.eq(u.id)).paginate(&db, 1, 10).await
    });
    let page = handle.await.unwrap().unwrap();
    assert_eq!(page.total, 1);
}

#[cfg(all(feature = "chrono", feature = "uuid", feature = "json"))]
mod typed_columns {
    use super::*;
    use rok_db::sqlx::types::chrono::{DateTime, NaiveDate, Utc};
    use rok_db::sqlx::types::{Json, Uuid};

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Meta {
        tags: Vec<String>,
    }

    #[derive(Debug, Clone, Model)]
    struct Event {
        id: Uuid,
        #[rok(generated)]
        created_at: DateTime<Utc>,
        day: Option<NaiveDate>,
        meta: Json<Meta>,
    }

    #[tokio::test]
    async fn chrono_uuid_json() {
        let Some(db) = setup().await else { return };
        db.execute(
            "CREATE TEMP TABLE events (id UUID PRIMARY KEY, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), day DATE, meta JSONB NOT NULL)",
        )
        .await
        .unwrap();

        let id = Uuid::from_u128(42);
        let event = Event {
            id,
            created_at: Utc::now(),
            day: NaiveDate::from_ymd_opt(2026, 10, 2),
            meta: Json(Meta {
                tags: vec!["a".into()],
            }),
        }
        .insert(&db)
        .await
        .unwrap();
        assert_eq!(event.meta.0.tags, ["a"]);

        let found = Event::filter(Event::CREATED_AT.lte(Utc::now()))
            .filter(Event::DAY.eq(NaiveDate::from_ymd_opt(2026, 10, 2)))
            .one(&db)
            .await
            .unwrap();
        assert_eq!(found.id, id);
        assert!(Event::find(&db, id).await.unwrap().is_some());
    }
}
