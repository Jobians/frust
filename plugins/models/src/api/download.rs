//! [`DownloadTracker`]: one download's progress as signals.

use std::sync::{Arc, Mutex, MutexGuard};

use frust::{ReadSignal, RwSignal, Set, spawn_blocking};

use super::ModelList;
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
/// The same state is never published twice in a row. A component reading the signal may still
/// miss short states between two frames; [`DownloadTracker::on_state`] sees every one.
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
/// `start_hub` builds its request on `frust::spawn_blocking`. The code that runs on the download
/// thread only writes signals: it never reads one, never blocks on the UI and never calls back
/// into the downloader. Signal writes go through a short lock the tracker owns, which keeps the
/// order [`DownloadState`] documents.
///
/// Dropping the tracker does not cancel the download. The thread keeps running to its end and
/// its signal writes are harmless; call [`cancel`](Self::cancel) first to stop it.
///
/// Link a [`ModelList`] with [`with_list`](Self::with_list) and a finished download rescans the
/// store on the download thread before the state becomes [`DownloadState::Done`], so a
/// component that sees `Done` also sees the new models.
pub struct DownloadTracker {
    publisher: Publisher,
    list: Option<ModelList>,
}

/// The signals and the lock every state write goes through.
#[derive(Clone)]
struct Publisher {
    state: RwSignal<DownloadState>,
    progress: RwSignal<DownloadProgress>,
    /// Read handles made once beside the writable signals, so an accessor allocates nothing.
    state_read: ReadSignal<DownloadState>,
    progress_read: ReadSignal<DownloadProgress>,
    observer: Option<StateObserver>,
    inner: Arc<Mutex<Inner>>,
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
    /// The last state written, kept here so the download thread never reads the signal.
    published: DownloadState,
}

impl Publisher {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, inner: &mut Inner, state: DownloadState) {
        if inner.published == state {
            return;
        }
        self.state.set(state.clone());
        if let Some(observer) = &self.observer {
            observer(&state);
        }
        inner.published = state;
    }

    /// Marks a download of `files` files as started: fresh progress and `Running`.
    fn begin(&self, inner: &mut Inner, files: usize) {
        inner.active = true;
        inner.cancel_requested = false;
        inner.handle = None;
        inner.files = files;
        self.progress.set(DownloadProgress {
            files,
            ..DownloadProgress::default()
        });
        self.publish(inner, DownloadState::Running);
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
                inner: Arc::default(),
            },
            list: None,
        }
    }

    /// Links `list` so a finished download refreshes it. Use the same store for both.
    pub fn with_list(mut self, list: ModelList) -> Self {
        self.list = Some(list);
        self
    }

    /// Calls `observer` with every state the tracker publishes, in order, right after the
    /// signal is written, for example to log a download. It runs on whichever thread
    /// published the state, with the tracker's lock held: keep it short and never call back
    /// into the tracker from it.
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
    /// reactive runtime a running app has. Fails with [`ModelsError::Busy`] while an earlier
    /// download of this tracker has not ended, leaving the signals as they were.
    pub fn start_hub(
        &self,
        client: &HubClient,
        store: &ModelStore,
        repo: &str,
        revision: &str,
        paths: &[&str],
    ) -> Result<(), ModelsError> {
        {
            let mut inner = self.publisher.lock();
            if inner.active {
                return Err(ModelsError::Busy);
            }
            self.publisher.begin(&mut inner, paths.len());
        }
        let publisher = self.publisher.clone();
        let sink = self.sink();
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
    /// Fails with [`ModelsError::Busy`] while an earlier download of this tracker has not
    /// ended, and with whatever [`Downloader::start`] reports (for example a file another
    /// download already owns). On failure the signals are left as they were.
    pub fn start(
        &self,
        client: &HubClient,
        store: &ModelStore,
        request: DownloadRequest,
    ) -> Result<(), ModelsError> {
        let mut inner = self.publisher.lock();
        if inner.active {
            return Err(ModelsError::Busy);
        }
        let files = request.items.len();
        // The download thread's first event waits for the lock held here, so `Running` is
        // published before anything the thread reports.
        let handle = Downloader::start(client, store, request, self.sink())?;
        self.publisher.begin(&mut inner, files);
        inner.handle = Some(handle);
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

    fn sink(&self) -> DownloadSink {
        let publisher = self.publisher.clone();
        let list = self.list.clone();
        Arc::new(move |event| {
            // The rescan touches the disk, so it runs before the lock is taken.
            if let (DownloadEvent::Done(_), Some(list)) = (&event, &list) {
                list.refresh_now();
            }
            let mut inner = publisher.lock();
            match event {
                DownloadEvent::Started { total, files, .. } => {
                    inner.files = files;
                    publisher.progress.set(DownloadProgress {
                        total,
                        files,
                        ..DownloadProgress::default()
                    });
                    publisher.publish(&mut inner, DownloadState::Running);
                }
                DownloadEvent::Progress {
                    bytes,
                    total,
                    bytes_per_sec,
                    file_index,
                } => {
                    publisher.progress.set(DownloadProgress {
                        bytes,
                        total,
                        bytes_per_sec,
                        file_index,
                        files: inner.files,
                    });
                    publisher.publish(&mut inner, DownloadState::Running);
                }
                DownloadEvent::Verifying { .. } => {
                    publisher.publish(&mut inner, DownloadState::Verifying);
                }
                DownloadEvent::Done(_) => publisher.finish(&mut inner, DownloadState::Done),
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
