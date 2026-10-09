//! The committed example (`examples/sqlgen`): it is up to date, and the
//! generated code works against PostgreSQL.

use std::path::PathBuf;

use rok_db::prelude::*;
use rok_db_codegen::{Dependency, Options, differences, generate};
use sqlgen_db::{post, types::UserRole, user};

fn example_options() -> Options {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/sqlgen");
    let mut options = Options::new();
    options.input = root.join("db");
    options.output = root.join("db-gen");
    options.crate_name = "sqlgen-db".into();
    options.rok_db = Dependency::Path(root.join("../../crates/rok-db"));
    options
}

/// Same as `rok-db-gen check`, without a database (types come from queries.json).
#[test]
fn committed_example_is_up_to_date() {
    let options = example_options();
    let generated = generate(&options).unwrap();
    assert!(generated.new_migration.is_none());
    assert!(generated.warnings.is_empty(), "{:?}", generated.warnings);
    let stale = differences(&options, &generated);
    assert!(
        stale.is_empty(),
        "out of date: {stale:?}; run `cargo run -p rok-db-codegen -- generate --config examples/sqlgen/rok-db.toml`"
    );
}

#[rok_db::test]
async fn generated_code_runs(db: Db) {
    assert_eq!(sqlgen_db::up(&db).await.unwrap(), [1]);

    let ann = user::User {
        id: 0,
        email: "Ann@example.com".into(),
        name: Some("Ann".into()),
        role: UserRole::Member,
        created_at: Default::default(),
        updated_at: Default::default(),
        deleted_at: None,
    }
    .insert(&db)
    .await
    .unwrap();

    // Model API on generated models.
    let found = user::User::filter(user::User::ROLE.eq(UserRole::Member))
        .one(&db)
        .await
        .unwrap();
    assert_eq!(found.id, ann.id);

    // :one with a function-wrapped parameter.
    let by_email = user::find_by_email(&db, "ann@EXAMPLE.com").await.unwrap();
    assert_eq!(by_email.map(|u| u.id), Some(ann.id));

    // :one! returning the model, with an array parameter.
    let post = post::create_post(&db, ann.id, "Hello", &["intro".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        (post.title.as_str(), post.tags.as_slice()),
        ("Hello", ["intro".to_owned()].as_slice())
    );

    // :exec, then :many with LIMIT.
    assert_eq!(post::add_view(&db, post.id).await.unwrap(), 1);
    let top = post::top_for_author(&db, ann.id, 5).await.unwrap();
    assert_eq!(top[0].views, 1);

    // Relations inferred from the foreign key.
    assert_eq!(ann.posts().count(&db).await.unwrap(), 1);
    assert_eq!(post.author().one(&db).await.unwrap().id, ann.id);

    // Custom row type, scalar rows, and :exec on another table.
    assert_eq!(user::promote(&db, ann.id).await.unwrap(), 1);
    let counts = user::count_by_role(&db).await.unwrap();
    assert_eq!(
        counts,
        [user::CountByRoleRow {
            role: UserRole::Admin,
            total: 1
        }]
    );
    assert_eq!(user::emails(&db).await.unwrap(), ["Ann@example.com"]);

    // :stream.
    rok_db::raw("UPDATE posts SET published_at = now()")
        .execute(&db)
        .await
        .unwrap();
    let feed: Vec<post::FeedRow> = post::feed(&db).try_collect().await.unwrap();
    assert_eq!(feed[0].author_email, "Ann@example.com");

    // Reverting the init migration removes everything.
    assert_eq!(sqlgen_db::down(&db, 1).await.unwrap(), [1]);
    let tables: i64 =
        rok_db::raw("SELECT count(*) FROM pg_tables WHERE tablename IN ('users', 'posts')")
            .scalar(&db)
            .await
            .unwrap();
    assert_eq!(tables, 0);
}
