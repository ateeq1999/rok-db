//! The schema-change workflow in a scratch project: init, a change that
//! needs `--migration`, the destructive-change guard, renames, and applying
//! every generated migration to PostgreSQL.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use rok_db::migration::{self, Migration};
use rok_db::prelude::*;
use rok_db_codegen::{Dependency, Options, Rename, differences, generate, write};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn project(schema: &str) -> Options {
    let dir = std::env::temp_dir().join(format!(
        "rok-db-gen-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(dir.join("db/app.sql"), schema).unwrap();
    let mut options = Options::new();
    options.input = dir.join("db");
    options.output = dir.join("db-gen");
    options.rok_db = Dependency::Path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rok-db"));
    options
}

fn set_schema(options: &Options, schema: &str) {
    fs::write(options.input.join("app.sql"), schema).unwrap();
}

fn read(options: &Options, path: &str) -> String {
    fs::read_to_string(options.output.join(path)).unwrap()
}

/// The migrations written so far, as the generated crate embeds them.
fn migrations(options: &Options) -> Vec<Migration> {
    let mut files: Vec<String> = fs::read_dir(options.output.join("migrations"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| f.ends_with(".up.sql"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let stem = f.trim_end_matches(".up.sql").to_owned();
            let (version, name) = stem.split_once('_').unwrap();
            let up = read(options, &format!("migrations/{f}"));
            let down =
                fs::read_to_string(options.output.join(format!("migrations/{stem}.down.sql")))
                    .unwrap_or_default();
            Migration {
                version: version.parse().unwrap(),
                name: Box::leak(name.to_owned().into_boxed_str()),
                up: Box::leak(up.into_boxed_str()),
                down: Box::leak(down.into_boxed_str()),
            }
        })
        .collect()
}

const V1: &str = "
CREATE TYPE status AS ENUM ('draft', 'live');
CREATE TABLE authors (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE articles (
    id BIGSERIAL PRIMARY KEY,
    author_id BIGINT NOT NULL REFERENCES authors,
    title TEXT NOT NULL,
    status status NOT NULL DEFAULT 'draft'
);
";

#[rok_db::test]
async fn schema_changes_become_migrations(db: Db) {
    let mut options = project(V1);

    // First run: the init migration, and nothing left to do afterwards.
    let generated = generate(&options).unwrap();
    assert_eq!(generated.new_migration.as_deref(), Some("init"));
    write(&options, &generated).unwrap();
    assert!(differences(&options, &generate(&options).unwrap()).is_empty());
    assert_eq!(
        migration::apply(&db, &migrations(&options)).await.unwrap(),
        [1]
    );

    // A change without --migration is refused, with a preview.
    let v2 = V1
        .replace("'draft', 'live'", "'draft', 'review', 'live'")
        .replace(
            "title TEXT NOT NULL,",
            "title TEXT NOT NULL,\n    slug TEXT NOT NULL DEFAULT '',",
        );
    set_schema(&options, &v2);
    let err = generate(&options).unwrap_err().to_string();
    assert!(
        err.contains("--migration") && err.contains("ADD COLUMN \"slug\""),
        "{err}"
    );

    options.migration = Some("add slug".into());
    let generated = generate(&options).unwrap();
    write(&options, &generated).unwrap();
    let up = read(&options, "migrations/0002_add_slug.up.sql");
    assert!(
        up.contains("ALTER TYPE \"status\" ADD VALUE 'review' AFTER 'draft'"),
        "{up}"
    );
    // Adding an enum value can't be undone, so there is no down file.
    assert!(
        !options
            .output
            .join("migrations/0002_add_slug.down.sql")
            .exists()
    );
    assert!(
        generated
            .warnings
            .iter()
            .any(|w| w.contains("can't be reverted"))
    );
    assert_eq!(
        migration::apply(&db, &migrations(&options)).await.unwrap(),
        [2]
    );

    // Dropping a column needs --allow-destructive...
    let v3 = v2.replace("title TEXT NOT NULL,", "headline TEXT NOT NULL,");
    set_schema(&options, &v3);
    options.migration = Some("headline".into());
    let err = generate(&options).unwrap_err().to_string();
    assert!(err.contains("drops column `articles.title`"), "{err}");
    // ...unless it is a rename.
    options.renames = vec![Rename::parse("articles.title=headline").unwrap()];
    write(&options, &generate(&options).unwrap()).unwrap();
    assert_eq!(
        read(&options, "migrations/0003_headline.up.sql"),
        "ALTER TABLE \"articles\" RENAME COLUMN \"title\" TO \"headline\";\n"
    );
    options.renames.clear();

    // A new table referencing an existing one.
    let v4 = format!(
        "{v3}\nCREATE TABLE tags (id SERIAL PRIMARY KEY, article_id BIGINT REFERENCES articles ON DELETE CASCADE, label VARCHAR(40) NOT NULL);\n"
    );
    set_schema(&options, &v4);
    options.migration = Some("tags".into());
    write(&options, &generate(&options).unwrap()).unwrap();
    assert_eq!(
        migration::apply(&db, &migrations(&options)).await.unwrap(),
        [3, 4]
    );

    // The database now matches the .sql files: reverting 4 and 3 works.
    assert_eq!(
        migration::revert(&db, &migrations(&options), 2)
            .await
            .unwrap(),
        [4, 3]
    );
    let columns: Vec<String> = rok_db::raw(
        "SELECT column_name::text FROM information_schema.columns WHERE table_name = 'articles' ORDER BY ordinal_position",
    )
    .fetch_all::<(String,), _>(&db)
    .await
    .unwrap()
    .into_iter()
    .map(|(c,)| c)
    .collect();
    // `slug` was added by a migration, so it comes last.
    assert_eq!(columns, ["id", "author_id", "title", "status", "slug"]);

    // `SELECT *` still returns the model although the column order differs.
    let with_query = format!("{v4}\n-- name: all_articles :many\nSELECT * FROM articles;\n");
    set_schema(&options, &with_query);
    options.migration = None;
    options.database_url = std::env::var("DATABASE_URL").ok();
    write(&options, &generate(&options).unwrap()).unwrap();
    let app = read(&options, "src/app.rs");
    assert!(
        app.contains("rok_db::Result<Vec<crate::app::Article>>"),
        "{app}"
    );

    // `check` sees an edited .sql file that wasn't regenerated.
    options.migration = None;
    set_schema(&options, &v4.replace("VARCHAR(40)", "VARCHAR(80)"));
    assert!(
        generate(&options)
            .unwrap_err()
            .to_string()
            .contains("--migration")
    );
}

#[test]
fn new_queries_need_a_database_once() {
    let mut options = project(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL);
         -- name: note_bodies :many
         SELECT body FROM notes;",
    );
    options.database_url = None;
    let err = generate(&options).unwrap_err().to_string();
    assert!(
        err.contains("DATABASE_URL") && err.contains("note_bodies"),
        "{err}"
    );
}

#[test]
fn reserved_and_colliding_names_are_rejected() {
    let options = project("CREATE TABLE users (id INT PRIMARY KEY);");
    fs::write(
        options.input.join("up.sql"),
        "CREATE TABLE x (id INT PRIMARY KEY);",
    )
    .unwrap();
    assert!(
        generate(&options)
            .unwrap_err()
            .to_string()
            .contains("rename the file")
    );
}
