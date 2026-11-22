//! Reactive handles over the store and the downloader, behind the default-on `frust-api`
//! feature.
//!
//! [`ModelList`] keeps a signal of the stored models current; [`DownloadTracker`] turns one
//! download's events into a state signal and a progress signal. Both follow the rule a
//! component's `build` reads and handlers write: the code that runs on the download thread
//! only ever writes signals. It never reads one, never blocks on the UI and never calls back
//! into the downloader. These two modules are the only place in the crate that names `frust`,
//! so `--no-default-features` drops the facade from the dependency graph.

#[cfg(feature = "hub")]
mod download;
mod list;

#[cfg(feature = "hub")]
pub use download::{DownloadProgress, DownloadState, DownloadTracker};
pub use list::ModelList;
