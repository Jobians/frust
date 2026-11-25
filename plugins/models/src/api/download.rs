//! [`DownloadTracker`]: one download's progress as signals.

use std::sync::{Arc, Mutex, MutexGuard};

use frust::{ReadSignal, RwSignal, Set, spawn_blocking};

use super::ModelList;
use super::list::Scan;
use super::publish::{Outbox, Work};
use crate::download::{DownloadEvent, DownloadHandle, DownloadRequest, DownloadSink, Downloader};
use crate::error::ModelsError;
use crate::hub::HubClient;
use crate::store::ModelStore;

/// Where a tracked download is.
///
/// For one download the tracker guarantees this order, from a [`DownloadTracker::start`] or
/// [`DownloadTracker::start_hub`] that returned `Ok`:
///
/// 1. The first state is [`Running`](Self::Running), published before the call returns.
/// 2. Then [`Verifying`](Self::Verifying) and `Running` may alternate: `Verifying` at most once
///    per file of the bundle, while that file is checked, and `Running` again when the next
///    file starts. A file already stored is skipped and publishes nothing.
/// 3. Exactly one terminal state, [`Done`](Self::Done), [`Cancelled`](Self::Cancelled) or
///    [`Failed`](Self::Failed), comes last. Nothing follows it until the next start.
///
/// The same state is never published twice in a row. Every state is written on the UI thread,
/// in this order. A component reading the signal may still miss short states between two
/// frames; [`DownloadTracker::on_state`] sees every one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DownloadState {
    /// Nothing has been started, or the tracker was never used.
    #[default]
    Idle,
    /// The request is being built or bytes are being fetched.
    Running,
    /// A file was received and its size and digest are being checked.
    Verifying,
    /// Every file is stored.
    Done,
    /// The download was cancelled; partial files are kept for a resume.
    Cancelled,
    /// The download failed; the text is the error's message.
    Failed(String),
}

impl DownloadState {
    /// Whether this state ends a download.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled | Self::Failed(_))
    }
}

/// How far a tracked download has got. Byte counts cover the whole bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes on disk, finished files and partial downloads included.
    pub bytes: u64,
    /// Bundle size in bytes, when every file's size is known.
    pub total: Option<u64>,
    /// Average transfer rate of this run.
    pub bytes_per_sec: u64,
    /// Index of the file being fetched.
    pub file_index: usize,
    /// Number of files in the request.
    pub files: usize,
}

type StateObserver = Arc<dyn Fn(&DownloadState) + Send + Sync>;

/// Runs one download at a time and publishes its progress through signals.
///
/// [`state`](Self::state) and [`progress`](Self::progress) are read-only signals for a
/// component's `build`. [`start_hub`](Self::start_hub), [`start`](Self::start) and
/// [`cancel`](Self::cancel) are called from event handlers and never block on the network:
/// `start_hub` builds its request on `frust::spawn_blocking`.
///
/// **Every signal is written on the UI thread.** The download thread and the blocking pool never
/// write one: they hand each state and progress value to the UI thread, which writes them in the
/// order they were made on its next pump of the local task queue (the runtime wakes the shell
/// for it). A start call applies the waiting writes itself before it returns. The download
/// thread never reads a signal, never blocks on the UI and never calls back into the
/// downloader; it takes the tracker's short lock only to queue a write, which keeps the order
/// [`DownloadState`] documents.
///
/// Construct the tracker anywhere; call `start_hub` and `start` on the UI thread, with the
/// reactive runtime a running app has. Called on another thread, a start panics before it changes
/// anything (the first one with `frust::spawn_local`'s message).
///
/// Dropping the tracker does not cancel the download. The thread keeps running to its end, the
/// UI thread still applies its writes, which are harmless, and then drops its task; call
/// [`cancel`](Self::cancel) first to stop it.
///
/// Link a [`ModelList`] with [`with_list`](Self::with_list) and a finished download rescans the
/// store on the download thread; the new listing is written before the state becomes
/// [`DownloadState::Done`], so a component that sees `Done` also sees the new models.
pub struct DownloadTracker {
    publisher: Publisher,
}

/// One signal write, made off the UI thread and applied on it.
enum Update {
    State(DownloadState),
    Progress(DownloadProgress),
    /// The linked list's rescan, written before the `Done` that follows it.
    List(Scan),
}

/// The signals, the lock every update is queued under and the queue itself.
#[derive(Clone)]
struct Publisher {
    state: RwSignal<DownloadState>,
    progress: RwSignal<DownloadProgress>,
    /// Read handles made once beside the writable signals, so an accessor allocates nothing.
    state_read: ReadSignal<DownloadState>,
    progress_read: ReadSignal<DownloadProgress>,
    observer: Option<StateObserver>,
    list: Option<ModelList>,
    inner: Arc<Mutex<Inner>>,
    /// The signal writes on their way to the UI thread, in the order they were queued.
    outbox: Outbox<Update>,
}

#[derive(Default)]
struct Inner {
    /// A download started by this tracker has not reached its terminal state.
    active: bool,
    /// `cancel` arrived while `start_hub` was still building its request.
    cancel_requested: bool,
    handle: Option<DownloadHandle>,
    /// Number of files of the running download.
    files: usize,
    /// The last state queued, kept here so the download thread never reads the signal.
    published: DownloadState,
}

impl Publisher {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `state` unless it is the last one queued. Called with the lock held, so updates
    /// are queued in the order they are made, whichever thread makes them.
    fn publish(&self, inner: &mut Inner, state: DownloadState) {
        if inner.published == state {
            return;
        }
        self.outbox.push(Update::State(state.clone()));
        inner.published = state;
    }

    /// Marks a download of `files` files as started: fresh progress and `Running`.
    fn begin(&self, inner: &mut Inner, files: usize) {
        inner.active = true;
        inner.cancel_requested = false;
        inner.handle = None;
        inner.files = files;
        self.outbox.push(Update::Progress(DownloadProgress {
            files,
            ..DownloadProgress::default()
        }));
        self.publish(inner, DownloadState::Running);
    }

    /// Starts a download's unit of work on the UI thread, which keeps the task that applies
    /// its updates running. Panics off the UI thread, before anything has changed.
    fn work(&self, call: &str) -> Work<Update> {
        let publisher = self.clone();
        self.outbox
            .begin(call, move |update| publisher.apply(update))
    }

    /// Writes every waiting update now. UI thread only.
    fn flush(&self) {
        self.outbox.flush(|update| self.apply(update));
    }

    /// Writes one update to its signals. UI thread only: the outbox's drain task and a start
    /// call's flush are its only callers.
    fn apply(&self, update: Update) {
        match update {
            Update::State(state) => {
                self.state.set(state.clone());
                if let Some(observer) = &self.observer {
                    observer(&state);
                }
            }
            Update::Progress(progress) => self.progress.set(progress),
            Update::List(scan) => {
                if let Some(list) = &self.list {
                    list.apply(scan);
                }
            }
        }
    }

    fn finish(&self, inner: &mut Inner, terminal: DownloadState) {
        self.publish(inner, terminal);
        inner.active = false;
        inner.cancel_requested = false;
    }
}

impl Default for DownloadTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DownloadTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadTracker").finish_non_exhaustive()
    }
}

impl DownloadTracker {
    /// An idle tracker.
    pub fn new() -> Self {
        let state = RwSignal::new(DownloadState::Idle);
        let progress = RwSignal::new(DownloadProgress::default());
        Self {
            publisher: Publisher {
                state,
                progress,
                state_read: state.read_only(),
                progress_read: progress.read_only(),
                observer: None,
                list: None,
                inner: Arc::default(),
                outbox: Outbox::new(),
            },
        }
    }

    /// Links `list` so a finished download refreshes it. Use the same store for both.
    pub fn with_list(mut self, list: ModelList) -> Self {
        self.publisher.list = Some(list);
        self
    }

    /// Calls `observer` with every state the tracker publishes, in order, right after the
    /// signal is written, for example to log a download. It runs on the UI thread, where every
    /// write is made: inside the start call for the first `Running`, and otherwise in the pump of
    /// the UI thread's local queue that applies the state. No lock of the tracker is held, so it
    /// may read the tracker's signals; keep it short and never start or cancel from it.
    pub fn on_state(mut self, observer: impl Fn(&DownloadState) + Send + Sync + 'static) -> Self {
        self.publisher.observer = Some(Arc::new(observer));
        self
    }

    /// The download's state. Every call returns the same signal and is cheap to call from
    /// `build`.
    pub fn state(&self) -> ReadSignal<DownloadState> {
        self.publisher.state_read
    }

    /// The download's progress. It resets when a new download starts. Every call returns the
    /// same signal and is cheap to call from `build`.
    pub fn progress(&self) -> ReadSignal<DownloadProgress> {
        self.publisher.progress_read
    }

    /// Downloads `paths` from the Hub `repo` at `revision`, returning at once. The state is
    /// [`DownloadState::Running`] from this call on, while the request is built from the
    /// repository listing on `frust::spawn_blocking` and the download then runs. A listing
    /// that cannot be fetched, a missing path or a refused start (for example a file another
    /// download already owns) ends in [`DownloadState::Failed`].
    ///
    /// This is the path for an event handler: it never blocks on the network. It needs the
    /// reactive runtime a running app has and the UI thread. Fails with [`ModelsError::Busy`]
    /// while an earlier download of this tracker has not ended, leaving the signals as they
    /// were.
    pub fn start_hub(
        &self,
        client: &HubClient,
        store: &ModelStore,
        repo: &str,
        revision: &str,
        paths: &[&str],
    ) -> Result<(), ModelsError> {
        let work = self.publisher.work("DownloadTracker::start_hub");
        {
            let mut inner = self.publisher.lock();
            if inner.active {
                return Err(ModelsError::Busy);
            }
            self.publisher.begin(&mut inner, paths.len());
        }
        // `Running` is written before the call returns.
        self.publisher.flush();
        let publisher = self.publisher.clone();
        let sink = self.sink(work);
        let (client, store) = (client.clone(), store.clone());
        let (repo, revision) = (repo.to_owned(), revision.to_owned());
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        spawn_blocking(move || {
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            let request = client.request_for(&repo, &revision, &paths);
            let mut inner = publisher.lock();
            if inner.cancel_requested {
                publisher.finish(&mut inner, DownloadState::Cancelled);
                return;
            }
            match request.and_then(|request| Downloader::start(&client, &store, request, sink)) {
                Ok(handle) => inner.handle = Some(handle),
                Err(err) => publisher.finish(&mut inner, DownloadState::Failed(err.to_string())),
            }
        });
        Ok(())
    }

    /// Starts a request built beforehand and returns at once; the state becomes
    /// [`DownloadState::Running`] before the call returns. Building a Hub request blocks, so
    /// a handler uses [`start_hub`](Self::start_hub) for Hub files; this suits a
    /// [`DownloadRequest::from_url`] request, which builds without the network.
    ///
    /// Call it on the UI thread, with the reactive runtime a running app has. Fails with
    /// [`ModelsError::Busy`] while an earlier download of this tracker has not ended, and with
    /// whatever [`Downloader::start`] reports (for example a file another download already
    /// owns). On failure the signals are left as they were.
    pub fn start(
        &self,
        client: &HubClient,
        store: &ModelStore,
        request: DownloadRequest,
    ) -> Result<(), ModelsError> {
        let work = self.publisher.work("DownloadTracker::start");
        {
            let mut inner = self.publisher.lock();
            if inner.active {
                return Err(ModelsError::Busy);
            }
            let files = request.items.len();
            // The download thread's first event waits for the lock held here, so `Running` is
            // queued before anything the thread reports.
            let handle = Downloader::start(client, store, request, self.sink(work))?;
            self.publisher.begin(&mut inner, files);
            inner.handle = Some(handle);
        }
        // `Running` is written before the call returns.
        self.publisher.flush();
        Ok(())
    }

    /// Asks the running download to stop at its next chunk. The state becomes
    /// [`DownloadState::Cancelled`] once it has. Does nothing when no download runs.
    pub fn cancel(&self) {
        let mut inner = self.publisher.lock();
        if !inner.active {
            return;
        }
        match &inner.handle {
            Some(handle) => handle.cancel(),
            None => inner.cancel_requested = true,
        }
    }

    /// The download's sink. It owns `work`, so the UI thread's task applying the updates runs
    /// until the last copy of the sink is dropped: when the download thread ends, or when the
    /// request is never started.
    fn sink(&self, work: Work<Update>) -> DownloadSink {
        let publisher = self.publisher.clone();
        Arc::new(move |event| {
            let _running = &work;
            // The rescan touches the disk, so it runs before the lock is taken.
            let scan = match (&event, &publisher.list) {
                (DownloadEvent::Done(_), Some(list)) => Some(list.scan()),
                _ => None,
            };
            let mut inner = publisher.lock();
            match event {
                DownloadEvent::Started { total, files, .. } => {
                    inner.files = files;
                    publisher.outbox.push(Update::Progress(DownloadProgress {
                        total,
                        files,
                        ..DownloadProgress::default()
                    }));
                    publisher.publish(&mut inner, DownloadState::Running);
                }
                DownloadEvent::Progress {
                    bytes,
                    total,
                    bytes_per_sec,
                    file_index,
                } => {
                    publisher.outbox.push(Update::Progress(DownloadProgress {
                        bytes,
                        total,
                        bytes_per_sec,
                        file_index,
                        files: inner.files,
                    }));
                    publisher.publish(&mut inner, DownloadState::Running);
                }
                DownloadEvent::Verifying { .. } => {
                    publisher.publish(&mut inner, DownloadState::Verifying);
                }
                DownloadEvent::Done(_) => {
                    if let Some(scan) = scan {
                        publisher.outbox.push(Update::List(scan));
                    }
                    publisher.finish(&mut inner, DownloadState::Done);
                }
                DownloadEvent::Cancelled { .. } | DownloadEvent::Error(ModelsError::Cancelled) => {
                    publisher.finish(&mut inner, DownloadState::Cancelled);
                }
                DownloadEvent::Error(err) => {
                    publisher.finish(&mut inner, DownloadState::Failed(err.to_string()));
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_return_the_same_signal_on_every_call() {
        let tracker = DownloadTracker::new();
        let (state, progress) = (tracker.state(), tracker.progress());
        for _ in 0..1000 {
            assert!(tracker.state() == state);
            assert!(tracker.progress() == progress);
        }
    }
}
