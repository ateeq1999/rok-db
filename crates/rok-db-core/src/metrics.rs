//! Metrics through the [`metrics`](https://docs.rs/metrics) facade (feature
//! `metrics`). Install any recorder (Prometheus, StatsD, OpenTelemetry, …)
//! and rok-db reports:
//!
//! | name | type | labels |
//! |---|---|---|
//! | `rok_db_queries_total` | counter | `kind` (`select`, `insert`, `update`, `delete`, `with`, `copy`, `other`), `outcome` (`ok`/`error`) |
//! | `rok_db_query_duration_seconds` | histogram | `kind` |
//! | `rok_db_slow_queries_total` | counter | `kind` |
//! | `rok_db_rows_total` | counter | `kind` — rows returned or affected |
//! | `rok_db_cache_requests_total` | counter | `result` (`hit`/`miss`) |
//! | `rok_db_pool_connections` | gauge | `state` (`total`/`idle`/`max`), via [`Db::record_pool_metrics`](crate::Db::record_pool_metrics) |

use std::time::Duration;

/// The statement kind used as a metrics label: its first keyword.
#[cfg(any(feature = "metrics", test))]
pub(crate) fn kind(sql: &str) -> &'static str {
    let first = sql
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("");
    match first.to_ascii_lowercase().as_str() {
        "select" => "select",
        "insert" => "insert",
        "update" => "update",
        "delete" => "delete",
        "with" => "with",
        "copy" => "copy",
        _ => "other",
    }
}

#[cfg(feature = "metrics")]
pub(crate) fn query(sql: &str, elapsed: Duration, rows: Option<u64>, slow: bool) {
    let kind = kind(sql);
    let outcome = if rows.is_some() { "ok" } else { "error" };
    ::metrics::counter!("rok_db_queries_total", "kind" => kind, "outcome" => outcome).increment(1);
    ::metrics::histogram!("rok_db_query_duration_seconds", "kind" => kind)
        .record(elapsed.as_secs_f64());
    if let Some(rows) = rows {
        ::metrics::counter!("rok_db_rows_total", "kind" => kind).increment(rows);
    }
    if slow {
        ::metrics::counter!("rok_db_slow_queries_total", "kind" => kind).increment(1);
    }
}

#[cfg(not(feature = "metrics"))]
pub(crate) fn query(_sql: &str, _elapsed: Duration, _rows: Option<u64>, _slow: bool) {}

#[cfg(feature = "metrics")]
pub(crate) fn cache(hit: bool) {
    let result = if hit { "hit" } else { "miss" };
    ::metrics::counter!("rok_db_cache_requests_total", "result" => result).increment(1);
}

#[cfg(not(feature = "metrics"))]
pub(crate) fn cache(_hit: bool) {}

#[cfg(feature = "metrics")]
pub(crate) fn pool(stats: crate::PoolStats) {
    ::metrics::gauge!("rok_db_pool_connections", "state" => "total").set(f64::from(stats.size));
    ::metrics::gauge!("rok_db_pool_connections", "state" => "idle").set(stats.idle as f64);
    ::metrics::gauge!("rok_db_pool_connections", "state" => "max")
        .set(f64::from(stats.max_connections));
}

#[cfg(test)]
mod tests {
    #[test]
    fn statement_kinds() {
        assert_eq!(super::kind("SELECT 1"), "select");
        assert_eq!(super::kind("  insert into t"), "insert");
        assert_eq!(super::kind("WITH x AS (…) SELECT"), "with");
        assert_eq!(super::kind("COPY t FROM STDIN"), "copy");
        assert_eq!(super::kind("VACUUM"), "other");
        assert_eq!(super::kind(""), "other");
    }
}
