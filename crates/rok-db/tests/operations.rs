//! Change tracking and bulk COPY. Each test gets its own temporary database.
#![cfg(feature = "testing")]

use std::time::Duration;

use rok_db::prelude::*;
use rok_db::{Error, Value, raw};

#[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
enum Plan {
    Free,
    Pro,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Customer {
    #[rok(generated)]
    id: i64,
    #[rok(validate(non_empty))]
    name: String,
    email: Option<String>,
    plan: Plan,
    credits: i32,
    #[rok(version)]
    version: i32,
}

const SCHEMA: &str = "CREATE TABLE customers (
    id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, email TEXT, plan TEXT NOT NULL,
    credits INT NOT NULL, version INT NOT NULL
)";

fn customer(name: &str, credits: i32) -> Customer {
    Customer {
        id: 0,
        name: name.into(),
        email: None,
        plan: Plan::Free,
        credits,
        version: 1,
    }
}

#[rok_db::test]
async fn tracked_saves_only_changes(db: Db) {
    db.execute(SCHEMA).await.unwrap();
    let stored = customer("ann", 10).insert(&db).await.unwrap();

    let mut ann = stored.clone().track();
    assert!(!ann.is_dirty());
    assert!(!ann.save(&db).await.unwrap(), "nothing to write");

    ann.name = "Ann".into();
    ann.plan = Plan::Pro;
    assert!(ann.is_dirty() && ann.is_changed(Customer::NAME) && !ann.is_changed(Customer::CREDITS));
    let changes = ann.changes();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0], ("name", Value::from("ann"), Value::from("Ann")));

    // Someone else changes `credits` concurrently (bypassing the version
    // column), then our save writes only name and plan.
    raw("UPDATE customers SET credits = 99 WHERE id = ?")
        .bind(stored.id)
        .execute(&db)
        .await
        .unwrap();
    assert!(ann.save(&db).await.unwrap());
    assert!(!ann.is_dirty(), "snapshot refreshed");
    assert_eq!(
        (ann.name.as_str(), ann.plan, ann.credits, ann.version),
        ("Ann", Plan::Pro, 99, 2)
    );

    // Version checks still apply.
    let mut stale = stored.track();
    stale.credits = 1;
    assert!(stale.save(&db).await.unwrap_err().is_conflict());

    // Validation still applies.
    ann.name.clear();
    assert!(
        ann.save(&db)
            .await
            .unwrap_err()
            .validation_errors()
            .is_some()
    );
    ann.mark_clean();
    assert!(!ann.is_dirty());
    let ann: Customer = ann.into_inner();
    assert_eq!(ann.reload(&db).await.unwrap().name, "Ann");

    // save_only writes just the listed columns.
    let mut fresh = ann.reload(&db).await.unwrap();
    fresh.name = "ignored".into();
    fresh.credits = 5;
    let saved = fresh
        .save_only(&db, Some(vec![Customer::CREDITS]))
        .await
        .unwrap();
    assert_eq!((saved.name.as_str(), saved.credits), ("Ann", 5));
}

#[rok_db::test]
async fn copy_in_bulk_loads(db: Db) {
    db.execute(SCHEMA).await.unwrap();
    let rows: Vec<Customer> = (0..10_000)
        .map(|i| Customer {
            email: (i % 2 == 0).then(|| format!("c{i}@x.io")),
            plan: if i % 10 == 0 { Plan::Pro } else { Plan::Free },
            ..customer(&format!("c{i}"), i)
        })
        .collect();
    assert_eq!(Customer::copy_in(&db, &rows).await.unwrap(), 10_000);
    assert_eq!(Customer::count(&db).await.unwrap(), 10_000);
    assert_eq!(
        Customer::filter(Customer::PLAN.eq(Plan::Pro))
            .count(&db)
            .await
            .unwrap(),
        1_000
    );
    assert_eq!(
        Customer::filter(Customer::EMAIL.is_null())
            .count(&db)
            .await
            .unwrap(),
        5_000
    );
    let last = Customer::filter(Customer::NAME.eq("c9999"))
        .one(&db)
        .await
        .unwrap();
    assert_eq!((last.credits, last.email), (9_999, None));
    assert_eq!(Customer::copy_in(&db, &[]).await.unwrap(), 0);

    // In a transaction, rolled back.
    let mut tx = db.begin().await.unwrap();
    Customer::copy_in(&mut tx, &rows[..10]).await.unwrap();
    assert_eq!(Customer::count(&mut *tx).await.unwrap(), 10_010);
    tx.rollback().await.unwrap();
    assert_eq!(Customer::count(&db).await.unwrap(), 10_000);

    // Validation runs before anything is sent.
    let err = Customer::copy_in(&db, &[customer("ok", 1), customer("", 1)])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)));

    // Database errors surface and leave the connection usable.
    let err = Customer::copy_in(
        &db,
        &[Customer {
            name: "x\0y".into(),
            ..customer("x", 1)
        }],
    )
    .await;
    assert!(err.is_err());
    assert_eq!(Customer::count(&db).await.unwrap(), 10_000);
}

#[rok_db::test]
async fn copy_invalidates_the_cache(db: Db) {
    db.execute(SCHEMA).await.unwrap();
    let db = Db::builder()
        .query_cache(10)
        .build_with_pool(db.pool().clone());
    let count = || Customer::query().memoize(Duration::from_secs(60));
    assert_eq!(count().count(&db).await.unwrap(), 0);
    Customer::copy_in(&db, &[customer("a", 1)]).await.unwrap();
    assert_eq!(count().count(&db).await.unwrap(), 1);
}

#[rok_db::test]
async fn pool_stats(db: Db) {
    raw("SELECT 1").execute(&db).await.unwrap();
    let stats = db.stats();
    assert!(stats.size >= 1 && stats.max_connections == 5, "{stats:?}");
    assert!(stats.idle <= stats.size as usize);
}
