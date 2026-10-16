//! `rok-db-gen migrate status|up|down`: run the generated migrations from the
//! files in `<output>/migrations`, without a Rust program.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use rok_db::Db;
use rok_db::migration::{self, AppliedMigration, Migration};
use sqlx::Connection;

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Status,
    Up,
    Down { steps: usize },
}

/// Run `action` against `database_url` with the migrations in `dir`.
pub(crate) fn run(action: Action, dir: &Path, database_url: &str) -> Result<ExitCode, String> {
    let migrations = load(dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("can't start the async runtime: {e}"))?;
    runtime.block_on(async {
        // One direct connection first: the pool only reports "timed out",
        // while this says why (refused, unknown host, authentication). The
        // connection string carries the password, so errors never print it;
        // sqlx's messages don't include it either.
        let timeout = std::time::Duration::from_secs(10);
        let probe = tokio::time::timeout(timeout, sqlx::PgConnection::connect(database_url))
            .await
            .map_err(|_| "can't connect to the database: no answer within 10 seconds".to_owned())?
            .map_err(|e| format!("can't connect to the database: {e}"))?;
        let _ = probe.close().await;
        let db = Db::builder()
            .max_connections(1)
            .acquire_timeout(timeout)
            .connect(database_url)
            .await
            .map_err(|e| format!("can't connect to the database: {e}"))?;
        let result = match action {
            Action::Status => status(&db, &migrations).await,
            Action::Up => {
                let done = migration::apply(&db, &migrations)
                    .await
                    .map_err(|e| e.to_string())?;
                report(
                    "applied",
                    &done,
                    &migrations,
                    "nothing to apply: up to date",
                );
                Ok(ExitCode::SUCCESS)
            }
            Action::Down { steps } => {
                let done = migration::revert(&db, &migrations, steps)
                    .await
                    .map_err(|e| e.to_string())?;
                report("reverted", &done, &migrations, "nothing to revert");
                Ok(ExitCode::SUCCESS)
            }
        };
        db.close().await;
        result
    })
}

fn report(verb: &str, versions: &[i64], migrations: &[Migration], none: &str) {
    if versions.is_empty() {
        println!("{none}");
    }
    for version in versions {
        let name = migrations
            .iter()
            .find(|m| m.version == *version)
            .map_or("", |m| m.name);
        println!("{verb} {version} {name}");
    }
}

/// Print every migration with its state. Fails (exit code 1) when the
/// database and the files disagree: an applied migration was edited, or the
/// database has one the files don't.
async fn status(db: &Db, migrations: &[Migration]) -> Result<ExitCode, String> {
    // Don't create the bookkeeping table just to look: a database that has
    // never been migrated has everything pending.
    let exists: bool = rok_db::raw("SELECT to_regclass('_rok_db_migrations') IS NOT NULL")
        .scalar(db)
        .await
        .map_err(|e| e.to_string())?;
    let applied: Vec<AppliedMigration> = if exists {
        migration::applied(db).await.map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    let rows = status_rows(migrations, &applied);
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);
    println!(
        "{:<8} {:<width$} {:<8} checksum",
        "version", "name", "state"
    );
    let mut drift = false;
    for row in &rows {
        drift |= row.drift;
        println!(
            "{:<8} {:<width$} {:<8} {}",
            row.version, row.name, row.state, row.checksum
        );
    }
    if drift {
        eprintln!(
            "error: the database and the migration files disagree (see `edited` / `unknown` above)"
        );
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, PartialEq, Eq)]
struct StatusRow {
    version: i64,
    name: String,
    state: &'static str,
    checksum: &'static str,
    drift: bool,
}

fn status_rows(migrations: &[Migration], applied: &[AppliedMigration]) -> Vec<StatusRow> {
    let mut rows = BTreeMap::new();
    for m in migrations {
        let row = match applied.iter().find(|a| a.version == m.version) {
            Some(a) if a.checksum == m.checksum() => ("applied", "ok", false),
            Some(_) => ("applied", "edited", true),
            None => ("pending", "", false),
        };
        rows.insert(m.version, (m.name.to_owned(), row));
    }
    for a in applied {
        rows.entry(a.version)
            .or_insert_with(|| (a.name.clone(), ("unknown", "", true)));
    }
    rows.into_iter()
        .map(|(version, (name, (state, checksum, drift)))| StatusRow {
            version,
            name,
            state,
            checksum,
            drift,
        })
        .collect()
}

/// Read `NNNN_name.up.sql` (and the optional `NNNN_name.down.sql`) from
/// `dir`, oldest first. The contents are exactly what the generated crate
/// embeds with `include_str!`, so checksums match `db_gen::up`.
fn load(dir: &Path) -> Result<Vec<Migration>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut ups = BTreeMap::new();
    let mut downs = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        let (stem, map) = if let Some(stem) = file.strip_suffix(".up.sql") {
            (stem, &mut ups)
        } else if let Some(stem) = file.strip_suffix(".down.sql") {
            (stem, &mut downs)
        } else {
            return Err(format!(
                "{}: expected only `NNNN_name.up.sql` and `NNNN_name.down.sql` files",
                entry.path().display()
            ));
        };
        let version = stem
            .split_once('_')
            .and_then(|(v, name)| Some((v.parse::<i64>().ok()?, name)))
            .filter(|(_, name)| !name.is_empty());
        let Some((version, name)) = version else {
            return Err(format!(
                "{}: the name must be `<version>_<name>`, like `0001_init.up.sql`",
                entry.path().display()
            ));
        };
        let sql = fs::read_to_string(entry.path())
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if map.insert(version, (name.to_owned(), sql)).is_some() {
            return Err(format!(
                "{}: two migrations have version {version}",
                dir.display()
            ));
        }
    }
    if let Some((version, (name, _))) = downs.iter().find(|(v, _)| !ups.contains_key(*v)) {
        return Err(format!(
            "{}: `{version:04}_{name}.down.sql` has no matching `.up.sql`",
            dir.display()
        ));
    }
    if ups.is_empty() {
        return Err(format!(
            "{} has no migrations; run `rok-db-gen generate` first",
            dir.display()
        ));
    }
    // `Migration` holds `&'static str`, like the `include_str!` constants of
    // the generated crate; this process runs one command and exits.
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    Ok(ups
        .into_iter()
        .map(|(version, (name, up))| Migration {
            version,
            name: leak(name),
            up: leak(up),
            down: leak(
                downs
                    .remove(&version)
                    .map(|(_, down)| down)
                    .unwrap_or_default(),
            ),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rok-db-gen-migrate-{}-{}",
            std::process::id(),
            files.iter().map(|(f, _)| f.len()).sum::<usize>() + files.len()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (file, sql) in files {
            fs::write(dir.join(file), sql).unwrap();
        }
        dir
    }

    #[test]
    fn loads_pairs_in_version_order() {
        let dir = dir_with(&[
            ("0002_add_slug.up.sql", "ALTER 2"),
            ("0001_init.up.sql", "CREATE 1"),
            ("0001_init.down.sql", "DROP 1"),
        ]);
        let m = load(&dir).unwrap();
        assert_eq!(
            m.iter()
                .map(|m| (m.version, m.name, m.up, m.down))
                .collect::<Vec<_>>(),
            [
                (1, "init", "CREATE 1", "DROP 1"),
                (2, "add_slug", "ALTER 2", "")
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_unexpected_files() {
        for files in [
            &[("0001_init.up.sql", ""), ("notes.txt", "")][..],
            &[("init.up.sql", "")][..],
            &[("0001_.up.sql", "")][..],
            &[("0001_init.up.sql", ""), ("0002_x.down.sql", "")][..],
            &[("0001_a.up.sql", ""), ("1_b.up.sql", "")][..],
            &[][..],
        ] {
            let dir = dir_with(files);
            assert!(load(&dir).is_err(), "{files:?}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn status_marks_drift() {
        let m = |version, up| Migration {
            version,
            name: "m",
            up,
            down: "",
        };
        let applied = |version, up: &'static str| AppliedMigration {
            version,
            name: "old".into(),
            checksum: m(version, up).checksum(),
        };
        let files = [m(1, "a"), m(2, "b"), m(3, "c")];
        let rows = status_rows(
            &files,
            &[applied(1, "a"), applied(2, "edited"), applied(9, "z")],
        );
        let states: Vec<_> = rows
            .iter()
            .map(|r| (r.version, r.state, r.checksum, r.drift))
            .collect();
        assert_eq!(
            states,
            [
                (1, "applied", "ok", false),
                (2, "applied", "edited", true),
                (3, "pending", "", false),
                (9, "unknown", "", true),
            ]
        );
    }
}
