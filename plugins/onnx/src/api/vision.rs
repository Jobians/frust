//! [`VisionHandle`]: the vision tasks' results and status as signals.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use frust::{ReadSignal, RwSignal, Set};

use super::publish::Outbox;
use crate::catalog::{VisionModel, VisionTask};
use crate::vision::{Face, VisionModels, VisionRun};
use crate::{OnnxError, Plane, Provider, RgbaView};

/// What a [`VisionHandle`] is doing.
///
/// From a request made while the handle is idle, the status is
/// [`Running`](Self::Running) before the request call returns; it then names each task as the
/// worker starts it, and becomes [`Idle`](Self::Idle) once the worker's last result is written,
/// or [`Failed`](Self::Failed) when a request fails. A failure stays until the next request
/// starts. Every status is written on the UI thread, in this order, the result it follows
/// written first. The same status is never published twice in a row; a component reading the
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

/// One signal write, made on the worker and applied on the UI thread.
enum Update {
    Status(VisionStatus),
    /// A task's result and its run report, written in this order.
    Result(Value, Option<VisionRun>),
}

/// A result already checked against the task that asked for it.
enum Value {
    Mask(Arc<Plane>),
    Faces(Arc<Vec<Face>>),
    Depth(Arc<Plane>),
}

impl Value {
    /// `output` as `task`'s result; an output of the wrong kind is an error.
    fn of(task: VisionTask, output: VisionOutput) -> Result<Self, OnnxError> {
        match (task, output) {
            (VisionTask::Segmentation, VisionOutput::Plane(plane)) => {
                Ok(Self::Mask(Arc::new(plane)))
            }
            (VisionTask::Depth, VisionOutput::Plane(plane)) => Ok(Self::Depth(Arc::new(plane))),
            (VisionTask::FaceDetection, VisionOutput::Faces(faces)) => {
                Ok(Self::Faces(Arc::new(faces)))
            }
            (task, _) => Err(OnnxError::Run(format!(
                "the {task:?} task returned another task's result"
            ))),
        }
    }
}

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
    /// The last status queued, kept here so the worker never reads the signal.
    published: VisionStatus,
}

/// The signals, the backend, the lock every update is queued under and the queue itself.
struct Shared {
    backend: Backend,
    status: RwSignal<VisionStatus>,
    mask: RwSignal<Option<Arc<Plane>>>,
    faces: RwSignal<Arc<Vec<Face>>>,
    depth: RwSignal<Option<Arc<Plane>>>,
    /// Each task's last completed run, in [`slot`] order.
    runs: [RwSignal<Option<VisionRun>>; TASKS],
    /// Read handles made once beside the writable signals, so an accessor allocates nothing.
    reads: Reads,
    observer: Mutex<Option<StatusObserver>>,
    inner: Mutex<Inner>,
    /// The signal writes on their way to the UI thread, in the order they were queued.
    outbox: Outbox<Update>,
}

/// The read side of [`Shared`]'s signals.
struct Reads {
    status: ReadSignal<VisionStatus>,
    mask: ReadSignal<Option<Arc<Plane>>>,
    faces: ReadSignal<Arc<Vec<Face>>>,
    depth: ReadSignal<Option<Arc<Plane>>>,
    runs: [ReadSignal<Option<VisionRun>>; TASKS],
}

/// The vision tasks for a component: requests from event handlers, results and status as
/// signals for `build`.
///
/// [`segment`](Self::segment), [`detect_faces`](Self::detect_faces) and
/// [`estimate_depth`](Self::estimate_depth) return at once. The request runs on the handle's
/// worker thread, `frust-onnx-vision`, which loads the model on first use and runs it through
/// [`VisionModels`]; requests run one at a time, in the order made.
///
/// **Every signal is written on the UI thread.** The worker never writes one: it hands the
/// task's result ([`mask`](Self::mask), [`faces`](Self::faces), [`depth`](Self::depth)), the
/// task's [`last_run`](Self::last_run) (provider and duration) and the next
/// [`status`](Self::status) to the UI thread, which writes them in that order on its next pump
/// of the local task queue (the runtime wakes the shell for it). A request call applies the
/// waiting writes itself before it returns. The worker never reads a signal, never waits on the
/// UI thread, and holds the handle's lock only while it queues.
///
/// Construct the handle anywhere; make the requests on the UI thread, from `init`, an event
/// handler or a `frust::spawn_local` task, with the reactive runtime a running app has. A
/// request made on another thread panics before it changes anything (the first one with
/// `frust::spawn_local`'s message).
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
/// queue: the worker runs what is queued and ends once the queue is empty, and the UI thread
/// still applies its writes, which are harmless, then drops its task. The thread holds no
/// thread-local state.
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
        let status = RwSignal::new(VisionStatus::Idle);
        let mask = RwSignal::new(None);
        let faces = RwSignal::new(Arc::new(Vec::new()));
        let depth = RwSignal::new(None);
        let runs: [RwSignal<Option<VisionRun>>; TASKS] = [(); TASKS].map(|()| RwSignal::new(None));
        let reads = Reads {
            status: status.read_only(),
            mask: mask.read_only(),
            faces: faces.read_only(),
            depth: depth.read_only(),
            runs: runs.map(|run| run.read_only()),
        };
        Self {
            shared: Arc::new(Shared {
                backend,
                status,
                mask,
                faces,
                depth,
                runs,
                reads,
                observer: Mutex::new(None),
                inner: Mutex::new(Inner::default()),
                outbox: Outbox::new(),
            }),
        }
    }

    /// Calls `observer` with every status the handle publishes, in order, right after the
    /// signal is written, for example to log the requests. It runs on the UI thread, where
    /// every write is made: inside the request call for the `Running` it publishes, and
    /// otherwise in the pump of the UI thread's local queue that applies the status. No lock of
    /// the handle is held, so it may read the handle's signals; keep it short and make no
    /// request from it.
    #[must_use]
    pub fn on_status(self, observer: impl Fn(&VisionStatus) + Send + Sync + 'static) -> Self {
        *self
            .shared
            .observer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(observer));
        self
    }

    /// What the handle is doing. This and the result accessors below ([`mask`](Self::mask),
    /// [`faces`](Self::faces), [`depth`](Self::depth), [`last_run`](Self::last_run)) return the
    /// same signal on every call and are cheap to call from `build`.
    pub fn status(&self) -> ReadSignal<VisionStatus> {
        self.shared.reads.status
    }

    /// The newest subject mask ([`VisionModels::segment_subject`]), `None` until one is ready.
    pub fn mask(&self) -> ReadSignal<Option<Arc<Plane>>> {
        self.shared.reads.mask
    }

    /// The newest face list ([`VisionModels::detect_faces`]), highest score first; empty until
    /// one is ready, and empty for an image without faces.
    pub fn faces(&self) -> ReadSignal<Arc<Vec<Face>>> {
        self.shared.reads.faces
    }

    /// The newest depth map ([`VisionModels::estimate_depth`]), near is 1; `None` until one is
    /// ready.
    pub fn depth(&self) -> ReadSignal<Option<Arc<Plane>>> {
        self.shared.reads.depth
    }

    /// `task`'s last completed run: the provider that ran it and how long it took, `None` until
    /// one has published its result. Written with the result, before the status turns
    /// [`Idle`](VisionStatus::Idle).
    pub fn last_run(&self, task: VisionTask) -> ReadSignal<Option<VisionRun>> {
        self.shared.reads.runs[slot(task)]
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
        let shared = self.shared.clone();
        // Panics off the UI thread, before anything has changed.
        let work = self
            .shared
            .outbox
            .begin("a VisionHandle request", move |update| shared.apply(update));
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
            // The running worker serves the job and holds its own token.
            drop(inner);
            return;
        }
        inner.worker = true;
        self.shared.publish(&mut inner, VisionStatus::Running(task));
        let shared = self.shared.clone();
        // The worker's first lock waits for the one held here, so `Running` is queued first.
        let spawned = thread::Builder::new()
            .name("frust-onnx-vision".to_owned())
            .spawn(move || {
                let _work = work;
                shared.work();
            });
        if let Err(err) = spawned {
            inner.worker = false;
            inner.queue.clear();
            self.shared.publish(
                &mut inner,
                VisionStatus::Failed(format!("the vision worker thread did not start: {err}")),
            );
        }
        drop(inner);
        // `Running`, and anything the worker has queued since, is written before the call
        // returns.
        self.shared.outbox.flush(|update| self.shared.apply(update));
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `status` unless it is the last one queued. Called with the lock held, so updates
    /// are queued in the order they are made, whichever thread makes them.
    fn publish(&self, inner: &mut Inner, status: VisionStatus) {
        if inner.published == status {
            return;
        }
        self.outbox.push(Update::Status(status.clone()));
        inner.published = status;
    }

    /// Writes one update to its signals. UI thread only: the outbox's drain task and the
    /// request call's flush are its only callers.
    fn apply(&self, update: Update) {
        match update {
            Update::Status(status) => {
                self.status.set(status.clone());
                let observer = self
                    .observer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(observer) = observer {
                    observer(&status);
                }
            }
            Update::Result(value, run) => {
                let task = match value {
                    Value::Mask(mask) => {
                        self.mask.set(Some(mask));
                        VisionTask::Segmentation
                    }
                    Value::Faces(faces) => {
                        self.faces.set(faces);
                        VisionTask::FaceDetection
                    }
                    Value::Depth(depth) => {
                        self.depth.set(Some(depth));
                        VisionTask::Depth
                    }
                };
                self.runs[slot(task)].set(run);
            }
        }
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
                match result.and_then(|output| Value::of(job.task, output)) {
                    Ok(value) => {
                        self.outbox.push(Update::Result(value, run));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_return_the_same_signal_on_every_call() {
        let handle =
            VisionHandle::with_runner(|_, _| Err(OnnxError::ModelMissing { key: "none".into() }));
        let (status, mask, faces, depth) = (
            handle.status(),
            handle.mask(),
            handle.faces(),
            handle.depth(),
        );
        let runs: Vec<_> = [
            VisionTask::Segmentation,
            VisionTask::FaceDetection,
            VisionTask::Depth,
        ]
        .into_iter()
        .map(|task| (task, handle.last_run(task)))
        .collect();
        for _ in 0..1000 {
            assert!(handle.status() == status);
            assert!(handle.mask() == mask);
            assert!(handle.faces() == faces);
            assert!(handle.depth() == depth);
            for (task, run) in &runs {
                assert!(handle.last_run(*task) == *run);
            }
        }
        assert!(handle.clone().status() == status);
    }
}
