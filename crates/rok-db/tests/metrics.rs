//! Metrics. Kept in its own test binary because it installs a global
//! recorder.
#![cfg(all(feature = "metrics", feature = "testing"))]

use std::time::Duration;

use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use rok_db::prelude::*;
use rok_db::raw;

#[derive(Debug, Clone, Model)]
struct Item {
    #[rok(generated)]
    id: i64,
    name: String,
}

#[rok_db::test]
async fn records_query_cache_and_pool_metrics(db: Db) {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    recorder.install().expect("only recorder in this binary");

    db.execute("CREATE TABLE items (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    let db = Db::builder()
        .query_cache(10)
        .slow_query_threshold(Duration::from_millis(50))
        .build_with_pool(db.pool().clone());
    Item::insert_all(
        &db,
        &[
            Item {
                id: 0,
                name: "a".into(),
            },
            Item {
                id: 0,
                name: "b".into(),
            },
        ],
    )
    .await
    .unwrap();
    Item::query().all(&db).await.unwrap();
    Item::query()
        .memoize(Duration::from_secs(60))
        .count(&db)
        .await
        .unwrap();
    Item::query()
        .memoize(Duration::from_secs(60))
        .count(&db)
        .await
        .unwrap();
    raw("SELECT pg_sleep(0.06)").execute(&db).await.unwrap();
    let _ = raw("SELECT * FROM missing").execute(&db).await;
    Item::copy_in(
        &db,
        &[Item {
            id: 0,
            name: "c".into(),
        }],
    )
    .await
    .unwrap();
    db.record_pool_metrics();

    let metrics: Vec<_> = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .map(|(key, _, _, value)| {
            let labels: Vec<String> = key
                .key()
                .labels()
                .map(|l| format!("{}={}", l.key(), l.value()))
                .collect();
            (
                format!("{}{{{}}}", key.key().name(), labels.join(",")),
                value,
            )
        })
        .collect();
    let counter = |name: &str| {
        metrics
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| match v {
                DebugValue::Counter(n) => *n,
                other => panic!("{name} is {other:?}"),
            })
    };
    assert_eq!(
        counter("rok_db_queries_total{kind=insert,outcome=ok}"),
        Some(1)
    );
    assert!(counter("rok_db_queries_total{kind=select,outcome=ok}").unwrap() >= 3);
    assert_eq!(
        counter("rok_db_queries_total{kind=select,outcome=error}"),
        Some(1)
    );
    assert_eq!(
        counter("rok_db_queries_total{kind=copy,outcome=ok}"),
        Some(1)
    );
    assert_eq!(counter("rok_db_rows_total{kind=insert}"), Some(2));
    assert_eq!(counter("rok_db_slow_queries_total{kind=select}"), Some(1));
    assert_eq!(counter("rok_db_cache_requests_total{result=hit}"), Some(1));
    assert_eq!(counter("rok_db_cache_requests_total{result=miss}"), Some(1));
    assert!(
        metrics
            .iter()
            .any(|(k, v)| k == "rok_db_query_duration_seconds{kind=select}"
                && matches!(v, DebugValue::Histogram(h) if !h.is_empty()))
    );
    assert!(
        metrics
            .iter()
            .any(|(k, v)| k == "rok_db_pool_connections{state=max}"
                && matches!(v, DebugValue::Gauge(g) if g.into_inner() == 5.0))
    );
}
