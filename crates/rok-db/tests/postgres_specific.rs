//! Arrays, JSONB, full-text search, read replicas and change notifications.
#![cfg(feature = "testing")]

use std::time::Duration;

use rok_db::prelude::*;
use rok_db::testing::TestDb;
use rok_db::{ChangeOp, raw};

#[derive(Debug, Clone, PartialEq, Model)]
struct Doc {
    #[rok(generated)]
    id: i64,
    title: String,
    body: String,
    tags: Vec<String>,
    scores: Vec<i32>,
}

const DOCS: &str = "CREATE TABLE docs (
    id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL,
    tags TEXT[] NOT NULL, scores INT[] NOT NULL
)";

fn doc(title: &str, body: &str, tags: &[&str], scores: &[i32]) -> Doc {
    Doc {
        id: 0,
        title: title.into(),
        body: body.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        scores: scores.to_vec(),
    }
}

async fn seed(db: &Db) -> Vec<Doc> {
    db.execute(DOCS).await.unwrap();
    Doc::insert_all(
        db,
        &[
            doc(
                "rust orm",
                "Running queries with a type-safe query builder",
                &["rust", "db"],
                &[1, 2],
            ),
            doc(
                "postgres tips",
                "Indexes make queries run faster",
                &["db", "postgres"],
                &[3],
            ),
            doc("cooking", "A recipe for bread", &["food"], &[]),
        ],
    )
    .await
    .unwrap()
}

#[rok_db::test]
async fn arrays(db: Db) {
    let docs = seed(&db).await;
    let titles = |v: Vec<Doc>| v.into_iter().map(|d| d.title).collect::<Vec<_>>();

    assert_eq!(docs[0].tags, ["rust", "db"]);
    let db_tagged = Doc::filter(Doc::TAGS.array_has("db"))
        .order_by(Doc::ID)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(titles(db_tagged), ["rust orm", "postgres tips"]);
    let both =
        Doc::filter(Doc::TAGS.array_contains(vec!["db".to_string(), "postgres".to_string()]))
            .all(&db)
            .await
            .unwrap();
    assert_eq!(titles(both), ["postgres tips"]);
    let any = Doc::filter(Doc::TAGS.array_overlaps(vec!["food".to_string(), "rust".to_string()]))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(any, 2);
    let ids: Vec<i64> = docs.iter().map(|d| d.id).take(2).collect();
    assert_eq!(
        Doc::filter(Doc::ID.eq_any(ids)).count(&db).await.unwrap(),
        2
    );
    assert_eq!(
        Doc::filter(Doc::SCORES.array_has(3))
            .count(&db)
            .await
            .unwrap(),
        1
    );

    // Arrays round-trip through save and COPY.
    let mut cooking = docs[2].clone();
    cooking.tags.push("baking".into());
    assert_eq!(cooking.save(&db).await.unwrap().tags, ["food", "baking"]);
    Doc::copy_in(&db, &[doc("copied", "x", &["a", "b"], &[9])])
        .await
        .unwrap();
    let copied = Doc::filter(Doc::TITLE.eq("copied")).one(&db).await.unwrap();
    assert_eq!(
        (copied.tags, copied.scores),
        (vec!["a".to_string(), "b".to_string()], vec![9])
    );
}

#[rok_db::test]
async fn full_text_search(db: Db) {
    seed(&db).await;
    let titles = |v: Vec<Doc>| v.into_iter().map(|d| d.title).collect::<Vec<_>>();

    // `english` stems "running"/"run"; the default config may not.
    let found = Doc::filter(Doc::BODY.search_in("english", "run"))
        .order_by(Doc::ID)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(titles(found), ["rust orm", "postgres tips"]);
    let phrase = Doc::filter(Doc::BODY.search_in("english", "\"query builder\" -indexes"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(titles(phrase), ["rust orm"]);
    assert_eq!(
        Doc::filter(Doc::BODY.search("bread"))
            .count(&db)
            .await
            .unwrap(),
        1
    );

    // Ranking: order by an expression, also through offset pagination.
    let ranked = Doc::filter(Doc::BODY.search("queries"))
        .order_by(Doc::BODY.search_rank("queries builder").desc())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(titles(ranked), ["rust orm", "postgres tips"]);
    let page = Doc::query()
        .order_by(Doc::BODY.search_rank("queries builder").desc())
        .order_by(Doc::ID)
        .paginate(&db, 1, 2)
        .await
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.items[0].title, "rust orm");
    // Keyset pagination works through expression orders.
    let rank = || Doc::BODY.search_rank("queries builder").desc();
    let first = Doc::query()
        .order_by(rank())
        .cursor_paginate(&db, None, 2)
        .await
        .unwrap();
    let rest = Doc::query()
        .order_by(rank())
        .cursor_paginate(&db, first.next.as_ref(), 2)
        .await
        .unwrap();
    assert_eq!(titles(first.items)[0], "rust orm");
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next.is_none());
}

#[cfg(feature = "json")]
#[rok_db::test]
async fn jsonb(db: Db) {
    use rok_db::sqlx::types::Json;
    use serde_json::json;

    #[derive(Debug, Clone, Model)]
    struct Profile {
        #[rok(generated)]
        id: i64,
        prefs: serde_json::Value,
        extra: Option<Json<serde_json::Value>>,
    }

    db.execute(
        "CREATE TABLE profiles (id BIGSERIAL PRIMARY KEY, prefs JSONB NOT NULL, extra JSONB)",
    )
    .await
    .unwrap();
    let p = |prefs: serde_json::Value| Profile {
        id: 0,
        prefs,
        extra: None,
    };
    Profile::insert_all(
        &db,
        &[
            p(json!({"theme": "dark", "lang": "en", "notify": {"email": true}})),
            p(json!({"theme": "light", "beta": true, "notify": {"email": false}})),
            p(json!({"lang": "ar"})),
        ],
    )
    .await
    .unwrap();

    let count = |e: Expr<Profile>| {
        let db = db.clone();
        async move { Profile::filter(e).count(&db).await.unwrap() }
    };
    assert_eq!(count(Profile::PREFS.json_has_key("theme")).await, 2);
    assert_eq!(
        count(Profile::PREFS.json_has_any_key(["beta", "lang"])).await,
        3
    );
    assert_eq!(
        count(Profile::PREFS.json_has_all_keys(["theme", "lang"])).await,
        1
    );
    assert_eq!(
        count(Profile::PREFS.json_contains(json!({"theme": "dark"}))).await,
        1
    );
    assert_eq!(count(Profile::PREFS.json_text("lang").eq("ar")).await, 1);
    assert_eq!(
        count(
            Profile::PREFS
                .json_path_text(["notify", "email"])
                .eq("true")
        )
        .await,
        1
    );

    let langs: Vec<(Option<String>,)> = Profile::query()
        .order_by(Profile::PREFS.json_text("lang").asc().nulls_last())
        .select(Profile::PREFS.json_text("lang"))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(langs, [(Some("ar".into()),), (Some("en".into()),), (None,)]);
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Item {
    #[rok(generated)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn read_replicas_route_reads() {
    let (Some(primary), Some(replica)) = (
        TestDb::create().await.unwrap(),
        TestDb::create().await.unwrap(),
    ) else {
        return;
    };
    // Different contents make the routing visible.
    for (test_db, name) in [(&primary, "from-primary"), (&replica, "from-replica")] {
        test_db
            .db()
            .execute("CREATE TABLE items (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)")
            .await
            .unwrap();
        raw("INSERT INTO items (name) VALUES (?)")
            .bind(name)
            .execute(test_db.db())
            .await
            .unwrap();
    }
    let db = Db::builder()
        .replica_pool(replica.db().pool().clone())
        .build_with_pool(primary.db().pool().clone());
    assert_eq!(db.replica_count(), 1);
    let name = |v: Vec<Item>| v.into_iter().map(|i| i.name).collect::<Vec<_>>();

    assert_eq!(name(Item::all(&db).await.unwrap()), ["from-replica"]);
    assert_eq!(Item::query().count(&db).await.unwrap(), 1);
    assert_eq!(
        name(Item::query().on_primary().all(&db).await.unwrap()),
        ["from-primary"]
    );
    assert_eq!(
        name(Item::all(&db.primary()).await.unwrap()),
        ["from-primary"]
    );

    // Writes go to the primary; their RETURNING rows come from there too.
    let created = Item::create()
        .set(Item::NAME, "new")
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(created.name, "new");
    assert_eq!(Item::query().on_primary().count(&db).await.unwrap(), 2);
    assert_eq!(
        Item::query().count(&db).await.unwrap(),
        1,
        "replica unchanged"
    );

    // Locks and transactions stay on the primary.
    let mut tx = db.begin().await.unwrap();
    assert_eq!(Item::query().count(&mut *tx).await.unwrap(), 2);
    tx.rollback().await.unwrap();
    assert_eq!(Item::query().for_update().all(&db).await.unwrap().len(), 2);

    // Raw SQL: primary unless opted in.
    let n: i64 = raw("SELECT COUNT(*) FROM items").scalar(&db).await.unwrap();
    assert_eq!(n, 2);
    let n: i64 = raw("SELECT COUNT(*) FROM items")
        .on_replica()
        .scalar(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // An unreachable replica falls back to the primary.
    replica.db().pool().close().await;
    assert_eq!(Item::query().count(&db).await.unwrap(), 2);
}

#[rok_db::test]
async fn listen_and_notify(db: Db) {
    let mut listener = db.listen(&["jobs", "other"]).await.unwrap();
    db.notify("jobs", "resize:42").await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (n.channel.as_str(), n.payload.as_str()),
        ("jobs", "resize:42")
    );

    // Notifications inside a transaction arrive on commit.
    let mut tx = db.begin().await.unwrap();
    raw("SELECT pg_notify(?, ?)")
        .bind("other")
        .bind("in-tx")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), listener.recv())
            .await
            .is_err(),
        "not delivered before commit"
    );
    assert!(listener.try_recv().is_none());
    tx.commit().await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n.payload, "in-tx");
}

#[rok_db::test]
async fn model_change_feed(db: Db) {
    db.execute("CREATE TABLE items (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    Item::install_change_notifications(&db).await.unwrap();
    Item::install_change_notifications(&db).await.unwrap(); // idempotent
    let mut changes = Item::changes(&db).await.unwrap();
    async fn next(c: &mut rok_db::ChangeStream<Item>) -> rok_db::Change<Item> {
        tokio::time::timeout(Duration::from_secs(5), c.recv())
            .await
            .unwrap()
            .unwrap()
    }

    let item = Item::create().set(Item::NAME, "a").exec(&db).await.unwrap();
    let change = next(&mut changes).await;
    assert_eq!(
        (change.op, change.key.clone()),
        (ChangeOp::Insert, item.id.to_string())
    );
    assert_eq!(change.fetch(&db).await.unwrap(), Some(item.clone()));

    Item::filter(Item::ID.eq(item.id))
        .update()
        .set(Item::NAME, "b")
        .exec(&db)
        .await
        .unwrap();
    let change = next(&mut changes).await;
    assert_eq!(change.op, ChangeOp::Update);
    assert_eq!(change.fetch(&db).await.unwrap().unwrap().name, "b");

    item.delete(&db).await.unwrap();
    let change = next(&mut changes).await;
    assert_eq!(change.op, ChangeOp::Delete);
    assert_eq!(change.fetch(&db).await.unwrap(), None);

    Item::uninstall_change_notifications(&db).await.unwrap();
    Item::create()
        .set(Item::NAME, "silent")
        .exec(&db)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut stream = changes;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), stream.recv())
            .await
            .is_err(),
        "no trigger, no events"
    );
}
