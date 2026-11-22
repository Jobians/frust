//! On-device model storage for Frust apps.
//!
//! `frust-models` is the one place model files live. It lays files out under the app data
//! directory, keeps a sidecar manifest beside each, and lists, imports and removes them. With
//! the default `hub` feature it also searches the Hugging Face Hub and downloads Hub files or
//! any HTTPS URL into the store, resumably and verified, on its own thread. It does no
//! inference and knows no model format. See the README for the on-disk layout.

mod apple;
#[cfg(feature = "hub")]
mod download;
mod error;
#[cfg(feature = "hub")]
mod hub;
mod id;
mod store;

#[cfg(feature = "hub")]
pub use download::{
    DownloadEvent, DownloadHandle, DownloadItem, DownloadRequest, DownloadSink, Downloader,
};
#[cfg(feature = "hub")]
pub use error::HubError;
pub use error::ModelsError;
#[cfg(feature = "hub")]
pub use hub::{Gated, HUB_URL, HubClient, HubFile, HubModel, HubSort, SearchOptions};
pub use id::ModelId;
pub use store::{LocalModel, ModelManifest, ModelSource, ModelStore};
