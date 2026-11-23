//! Warm sessions within a bound: [`SessionCache`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{OnnxError, Session, SessionOptions};

/// One key's session, filled by the first caller to load it; later callers for the key wait on
/// it instead of loading again.
type Slot = Arc<Mutex<Option<Arc<Session>>>>;

struct Entry {
    path: PathBuf,
    options: SessionOptions,
    slot: Slot,
    last_used: u64,
}

#[derive(Default)]
struct Entries {
    entries: Vec<Entry>,
    clock: u64,
}

/// Loaded sessions keyed by (canonical model path, [`SessionOptions`]), at most
/// [`max_sessions`](Self::max_sessions) of them, the least recently used evicted first.
///
/// Eviction only drops the cache's own reference: a session still held elsewhere stays alive
/// until that holder lets go. Call [`clear`](Self::clear) when the platform reports memory
/// pressure. Share one cache between threads; it is `Send + Sync`.
pub struct SessionCache {
    max_sessions: usize,
    entries: Mutex<Entries>,
    loads: AtomicU64,
}

impl SessionCache {
    /// The bound [`SessionCache::default`] uses.
    pub const DEFAULT_MAX_SESSIONS: usize = 3;

    /// An empty cache holding at most `max_sessions` sessions (at least one).
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            entries: Mutex::new(Entries::default()),
            loads: AtomicU64::new(0),
        }
    }

    /// The bound.
    pub fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    /// The cached session for `path` and `options`, loading it with
    /// [`Session::from_file_with`] on a miss.
    ///
    /// Callers asking for the same key at once share one load. A failed load is not cached: the
    /// next call tries again. A path that cannot be canonicalised (a missing file) is
    /// [`OnnxError::Load`].
    pub fn get_or_load(
        &self,
        path: impl AsRef<Path>,
        options: &SessionOptions,
    ) -> Result<Arc<Session>, OnnxError> {
        let given = path.as_ref();
        let path = std::fs::canonicalize(given).map_err(|err| OnnxError::Load {
            path: Some(given.to_path_buf()),
            reason: err.to_string(),
        })?;
        let slot = self.slot(&path, options);
        let mut loaded = lock(&slot);
        if let Some(session) = loaded.as_ref() {
            return Ok(Arc::clone(session));
        }
        match Session::from_file_with(&path, options) {
            Ok(session) => {
                self.loads.fetch_add(1, Ordering::Relaxed);
                let session = Arc::new(session);
                *loaded = Some(Arc::clone(&session));
                Ok(session)
            }
            Err(err) => {
                drop(loaded);
                lock(&self.entries)
                    .entries
                    .retain(|entry| !Arc::ptr_eq(&entry.slot, &slot));
                Err(err)
            }
        }
    }

    /// The key's slot, marked used; a new one (evicting the least recently used entry when the
    /// cache is full) on a miss.
    fn slot(&self, path: &Path, options: &SessionOptions) -> Slot {
        let mut entries = lock(&self.entries);
        entries.clock += 1;
        let now = entries.clock;
        if let Some(entry) = entries
            .entries
            .iter_mut()
            .find(|entry| entry.path == path && entry.options == *options)
        {
            entry.last_used = now;
            return Arc::clone(&entry.slot);
        }
        while entries.entries.len() >= self.max_sessions {
            let oldest = entries
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                let evicted = entries.entries.swap_remove(index);
                log::debug!("frust-onnx: cache evicts {}", evicted.path.display());
            }
        }
        let slot = Slot::default();
        entries.entries.push(Entry {
            path: path.to_path_buf(),
            options: options.clone(),
            slot: Arc::clone(&slot),
            last_used: now,
        });
        slot
    }

    /// The session already loaded for `path` and `options`, if any. Never loads, never waits
    /// on a load under way and does not count as a use for eviction.
    pub(crate) fn peek(
        &self,
        path: impl AsRef<Path>,
        options: &SessionOptions,
    ) -> Option<Arc<Session>> {
        let path = std::fs::canonicalize(path.as_ref()).ok()?;
        let slot = lock(&self.entries)
            .entries
            .iter()
            .find(|entry| entry.path == path && entry.options == *options)
            .map(|entry| Arc::clone(&entry.slot))?;
        let loaded = slot.try_lock().ok()?;
        loaded.as_ref().map(Arc::clone)
    }

    /// Drops every cached session for `path`, whatever its options; returns how many there were.
    pub fn evict(&self, path: impl AsRef<Path>) -> usize {
        let given = path.as_ref();
        let path = std::fs::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
        let mut entries = lock(&self.entries);
        let before = entries.entries.len();
        entries.entries.retain(|entry| entry.path != path);
        before - entries.entries.len()
    }

    /// Drops every cached session.
    pub fn clear(&self) {
        lock(&self.entries).entries.clear();
    }

    /// How many sessions the cache holds (loads under way included).
    pub fn len(&self) -> usize {
        lock(&self.entries).entries.len()
    }

    /// True when the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many sessions this cache has loaded over its life: misses that succeeded.
    pub fn load_count(&self) -> u64 {
        self.loads.load(Ordering::Relaxed)
    }
}

impl Default for SessionCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_SESSIONS)
    }
}

impl fmt::Debug for SessionCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCache")
            .field("max_sessions", &self.max_sessions)
            .field("len", &self.len())
            .field("load_count", &self.load_count())
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
