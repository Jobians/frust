//! [`VisionHandle`]: the vision tasks' results and status as signals.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use frust::{ReadSignal, RwSignal, Set};

use crate::catalog::{VisionModel, VisionTask};
use crate::vision::{Face, VisionModels, VisionRun};
use crate::{OnnxError, Plane, Provider, RgbaView};

/// What a [`VisionHandle`] is doing.
///
/// From a request made while the handle is idle, the status is
/// [`Running`](Self::Running) before the request call returns; it then names each task as the
/// worker starts it, and becomes [`Idle`](Self::Idle) once the worker has published its last
/// result, or [`Failed`](Self::Failed) when a request fails. A failure stays until the next
/// request starts. The same status is never published twice in a row; a component reading the
/// signal may still miss a short one between two frames, which
/// [`VisionHandle::on_status`] does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum VisionStatus {
    /// No request is queued or running.
    #[default]
    Idle,
    /// The worker is running (or about to run) this task.
    Running(VisionTask),
    /// The last request failed; the text is the error's message. The result signal of its task
    /// keeps its previous value.
    Failed(String),
}

/// One task result, as a [`VisionHandle`]'s runner returns it.
///
/// Public only for the handle's own test seam; an app reads the result signals instead.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub enum VisionOutput {
    /// A subject mask or a depth map.
    Plane(Plane),
    /// Detected faces.
    Faces(Vec<Face>),
}

/// What runs a request: the models themselves, or a stand-in a test injects.
enum Backend {
    Models(Arc<VisionModels>),
    /// The runner, and the provider its runs report.
    Runner(Box<RunnerFn>, Option<Provider>),
}

type RunnerFn =
    dyn Fn(VisionTask, RgbaView<'_>) -> Result<VisionOutput, OnnxError> + Send + Sync + 'static;

type StatusObserver = Arc<dyn Fn(&VisionStatus) + Send + Sync>;

/// One queued request.
struct Job {
    task: VisionTask,
    /// The request's number among its task's requests; an older one is stale.
    generation: u64,
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
    min_score: Option<f32>,
}

/// Every task, in [`slot`] order.
const TASKS: usize = 3;

fn slot(task: VisionTask) -> usize {
    match task {
        VisionTask::Segmentation => 0,
        VisionTask::FaceDetection => 1,
        VisionTask::Depth => 2,
    }
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<Job>,
    /// A worker thread is serving the queue.
    worker: bool,
    /// The newest request's generation, per task.
    latest: [u64; TASKS],
    /// The last status written, kept here so the worker never reads the signal.
    published: VisionStatus,
    observer: Option<StatusObserver>,
}

/// The signals, the backend and the lock every status write goes through.
struct Shared {
    backend: Backend,
    status: RwSignal<VisionStatus>,
    mask: RwSignal<Option<Arc<Plane>>>,
    faces: RwSignal<Arc<Vec<Face>>>,
    depth: RwSignal<Option<Arc<Plane>>>,
    /// Each task's last completed run, in [`slot`] order.
    runs: [RwSignal<Option<VisionRun>>; TASKS],
    inner: Mutex<Inner>,
}

/// The vision tasks for a component: requests from event handlers, results and status as
/// signals for `build`.
///
/// [`segment`](Self::segment), [`detect_faces`](Self::detect_faces) and
/// [`estimate_depth`](Self::estimate_depth) return at once. The request runs on the handle's
/// worker thread, `frust-onnx-vision`, which loads the model on first use and runs it through
/// [`VisionModels`]; requests run one at a time, in the order made. The worker then writes the
/// task's result signal ([`mask`](Self::mask), [`faces`](Self::faces),
/// [`depth`](Self::depth)), then the task's [`last_run`](Self::last_run) (provider and
/// duration), then [`status`](Self::status). It only writes signals: it never
/// reads one, never waits on the UI thread, and holds the handle's lock only while it writes.
///
/// **A newer request for a task supersedes an older one.** A queued older request is dropped
/// unrun; one already running finishes, and its result is dropped, so a result signal never
/// goes back to an older image's answer. Requests for different tasks do not supersede each
/// other.
///
/// Every result is relative to the request's image (see [`VisionModels`] for each task's
/// meaning). A missing model is reported as [`VisionStatus::Failed`] with
/// [`OnnxError::ModelMissing`]'s message; check [`missing_models`](Self::missing_models) first.
///
/// Clones share the worker, the queue and the signals. Dropping the handle does not cancel the
/// queue: the worker runs what is queued, its writes are harmless, and it ends once the queue is
/// empty. The thread holds no thread-local state.
#[derive(Clone)]
pub struct VisionHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for VisionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionHandle").finish_non_exhaustive()
    }
}

impl VisionHandle {
    /// An idle handle over `models`. Keep a clone of the `Arc` to clear its session cache on
    /// memory pressure ([`VisionModels::cache`]).
    pub fn new(models: impl Into<Arc<VisionModels>>) -> Self {
        Self::with_backend(Backend::Models(models.into()))
    }

    /// An idle handle whose requests are run by `runner` instead of a [`VisionModels`]: the
    /// test seam. `runner` is called on the worker thread with the request's task and image.
    #[doc(hidden)]
    pub fn with_runner(
        runner: impl Fn(VisionTask, RgbaView<'_>) -> Result<VisionOutput, OnnxError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with_backend(Backend::Runner(Box::new(runner), None))
    }

    /// [`with_runner`](Self::with_runner) whose runs report `provider` in
    /// [`last_run`](Self::last_run).
    #[doc(hidden)]
    pub fn with_runner_on(
        provider: Provider,
        runner: impl Fn(VisionTask, RgbaView<'_>) -> Result<VisionOutput, OnnxError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with_backend(Backend::Runner(Box::new(runner), Some(provider)))
    }

    fn with_backend(backend: Backend) -> Self {
        Self {
            shared: Arc::new(Shared {
                backend,
                status: RwSignal::new(VisionStatus::Idle),
                mask: RwSignal::new(None),
                faces: RwSignal::new(Arc::new(Vec::new())),
                depth: RwSignal::new(None),
                runs: [(); TASKS].map(|()| RwSignal::new(None)),
                inner: Mutex::new(Inner::default()),
            }),
        }
    }

    /// Calls `observer` with every status the handle publishes, in order, right after the
    /// signal is written, for example to log the requests. It runs on whichever thread
    /// published the status, with the handle's lock held: keep it short and never call back
    /// into the handle from it.
    #[must_use]
    pub fn on_status(self, observer: impl Fn(&VisionStatus) + Send + Sync + 'static) -> Self {
        self.shared.lock().observer = Some(Arc::new(observer));
        self
    }

    /// What the handle is doing.
    pub fn status(&self) -> ReadSignal<VisionStatus> {
        self.shared.status.read_only()
    }

    /// The newest subject mask ([`VisionModels::segment_subject`]), `None` until one is ready.
    pub fn mask(&self) -> ReadSignal<Option<Arc<Plane>>> {
        self.shared.mask.read_only()
    }

    /// The newest face list ([`VisionModels::detect_faces`]), highest score first; empty until
    /// one is ready, and empty for an image without faces.
    pub fn faces(&self) -> ReadSignal<Arc<Vec<Face>>> {
        self.shared.faces.read_only()
    }

    /// The newest depth map ([`VisionModels::estimate_depth`]), near is 1; `None` until one is
    /// ready.
    pub fn depth(&self) -> ReadSignal<Option<Arc<Plane>>> {
        self.shared.depth.read_only()
    }

    /// `task`'s last completed run: the provider that ran it and how long it took, `None` until
    /// one has published its result. Written with the result, before the status turns
    /// [`Idle`](VisionStatus::Idle).
    pub fn last_run(&self, task: VisionTask) -> ReadSignal<Option<VisionRun>> {
        self.shared.runs[slot(task)].read_only()
    }

    /// The catalog models still to download ([`VisionModels::missing`]). Reads the store's
    /// directory: call it from `Component::init` or a worker, not on every frame.
    pub fn missing_models(&self) -> Vec<&'static VisionModel> {
        match &self.shared.backend {
            Backend::Models(models) => models.missing(),
            Backend::Runner(..) => Vec::new(),
        }
    }

    /// Asks for the subject mask of the RGBA8 image `pixels` (`width * height * 4` bytes, row
    /// by row), published to [`mask`](Self::mask). Returns at once; a buffer of the wrong size
    /// ends as [`VisionStatus::Failed`].
    pub fn segment(&self, pixels: Arc<[u8]>, width: u32, height: u32) {
        self.submit(VisionTask::Segmentation, pixels, width, height, None);
    }

    /// Asks for the faces in an RGBA8 image, published to [`faces`](Self::faces); `min_score`
    /// as [`VisionModels::detect_faces`] takes it (`None` keeps faces scoring at least 0.6).
    /// Returns at once.
    pub fn detect_faces(&self, pixels: Arc<[u8]>, width: u32, height: u32, min_score: Option<f32>) {
        self.submit(VisionTask::FaceDetection, pixels, width, height, min_score);
    }

    /// Asks for the relative depth of an RGBA8 image, published to [`depth`](Self::depth).
    /// Returns at once.
    pub fn estimate_depth(&self, pixels: Arc<[u8]>, width: u32, height: u32) {
        self.submit(VisionTask::Depth, pixels, width, height, None);
    }

    fn submit(
        &self,
        task: VisionTask,
        pixels: Arc<[u8]>,
        width: u32,
        height: u32,
        min_score: Option<f32>,
    ) {
        let mut inner = self.shared.lock();
        let latest = &mut inner.latest[slot(task)];
        *latest += 1;
        let generation = *latest;
        // A queued request for the same task is superseded unrun.
        inner.queue.retain(|job| job.task != task);
        inner.queue.push_back(Job {
            task,
            generation,
            pixels,
            width,
            height,
            min_score,
        });
        if inner.worker {
            return;
        }
        inner.worker = true;
        self.shared.publish(&mut inner, VisionStatus::Running(task));
        let shared = self.shared.clone();
        // The worker's first lock waits for the one held here, so `Running` comes first.
        let spawned = thread::Builder::new()
            .name("frust-onnx-vision".to_owned())
            .spawn(move || shared.work());
        if let Err(err) = spawned {
            inner.worker = false;
            inner.queue.clear();
            self.shared.publish(
                &mut inner,
                VisionStatus::Failed(format!("the vision worker thread did not start: {err}")),
            );
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, inner: &mut Inner, status: VisionStatus) {
        if inner.published == status {
            return;
        }
        self.status.set(status.clone());
        if let Some(observer) = &inner.observer {
            observer(&status);
        }
        inner.published = status;
    }

    /// The worker thread: runs the queue to its end.
    fn work(&self) {
        loop {
            let job = {
                let mut inner = self.lock();
                let Some(job) = inner.queue.pop_front() else {
                    inner.worker = false;
                    return;
                };
                self.publish(&mut inner, VisionStatus::Running(job.task));
                job
            };
            // The model runs without the lock, so requests can queue meanwhile.
            let loads = self.load_count();
            let started = Instant::now();
            let result = catch_unwind(AssertUnwindSafe(|| self.run(&job))).unwrap_or_else(|_| {
                Err(OnnxError::Run(format!("the {:?} task panicked", job.task)))
            });
            let duration = started.elapsed();
            // Read before taking the lock: the provider lookup reads the store's directory.
            let run = result.is_ok().then(|| VisionRun {
                provider: self.provider(job.task),
                duration,
                loaded_model: self.load_count() != loads,
            });
            let mut inner = self.lock();
            let failed = if inner.latest[slot(job.task)] != job.generation {
                // Superseded while it ran: the result is dropped.
                false
            } else {
                match result.and_then(|output| self.write(job.task, output)) {
                    Ok(()) => {
                        self.runs[slot(job.task)].set(run);
                        false
                    }
                    Err(err) => {
                        self.publish(&mut inner, VisionStatus::Failed(err.to_string()));
                        true
                    }
                }
            };
            if !failed && inner.queue.is_empty() {
                self.publish(&mut inner, VisionStatus::Idle);
            }
        }
    }

    fn run(&self, job: &Job) -> Result<VisionOutput, OnnxError> {
        let image = RgbaView::new(job.width, job.height, &job.pixels)?;
        match &self.backend {
            Backend::Models(models) => match job.task {
                VisionTask::Segmentation => models.segment_subject(image).map(VisionOutput::Plane),
                VisionTask::FaceDetection => models
                    .detect_faces(image, job.min_score)
                    .map(VisionOutput::Faces),
                VisionTask::Depth => models.estimate_depth(image).map(VisionOutput::Plane),
            },
            Backend::Runner(run, _) => run(job.task, image),
        }
    }

    /// The provider `task`'s session runs on, when it is loaded.
    fn provider(&self, task: VisionTask) -> Option<Provider> {
        match &self.backend {
            Backend::Models(models) => models.loaded_provider(task),
            Backend::Runner(_, provider) => *provider,
        }
    }

    /// The loads the models' session cache has made; zero for a runner.
    fn load_count(&self) -> u64 {
        match &self.backend {
            Backend::Models(models) => models.cache().load_count(),
            Backend::Runner(..) => 0,
        }
    }

    /// Writes `output` to `task`'s result signal; an output of the wrong kind is an error.
    fn write(&self, task: VisionTask, output: VisionOutput) -> Result<(), OnnxError> {
        match (task, output) {
            (VisionTask::Segmentation, VisionOutput::Plane(plane)) => {
                self.mask.set(Some(Arc::new(plane)));
            }
            (VisionTask::Depth, VisionOutput::Plane(plane)) => {
                self.depth.set(Some(Arc::new(plane)));
            }
            (VisionTask::FaceDetection, VisionOutput::Faces(faces)) => {
                self.faces.set(Arc::new(faces));
            }
            (task, _) => {
                return Err(OnnxError::Run(format!(
                    "the {task:?} task returned another task's result"
                )));
            }
        }
        Ok(())
    }
}
