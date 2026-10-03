//! Shared cache invalidation, multi-tenancy and the audit log.
#![cfg(feature = "testing")]

use std::time::Duration;

use rok_db::prelude::*;
use rok_db::tenant::with_tenant;
use rok_db::{Error, raw};

#[derive(Debug, Clone, PartialEq, Model)]
struct Invoice {
    #[rok(generated)]
    id: i64,
    #[rok(tenant)]
    org_id: i64,
    total: i64,
}

const INVOICES: &str = "CREATE TABLE invoices (id BIGSERIAL PRIMARY KEY, org_id BIGINT NOT NULL, total BIGINT NOT NULL)";

fn invoice(org_id: i64, total: i64) -> Invoice {
    Invoice {
        id: 0,
        org_id,
        total,
    }
}

#[rok_db::test]
async fn shared_cache_invalidation(db: Db) {
    db.execute(INVOICES).await.unwrap();
    let instance = || {
        Db::builder()
            .query_cache(100)
            .shared_cache_invalidation()
            .build_with_pool(db.pool().clone())
    };
    let (a, b) = (instance(), instance());
    // Let both background listeners subscribe.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cached = |db: Db| async move {
        Invoice::query()
            .all_tenants()
            .memoize(Duration::from_secs(600))
            .count(&db)
            .await
            .unwrap()
    };
    assert_eq!(cached(b.clone()).await, 0);
    assert_eq!(cached(b.clone()).await, 0);
    assert_eq!(b.cache().unwrap().stats().hits, 1);

    // A write on instance `a` invalidates instance `b`'s cache.
    invoice(1, 10).insert(&a).await.unwrap();
    let mut seen = 0;
    for _ in 0..50 {
        seen = cached(b.clone()).await;
        if seen == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(seen, 1, "instance b saw the write");

    // Clearing propagates too.
    assert_eq!(cached(a.clone()).await, 1);
    b.cache().unwrap().clear();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.cache().unwrap().stats().entries, 0);
}

#[rok_db::test]
async fn tenancy_is_enforced(db: Db) {
    db.execute(INVOICES).await.unwrap();

    // Inserts are stamped with the current tenant, whatever the record says.
    let (mine, theirs) = (
        with_tenant(1_i64, invoice(999, 10).insert(&db))
            .await
            .unwrap(),
        with_tenant(2_i64, Invoice::create().set(Invoice::TOTAL, 20).exec(&db))
            .await
            .unwrap(),
    );
    assert_eq!((mine.org_id, theirs.org_id), (1, 2));
    with_tenant(
        1_i64,
        Invoice::insert_all(&db, &[invoice(0, 30), invoice(0, 40)]),
    )
    .await
    .unwrap();
    with_tenant(1_i64, Invoice::copy_in(&db, &[invoice(7, 50)]))
        .await
        .unwrap();

    with_tenant(1_i64, async {
        assert_eq!(Invoice::count(&db).await.unwrap(), 4);
        assert!(
            Invoice::find(&db, theirs.id).await.unwrap().is_none(),
            "other tenant's row is invisible"
        );
        assert_eq!(
            Invoice::query().sum(&db, Invoice::TOTAL).await.unwrap(),
            Some(130)
        );

        // Bulk writes only touch this tenant.
        assert_eq!(
            Invoice::update_all()
                .increment(Invoice::TOTAL, 1)
                .exec(&db)
                .await
                .unwrap(),
            4
        );
        let err = Invoice::update_all()
            .set(Invoice::ORG_ID, 2)
            .exec(&db)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidQuery(_)));

        // Record operations can't reach another tenant's row.
        let mut stolen = theirs.clone();
        stolen.total = 0;
        assert!(stolen.save(&db).await.unwrap_err().is_not_found());
        assert!(theirs.delete(&db).await.unwrap_err().is_not_found());

        // Upserting another tenant's primary key does nothing.
        let hijack = Invoice {
            id: theirs.id,
            org_id: 1,
            total: 0,
        };
        assert!(hijack.upsert(&db).await.unwrap_err().is_not_found());

        // Saving keeps the tenant even if the field was changed.
        let mut moved = mine.reload(&db).await.unwrap();
        moved.org_id = 2;
        assert_eq!(moved.save(&db).await.unwrap().org_id, 1);
    })
    .await;

    // Outside a scope: fail closed, unless explicitly opted out.
    assert_eq!(Invoice::count(&db).await.unwrap(), 0);
    assert_eq!(Invoice::query().all_tenants().count(&db).await.unwrap(), 5);
    let untouched = Invoice::query()
        .all_tenants()
        .filter(Invoice::ID.eq(theirs.id))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(untouched.total, 20);
    // Raw SQL is not filtered.
    let n: i64 = raw("SELECT COUNT(*) FROM invoices")
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(n, 5);

    // Tenants don't share memoized results.
    let db = Db::builder()
        .query_cache(10)
        .build_with_pool(db.pool().clone());
    let count = |t: i64| {
        let db = db.clone();
        with_tenant(t, async move {
            Invoice::query()
                .memoize(Duration::from_secs(60))
                .count(&db)
                .await
                .unwrap()
        })
    };
    assert_eq!((count(1).await, count(2).await), (4, 1));
}

#[cfg(feature = "json")]
#[rok_db::test]
async fn audit_log(db: Db) {
    use rok_db::audit::{self, AuditEntry};

    #[derive(Debug, Clone, Model)]
    struct Account {
        #[rok(generated)]
        id: i64,
        email: String,
        password_hash: String,
        logins: i32,
    }

    db.execute("CREATE TABLE accounts (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL, password_hash TEXT NOT NULL, logins INT NOT NULL)")
        .await
        .unwrap();
    audit::install(&db).await.unwrap();
    audit::install(&db).await.unwrap(); // idempotent
    audit::enable::<Account>(&db, &[Account::PASSWORD_HASH])
        .await
        .unwrap();

    let acct = audit::with_actor("user:7", async {
        db.transaction(|tx| {
            Box::pin(async move {
                Account {
                    id: 0,
                    email: "a@x.io".into(),
                    password_hash: "secret".into(),
                    logins: 0,
                }
                .insert(&mut *tx)
                .await
            })
        })
        .await
    })
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    tx.set_actor("admin").await.unwrap();
    let mut edited = acct.clone();
    edited.email = "b@x.io".into();
    let edited = edited.save(&mut *tx).await.unwrap();
    edited.save(&mut *tx).await.unwrap(); // no change: not logged
    tx.commit().await.unwrap();

    // Bulk and raw writes are captured too (no actor outside a transaction).
    Account::update_all()
        .increment(Account::LOGINS, 1)
        .exec(&db)
        .await
        .unwrap();
    raw("UPDATE accounts SET password_hash = 'rotated'")
        .execute(&db)
        .await
        .unwrap(); // excluded column only
    edited.delete(&db).await.unwrap();

    let history = audit::history::<Account>(&db, acct.id).await.unwrap();
    let summary: Vec<_> = history
        .iter()
        .map(|e| (e.op.as_str(), e.actor.as_deref(), e.changed.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            ("INSERT", Some("user:7"), vec![]),
            ("UPDATE", Some("admin"), vec!["email".to_string()]),
            ("UPDATE", None, vec!["logins".to_string()]),
            ("DELETE", None, vec![]),
        ]
    );
    let insert = &history[0];
    assert_eq!(insert.new_data.as_ref().unwrap()["email"], "a@x.io");
    assert!(
        insert
            .new_data
            .as_ref()
            .unwrap()
            .get("password_hash")
            .is_none(),
        "excluded column"
    );
    assert_eq!(history[1].old_data.as_ref().unwrap()["email"], "a@x.io");
    assert!(history[3].new_data.is_none());
    assert!(history[0].at() <= std::time::SystemTime::now());

    // The log is a regular model.
    let by_admin = AuditEntry::for_table::<Account>()
        .filter(AuditEntry::ACTOR.eq("admin"))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(by_admin, 1);

    audit::disable::<Account>(&db).await.unwrap();
    Account {
        id: 0,
        email: "c@x.io".into(),
        password_hash: "x".into(),
        logins: 0,
    }
    .insert(&db)
    .await
    .unwrap();
    assert_eq!(
        AuditEntry::for_table::<Account>().count(&db).await.unwrap(),
        4
    );
}
