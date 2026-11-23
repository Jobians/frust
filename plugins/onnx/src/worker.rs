//! Runs off the calling thread: one `frust-onnx-run` thread per session, started on the first
//! [`Session::run`] and gone once the session and its queued runs are, delivering exactly one
//! [`RunEvent`] per run.
//!
//! Cancelling a run that is under way terminates it through ONNX Runtime's run options, which
//! stop graph execution at the next node boundary; a run that finishes before the flag is seen is
//! still reported as [`RunEvent::Cancelled`].

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use ort::session::RunOptions;

use crate::{OnnxError, Session, Tensor};

/// The worker thread's name.
const THREAD_NAME: &str = "frust-onnx-run";

/// The one outcome a [`Session::run`] delivers to its sink.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    /// The run finished: every output, named, in declaration order.
    Done(Vec<(String, Tensor)>),
    /// [`RunHandle::cancel`] was called before the outcome was delivered.
    Cancelled,
    /// The run failed, or its inputs were refused ([`OnnxError::InvalidInput`]).
    Error(OnnxError),
}

/// The sink a [`Session::run`] delivers its [`RunEvent`] to, on the session's worker thread.
pub type RunSink = Arc<dyn Fn(RunEvent) + Send + Sync>;

type Job = Box<dyn FnOnce() + Send>;

/// The session's queue onto its worker thread, which is spawned on demand.
#[derive(Default)]
pub(crate) struct Worker {
    jobs: Mutex<Option<Sender<Job>>>,
}

impl Worker {
    /// Queues `job`, starting the thread if there is none (or it is gone). Hands the job back if
    /// no thread can be started.
    fn submit(&self, job: Job) -> Result<(), (Job, std::io::Error)> {
        let mut jobs = lock(&self.jobs);
        let job = match jobs.as_ref() {
            Some(sender) => match sender.send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (sender, receiver) = mpsc::channel::<Job>();
        let spawned = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            });
        if let Err(err) = spawned {
            return Err((job, err));
        }
        let sent = sender.send(job);
        *jobs = Some(sender);
        sent.map_err(|mpsc::SendError(job)| {
            (
                job,
                std::io::Error::other("the worker thread exited at once"),
            )
        })
    }
}

/// Where one run stands; guarded by [`Shared::state`].
#[derive(Default)]
struct State {
    cancelled: bool,
    /// The outcome is decided; a later cancel changes nothing.
    decided: bool,
    finished: bool,
    terminator: Option<Arc<RunOptions>>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    finished: Condvar,
}

/// Controls one [`Session::run`]. Dropping it neither cancels nor waits for the run.
#[derive(Clone)]
pub struct RunHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.shared.state);
        f.debug_struct("RunHandle")
            .field("cancelled", &state.cancelled)
            .field("finished", &state.finished)
            .finish()
    }
}

impl RunHandle {
    /// Cancels the run: a queued run is skipped, a running one is terminated, and either way the
    /// sink receives [`RunEvent::Cancelled`]. Does nothing once the outcome is decided.
    pub fn cancel(&self) {
        let mut state = lock(&self.shared.state);
        if state.decided {
            return;
        }
        state.cancelled = true;
        if let Some(terminator) = &state.terminator
            && let Err(err) = terminator.terminate()
        {
            log::warn!("frust-onnx: could not terminate a run: {err}");
        }
    }

    /// Whether the sink has received the run's event and returned.
    pub fn is_finished(&self) -> bool {
        lock(&self.shared.state).finished
    }

    /// Blocks until [`is_finished`](Self::is_finished). Never call it from the sink, which would
    /// wait for itself.
    pub fn wait(&self) {
        let mut state = lock(&self.shared.state);
        while !state.finished {
            state = self
                .shared
                .finished
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Session {
    /// Runs the model on `inputs` on the session's worker thread (`frust-onnx-run`) and returns
    /// at once. `sink` receives exactly one [`RunEvent`], on that thread.
    ///
    /// Runs on one session queue behind each other in submission order; the queued runs keep the
    /// session alive. Inputs are checked as [`run_blocking`](Self::run_blocking) checks them,
    /// a refusal arriving as [`RunEvent::Error`]. If no worker thread can be started, the sink
    /// receives that error on the calling thread before `run` returns.
    pub fn run(self: &Arc<Self>, inputs: Vec<(String, Tensor)>, sink: RunSink) -> RunHandle {
        let shared = Arc::new(Shared::default());
        let handle = RunHandle {
            shared: Arc::clone(&shared),
        };
        let session = Arc::clone(self);
        let job_sink = Arc::clone(&sink);
        let job: Job = Box::new(move || {
            let event = session.run_job(&inputs, &shared);
            deliver(&shared, &job_sink, event);
        });
        if let Err((job, err)) = self.worker.submit(job) {
            drop(job);
            let event = RunEvent::Error(OnnxError::Run(format!(
                "could not start the {THREAD_NAME} thread: {err}"
            )));
            deliver(&handle.shared, &sink, event);
        }
        handle
    }

    /// The run itself, on the worker thread: skipped when cancelled first, terminable while it
    /// runs, and always settling the outcome.
    fn run_job(&self, inputs: &[(String, Tensor)], shared: &Shared) -> RunEvent {
        let run_options = match self.run_options() {
            Ok(run_options) => Arc::new(run_options),
            Err(err) => return decide(&shared.state, RunEvent::Error(err)),
        };
        {
            let mut state = lock(&shared.state);
            if state.cancelled {
                state.decided = true;
                return RunEvent::Cancelled;
            }
            state.terminator = Some(Arc::clone(&run_options));
        }
        let pairs: Vec<(&str, &Tensor)> = inputs.iter().map(|(n, t)| (n.as_str(), t)).collect();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            self.run_pairs(&pairs, Some(&run_options))
        }));
        let event = match outcome {
            Ok(Ok(outputs)) => RunEvent::Done(outputs),
            Ok(Err(err)) => RunEvent::Error(err),
            Err(_) => RunEvent::Error(OnnxError::Run("the run panicked".to_owned())),
        };
        decide(&shared.state, event)
    }
}

/// Settles the outcome: [`RunEvent::Cancelled`] when a cancel got in first, `event` otherwise.
fn decide(state: &Mutex<State>, event: RunEvent) -> RunEvent {
    let mut state = lock(state);
    state.terminator = None;
    state.decided = true;
    if state.cancelled {
        RunEvent::Cancelled
    } else {
        event
    }
}

/// Hands `event` to the sink (a panicking sink is logged, not propagated, so the worker thread
/// survives it) and marks the run finished.
fn deliver(shared: &Shared, sink: &RunSink, event: RunEvent) {
    lock(&shared.state).decided = true;
    if catch_unwind(AssertUnwindSafe(|| sink(event))).is_err() {
        log::warn!("frust-onnx: a run's sink panicked");
    }
    lock(&shared.state).finished = true;
    shared.finished.notify_all();
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
