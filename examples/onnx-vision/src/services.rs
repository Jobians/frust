//! What both screens share: the model store, its listing, one download tracker per catalog row
//! and the vision handle. Built once by the app shell and handed to each screen.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use frust::{RwSignal, Set, spawn_blocking, spawn_local};
use frust_device_info::DeviceMemory;
use frust_models::api::{DownloadTracker, ModelList};
use frust_models::{HubClient, LocalModel, ModelId, ModelStore};
use frust_onnx::api::{VisionHandle, VisionStatus};
use frust_onnx::catalog::{VISION_CATALOG, VisionModel, VisionTask};
use frust_onnx::{Provider, SessionOptions, VisionModels};

use crate::format::RunTiming;

/// One catalog row: the model and the tracker of its download.
pub struct ModelRow {
    /// The catalog entry.
    pub model: &'static VisionModel,
    /// Its download; one tracker per row, so rows download independently.
    pub tracker: DownloadTracker,
    /// Why the last Download press could not start, shown under the row.
    pub problem: RwSignal<String>,
}

/// The store's disk use, measured off the UI thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskUsage {
    /// Bytes of finished models; `None` until the first measurement lands.
    pub total: Option<u64>,
    /// Partial downloads kept for a resume, with their byte counts.
    pub partial: Vec<(ModelId, u64)>,
}

impl DiskUsage {
    /// Bytes kept for `id`'s resume, zero when none.
    pub fn partial_for(&self, id: &ModelId) -> u64 {
        self.partial
            .iter()
            .find(|(p, _)| p == id)
            .map_or(0, |(_, n)| *n)
    }

    /// Bytes of every partial download.
    pub fn partial_total(&self) -> u64 {
        self.partial.iter().map(|(_, n)| n).sum()
    }
}

/// The app's long-lived handles.
pub struct Services {
    /// Where the models live.
    pub store: ModelStore,
    /// The stored models as a signal.
    pub list: ModelList,
    /// The HTTP client downloads go through, or why it could not be built.
    pub hub: Result<HubClient, String>,
    /// One row per catalog model, in catalog order.
    pub rows: Vec<ModelRow>,
    /// The models behind `vision`; kept to read the provider and to drop sessions.
    pub models: Arc<VisionModels>,
    /// The vision tasks, run off the UI thread.
    pub vision: VisionHandle,
    /// The last successful run, written as the handle reports its statuses.
    pub last_run: RwSignal<Option<RunTiming>>,
    /// The store's disk use.
    pub disk: RwSignal<DiskUsage>,
    /// Total RAM, when the platform reports it.
    pub memory_total: Option<u64>,
}

impl Services {
    /// Opens the store under the app data directory, or at `root` when given (the tests).
    pub fn open(root: Option<PathBuf>) -> Result<Self, String> {
        let store = match root {
            Some(root) => ModelStore::with_root(root),
            None => ModelStore::open(),
        }
        .map_err(|err| format!("the model store cannot open: {err}"))?;
        Ok(Self::with_store(store))
    }

    fn with_store(store: ModelStore) -> Self {
        let list = ModelList::new(store.clone());
        list.refresh();
        let rows = VISION_CATALOG
            .iter()
            .map(|model| ModelRow {
                model,
                tracker: DownloadTracker::new().with_list(list.clone()),
                problem: RwSignal::new(String::new()),
            })
            .collect();
        let models = Arc::new(VisionModels::new(store.clone(), SessionOptions::default()));
        let last_run = RwSignal::new(None);
        let vision = VisionHandle::new(Arc::clone(&models))
            .on_status(timing_observer(Arc::clone(&models), last_run));
        Self {
            store,
            list,
            hub: HubClient::new().map_err(|err| err.to_string()),
            rows,
            models,
            vision,
            last_run,
            disk: RwSignal::new(DiskUsage::default()),
            memory_total: DeviceMemory::query().map(|m| m.total_bytes),
        }
    }

    /// Measures the store's disk use on a blocking thread and publishes it.
    pub fn refresh_disk(&self) {
        let store = self.store.clone();
        let disk = self.disk;
        spawn_local(async move {
            let measured = spawn_blocking(move || DiskUsage {
                total: Some(store.total_bytes()),
                partial: store.in_progress(),
            })
            .await;
            if let Ok(usage) = measured {
                disk.set(usage);
            }
        });
    }

    /// Deletes a model, its partial download and its loaded session, off the UI thread.
    pub fn delete(&self, model: &'static VisionModel) {
        let id = (model.id)();
        let store = self.store.clone();
        let models = Arc::clone(&self.models);
        let list = self.list.clone();
        spawn_local(async move {
            let removed = spawn_blocking(move || {
                // A loaded session holds the file open: drop it before the file goes.
                models.cache().evict(store.path_for(&id));
                store.remove(&id)
            })
            .await;
            if let Ok(Err(err)) = removed {
                log::warn!("deleting a model failed: {err}");
            }
            list.refresh();
        });
    }

    /// Whether `task`'s catalog model is in `stored` (the list signal's value).
    pub fn installed(stored: &[LocalModel], task: VisionTask) -> bool {
        VISION_CATALOG
            .iter()
            .filter(|m| m.task == task)
            .any(|m| stored.iter().any(|s| s.id == (m.id)()))
    }
}

/// The provider of the session that ran `task`. The run just loaded it, so this is a cache hit.
fn provider_of(models: &VisionModels, task: VisionTask) -> Option<Provider> {
    let model = VISION_CATALOG.iter().find(|m| m.task == task)?;
    let local = models.store().get(&(model.id)())?;
    let session = models
        .cache()
        .get_or_load(&local.path, models.options())
        .ok()?;
    Some(session.provider())
}

/// Turns the handle's status sequence into [`RunTiming`]s: a task runs from its `Running` to the
/// next status, and succeeded unless that status is `Failed`.
fn timing_observer(
    models: Arc<VisionModels>,
    last_run: RwSignal<Option<RunTiming>>,
) -> impl Fn(&VisionStatus) + Send + Sync + 'static {
    #[derive(Default)]
    struct Clock {
        running: Option<(VisionTask, Instant)>,
        ran: Vec<VisionTask>,
    }
    let clock = Mutex::new(Clock::default());
    move |status: &VisionStatus| {
        let mut clock = clock.lock().unwrap_or_else(|e| e.into_inner());
        let (ended, ok) = match status {
            VisionStatus::Running(task) => (clock.running.replace((*task, Instant::now())), true),
            VisionStatus::Idle => (clock.running.take(), true),
            VisionStatus::Failed(_) => (clock.running.take(), false),
        };
        let Some((task, started)) = ended else {
            return;
        };
        if !ok {
            return;
        }
        let first = !clock.ran.contains(&task);
        if first {
            clock.ran.push(task);
        }
        last_run.set(Some(RunTiming {
            task,
            millis: started.elapsed().as_millis() as u64,
            provider: provider_of(&models, task),
            first,
        }));
    }
}
