//! Composite primary keys.
#![cfg(feature = "testing")]

use std::time::Duration;

use rok_db::prelude::*;
use rok_db::{ChangeOp, Error};

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(has_many(notes = Note::MEMBERSHIP_ORG))]
struct Membership {
    #[rok(primary_key)]
    org_id: i64,
    #[rok(primary_key)]
    user_id: i64,
    role: String,
    #[rok(version)]
    version: i32,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Note {
    #[rok(generated)]
    id: i64,
    membership_org: i64,
}

const SCHEMA: &str = "
    CREATE TABLE memberships (
        org_id BIGINT NOT NULL, user_id BIGINT NOT NULL, role TEXT NOT NULL, version INT NOT NULL,
        PRIMARY KEY (org_id, user_id)
    );
    CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, membership_org BIGINT NOT NULL);
";

fn m(org_id: i64, user_id: i64, role: &str) -> Membership {
    Membership {
        org_id,
        user_id,
        role: role.into(),
        version: 1,
    }
}

#[test]
fn metadata() {
    assert_eq!(Membership::PRIMARY_KEYS, ["org_id", "user_id"]);
    assert_eq!(Membership::PRIMARY_KEY, "org_id");
    assert_eq!(
        Note::PRIMARY_KEYS,
        ["id"],
        "single keys default to PRIMARY_KEY"
    );
    assert_eq!(
        m(1, 2, "x").key_values(),
        [rok_db::Value::from(1_i64), rok_db::Value::from(2_i64)]
    );
}

#[rok_db::test]
async fn crud_by_composite_key(db: Db) {
    db.execute(SCHEMA).await.unwrap();
    Membership::insert_all(
        &db,
        &[m(1, 1, "owner"), m(1, 2, "member"), m(2, 1, "member")],
    )
    .await
    .unwrap();

    let found = Membership::find(&db, (1_i64, 2_i64))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.role, "member");
    assert!(
        Membership::find(&db, (2_i64, 2_i64))
            .await
            .unwrap()
            .is_none()
    );
    let err = Membership::find_or_fail(&db, (9_i64, 9_i64))
        .await
        .unwrap_err();
    assert!(
        err.is_not_found() && err.to_string().contains("(9, 9)"),
        "{err}"
    );
    let err = Membership::find(&db, 1_i64).await.unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery(_)),
        "wrong number of key values"
    );

    let many = Membership::find_many(&db, [(1_i64, 1_i64), (2_i64, 1_i64), (3_i64, 3_i64)])
        .await
        .unwrap();
    assert_eq!(many.len(), 2);

    // save/delete touch exactly one row, with version checks.
    let mut promoted = found.clone();
    promoted.role = "admin".into();
    let promoted = promoted.save(&db).await.unwrap();
    assert_eq!((promoted.role.as_str(), promoted.version), ("admin", 2));
    assert!(found.save(&db).await.unwrap_err().is_conflict());
    assert_eq!(
        Membership::filter(Membership::ROLE.eq("admin"))
            .count(&db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(promoted.reload(&db).await.unwrap(), promoted);
    promoted.delete(&db).await.unwrap();
    assert_eq!(Membership::count(&db).await.unwrap(), 2);

    // Upsert targets the whole key.
    let upserted = Membership {
        role: "billing".into(),
        ..m(1, 1, "")
    }
    .upsert(&db)
    .await
    .unwrap();
    assert_eq!((upserted.role.as_str(), upserted.version), ("billing", 2));
    let inserted = m(5, 5, "new").upsert(&db).await.unwrap();
    assert_eq!(inserted.version, 1);

    // Keyset pagination uses every key column as a tiebreaker.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = Membership::order_by(Membership::ROLE)
            .cursor_paginate(&db, cursor.as_ref(), 1)
            .await
            .unwrap();
        seen.extend(page.items.iter().map(|m| (m.org_id, m.user_id)));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    seen.sort();
    assert_eq!(seen, [(1, 1), (2, 1), (5, 5)]);

    // Relations need a single-column parent key.
    let err = Membership::NOTES.load(&db, &[inserted]).await.unwrap_err();
    assert!(err.to_string().contains("single-column primary key"));
}

#[rok_db::test]
async fn change_feed_with_composite_keys(db: Db) {
    db.execute(SCHEMA).await.unwrap();
    Membership::install_change_notifications(&db).await.unwrap();
    let mut changes = Membership::changes(&db).await.unwrap();

    m(7, 42, "x").insert(&db).await.unwrap();
    let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (change.op, change.key.as_str()),
        (ChangeOp::Insert, r#"["7", "42"]"#)
    );
    assert_eq!(change.fetch(&db).await.unwrap().unwrap().role, "x");
}

#[cfg(feature = "json")]
#[rok_db::test]
async fn audit_history_with_composite_keys(db: Db) {
    use rok_db::audit;

    db.execute(SCHEMA).await.unwrap();
    audit::install(&db).await.unwrap();
    audit::enable::<Membership>(&db, &[]).await.unwrap();
    let row = m(3, 4, "a").insert(&db).await.unwrap();
    m(3, 5, "other").insert(&db).await.unwrap();
    let mut edited = row.clone();
    edited.role = "b".into();
    edited.save(&db).await.unwrap();

    let history = audit::history::<Membership>(&db, (3_i64, 4_i64))
        .await
        .unwrap();
    assert_eq!(
        history.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        ["INSERT", "UPDATE"]
    );
    assert_eq!(history[1].changed, ["role", "version"]);
}
