//! Error types shared by every model-store and download operation.

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
    /// A Hub request or a download failed.
    #[cfg(feature = "hub")]
    #[error("hub error: {0}")]
    Hub(#[from] HubError),
    /// The download was cancelled; its partial files are kept for a later resume.
    #[cfg(feature = "hub")]
    #[error("download cancelled")]
    Cancelled,
    /// A call that blocks on the network or on a download was made on the platform UI thread
    /// and refused. Run it on `frust::spawn_blocking` or another worker thread.
    #[cfg(feature = "hub")]
    #[error("this call blocks and cannot run on the UI thread")]
    UiThread,
}

impl ModelsError {
    /// A copy of this error; an I/O error keeps only its kind and message.
    #[cfg(feature = "hub")]
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Storage(msg) => Self::Storage(msg.clone()),
            Self::InvalidModelId(msg) => Self::InvalidModelId(msg.clone()),
            Self::Io(err) => Self::Io(std::io::Error::new(err.kind(), err.to_string())),
            Self::Busy => Self::Busy,
            Self::Unsupported(msg) => Self::Unsupported(msg.clone()),
            Self::Hub(err) => Self::Hub(err.clone()),
            Self::Cancelled => Self::Cancelled,
            Self::UiThread => Self::UiThread,
        }
    }
}

/// Failure of a Hub request or a download.
#[cfg(feature = "hub")]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    /// HTTP 401; `gated` when the Hub answered `X-Error-Code: GatedRepo` (accept its terms).
    #[error("unauthorized{}", if *gated { " (gated repository: accept its terms and use a token)" } else { "" })]
    Unauthorized { gated: bool },
    /// HTTP 403.
    #[error("forbidden")]
    Forbidden,
    /// HTTP 429; `retry_after` is the `Retry-After` header when it holds seconds.
    #[error("rate limited")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    /// HTTP 404, or a requested file is not in the repository.
    #[error("not found")]
    NotFound,
    /// Any other unexpected HTTP status, with at most 256 bytes of the response body.
    #[error("unexpected status {code}: {body_excerpt}")]
    Status { code: u16, body_excerpt: String },
    /// The connection, TLS handshake or transfer failed.
    #[error("transport: {0}")]
    Transport(String),
    /// A Hub response was not the JSON shape expected.
    #[error("malformed response: {0}")]
    Json(String),
    /// The content's SHA-256 (`actual`, lowercase hex) is not the `expected` one.
    #[error("sha-256 mismatch: expected {expected}, got {actual}")]
    Integrity { expected: String, actual: String },
    /// The size received or announced by the server (`actual`) is not the `expected` one.
    #[error("size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    /// The bundle would reach `actual` bytes, past `DownloadRequest::max_bytes` (`limit`).
    #[error("download of {actual} bytes exceeds the limit of {limit}")]
    TooLarge { limit: u64, actual: u64 },
    /// The URL, a redirect target, or the base URL a token would travel to is not `https://`.
    /// Plain `http://` is accepted only when the first URL was on a loopback address and the
    /// target is too.
    #[error("only https urls are allowed")]
    InsecureUrl,
    /// A request was redirected more than 5 times.
    #[error("too many redirects")]
    TooManyRedirects,
    /// A listing's `Link: rel="next"` page is not on the scheme, host and port of the base URL.
    #[error("the next page of a listing is on another origin")]
    CrossOriginPage,
}
