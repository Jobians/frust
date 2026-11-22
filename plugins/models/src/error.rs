//! Error type shared by every model-store operation.

/// Failure of a model-store operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelsError {
    /// The storage location is unavailable or its contents are unusable.
    #[error("model storage error: {0}")]
    Storage(String),
    /// A model identifier broke the naming rules.
    #[error("invalid model id: {0}")]
    InvalidModelId(String),
    /// A filesystem operation failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The model is in use by another operation.
    #[error("model is busy")]
    Busy,
    /// The operation is not supported on this target or build.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
