//! A short, verified list of vision models an app can download through `frust-models`.
//!
//! Every row pins its source (a release asset or a commit), byte size and SHA-256, and names its
//! licence. Only permissively licensed models are listed.

use frust_models::{DownloadItem, DownloadRequest, ModelId, ModelSource};

/// What a catalog model does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisionTask {
    /// Salient-subject segmentation (a soft mask).
    Segmentation,
    /// Face detection (boxes and landmarks).
    FaceDetection,
    /// Relative depth estimation (a depth map).
    Depth,
}

/// Where a catalog file is fetched from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    /// A direct HTTPS URL.
    Url(&'static str),
    /// A Hugging Face Hub file at a pinned revision.
    Hub {
        /// `author/repo`.
        repo: &'static str,
        /// A commit hash.
        revision: &'static str,
        /// File path inside the repository.
        path: &'static str,
    },
}

/// One catalog row.
#[derive(Debug, Clone, Copy)]
pub struct VisionModel {
    /// Stable short key, for example `u2netp`.
    pub key: &'static str,
    /// Human-readable name.
    pub display: &'static str,
    /// What the model does.
    pub task: VisionTask,
    /// The store identifier the file is kept under.
    pub id: fn() -> ModelId,
    /// Where the file comes from.
    pub source: CatalogSource,
    /// Exact file size in bytes.
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    /// SPDX licence identifier of the model.
    pub licence: &'static str,
    /// Input width and height the model expects.
    pub input: (u32, u32),
}

fn u2netp_id() -> ModelId {
    ModelId::new("rembg", "u2netp", "u2netp.onnx").expect("literal identifier is valid")
}

fn yunet_id() -> ModelId {
    ModelId::new("opencv", "yunet", "face_detection_yunet_2023mar.onnx")
        .expect("literal identifier is valid")
}

fn depth_id() -> ModelId {
    ModelId::hub(
        "onnx-community/depth-anything-v2-small",
        "onnx/model_quantized.onnx",
    )
    .expect("literal identifier is valid")
}

/// Every model in the catalog.
pub const VISION_CATALOG: &[VisionModel] = &[
    VisionModel {
        key: "u2netp",
        display: "U2-NetP",
        task: VisionTask::Segmentation,
        id: u2netp_id,
        source: CatalogSource::Url(
            "https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2netp.onnx",
        ),
        size_bytes: 4_574_861,
        sha256: "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8",
        licence: "Apache-2.0",
        input: (320, 320),
    },
    VisionModel {
        key: "yunet",
        display: "YuNet 2023mar",
        task: VisionTask::FaceDetection,
        id: yunet_id,
        source: CatalogSource::Url(
            "https://github.com/opencv/opencv_zoo/raw/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
        ),
        size_bytes: 232_589,
        sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
        licence: "MIT",
        input: (640, 640),
    },
    VisionModel {
        key: "depth-anything-v2-small",
        display: "Depth Anything V2 Small (quantized)",
        task: VisionTask::Depth,
        id: depth_id,
        source: CatalogSource::Hub {
            repo: "onnx-community/depth-anything-v2-small",
            revision: "4472b7362082ad9968fee890ca0f1e5aca36b93d",
            path: "onnx/model_quantized.onnx",
        },
        size_bytes: 27_258_801,
        sha256: "fcf51f1b230362b28690bb9d1809bf0431f29cad20534e3f589bd7285547f20d",
        licence: "Apache-2.0",
        input: (518, 518),
    },
];

/// The row with this key.
pub fn by_key(key: &str) -> Option<&'static VisionModel> {
    VISION_CATALOG.iter().find(|m| m.key == key)
}

/// Every row for `task`, in catalog order.
pub fn for_task(task: VisionTask) -> impl Iterator<Item = &'static VisionModel> {
    VISION_CATALOG.iter().filter(move |m| m.task == task)
}

impl VisionModel {
    /// A download of this model with its pinned size and SHA-256, marked verified and carrying
    /// the licence. Building it does not touch the network.
    pub fn download_request(&self) -> DownloadRequest {
        let (url, source) = match self.source {
            CatalogSource::Url(url) => (
                url.to_owned(),
                ModelSource::Url {
                    url: url.to_owned(),
                },
            ),
            CatalogSource::Hub {
                repo,
                revision,
                path,
            } => (
                format!("{}/{repo}/resolve/{revision}/{path}", frust_models::HUB_URL),
                ModelSource::HuggingFace {
                    repo: repo.to_owned(),
                    revision: revision.to_owned(),
                },
            ),
        };
        DownloadRequest {
            items: vec![DownloadItem {
                id: (self.id)(),
                url,
                source,
                size: Some(self.size_bytes),
                sha256: Some(self.sha256.to_owned()),
            }],
            licence: Some(self.licence.to_owned()),
            verified: true,
            max_bytes: Some(self.size_bytes),
        }
    }
}
