//! The crate's one error type.

use std::path::{Path, PathBuf};

/// Every failure `frust-onnx` reports. Runtime messages are carried as text, so no ONNX Runtime
/// type is part of this API.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OnnxError {
    /// The process-wide ONNX Runtime environment could not be created (for a dynamically loaded
    /// runtime: the library was not found or is too old). Cached: every later call returns it.
    #[error("ONNX Runtime is unavailable: {0}")]
    Runtime(String),
    /// A model could not be loaded. `path` is `None` for a model given as bytes.
    #[error("failed to load the model {}: {reason}", model_source(.path.as_deref()))]
    Load {
        /// The model file, or `None` for [`Session::from_bytes`](crate::Session::from_bytes).
        path: Option<PathBuf>,
        /// The runtime's or the filesystem's explanation.
        reason: String,
    },
    /// A tensor or a run's inputs do not fit: a data length that does not match its shape, an
    /// unknown, missing or repeated input name, a wrong element type, or a wrong static dimension.
    /// Reported before the runtime is called.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The runtime failed while running the model.
    #[error("inference failed: {0}")]
    Run(String),
    /// The run was cancelled before it finished. A cancelled [`Session::run`](crate::Session::run)
    /// reports [`RunEvent::Cancelled`](crate::RunEvent::Cancelled) instead, and a blocking run
    /// never reports it.
    #[error("inference was cancelled")]
    Cancelled,
    /// The model or the target needs something this crate does not support, such as an output
    /// element type other than f32, u8 or i64, or a strict accelerator that cannot be used.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

fn model_source(path: Option<&Path>) -> String {
    match path {
        Some(path) => format!("`{}`", path.display()),
        None => "from memory".to_owned(),
    }
}
