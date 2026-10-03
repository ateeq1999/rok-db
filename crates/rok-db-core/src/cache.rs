use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// An in-memory cache for memoized query results.
///
/// Enable it on a pool with [`DbBuilder::query_cache`](crate::DbBuilder::query_cache)
/// and opt queries in with [`Select::memoize`](crate::Select::memoize):
///
/// ```ignore
/// let db = Db::builder().query_cache(10_000).connect(url).await?;
/// let admins = User::filter(User::ROLE.eq("admin"))
///     .memoize(Duration::from_secs(30))
///     .all(&db)
///     .await?;
/// ```
///
/// Entries expire after their time-to-live, and every write that rok-db
/// performs on a table (insert, save, upsert, delete, bulk update/delete —
/// including inside transactions, again on commit) invalidates that table's
/// entries. Changes rok-db can't see are not tracked: writes from other
/// processes, [`raw`](crate::raw) SQL that doesn't declare
/// [`Raw::invalidates`](crate::Raw::invalidates), and rows changed by
/// `ON DELETE CASCADE`, `ON UPDATE` actions or triggers on *other* tables.
/// For those, call [`invalidate`](Self::invalidate) yourself or rely on a
/// short TTL.
///
/// The cache is local to the process; cloning it is cheap and shares state.
#[derive(Clone)]
pub struct QueryCache {
    inner: Arc<Mutex<Inner>>,
}

pub(crate) struct Inner {
    /// Publishes local invalidations to other instances (see
    /// [`DbBuilder::shared_cache_invalidation`](crate::DbBuilder::shared_cache_invalidation)).
    broadcast: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    capacity: usize,
    entries: HashMap<String, Entry>,
    generations: HashMap<String, u64>,
    hits: u64,
    misses: u64,
}

struct Entry {
    value: Arc<dyn Any + Send + Sync>,
    table: &'static str,
    generation: u64,
    expires: Instant,
}

/// Counters describing how a [`QueryCache`] is performing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Lookups answered from the cache.
    pub hits: u64,
    /// Lookups that had to query the database.
    pub misses: u64,
    /// Entries currently stored (including expired ones not yet evicted).
    pub entries: usize,
}

impl QueryCache {
    /// A cache holding at most `capacity` query results.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                broadcast: None,
                capacity: capacity.max(1),
                entries: HashMap::new(),
                generations: HashMap::new(),
                hits: 0,
                misses: 0,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock can't leave the map inconsistent.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop every cached result read from `table` (on every instance, when
    /// shared invalidation is enabled).
    pub fn invalidate(&self, table: &str) {
        let mut inner = self.lock();
        inner.invalidate(table);
        if let Some(tx) = &inner.broadcast {
            let _ = tx.send(table.to_owned());
        }
    }

    /// Drop every cached result (on every instance, when shared
    /// invalidation is enabled).
    pub fn clear(&self) {
        let mut inner = self.lock();
        if let Some(tx) = &inner.broadcast {
            let _ = tx.send(String::new());
        }
        inner.clear();
    }

    pub(crate) fn downgrade(&self) -> Weak<Mutex<Inner>> {
        Arc::downgrade(&self.inner)
    }

    /// Route future invalidations to `tx` as well.
    pub(crate) fn set_broadcast(&self, tx: tokio::sync::mpsc::UnboundedSender<String>) {
        self.lock().broadcast = Some(tx);
    }
}

/// Apply an invalidation received from another instance (`""` = clear all).
pub(crate) fn apply_remote(inner: &Mutex<Inner>, table: &str) {
    let mut inner = inner.lock().unwrap_or_else(|e| e.into_inner());
    if table.is_empty() {
        inner.clear();
    } else {
        inner.invalidate(table);
    }
}

impl Inner {
    fn invalidate(&mut self, table: &str) {
        *self.generations.entry(table.to_owned()).or_default() += 1;
        self.entries.retain(|_, e| e.table != table);
    }

    fn clear(&mut self) {
        let inner = self;
        // Bumping the global generation (keyed by "") also rejects results
        // that were computed before this call but stored after it.
        *inner.generations.entry(String::new()).or_default() += 1;
        inner.entries.clear();
    }
}

impl QueryCache {
    /// Hit/miss counters and the current size.
    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            hits: inner.hits,
            misses: inner.misses,
            entries: inner.entries.len(),
        }
    }

    /// Current generation of `table`; results computed under an older
    /// generation are discarded instead of stored.
    pub(crate) fn generation(&self, table: &str) -> u64 {
        let inner = self.lock();
        inner.generation(table)
    }

    pub(crate) fn get<T: Clone + 'static>(&self, key: &str) -> Option<T> {
        let mut inner = self.lock();
        let now = Instant::now();
        let fresh = inner.entries.get(key).and_then(|e| {
            (e.expires > now && e.generation == inner.generation(e.table))
                .then(|| e.value.downcast_ref::<T>().cloned())
                .flatten()
        });
        match fresh {
            Some(value) => {
                inner.hits += 1;
                crate::metrics::cache(true);
                Some(value)
            }
            None => {
                inner.misses += 1;
                crate::metrics::cache(false);
                inner.entries.remove(key);
                None
            }
        }
    }

    pub(crate) fn put<T: Send + Sync + 'static>(
        &self,
        key: String,
        table: &'static str,
        generation: u64,
        ttl: Duration,
        value: T,
    ) {
        let mut inner = self.lock();
        if generation != inner.generation(table) {
            return; // A write happened while the query ran.
        }
        let now = Instant::now();
        if inner.entries.len() >= inner.capacity && !inner.entries.contains_key(&key) {
            inner.entries.retain(|_, e| e.expires > now);
            if inner.entries.len() >= inner.capacity {
                let oldest = inner
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.expires)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    inner.entries.remove(&oldest);
                }
            }
        }
        inner.entries.insert(
            key,
            Entry {
                value: Arc::new(value),
                table,
                generation,
                expires: now + ttl,
            },
        );
    }
}

impl Inner {
    fn generation(&self, table: &str) -> u64 {
        let global = self.generations.get("").copied().unwrap_or(0);
        let table = self.generations.get(table).copied().unwrap_or(0);
        global.wrapping_add(table)
    }
}

impl fmt::Debug for QueryCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("QueryCache")
            .field("capacity", &inner.capacity)
            .field("entries", &inner.entries.len())
            .field("hits", &inner.hits)
            .field("misses", &inner.misses)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_put_invalidate() {
        let cache = QueryCache::new(2);
        let ttl = Duration::from_secs(60);
        let g = cache.generation("users");
        cache.put("a".into(), "users", g, ttl, vec![1, 2]);
        assert_eq!(cache.get::<Vec<i32>>("a"), Some(vec![1, 2]));
        assert_eq!(cache.get::<String>("a"), None, "type mismatch is a miss");

        cache.put("a".into(), "users", g, ttl, vec![1, 2]);
        cache.invalidate("users");
        assert_eq!(cache.get::<Vec<i32>>("a"), None);

        // A result computed before an invalidation is not stored.
        cache.put("b".into(), "users", g, ttl, 5);
        assert_eq!(cache.get::<i32>("b"), None);

        let g = cache.generation("users");
        cache.put("c".into(), "users", g, Duration::ZERO, 5);
        assert_eq!(cache.get::<i32>("c"), None, "expired");

        let stats = cache.stats();
        assert_eq!((stats.hits, stats.misses), (1, 4));
    }

    #[test]
    fn capacity_and_clear() {
        let cache = QueryCache::new(2);
        let ttl = Duration::from_secs(60);
        for (i, key) in ["a", "b", "c"].into_iter().enumerate() {
            let g = cache.generation("t");
            cache.put(key.into(), "t", g, ttl + Duration::from_secs(i as u64), i);
        }
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.get::<usize>("a"), None, "oldest evicted");
        let g = cache.generation("other");
        cache.clear();
        cache.put("d".into(), "other", g, ttl, 1);
        assert_eq!(cache.get::<usize>("d"), None);
    }
}
