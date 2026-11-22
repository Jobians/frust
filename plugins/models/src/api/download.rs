//! [`DownloadTracker`]: one download's progress as signals.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use frust::{GetUntracked, ReadSignal, RwSignal, Set};

use super::ModelList;
use crate::download::{DownloadEvent, DownloadHandle, DownloadRequest, DownloadSink, Downloader};
use crate::error::ModelsError;
use crate::hub::HubClient;
use crate::store::ModelStore;

/// Where a tracked download is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DownloadState {
    /// Nothing has been started, or the tracker was never used.
    #[default]
    Idle,
    /// Bytes are being fetched.
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

/// Runs one download at a time and publishes its progress through signals.
///
/// [`state`](Self::state) and [`progress`](Self::progress) are read-only signals for a
/// component's `build`. [`start`](Self::start) and [`cancel`](Self::cancel) are called from
/// event handlers. The event sink that runs on the download thread only writes signals: it
/// never reads one, never blocks and never calls back into the downloader.
///
/// Dropping the tracker does not cancel the download. The thread keeps running to its end and
/// its signal writes are harmless; call [`cancel`](Self::cancel) first to stop it.
///
/// Link a [`ModelList`] with [`with_list`](Self::with_list) and a finished download rescans the
/// store on the download thread before the state becomes [`DownloadState::Done`], so a
/// component that sees `Done` also sees the new models.
pub struct DownloadTracker {
    state: RwSignal<DownloadState>,
    progress: RwSignal<DownloadProgress>,
    list: Option<ModelList>,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// A download started by this tracker has not reached its terminal event.
    active: bool,
    handle: Option<DownloadHandle>,
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
        Self {
            state: RwSignal::new(DownloadState::Idle),
            progress: RwSignal::new(DownloadProgress::default()),
            list: None,
            inner: Arc::default(),
        }
    }

    /// Links `list` so a finished download refreshes it. Use the same store for both.
    pub fn with_list(mut self, list: ModelList) -> Self {
        self.list = Some(list);
        self
    }

    /// The download's state.
    pub fn state(&self) -> ReadSignal<DownloadState> {
        self.state.read_only()
    }

    /// The download's progress. It resets when a new download starts.
    pub fn progress(&self) -> ReadSignal<DownloadProgress> {
        self.progress.read_only()
    }

    /// Starts `request` and returns at once; the state becomes [`DownloadState::Running`].
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active {
            return Err(ModelsError::Busy);
        }
        let before = (self.state.get_untracked(), self.progress.get_untracked());
        self.progress.set(DownloadProgress {
            files: request.items.len(),
            ..DownloadProgress::default()
        });
        self.state.set(DownloadState::Running);

        let sink = self.sink();
        match Downloader::start(client, store, request, sink) {
            Ok(handle) => {
                inner.active = true;
                inner.handle = Some(handle);
                Ok(())
            }
            Err(err) => {
                self.state.set(before.0);
                self.progress.set(before.1);
                Err(err)
            }
        }
    }

    /// Asks the running download to stop at its next chunk. The state becomes
    /// [`DownloadState::Cancelled`] once it has. Does nothing when no download runs.
    pub fn cancel(&self) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active
            && let Some(handle) = &inner.handle
        {
            handle.cancel();
        }
    }

    fn sink(&self) -> DownloadSink {
        let state = self.state;
        let progress = self.progress;
        let list = self.list.clone();
        let inner = self.inner.clone();
        // Plain data the sink keeps for itself; it is not a signal.
        let files_seen = AtomicUsize::new(0);
        Arc::new(move |event| {
            let finish = |terminal: DownloadState| {
                // Holding the lock across the write keeps a following `start` from being
                // overwritten by this download's final state.
                let mut inner = inner.lock().unwrap_or_else(|e| e.into_inner());
                state.set(terminal);
                inner.active = false;
            };
            match event {
                DownloadEvent::Started { total, files, .. } => {
                    files_seen.store(files, Ordering::Relaxed);
                    progress.set(DownloadProgress {
                        total,
                        files,
                        ..DownloadProgress::default()
                    });
                    state.set(DownloadState::Running);
                }
                DownloadEvent::Progress {
                    bytes,
                    total,
                    bytes_per_sec,
                    file_index,
                } => {
                    progress.set(DownloadProgress {
                        bytes,
                        total,
                        bytes_per_sec,
                        file_index,
                        files: files_seen.load(Ordering::Relaxed),
                    });
                    state.set(DownloadState::Running);
                }
                DownloadEvent::Verifying { .. } => state.set(DownloadState::Verifying),
                DownloadEvent::Done(_) => {
                    if let Some(list) = &list {
                        list.refresh_now();
                    }
                    finish(DownloadState::Done);
                }
                DownloadEvent::Cancelled { .. } => finish(DownloadState::Cancelled),
                DownloadEvent::Error(ModelsError::Cancelled) => finish(DownloadState::Cancelled),
                DownloadEvent::Error(err) => finish(DownloadState::Failed(err.to_string())),
            }
        })
    }
}
