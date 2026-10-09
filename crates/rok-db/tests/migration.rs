//! Embedded migrations (`rok_db::migration`).
#![cfg(feature = "testing")]

use rok_db::migration::{self, Migration};
use rok_db::prelude::*;

const V1: Migration = Migration {
    version: 1,
    name: "init",
    up: "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL);
         CREATE INDEX notes_body_idx ON notes (body);",
    down: "DROP TABLE notes;",
};
const V2: Migration = Migration {
    version: 2,
    name: "add_pinned",
    up: "ALTER TABLE notes ADD COLUMN pinned BOOLEAN NOT NULL DEFAULT false;",
    down: "ALTER TABLE notes DROP COLUMN pinned;",
};

async fn has_column(db: &Db, column: &str) -> bool {
    rok_db::raw(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'notes' AND column_name = ?)",
    )
    .bind(column)
    .scalar::<bool, _>(db)
    .await
    .unwrap()
}

#[rok_db::test]
async fn apply_and_revert(db: Db) {
    assert_eq!(migration::apply(&db, &[V1, V2]).await.unwrap(), [1, 2]);
    assert!(has_column(&db, "pinned").await);
    // Up to date: nothing to do.
    assert!(migration::apply(&db, &[V1, V2]).await.unwrap().is_empty());
    let applied = migration::applied(&db).await.unwrap();
    assert_eq!(
        applied.iter().map(|a| a.version).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(applied[1].checksum, V2.checksum());

    assert_eq!(migration::revert(&db, &[V1, V2], 1).await.unwrap(), [2]);
    assert!(!has_column(&db, "pinned").await);
    assert_eq!(migration::apply(&db, &[V1, V2]).await.unwrap(), [2]);
    assert_eq!(migration::revert(&db, &[V1, V2], 5).await.unwrap(), [2, 1]);
    assert!(!has_column(&db, "body").await);
}

#[rok_db::test]
async fn guards(db: Db) {
    migration::apply(&db, &[V1]).await.unwrap();

    // An applied migration was edited.
    let edited = Migration {
        up: "CREATE TABLE other (id INT);",
        ..V1
    };
    let err = migration::apply(&db, &[edited, V2]).await.unwrap_err();
    assert!(err.to_string().contains("edited"), "{err}");

    // The database knows a migration the code doesn't.
    migration::apply(&db, &[V1, V2]).await.unwrap();
    let err = migration::apply(&db, &[V1]).await.unwrap_err();
    assert!(err.to_string().contains("doesn't know"), "{err}");

    // A failing migration is rolled back and reported with its name.
    let broken = Migration {
        version: 3,
        name: "broken",
        up: "ALTER TABLE nope ADD x INT;",
        down: "",
    };
    let err = migration::apply(&db, &[V1, V2, broken]).await.unwrap_err();
    assert!(err.to_string().contains("`broken`"), "{err}");
    assert_eq!(migration::applied(&db).await.unwrap().len(), 2);

    // Irreversible migrations say so.
    let irreversible = Migration {
        version: 3,
        name: "seed",
        up: "SELECT 1;",
        down: "",
    };
    migration::apply(&db, &[V1, V2, irreversible])
        .await
        .unwrap();
    let err = migration::revert(&db, &[V1, V2, irreversible], 1)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("can't be reverted"), "{err}");
}

/// Compile-time check: migrations can run on a spawned task.
#[allow(dead_code)]
fn futures_are_spawnable(db: Db) {
    fn spawnable<F: std::future::Future + Send + 'static>(_: F) {}
    let d = db.clone();
    spawnable(async move { migration::apply(&d, &[V1, V2]).await });
    let d = db.clone();
    spawnable(async move { migration::revert(&d, &[V1, V2], 1).await });
    spawnable(async move { migration::applied(&db).await });
}
