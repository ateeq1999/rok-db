//! `rok-db-gen migrate status|up|down`, run as a process against the
//! example's migrations (`examples/sqlgen/db-gen/migrations`).

use std::path::PathBuf;
use std::process::{Command, Output};

use rok_db::prelude::*;
use rok_db::testing::TestDb;

fn example_output() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/sqlgen/db-gen")
}

/// The URL of the test database: `DATABASE_URL` with its database name replaced.
fn url_of(test_db: &TestDb) -> String {
    let base = std::env::var("DATABASE_URL").unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    format!("{server}/{}", test_db.name())
}

fn cli(url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rok-db-gen"))
        .arg("migrate")
        .args(args)
        .arg("--output")
        .arg(example_output())
        .arg("--database-url")
        .arg(url)
        .env_remove("DATABASE_URL")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

async fn has_table(db: &Db) -> bool {
    rok_db::raw("SELECT to_regclass('_rok_db_migrations') IS NOT NULL")
        .scalar(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn status_up_down() {
    let Some(test_db) = TestDb::create().await.unwrap() else {
        return;
    };
    let url = url_of(&test_db);

    // A fresh database: everything pending, and looking creates nothing.
    let status = cli(&url, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let text = stdout(&status);
    assert!(text.starts_with("version"), "{text}");
    assert!(text.contains("1        init"), "{text}");
    assert_eq!(text.matches("pending").count(), 2, "{text}");
    assert!(!has_table(test_db.db()).await);

    let up = cli(&url, &["up"]);
    assert!(up.status.success(), "{}", stderr(&up));
    assert_eq!(stdout(&up), "applied 1 init\napplied 2 add_plans\n");
    assert_eq!(
        stdout(&cli(&url, &["up"])),
        "nothing to apply: up to date\n"
    );
    let text = stdout(&cli(&url, &["status"]));
    assert_eq!(text.matches("applied  ok").count(), 2, "{text}");

    // One step by default, then as many as are left.
    assert_eq!(stdout(&cli(&url, &["down"])), "reverted 2 add_plans\n");
    assert_eq!(
        stdout(&cli(&url, &["down", "--steps", "5"])),
        "reverted 1 init\n"
    );
    assert_eq!(stdout(&cli(&url, &["down"])), "nothing to revert\n");
}

#[tokio::test]
async fn agrees_with_the_generated_crate_and_reports_drift() {
    let Some(test_db) = TestDb::create().await.unwrap() else {
        return;
    };
    let url = url_of(&test_db);
    let db = test_db.db();

    // Applied by the generated crate's `up()`: the CLI sees the same checksums.
    assert_eq!(sqlgen_db::up(db).await.unwrap(), [1, 2]);
    let status = cli(&url, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(stdout(&status).matches("applied  ok").count(), 2);

    // An applied migration whose file changed, and one the files don't have.
    rok_db::raw(r#"UPDATE "_rok_db_migrations" SET "checksum" = 'x' WHERE "version" = 1"#)
        .execute(db)
        .await
        .unwrap();
    rok_db::raw(
        r#"INSERT INTO "_rok_db_migrations" ("version", "name", "checksum") VALUES (9, 'later', 'y')"#,
    )
    .execute(db)
    .await
    .unwrap();
    let status = cli(&url, &["status"]);
    assert!(!status.status.success());
    let text = stdout(&status);
    assert!(text.contains("applied  edited"), "{text}");
    assert!(text.contains("9        later"), "{text}");
    assert!(text.contains("unknown"), "{text}");
    assert!(stderr(&status).contains("disagree"));

    // `up` refuses to run on a database that disagrees with the files.
    let up = cli(&url, &["up"]);
    assert!(!up.status.success());
    assert!(stderr(&up).contains("edited"), "{}", stderr(&up));
}

#[test]
fn usage_errors() {
    let bin = env!("CARGO_BIN_EXE_rok-db-gen");
    let run = |args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .env_remove("DATABASE_URL")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let output = example_output();
    let output = output.to_str().unwrap();
    for (args, expected) in [
        (&["migrate"][..], "needs status, up or down"),
        (&["migrate", "sideways"][..], "unknown migrate command"),
        (
            &["migrate", "up", "--steps", "2"][..],
            "only applies to `migrate down`",
        ),
        (
            &["migrate", "down", "--steps", "0"][..],
            "expected a positive number",
        ),
        (
            &["migrate", "up", "--migration", "x"][..],
            "doesn't take `--migration`",
        ),
        (
            &["migrate", "status", "--output", output][..],
            "set DATABASE_URL",
        ),
    ] {
        let (ok, err) = run(args);
        assert!(!ok, "{args:?}");
        assert!(err.contains(expected), "{args:?}: {err}");
    }
}

#[test]
fn connection_errors_never_print_the_password() {
    // Port 1 on localhost refuses connections; the password must not leak
    // into the error output.
    let out = cli(
        "postgres://someone:s3cret-pa55@127.0.0.1:1/nowhere",
        &["status"],
    );
    assert!(!out.status.success());
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(all.contains("can't connect"), "{all}");
    assert!(!all.contains("s3cret-pa55"), "{all}");
}
