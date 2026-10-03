//! Custom column types, validation, hooks, scopes, transaction retries and
//! the `#[rok_db::test]` helper. Every test gets its own temporary database.
#![cfg(feature = "testing")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use rok_db::prelude::*;
use rok_db::testing::TestDb;
use rok_db::{Error, Hooks, Isolation, TxOptions, ValidationErrors, raw};

// ----- custom column types -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
enum Role {
    Admin,
    Member,
    #[rok(rename = "ro")]
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
#[rok(type_name = "mood", rename_all = "SCREAMING_SNAKE_CASE")]
enum Mood {
    VeryHappy,
    Sad,
}

#[derive(Debug, Clone, PartialEq, DbNewtype)]
struct Email(String);

#[derive(Debug, Clone, PartialEq, Model)]
struct Member {
    #[rok(generated)]
    id: i64,
    email: Email,
    role: Role,
    mood: Option<Mood>,
}

const MEMBERS: &str = "
    CREATE TYPE mood AS ENUM ('VERY_HAPPY', 'SAD');
    CREATE TABLE members (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL, role TEXT NOT NULL, mood mood);
";

#[test]
fn enum_strings() {
    assert_eq!(Role::ReadOnly.as_str(), "ro");
    assert_eq!(Role::Admin.to_string(), "admin");
    assert_eq!("member".parse::<Role>(), Ok(Role::Member));
    assert!(
        "nope"
            .parse::<Role>()
            .unwrap_err()
            .contains("expected one of: admin, member, ro")
    );
    assert_eq!(Mood::VeryHappy.as_str(), "VERY_HAPPY");
}

#[rok_db::test]
async fn custom_types_round_trip(db: Db) {
    db.execute(MEMBERS).await.unwrap();
    let ann = Member {
        id: 0,
        email: Email("ann@x.io".into()),
        role: Role::Admin,
        mood: Some(Mood::VeryHappy),
    }
    .insert(&db)
    .await
    .unwrap();
    let bob = Member {
        id: 0,
        email: Email("bob@x.io".into()),
        role: Role::ReadOnly,
        mood: None,
    }
    .insert(&db)
    .await
    .unwrap();
    assert_eq!(ann.role, Role::Admin);
    assert_eq!(bob.mood, None);

    let admins = Member::filter(Member::ROLE.eq(Role::Admin))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(admins, std::slice::from_ref(&ann));
    let found = Member::filter(Member::EMAIL.eq(Email("bob@x.io".into())))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(found.role, Role::ReadOnly);
    let happy = Member::filter(Member::MOOD.eq(Mood::VeryHappy))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(happy, 1);
    assert_eq!(
        Member::filter(Member::ROLE.is_in([Role::Admin, Role::ReadOnly]))
            .count(&db)
            .await
            .unwrap(),
        2
    );

    // Update through `set` and `save`, including NULL of a custom type.
    Member::filter(Member::ID.eq(bob.id))
        .update()
        .set(Member::MOOD, Some(Mood::Sad))
        .exec(&db)
        .await
        .unwrap();
    let mut ann = ann;
    ann.mood = None;
    ann.role = Role::Member;
    let ann = ann.save(&db).await.unwrap();
    assert_eq!((ann.role, ann.mood), (Role::Member, None));
    assert_eq!(bob.reload(&db).await.unwrap().mood, Some(Mood::Sad));

    // The stored text is the renamed variant.
    let stored: String = raw("SELECT role FROM members WHERE id = ?")
        .bind(bob.id)
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(stored, "ro");
    // Unknown values fail to decode with a clear message.
    raw("UPDATE members SET role = 'ghost' WHERE id = ?")
        .bind(bob.id)
        .execute(&db)
        .await
        .unwrap();
    let err = bob.reload(&db).await.unwrap_err();
    assert!(
        err.to_string().contains("invalid `Role` value \"ghost\""),
        "{err}"
    );
}

// ----- validation & hooks --------------------------------------------------------

static INSERTED: AtomicU32 = AtomicU32::new(0);
static DELETED: AtomicU32 = AtomicU32::new(0);

fn no_spaces(handle: &str) -> Result<(), String> {
    if handle.contains(' ') {
        Err("must not contain spaces".into())
    } else {
        Ok(())
    }
}

fn adults_need_email(p: &Person) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();
    if p.age >= 18 && p.email.is_none() {
        errors.add("email", "required", "is required for adults");
    }
    errors.into_result()
}

#[derive(Debug, Clone, Model)]
#[rok(hooks, validate_with = adults_need_email)]
struct Person {
    #[rok(generated)]
    id: i64,
    #[rok(validate(length(min = 1, max = 10)))]
    name: String,
    #[rok(validate(email))]
    email: Option<String>,
    #[rok(validate(range(min = 0, max = 150)))]
    age: i32,
    #[rok(validate(non_empty, custom = no_spaces))]
    handle: String,
}

impl Hooks for Person {
    fn before_insert(&self) -> rok_db::Result<()> {
        if self.handle == "root" {
            return Err(Error::hook("`root` is reserved"));
        }
        Ok(())
    }
    fn after_insert(&self) -> rok_db::Result<()> {
        INSERTED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn before_save(&self) -> rok_db::Result<()> {
        if self.name == "frozen" {
            return Err(Error::hook("frozen records can't change"));
        }
        Ok(())
    }
    fn after_delete(&self) -> rok_db::Result<()> {
        DELETED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn person(name: &str, age: i32, email: Option<&str>, handle: &str) -> Person {
    Person {
        id: 0,
        name: name.into(),
        email: email.map(Into::into),
        age,
        handle: handle.into(),
    }
}

#[test]
fn validation_collects_every_error() {
    let errors = person("", 200, Some("nope"), "a b").validate().unwrap_err();
    let codes: Vec<_> = errors.errors().iter().map(|e| (e.field, e.code)).collect();
    assert_eq!(
        codes,
        [
            ("name", "length"),
            ("email", "email"),
            ("age", "range"),
            ("handle", "custom")
        ]
    );
    assert_eq!(
        person("ok", 30, None, "x")
            .validate()
            .unwrap_err()
            .to_string(),
        "email: is required for adults"
    );
    assert!(person("ok", 10, None, "kid").validate().is_ok());
    let errors = person("ok", 10, None, "").validate().unwrap_err();
    assert_eq!(
        errors.field("handle").next().unwrap().message,
        "must not be empty"
    );
}

#[rok_db::test]
async fn validation_and_hooks_guard_writes(db: Db) {
    db.execute("CREATE TABLE persons (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, email TEXT, age INT NOT NULL, handle TEXT NOT NULL)")
        .await
        .unwrap();
    let before = INSERTED.load(Ordering::SeqCst);

    let err = person("", 5, None, "x").insert(&db).await.unwrap_err();
    assert!(
        err.validation_errors()
            .is_some_and(|e| e.field("name").count() == 1),
        "{err}"
    );
    let err = person("ok", 5, None, "root").insert(&db).await.unwrap_err();
    assert!(matches!(err, Error::Hook(_)) && err.to_string().contains("reserved"));
    assert_eq!(Person::count(&db).await.unwrap(), 0, "nothing was written");

    let mut ann = person("ann", 30, Some("ann@x.io"), "ann")
        .insert(&db)
        .await
        .unwrap();
    Person::insert_all(&db, &[person("kid", 9, None, "kid")])
        .await
        .unwrap();
    assert_eq!(
        INSERTED.load(Ordering::SeqCst) - before,
        2,
        "after_insert ran for each row"
    );
    let err = Person::insert_many(&[person("ok", 1, None, "a"), person("bad", 999, None, "b")])
        .exec(&db)
        .await
        .unwrap_err();
    assert!(err.validation_errors().is_some());
    assert_eq!(
        Person::count(&db).await.unwrap(),
        2,
        "the whole batch was rejected"
    );

    ann.age = -1;
    assert!(
        ann.save(&db)
            .await
            .unwrap_err()
            .validation_errors()
            .is_some()
    );
    ann.age = 31;
    ann.name = "frozen".into();
    assert!(matches!(ann.save(&db).await.unwrap_err(), Error::Hook(_)));
    ann.name = "ann".into();
    let ann = ann.save(&db).await.unwrap();
    assert_eq!(ann.age, 31);

    // Upserts are validated like inserts.
    assert!(
        person("x", 999, None, "x")
            .upsert(&db)
            .await
            .unwrap_err()
            .validation_errors()
            .is_some()
    );

    let deleted = DELETED.load(Ordering::SeqCst);
    ann.delete(&db).await.unwrap();
    assert_eq!(DELETED.load(Ordering::SeqCst), deleted + 1);
    // Bulk operations bypass hooks and validation by design.
    Person::query()
        .update()
        .set(Person::AGE, 999)
        .exec(&db)
        .await
        .unwrap();
}

// ----- scopes ------------------------------------------------------------------

fn only_published() -> Expr<Article> {
    Article::PUBLISHED.eq(true)
}

fn popular(q: Select<Article>) -> Select<Article> {
    q.filter(Article::VIEWS.gte(100))
        .order_by(Article::VIEWS.desc())
}

#[derive(Debug, Clone, Model)]
#[rok(default_scope = only_published)]
struct Article {
    #[rok(generated)]
    id: i64,
    title: String,
    published: bool,
    views: i32,
}

#[rok_db::test]
async fn default_and_named_scopes(db: Db) {
    db.execute("CREATE TABLE articles (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, published BOOL NOT NULL, views INT NOT NULL)")
        .await
        .unwrap();
    let a = |title: &str, published, views| Article {
        id: 0,
        title: title.into(),
        published,
        views,
    };
    let rows = Article::insert_all(
        &db,
        &[
            a("draft", false, 500),
            a("hit", true, 300),
            a("meh", true, 5),
            a("ok", true, 150),
        ],
    )
    .await
    .unwrap();

    assert_eq!(Article::count(&db).await.unwrap(), 3);
    assert_eq!(Article::query().unscoped().count(&db).await.unwrap(), 4);
    assert!(
        Article::find(&db, rows[0].id).await.unwrap().is_none(),
        "find is scoped"
    );
    assert_eq!(
        rows[0].reload(&db).await.unwrap().title,
        "draft",
        "record operations are not"
    );

    let top: Vec<String> = Article::query()
        .scope(popular)
        .all(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.title)
        .collect();
    assert_eq!(top, ["hit", "ok"]);
    let all_top = Article::query()
        .unscoped()
        .scope(popular)
        .count(&db)
        .await
        .unwrap();
    assert_eq!(all_top, 3);

    // Bulk updates respect the scope; saving an out-of-scope record works.
    assert_eq!(
        Article::update_all()
            .set(Article::VIEWS, 0)
            .exec(&db)
            .await
            .unwrap(),
        3
    );
    let mut draft = rows[0].clone();
    draft.title = "draft v2".into();
    assert_eq!(draft.save(&db).await.unwrap().views, 500);
    draft.delete(&db).await.unwrap();
    assert_eq!(Article::query().unscoped().count(&db).await.unwrap(), 3);
}

// ----- transactions with retries -----------------------------------------------

#[rok_db::test]
async fn transactions_retry_serialization_failures(db: Db) {
    db.execute("CREATE TABLE counters (id INT PRIMARY KEY, n INT NOT NULL); INSERT INTO counters VALUES (1, 0);")
        .await
        .unwrap();

    // Fail the first two attempts with SQLSTATE 40001, as PostgreSQL does for
    // conflicting serializable transactions.
    let attempts = Arc::new(AtomicU32::new(0));
    let opts = TxOptions::new()
        .isolation(Isolation::Serializable)
        .retries(3);
    let counter = attempts.clone();
    let level: String = db
        .transaction_with(opts.clone(), move |tx| {
            let counter = counter.clone();
            Box::pin(async move {
                raw("UPDATE counters SET n = n + 1 WHERE id = 1")
                    .execute(&mut *tx)
                    .await?;
                if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                    raw("DO $$ BEGIN RAISE EXCEPTION 'conflict' USING ERRCODE = '40001'; END $$")
                        .execute(&mut *tx)
                        .await?;
                }
                raw("SHOW transaction_isolation")
                    .scalar::<String, _>(&mut *tx)
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(level, "serializable");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let n: i32 = raw("SELECT n FROM counters").scalar(&db).await.unwrap();
    assert_eq!(n, 1, "failed attempts were rolled back");

    // Retries run out.
    let err = db
        .transaction_with(TxOptions::new().retries(1), |tx| {
            Box::pin(async move {
                raw("DO $$ BEGIN RAISE EXCEPTION 'deadlock' USING ERRCODE = '40P01'; END $$")
                    .execute(&mut *tx)
                    .await
            })
        })
        .await
        .unwrap_err();
    assert!(err.is_serialization_failure());

    // Other errors are not retried.
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let err = db
        .transaction_with(TxOptions::new().retries(5), move |tx| {
            let c = c.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                raw("SELECT * FROM missing").execute(&mut *tx).await
            })
        })
        .await
        .unwrap_err();
    assert!(!err.is_serialization_failure());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Read-only transactions reject writes.
    let err = db
        .transaction_with(TxOptions::new().read_only(), |tx| {
            Box::pin(async move { raw("UPDATE counters SET n = 0").execute(&mut *tx).await })
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
}

// ----- the test helper itself ----------------------------------------------------

#[rok_db::test(sql = "tests/fixtures/schema.sql")]
async fn test_attribute_runs_sql_files(db: Db) -> rok_db::Result<()> {
    let n: i64 = raw("SELECT COUNT(*) FROM fixture_items")
        .scalar(&db)
        .await?;
    assert_eq!(n, 2);
    Ok(())
}

#[tokio::test]
async fn test_databases_are_dropped() {
    let Some(test_db) = TestDb::create().await.unwrap() else {
        return;
    };
    let name = test_db.name().to_owned();
    let current: String = raw("SELECT current_database()")
        .scalar(test_db.db())
        .await
        .unwrap();
    assert_eq!(current, name);
    drop(test_db);

    let admin = Db::connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let exists: bool = raw("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = ?)")
        .bind(name.as_str())
        .scalar(&admin)
        .await
        .unwrap();
    assert!(!exists, "{name} should have been dropped");
}
