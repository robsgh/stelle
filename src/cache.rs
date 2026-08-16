use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock, Weak},
    time::{Duration, Instant},
};

use tokio::sync::Mutex as AsyncMutex;

struct Entry<C, V> {
    config: C,
    value: V,
    stored_at: Instant,
}

pub struct Hit<V> {
    pub value: V,
    pub fresh: bool,
}

pub struct TimedCache<C, V> {
    entries: RwLock<HashMap<String, Entry<C, V>>>,
}

impl<C, V> Default for TimedCache<C, V> {
    fn default() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl<C, V> TimedCache<C, V> {
    pub fn clear(&self) {
        self.entries.write().expect("cache lock poisoned").clear();
    }

    pub fn insert(&self, id: String, config: C, value: V) {
        self.insert_at(id, config, value, Instant::now());
    }

    fn insert_at(&self, id: String, config: C, value: V, stored_at: Instant) {
        self.entries.write().expect("cache lock poisoned").insert(
            id,
            Entry {
                config,
                value,
                stored_at,
            },
        );
    }
}

impl<C: PartialEq, V: Clone> TimedCache<C, V> {
    pub fn get(&self, id: &str, config: &C, ttl: Duration) -> Option<Hit<V>> {
        self.get_with_ttl(id, config, |_| ttl)
    }

    pub fn get_with_ttl(
        &self,
        id: &str,
        config: &C,
        ttl: impl FnOnce(&V) -> Duration,
    ) -> Option<Hit<V>> {
        let entries = self.entries.read().expect("cache lock poisoned");
        let entry = entries.get(id)?;
        (entry.config == *config).then(|| Hit {
            value: entry.value.clone(),
            fresh: entry.stored_at.elapsed() < ttl(&entry.value),
        })
    }
}

impl<C, V> TimedCache<C, V> {
    pub fn find_map<T>(&self, mut find: impl FnMut(&V) -> Option<T>) -> Option<T> {
        self.entries
            .read()
            .expect("cache lock poisoned")
            .values()
            .find_map(|entry| find(&entry.value))
    }
}

#[derive(Default)]
pub struct KeyedLocks {
    locks: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

impl KeyedLocks {
    pub fn get(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().expect("keyed lock map poisoned");
        locks.retain(|_, lock| lock.strong_count() > 0);

        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_rejects_entries_from_a_different_configuration() {
        let cache = TimedCache::default();
        cache.insert("widget".into(), "old", 1);

        assert!(
            cache
                .get("widget", &"old", Duration::from_secs(1))
                .is_some()
        );
        assert!(
            cache
                .get("widget", &"new", Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn cache_reports_expired_entries_as_stale() {
        let cache = TimedCache::default();
        cache.insert_at(
            "widget".into(),
            (),
            1,
            Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
        );

        let hit = cache
            .get("widget", &(), Duration::from_secs(1))
            .expect("cached value");
        assert_eq!(hit.value, 1);
        assert!(!hit.fresh);
    }

    #[test]
    fn keyed_locks_share_live_keys_and_release_unused_ones() {
        let locks = KeyedLocks::default();
        let first = locks.get("widget");
        assert!(Arc::ptr_eq(&first, &locks.get("widget")));

        drop(first);
        let replacement = locks.get("widget");
        assert_eq!(Arc::strong_count(&replacement), 1);
    }
}
