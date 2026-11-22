//! Resumable, verified downloads into the [`ModelStore`] on a plugin-owned thread.
//!
//! [`Downloader::start`] claims every target file of a [`DownloadRequest`], spawns one
//! `frust-models-dl` thread and returns at once. Per file the thread skips a finished file whose
//! manifest already records the expected SHA-256, otherwise hashes any `.part` left by an
//! earlier run and asks for the rest with `Range`; a `206` appends, a `200` restarts from zero.
//! The body is hashed as it is written, the size and then the SHA-256 are checked at the end,
//! and the part is promoted with [`ModelStore::finalize_file`]. A cancel keeps the part for a
//! later resume; a size or digest mismatch deletes it.
//!
//! # Sink contract
//!
//! The sink runs on the download thread, never the caller's and never the UI thread. Every
//! event of one download arrives in order, and exactly one terminal event ([`DownloadEvent::Done`],
//! [`DownloadEvent::Cancelled`] or [`DownloadEvent::Error`]) ends it. Keep the sink short: hand
//! the event to the UI thread (a signal or channel) and return, because the transfer waits
//! while it runs.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use ureq::Body;
use ureq::http::Response;

use crate::error::{HubError, ModelsError};
use crate::hub::{HubClient, check_status, check_url, header, refuse_ui_thread};
use crate::id::ModelId;
use crate::store::{LocalModel, ModelManifest, ModelSource, ModelStore};

const CHUNK: usize = 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const PROGRESS_BYTES: u64 = 1024 * 1024;

/// Final paths of every file a running download owns.
static ACTIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Receives the events of one download; see the module's sink contract.
pub type DownloadSink = Arc<dyn Fn(DownloadEvent) + Send + Sync>;

/// One file to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    /// Where the file is stored.
    pub id: ModelId,
    /// Where it is fetched from: `https://`, or `http://` on a loopback address. Redirects
    /// must stay on `https://` (or loopback to loopback) and are followed at most 5 times.
    pub url: String,
    /// Recorded in the manifest.
    pub source: ModelSource,
    /// Expected size in bytes, when known.
    pub size: Option<u64>,
    /// Expected lowercase hex SHA-256, when known.
    pub sha256: Option<String>,
}

/// A set of files fetched as one download, for example a model and its tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// The files, fetched in order.
    pub items: Vec<DownloadItem>,
    /// Licence identifier recorded in every manifest.
    pub licence: Option<String>,
    /// Whether the expected digests come from the publisher. A manifest is marked verified only
    /// when this is set and its file had a digest to check.
    pub verified: bool,
    /// Upper bound on the bundle's total size in bytes.
    pub max_bytes: Option<u64>,
}

impl DownloadRequest {
    /// A single-file download of any URL. Only `https://` is accepted, or `http://` when the
    /// host is a loopback address; anything else is [`HubError::InsecureUrl`].
    pub fn from_url(
        url: impl Into<String>,
        id: ModelId,
        sha256: Option<String>,
        size: Option<u64>,
    ) -> Result<Self, ModelsError> {
        let url = url.into();
        check_url(&url)?;
        let verified = sha256.is_some();
        Ok(Self {
            items: vec![DownloadItem {
                id,
                source: ModelSource::Url { url: url.clone() },
                url,
                size,
                sha256,
            }],
            licence: None,
            verified,
            max_bytes: None,
        })
    }
}

impl HubClient {
    /// A download of `paths` from `repo` at `revision`, with sizes and SHA-256 digests taken
    /// from the repository listing. A path missing from the listing is [`HubError::NotFound`].
    ///
    /// Blocks on the listing request and refuses the UI thread like [`HubClient::files`]; a
    /// component uses `DownloadTracker::start_hub` instead.
    pub fn request_for(
        &self,
        repo: &str,
        revision: &str,
        paths: &[&str],
    ) -> Result<DownloadRequest, ModelsError> {
        refuse_ui_thread()?;
        let files = self.files(repo, revision)?;
        let mut items = Vec::with_capacity(paths.len());
        for path in paths {
            let file = files
                .iter()
                .find(|f| f.path == *path)
                .ok_or(HubError::NotFound)?;
            items.push(DownloadItem {
                id: ModelId::hub(repo, *path)?,
                url: self.resolve_url(repo, revision, path),
                source: ModelSource::HuggingFace {
                    repo: repo.to_owned(),
                    revision: revision.to_owned(),
                },
                size: Some(file.size),
                sha256: file.sha256.clone(),
            });
        }
        Ok(DownloadRequest {
            items,
            licence: None,
            verified: true,
            max_bytes: None,
        })
    }

    /// [`request_for`](Self::request_for) at the `main` revision. Blocks the same way.
    pub fn request_for_main(
        &self,
        repo: &str,
        paths: &[&str],
    ) -> Result<DownloadRequest, ModelsError> {
        refuse_ui_thread()?;
        self.request_for(repo, "main", paths)
    }
}

/// What a download reports to its sink. Byte counts cover the whole bundle.
#[derive(Debug)]
#[non_exhaustive]
pub enum DownloadEvent {
    /// The thread started. `total` is known when every file's size is; `resumed_from` counts
    /// bytes already on disk from finished files and partial downloads.
    Started {
        /// Bundle size in bytes, when known.
        total: Option<u64>,
        /// Bytes already present.
        resumed_from: u64,
        /// Number of files.
        files: usize,
    },
    /// Bytes on disk so far. Sent at most every 100 ms or 1 MiB, and when a file completes.
    Progress {
        /// Bytes on disk.
        bytes: u64,
        /// Bundle size in bytes, when known.
        total: Option<u64>,
        /// Average transfer rate of this run.
        bytes_per_sec: u64,
        /// Index of the file being fetched.
        file_index: usize,
    },
    /// A file was received and is being checked.
    Verifying {
        /// Index of the file.
        file_index: usize,
    },
    /// Every file is stored, in request order.
    Done(Vec<LocalModel>),
    /// The download was cancelled. `bytes_kept` is what stays on disk for a resume, finished
    /// files included.
    Cancelled {
        /// Bytes kept on disk.
        bytes_kept: u64,
    },
    /// The download failed. Files finished before the failure stay stored.
    Error(ModelsError),
}

/// Starts downloads.
#[derive(Debug)]
pub struct Downloader;

impl Downloader {
    /// Starts `request` on a new `frust-models-dl` thread and returns at once.
    ///
    /// Fails with [`ModelsError::Busy`] when another download already targets one of its files
    /// (or the request names a file twice), and with [`HubError::InsecureUrl`] for a URL that is
    /// neither `https://` nor loopback `http://`.
    pub fn start(
        client: &HubClient,
        store: &ModelStore,
        request: DownloadRequest,
        sink: DownloadSink,
    ) -> Result<DownloadHandle, ModelsError> {
        for item in &request.items {
            check_url(&item.url)?;
        }
        let claim = Claim::acquire(request.items.iter().map(|i| store.path_for(&i.id)))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let mut request = request;
        for item in &mut request.items {
            item.sha256 = item.sha256.take().map(|s| s.trim().to_ascii_lowercase());
        }
        let job = Job {
            client: client.clone(),
            store: store.clone(),
            request,
            sink,
            cancel: cancel.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("frust-models-dl".into())
            .spawn(move || job.run(claim))?;
        Ok(DownloadHandle { cancel, thread })
    }
}

/// A running download.
///
/// Dropping the handle does not stop the download; call [`cancel`](Self::cancel) for that.
pub struct DownloadHandle {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<Result<Vec<LocalModel>, ModelsError>>,
}

impl std::fmt::Debug for DownloadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadHandle")
            .field("finished", &self.thread.is_finished())
            .finish_non_exhaustive()
    }
}

impl DownloadHandle {
    /// Asks the download to stop at the next chunk. Partial files are kept.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether the download thread has exited.
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Blocks until the download ends. A cancelled download returns
    /// [`ModelsError::Cancelled`].
    ///
    /// On iOS and macOS a call on the main thread fails with [`ModelsError::UiThread`] at once;
    /// the download keeps running, as it does whenever the handle is dropped.
    pub fn wait(self) -> Result<Vec<LocalModel>, ModelsError> {
        refuse_ui_thread()?;
        self.thread
            .join()
            .unwrap_or_else(|_| Err(ModelsError::Storage("download thread panicked".into())))
    }
}

/// Exclusive ownership of the final paths of one download.
struct Claim(Vec<PathBuf>);

impl Claim {
    fn acquire(paths: impl Iterator<Item = PathBuf>) -> Result<Self, ModelsError> {
        let paths: Vec<PathBuf> = paths.collect();
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        for (i, path) in paths.iter().enumerate() {
            if active.contains(path) || paths[..i].contains(path) {
                return Err(ModelsError::Busy);
            }
        }
        active.extend(paths.iter().cloned());
        Ok(Self(paths))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        active.retain(|p| !self.0.contains(p));
    }
}

struct Progress {
    bytes: u64,
    totals: Vec<Option<u64>>,
    transferred: u64,
    started: Instant,
    last_emit: Instant,
    last_bytes: u64,
}

impl Progress {
    fn total(&self) -> Option<u64> {
        self.totals.iter().copied().sum()
    }

    fn known_total(&self) -> u64 {
        self.totals.iter().flatten().sum()
    }
}

struct Job {
    client: HubClient,
    store: ModelStore,
    request: DownloadRequest,
    sink: DownloadSink,
    cancel: Arc<AtomicBool>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// Parses `Content-Range: bytes <start>-<end>/<total>` or `bytes */<total>`.
fn content_range(value: &str) -> (Option<u64>, Option<u64>) {
    let Some(rest) = value.trim().strip_prefix("bytes ") else {
        return (None, None);
    };
    let Some((range, total)) = rest.split_once('/') else {
        return (None, None);
    };
    let start = range
        .split_once('-')
        .and_then(|(s, _)| s.trim().parse().ok());
    (start, total.trim().parse().ok())
}

/// Errors after which the partial file cannot be resumed.
fn discards_part(err: &ModelsError) -> bool {
    matches!(
        err,
        ModelsError::Hub(
            HubError::Integrity { .. } | HubError::SizeMismatch { .. } | HubError::TooLarge { .. }
        )
    )
}

impl Job {
    fn run(self, claim: Claim) -> Result<Vec<LocalModel>, ModelsError> {
        let started = Instant::now();
        let mut progress = Progress {
            bytes: 0,
            totals: vec![None; self.request.items.len()],
            transferred: 0,
            started,
            last_emit: started,
            last_bytes: 0,
        };
        let result = self.download_all(&mut progress);
        drop(claim);
        let event = match &result {
            Ok(models) => DownloadEvent::Done(models.clone()),
            Err(ModelsError::Cancelled) => DownloadEvent::Cancelled {
                bytes_kept: progress.bytes,
            },
            Err(err) => DownloadEvent::Error(err.duplicate()),
        };
        (self.sink)(event);
        result
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The stored model for `item` when it can be reused as is.
    fn already_stored(&self, item: &DownloadItem) -> Option<LocalModel> {
        let model = self.store.get(&item.id)?;
        let manifest = model.manifest.as_ref()?;
        let size_ok = item.size.is_none_or(|s| s == model.size_bytes);
        let same = match &item.sha256 {
            Some(expected) => manifest.sha256.as_deref() == Some(expected.as_str()),
            None => manifest.source == item.source,
        };
        (size_ok && same).then_some(model)
    }

    /// Length of the part file worth resuming: none when it is longer than the expected size.
    fn usable_part_len(&self, item: &DownloadItem) -> u64 {
        let len = fs::metadata(self.store.part_path_for(&item.id)).map_or(0, |m| m.len());
        if item.size.is_some_and(|size| len > size) {
            0
        } else {
            len
        }
    }

    fn download_all(&self, p: &mut Progress) -> Result<Vec<LocalModel>, ModelsError> {
        let items = &self.request.items;
        let mut reused = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let stored = self.already_stored(item);
            match &stored {
                Some(model) => {
                    p.bytes += model.size_bytes;
                    p.totals[index] = Some(model.size_bytes);
                }
                None => {
                    p.bytes += self.usable_part_len(item);
                    p.totals[index] = item.size;
                }
            }
            reused.push(stored);
        }
        p.last_bytes = p.bytes;
        (self.sink)(DownloadEvent::Started {
            total: p.total(),
            resumed_from: p.bytes,
            files: items.len(),
        });
        self.check_known_total(p)?;
        let mut models = Vec::with_capacity(items.len());
        for (index, (item, stored)) in items.iter().zip(reused).enumerate() {
            match stored {
                Some(model) => models.push(model),
                None => models.push(self.fetch_file(index, item, p)?),
            }
        }
        Ok(models)
    }

    fn check_known_total(&self, p: &Progress) -> Result<(), HubError> {
        match self.request.max_bytes {
            Some(limit) if p.known_total() > limit => Err(HubError::TooLarge {
                limit,
                actual: p.known_total(),
            }),
            _ => Ok(()),
        }
    }

    fn fetch_file(
        &self,
        index: usize,
        item: &DownloadItem,
        p: &mut Progress,
    ) -> Result<LocalModel, ModelsError> {
        let part = self.store.part_path_for(&item.id);
        let filled = self.fill_part(index, item, &part, p);
        let (len, actual) = match filled {
            Ok(done) => done,
            Err(err) => {
                if discards_part(&err) {
                    let _ = fs::remove_file(&part);
                }
                return Err(err);
            }
        };
        let mismatch = match (item.size, &item.sha256) {
            (Some(expected), _) if expected != len => Some(HubError::SizeMismatch {
                expected,
                actual: len,
            }),
            (_, Some(expected)) if *expected != actual => Some(HubError::Integrity {
                expected: expected.clone(),
                actual: actual.clone(),
            }),
            _ => None,
        };
        if let Some(err) = mismatch {
            let _ = fs::remove_file(&part);
            return Err(err.into());
        }
        let manifest = ModelManifest {
            source: item.source.clone(),
            size_bytes: len,
            verified: self.request.verified && item.sha256.is_some(),
            sha256: Some(actual),
            downloaded_at_unix: now_unix(),
            licence: self.request.licence.clone(),
        };
        self.store.finalize_file(&item.id, &manifest)
    }

    /// Brings the part file of `item` to its full length. Returns its length and SHA-256.
    fn fill_part(
        &self,
        index: usize,
        item: &DownloadItem,
        part: &Path,
        p: &mut Progress,
    ) -> Result<(u64, String), ModelsError> {
        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(part)?;
        let mut len = self.usable_part_len(item);
        if len == 0 {
            file.set_len(0)?;
        }
        let mut hasher = Sha256::new();
        self.hash_existing(&mut file, &mut hasher, len)?;
        file.seek(SeekFrom::Start(len))?;
        if !(len > 0 && item.size == Some(len)) {
            self.transfer(index, item, &mut file, &mut hasher, &mut len, p)?;
        }
        self.emit_progress(p, index, true);
        (self.sink)(DownloadEvent::Verifying { file_index: index });
        file.sync_all()?;
        Ok((len, to_hex(&hasher.finalize())))
    }

    fn hash_existing(
        &self,
        file: &mut File,
        hasher: &mut Sha256,
        len: u64,
    ) -> Result<(), ModelsError> {
        let mut buf = vec![0u8; CHUNK];
        let mut left = len;
        file.seek(SeekFrom::Start(0))?;
        while left > 0 {
            if self.is_cancelled() {
                return Err(ModelsError::Cancelled);
            }
            let want = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
            file.read_exact(&mut buf[..want])?;
            hasher.update(&buf[..want]);
            left -= want as u64;
        }
        Ok(())
    }

    fn restart(
        file: &mut File,
        hasher: &mut Sha256,
        len: &mut u64,
        p: &mut Progress,
    ) -> Result<(), ModelsError> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        *hasher = Sha256::new();
        p.bytes -= *len;
        *len = 0;
        Ok(())
    }

    fn transfer(
        &self,
        index: usize,
        item: &DownloadItem,
        file: &mut File,
        hasher: &mut Sha256,
        len: &mut u64,
        p: &mut Progress,
    ) -> Result<(), ModelsError> {
        for attempt in 1..=2 {
            let range_from = (*len > 0).then_some(*len);
            let mut response = self.client.get(&item.url, &[], range_from)?;
            let code = response.status().as_u16();
            let range = header(&response, "content-range").map(|v| content_range(&v));
            let total = match (code, range_from) {
                (206, Some(from)) => {
                    let (start, total) = range.unwrap_or((None, None));
                    if start != Some(from) {
                        if attempt == 2 {
                            return Err(unexpected(code, "content range does not match"));
                        }
                        Self::restart(file, hasher, len, p)?;
                        continue;
                    }
                    total
                }
                (200, _) => {
                    if *len > 0 {
                        Self::restart(file, hasher, len, p)?;
                    }
                    header(&response, "content-length").and_then(|v| v.trim().parse().ok())
                }
                (416, Some(from)) => {
                    if range.and_then(|(_, total)| total) == Some(from) {
                        p.totals[index] = Some(from);
                        return Ok(());
                    }
                    if attempt == 2 {
                        return Err(unexpected(code, "range not satisfiable"));
                    }
                    Self::restart(file, hasher, len, p)?;
                    continue;
                }
                _ => {
                    check_status(&mut response)?;
                    return Err(unexpected(code, "unexpected response"));
                }
            };
            if let Some(total) = total {
                if let Some(expected) = item.size
                    && expected != total
                {
                    return Err(HubError::SizeMismatch {
                        expected,
                        actual: total,
                    }
                    .into());
                }
                p.totals[index] = Some(total);
                self.check_known_total(p)?;
            }
            return self.copy_body(index, response, file, hasher, len, p);
        }
        Err(unexpected(0, "no usable response"))
    }

    fn copy_body(
        &self,
        index: usize,
        response: Response<Body>,
        file: &mut File,
        hasher: &mut Sha256,
        len: &mut u64,
        p: &mut Progress,
    ) -> Result<(), ModelsError> {
        let mut reader = response.into_body().into_reader();
        let mut buf = vec![0u8; CHUNK];
        loop {
            if self.is_cancelled() {
                file.flush()?;
                return Err(ModelsError::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(HubError::Transport(err.to_string()).into()),
            };
            let after = p.bytes + n as u64;
            if let Some(limit) = self.request.max_bytes
                && after > limit
            {
                return Err(HubError::TooLarge {
                    limit,
                    actual: after,
                }
                .into());
            }
            file.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            *len += n as u64;
            p.bytes = after;
            p.transferred += n as u64;
            self.emit_progress(p, index, false);
        }
    }

    fn emit_progress(&self, p: &mut Progress, index: usize, file_done: bool) {
        let now = Instant::now();
        let due = now.duration_since(p.last_emit) >= PROGRESS_INTERVAL
            || p.bytes.saturating_sub(p.last_bytes) >= PROGRESS_BYTES;
        if !(due || (file_done && p.bytes != p.last_bytes)) {
            return;
        }
        let elapsed = now.duration_since(p.started).as_secs_f64();
        let bytes_per_sec = if elapsed > 0.0 {
            (p.transferred as f64 / elapsed) as u64
        } else {
            0
        };
        p.last_emit = now;
        p.last_bytes = p.bytes;
        (self.sink)(DownloadEvent::Progress {
            bytes: p.bytes,
            total: p.total(),
            bytes_per_sec,
            file_index: index,
        });
    }
}

fn unexpected(code: u16, why: &str) -> ModelsError {
    HubError::Status {
        code,
        body_excerpt: why.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_forms() {
        assert_eq!(content_range("bytes 10-99/100"), (Some(10), Some(100)));
        assert_eq!(content_range("bytes */100"), (None, Some(100)));
        assert_eq!(content_range("bytes 0-9/*"), (Some(0), None));
        assert_eq!(content_range("items 0-1/2"), (None, None));
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(to_hex(&[0x00, 0xab, 0x7f]), "00ab7f");
    }

    #[test]
    fn url_policy() {
        assert!(check_url("https://example.com/x").is_ok());
        assert!(check_url("http://127.0.0.1:9/x").is_ok());
        assert!(check_url("http://[::1]:9/x").is_ok());
        assert_eq!(
            check_url("http://example.com/x"),
            Err(HubError::InsecureUrl)
        );
        assert_eq!(check_url("file:///etc/passwd"), Err(HubError::InsecureUrl));
    }
}
