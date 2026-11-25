//! [`ModelList`]: the stored models as a signal.

use std::fmt::Display;
use std::future::Future;
use std::sync::{Arc, Mutex};

use frust::{ReadSignal, RwSignal, Set, spawn_blocking, spawn_local};

use crate::id::ModelId;
use crate::store::{LocalModel, ModelStore};

/// The models in a [`ModelStore`] as a signal a component's `build` reads.
///
/// The list starts empty: scanning the store touches the disk, so it never runs on
/// construction. Call [`refresh`](Self::refresh) once, for example from `Component::init`,
/// and again whenever the store changes behind the list's back. Clones share the same signal.
///
/// [`refresh`](Self::refresh) and [`remove`](Self::remove) hand the disk work to
/// `frust::spawn_blocking`, and the signal is written on the UI thread, by a `frust::spawn_local`
/// task, on the pump after the scan ends: no write ever lands on another thread. So they need the
/// reactive runtime the shell starts and must be called on the UI thread (from `init`, an event
/// handler or a local task); called on another thread they panic with `frust::spawn_local`'s
/// message. Overlapping refreshes are applied in the order they were requested, so a slow scan
/// never overwrites a newer one. Construct the list anywhere.
#[derive(Clone)]
pub struct ModelList {
    store: ModelStore,
    models: RwSignal<Vec<LocalModel>>,
    /// The read handle, made once beside `models` so the accessor allocates nothing.
    models_read: ReadSignal<Vec<LocalModel>>,
    /// The newest refresh ticket applied so far.
    applied: Arc<Mutex<u64>>,
    /// The last ticket handed out.
    issued: Arc<Mutex<u64>>,
}

impl std::fmt::Debug for ModelList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelList")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl ModelList {
    /// A list over `store`, empty until the first [`refresh`](Self::refresh).
    pub fn new(store: ModelStore) -> Self {
        let models = RwSignal::new(Vec::new());
        Self {
            store,
            models,
            models_read: models.read_only(),
            applied: Arc::default(),
            issued: Arc::default(),
        }
    }

    /// The stored models, in the store's listing order. Every call returns the same signal and
    /// is cheap to call from `build`.
    pub fn models(&self) -> ReadSignal<Vec<LocalModel>> {
        self.models_read
    }

    /// The store this list reads.
    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    /// Rescans the store on a blocking thread; the UI thread publishes the result.
    pub fn refresh(&self) {
        let list = self.clone();
        self.publish(spawn_blocking(move || list.scan()));
    }

    /// Deletes a model and its sidecars on a blocking thread; the UI thread then publishes the
    /// new listing. A failure is logged and the listing is refreshed anyway.
    pub fn remove(&self, id: ModelId) {
        let list = self.clone();
        self.publish(spawn_blocking(move || {
            if let Err(err) = list.store.remove(&id) {
                log::warn!("removing model {id} failed: {err}");
            }
            list.scan()
        }));
    }

    /// Writes the scan `job` returns, from a task on the UI thread's local queue.
    fn publish<E: Display>(&self, job: impl Future<Output = Result<Scan, E>> + 'static) {
        let list = self.clone();
        spawn_local(async move {
            match job.await {
                Ok(scan) => list.apply(scan),
                Err(err) => log::warn!("scanning the model store failed: {err}"),
            }
        });
    }

    /// Scans the store on the calling thread, which is never the UI thread: the disk half of a
    /// refresh. [`apply`](Self::apply) is the other half.
    pub(crate) fn scan(&self) -> Scan {
        let ticket = {
            let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            *issued += 1;
            *issued
        };
        Scan {
            ticket,
            models: self.store.list(),
        }
    }

    /// Writes `scan` to the signal unless a newer scan already has. UI thread only.
    pub(crate) fn apply(&self, scan: Scan) {
        let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
        if scan.ticket > *applied {
            *applied = scan.ticket;
            drop(applied);
            self.models.set(scan.models);
        }
    }
}

/// One listing of the store and its place among the list's scans.
pub(crate) struct Scan {
    /// Issued when the scan started; a smaller ticket is an older scan.
    ticket: u64,
    models: Vec<LocalModel>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_returns_the_same_signal_on_every_call() {
        let list = ModelList::new(
            ModelStore::with_root(std::env::temp_dir().join("frust-models-list")).unwrap(),
        );
        let first = list.models();
        for _ in 0..1000 {
            assert!(list.models() == first);
        }
        assert!(list.clone().models() == first);
    }
}
