//! Query logging. Kept in its own test binary (one test, one process) because
//! tracing's callsite interest cache is global and other tests running in
//! parallel without a subscriber would make captured output racy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rok_db::prelude::*;
use rok_db::{Error, raw};

#[derive(Debug, Model)]
struct User {
    id: i64,
    name: String,
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn query_logging() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };
    let db = Db::builder()
        .max_connections(1)
        .slow_query_threshold(Duration::from_millis(50))
        .connect(&url)
        .await
        .unwrap();
    db.execute("CREATE TEMP TABLE users (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    User::filter(User::NAME.eq("secret"))
        .count(&db)
        .await
        .unwrap();
    raw("SELECT pg_sleep(0.1)").execute(&db).await.unwrap();
    let err = raw("SELECT * FROM missing_table")
        .execute(&db)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Database(_)));

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains(r#"SELECT COUNT(*) FROM \"users\" WHERE \"name\" = $1"#),
        "{logs}"
    );
    assert!(
        !logs.contains("secret"),
        "parameters are only logged at TRACE level"
    );
    assert!(
        logs.contains("slow query") && logs.contains("pg_sleep"),
        "{logs}"
    );
    assert!(
        logs.contains("query failed") && logs.contains("missing_table"),
        "{logs}"
    );
}
