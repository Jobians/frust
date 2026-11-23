//! [`ModelList`]: the stored models as a signal.

use std::sync::{Arc, Mutex};

use frust::{ReadSignal, RwSignal, Set, spawn_blocking};

use crate::id::ModelId;
use crate::store::{LocalModel, ModelStore};

/// The models in a [`ModelStore`] as a signal a component's `build` reads.
///
/// The list starts empty: scanning the store touches the disk, so it never runs on
/// construction. Call [`refresh`](Self::refresh) once, for example from `Component::init`,
/// and again whenever the store changes behind the list's back. Clones share the same signal.
///
/// [`refresh`](Self::refresh) and [`remove`](Self::remove) hand the disk work to
/// `frust::spawn_blocking`, so they need the reactive runtime the shell starts. Overlapping
/// refreshes are applied in the order they were requested, so a slow scan never overwrites a
/// newer one.
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

    /// Rescans the store on a blocking thread and publishes the result.
    pub fn refresh(&self) {
        let list = self.clone();
        spawn_blocking(move || list.refresh_now());
    }

    /// Deletes a model and its sidecars on a blocking thread, then publishes the new listing.
    /// A failure is logged and the listing is refreshed anyway.
    pub fn remove(&self, id: ModelId) {
        let list = self.clone();
        spawn_blocking(move || {
            if let Err(err) = list.store.remove(&id) {
                log::warn!("removing model {id} failed: {err}");
            }
            list.refresh_now();
        });
    }

    /// Scans on the calling thread and publishes the result unless a newer scan already has.
    pub(crate) fn refresh_now(&self) {
        let ticket = {
            let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            *issued += 1;
            *issued
        };
        let scan = self.store.list();
        let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
        if ticket > *applied {
            *applied = ticket;
            self.models.set(scan);
        }
    }
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
