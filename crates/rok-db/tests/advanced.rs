//! Keyset pagination, soft deletes, optimistic locking, upserts and
//! subqueries.
//!
//! SQL-shape tests always run; database tests need `DATABASE_URL` (see
//! `tests/postgres.rs`).

use rok_db::prelude::*;
use rok_db::{__private, Cursor, Error};

#[derive(Debug, Clone, PartialEq, Model)]
struct Account {
    #[rok(generated)]
    id: i64,
    email: String,
    name: String,
    score: i32,
    #[rok(version)]
    version: i32,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(table = "notes", soft_delete)]
struct VersionedNote {
    #[rok(generated)]
    id: i64,
    body: String,
    #[rok(version)]
    version: i32,
    deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(soft_delete)]
struct Doc {
    #[rok(generated)]
    id: i64,
    owner_id: i64,
    title: String,
    deleted_at: Option<String>,
}

const SCHEMA: &str = r#"
CREATE TEMP TABLE accounts (
    id BIGSERIAL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    score INT NOT NULL,
    version INT NOT NULL DEFAULT 1
);
CREATE TEMP TABLE docs (
    id BIGSERIAL PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES accounts (id),
    title TEXT NOT NULL,
    deleted_at TEXT
);
CREATE TEMP TABLE notes (
    id BIGSERIAL PRIMARY KEY,
    body TEXT NOT NULL,
    version INT NOT NULL,
    deleted_at TEXT
);
"#;

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
    db.execute(SCHEMA).await.expect("schema");
    Some(db)
}

fn account(email: &str, score: i32) -> Account {
    Account {
        id: 0,
        email: email.into(),
        name: email.into(),
        score,
        version: 1,
    }
}

async fn seed_accounts(db: &Db, n: i32) -> Vec<Account> {
    // Scores repeat (0, 0, 1, 1, …) so the primary key tiebreaker matters.
    let accounts: Vec<_> = (0..n).map(|i| account(&format!("a{i}@x"), i / 2)).collect();
    Account::insert_all(db, &accounts).await.unwrap()
}

// ----- SQL shape -------------------------------------------------------------

#[test]
fn cursor_sql() {
    let cursor = Cursor::new([5.into(), 42_i64.into()]);
    let sql = __private::__cursor_sql(Account::order_by(Account::SCORE.desc()), Some(&cursor), 10)
        .unwrap();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "id", "email", "name", "score", "version" FROM "accounts" WHERE ("score" < $1 OR ("score" = $2 AND "id" > $3)) ORDER BY "score" DESC, "id" ASC LIMIT 11"#
    );
    let wrong = Cursor::new([1.into()]);
    assert!(matches!(
        __private::__cursor_sql(Account::order_by(Account::SCORE), Some(&wrong), 10),
        Err(Error::InvalidCursor(_))
    ));
}

#[test]
fn soft_delete_sql() {
    assert_eq!(Doc::DELETED_AT_COLUMN, Some("deleted_at"));
    assert!(
        Doc::filter(Doc::OWNER_ID.eq(1))
            .to_sql()
            .as_str()
            .ends_with(r#"WHERE "owner_id" = $1 AND "deleted_at" IS NULL"#)
    );
    assert!(
        Doc::query()
            .with_trashed()
            .to_sql()
            .as_str()
            .ends_with(r#"FROM "docs""#)
    );
    assert!(
        Doc::query()
            .only_trashed()
            .to_sql()
            .as_str()
            .ends_with(r#"WHERE "deleted_at" IS NOT NULL"#)
    );
    assert_eq!(
        Doc::filter(Doc::ID.eq(1)).delete_sql().as_str(),
        r#"UPDATE "docs" SET "deleted_at" = now() WHERE "id" = $1 AND "deleted_at" IS NULL"#
    );
    // Non-soft models still DELETE.
    assert!(
        Account::query()
            .delete_sql()
            .as_str()
            .starts_with("DELETE FROM")
    );
}

#[test]
fn version_sql() {
    assert_eq!(Account::VERSION_COLUMN, Some("version"));
    assert_eq!(
        Account::update_all()
            .set(Account::NAME, "x")
            .to_sql()
            .as_str(),
        r#"UPDATE "accounts" SET "name" = $1, "version" = "version" + 1"#
    );
}

#[test]
fn upsert_sql() {
    let rows = [account("a@x", 1)];
    assert_eq!(
        Account::insert_many(&rows)
            .on_conflict([Account::EMAIL])
            .do_update([Account::NAME])
            .to_sql()
            .as_str(),
        r#"INSERT INTO "accounts" ("email", "name", "score", "version") VALUES ($1, $2, $3, $4) ON CONFLICT ("email") DO UPDATE SET "name" = EXCLUDED."name", "version" = "accounts"."version" + 1 RETURNING "id", "email", "name", "score", "version""#
    );
    assert!(
        Account::insert_many(&rows)
            .do_nothing()
            .to_sql()
            .as_str()
            .contains("ON CONFLICT DO NOTHING RETURNING")
    );
    assert!(
        Account::create()
            .set(Account::EMAIL, "a@x")
            .on_constraint("accounts_email_key")
            .do_update_all()
            .to_sql()
            .as_str()
            .contains(r#"ON CONFLICT ON CONSTRAINT "accounts_email_key" DO UPDATE SET "email" = EXCLUDED."email", "version" = "accounts"."version" + 1"#)
    );
}

#[test]
fn subquery_sql() {
    let sql = Account::filter(Account::SCORE.gt(1))
        .filter(Account::ID.in_subquery(Doc::filter(Doc::TITLE.eq("x")).select(Doc::OWNER_ID)))
        .filter(Expr::not_exists(Doc::filter(
            Doc::OWNER_ID.eq_outer(Account::ID),
        )))
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "id", "email", "name", "score", "version" FROM "accounts" WHERE "score" > $1 AND "id" IN (SELECT "owner_id" FROM "docs" WHERE "title" = $2 AND "deleted_at" IS NULL) AND NOT EXISTS (SELECT 1 FROM "docs" WHERE "docs"."owner_id" = "accounts"."id" AND "deleted_at" IS NULL)"#
    );
    assert_eq!(sql.params().len(), 2);
}

// ----- database --------------------------------------------------------------

#[tokio::test]
async fn keyset_pagination_walks_everything_once() {
    let Some(db) = setup().await else { return };
    let all = seed_accounts(&db, 25).await;

    for (order, expected) in [
        (Account::SCORE.asc(), {
            let mut v = all.clone();
            v.sort_by_key(|a| (a.score, a.id));
            v
        }),
        (Account::SCORE.desc(), {
            let mut v = all.clone();
            v.sort_by_key(|a| (std::cmp::Reverse(a.score), a.id));
            v
        }),
    ] {
        let mut seen = Vec::new();
        let mut cursor: Option<Cursor> = None;
        let mut pages = 0;
        loop {
            let page = Account::order_by(order.clone())
                .cursor_paginate(&db, cursor.as_ref(), 7)
                .await
                .unwrap();
            pages += 1;
            seen.extend(page.items.iter().cloned());
            // Round-trip through the string form a client would send back.
            cursor = match page.next {
                Some(next) => Some(next.to_string().parse().unwrap()),
                None => break,
            };
        }
        assert_eq!(pages, 4);
        assert_eq!(seen, expected);
    }

    // Filters combine with the cursor condition.
    let page = Account::filter(Account::SCORE.gte(10))
        .cursor_paginate(&db, None, 100)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 5);
    assert!(!page.has_next());
    assert!(
        Account::query()
            .cursor_paginate(&db, None, 0)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn soft_deletes() {
    let Some(db) = setup().await else { return };
    let owner = account("o@x", 1).insert(&db).await.unwrap();
    let docs: Vec<Doc> = (0..3)
        .map(|i| Doc {
            id: 0,
            owner_id: owner.id,
            title: format!("d{i}"),
            deleted_at: None,
        })
        .collect();
    let docs = Doc::insert_all(&db, &docs).await.unwrap();

    docs[0].delete(&db).await.unwrap();
    assert_eq!(Doc::count(&db).await.unwrap(), 2);
    assert_eq!(Doc::query().with_trashed().count(&db).await.unwrap(), 3);
    assert_eq!(Doc::query().only_trashed().count(&db).await.unwrap(), 1);
    assert!(Doc::find(&db, docs[0].id).await.unwrap().is_none());

    let trashed = docs[0].reload(&db).await.unwrap();
    assert!(trashed.is_trashed() && !docs[1].is_trashed());
    assert!(
        docs[0].delete(&db).await.unwrap_err().is_not_found(),
        "already deleted"
    );

    let restored = trashed.restore(&db).await.unwrap();
    assert!(!restored.is_trashed());
    assert_eq!(Doc::count(&db).await.unwrap(), 3);

    // Bulk soft delete, scoped updates, restore and purge.
    assert_eq!(
        Doc::filter(Doc::TITLE.ne("d2")).delete(&db).await.unwrap(),
        2
    );
    assert_eq!(
        Doc::update_all()
            .set(Doc::TITLE, "x")
            .exec(&db)
            .await
            .unwrap(),
        1,
        "skips trashed"
    );
    assert_eq!(
        Doc::filter(Doc::TITLE.eq("d0")).restore(&db).await.unwrap(),
        1
    );
    assert_eq!(
        Doc::query().only_trashed().force_delete(&db).await.unwrap(),
        1
    );
    assert_eq!(Doc::query().with_trashed().count(&db).await.unwrap(), 2);

    docs[2].force_delete(&db).await.unwrap();
    assert_eq!(Doc::query().with_trashed().count(&db).await.unwrap(), 1);

    // Account has no soft deletes: restore is an error.
    assert!(matches!(
        Account::query().restore(&db).await,
        Err(Error::InvalidQuery(_))
    ));
}

#[tokio::test]
async fn optimistic_locking() {
    let Some(db) = setup().await else { return };
    let acct = account("v@x", 1).insert(&db).await.unwrap();
    assert_eq!(acct.version, 1);

    // Two copies of the same record.
    let mut alice = acct.clone();
    let mut bob = acct.clone();

    alice.score = 10;
    let alice = alice.save(&db).await.unwrap();
    assert_eq!(alice.version, 2);

    bob.score = 20;
    let err = bob.save(&db).await.unwrap_err();
    assert!(err.is_conflict(), "{err}");
    assert!(err.to_string().contains("modified concurrently"));
    assert_eq!(
        acct.reload(&db).await.unwrap().score,
        10,
        "bob's write was rejected"
    );

    // Reload and retry succeeds.
    let mut bob = bob.reload(&db).await.unwrap();
    bob.score = 20;
    let bob = bob.save(&db).await.unwrap();
    assert_eq!((bob.score, bob.version), (20, 3));

    // Bulk updates bump the version too.
    Account::update_all()
        .increment(Account::SCORE, 1)
        .exec(&db)
        .await
        .unwrap();
    let fresh = bob.reload(&db).await.unwrap();
    assert_eq!(fresh.version, 4);

    // Deletes are checked as well.
    assert!(bob.delete(&db).await.unwrap_err().is_conflict());
    fresh.delete(&db).await.unwrap();
    assert!(fresh.delete(&db).await.unwrap_err().is_not_found());
    assert!(
        fresh.save(&db).await.unwrap_err().is_not_found(),
        "missing, not conflict"
    );

    // Soft delete + version: a second delete is "not found", a stale one a conflict.
    let note = VersionedNote {
        id: 0,
        body: "n".into(),
        version: 1,
        deleted_at: None,
    }
    .insert(&db)
    .await
    .unwrap();
    let mut edited = note.clone();
    edited.body = "m".into();
    let edited = edited.save(&db).await.unwrap();
    assert!(note.delete(&db).await.unwrap_err().is_conflict());
    edited.delete(&db).await.unwrap();
    let trashed = edited.reload(&db).await.unwrap();
    assert!(trashed.is_trashed() && trashed.version == 3);
    assert!(trashed.delete(&db).await.unwrap_err().is_not_found());
    let restored = trashed.restore(&db).await.unwrap();
    assert_eq!(restored.version, 4);

    // Inside a transaction too.
    let acct = account("t@x", 1).insert(&db).await.unwrap();
    let stale = acct.clone();
    let mut tx = db.begin().await.unwrap();
    acct.save(&mut *tx).await.unwrap();
    assert!(stale.save(&mut *tx).await.unwrap_err().is_conflict());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn upserts() {
    let Some(db) = setup().await else { return };
    let first = account("u@x", 1).insert(&db).await.unwrap();

    // Upsert by a unique column; the primary key is kept, version bumped.
    let again = Account {
        name: "renamed".into(),
        score: 5,
        ..account("u@x", 0)
    }
    .upsert_on(&db, [Account::EMAIL])
    .await
    .unwrap();
    assert_eq!(
        (again.id, again.name.as_str(), again.score, again.version),
        (first.id, "renamed", 5, 2)
    );

    // DO NOTHING returns only inserted rows.
    let batch = [account("u@x", 9), account("new@x", 9)];
    let inserted = Account::insert_many(&batch)
        .on_conflict([Account::EMAIL])
        .do_nothing()
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(
        inserted
            .iter()
            .map(|a| a.email.as_str())
            .collect::<Vec<_>>(),
        ["new@x"]
    );
    assert_eq!(Account::count(&db).await.unwrap(), 2);

    // Partial DO UPDATE: only `score` changes.
    let updated = Account::insert_many(&[Account {
        name: "ignored".into(),
        ..account("u@x", 42)
    }])
    .on_conflict([Account::EMAIL])
    .do_update([Account::SCORE])
    .exec(&db)
    .await
    .unwrap();
    assert_eq!(
        (updated[0].name.as_str(), updated[0].score),
        ("renamed", 42)
    );

    // Column builder with a named constraint, and DO NOTHING → None.
    let skipped = Account::create()
        .set(Account::EMAIL, "u@x")
        .set(Account::NAME, "n")
        .set(Account::SCORE, 0)
        .on_conflict([Account::EMAIL])
        .do_nothing()
        .exec_optional(&db)
        .await
        .unwrap();
    assert!(skipped.is_none());
    let via_constraint = Account::create()
        .set(Account::EMAIL, "u@x")
        .set(Account::NAME, "c")
        .set(Account::SCORE, 7)
        .on_constraint("accounts_email_key")
        .do_update([Account::SCORE])
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(via_constraint.score, 7);

    // Misconfiguration is reported by exec.
    let err = Account::create()
        .set(Account::EMAIL, "z@x")
        .on_conflict([Account::EMAIL])
        .exec(&db)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery(_)));
    let err = Account::insert_many(&batch)
        .do_update_all()
        .exec(&db)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery(_)));
}

#[tokio::test]
async fn subqueries() {
    let Some(db) = setup().await else { return };
    let accounts = seed_accounts(&db, 4).await;
    let doc = |owner: &Account, title: &str| Doc {
        id: 0,
        owner_id: owner.id,
        title: title.into(),
        deleted_at: None,
    };
    let docs = Doc::insert_all(
        &db,
        &[
            doc(&accounts[0], "a"),
            doc(&accounts[0], "b"),
            doc(&accounts[1], "c"),
            doc(&accounts[2], "gone"),
        ],
    )
    .await
    .unwrap();
    docs[3].delete(&db).await.unwrap(); // soft-deleted: invisible to subqueries

    let ids = |v: Vec<Account>| v.into_iter().map(|a| a.id).collect::<Vec<_>>();

    let with_docs = Account::filter(Account::ID.in_subquery(Doc::query().select(Doc::OWNER_ID)))
        .order_by(Account::ID)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(ids(with_docs), [accounts[0].id, accounts[1].id]);

    let correlated = Account::filter(Expr::exists(
        Doc::filter(Doc::OWNER_ID.eq_outer(Account::ID)).filter(Doc::TITLE.eq("c")),
    ))
    .all(&db)
    .await
    .unwrap();
    assert_eq!(ids(correlated), [accounts[1].id]);

    let without = Account::filter(Expr::not_exists(Doc::filter(
        Doc::OWNER_ID.eq_outer(Account::ID),
    )))
    .order_by(Account::ID)
    .all(&db)
    .await
    .unwrap();
    assert_eq!(ids(without), [accounts[2].id, accounts[3].id]);

    let not_in = Account::filter(
        Account::ID.not_in_subquery(Doc::query().with_trashed().select(Doc::OWNER_ID)),
    )
    .all(&db)
    .await
    .unwrap();
    assert_eq!(ids(not_in), [accounts[3].id]);

    // Subqueries work in bulk updates and counts too.
    let n = Account::filter(
        Account::ID.in_subquery(Doc::filter(Doc::TITLE.eq("a")).select(Doc::OWNER_ID)),
    )
    .update()
    .set(Account::NAME, "author")
    .exec(&db)
    .await
    .unwrap();
    assert_eq!(n, 1);
}
