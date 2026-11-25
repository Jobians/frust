//! Reactive handles over the store and the downloader, behind the default-on `frust-api`
//! feature.
//!
//! [`ModelList`] keeps a signal of the stored models current; [`DownloadTracker`] turns one
//! download's events into a state signal and a progress signal. Both follow the rule a
//! component's `build` reads and handlers write, and every signal write is made on the UI
//! thread: the download thread and the blocking pool hand their results to the UI thread, which
//! writes them in order on its next pump of the local task queue. The code on those threads
//! never reads a signal, never blocks on the UI and never calls back into the downloader. These
//! modules are the only place in the crate that names `frust`, so `--no-default-features` drops
//! the facade from the dependency graph.

#[cfg(feature = "hub")]
mod download;
mod list;
#[cfg(feature = "hub")]
mod publish;

#[cfg(feature = "hub")]
pub use download::{DownloadProgress, DownloadState, DownloadTracker};
pub use list::ModelList;
