//! On-device model storage for Frust apps.
//!
//! `frust-models` is the one place model files live. It lays files out under the app data
//! directory, keeps a sidecar manifest beside each, and lists, imports and removes them. It
//! does no inference and knows no model format. See the README for the on-disk layout.

mod apple;
mod error;
mod id;
mod store;

pub use error::ModelsError;
pub use id::ModelId;
pub use store::{LocalModel, ModelManifest, ModelSource, ModelStore};
