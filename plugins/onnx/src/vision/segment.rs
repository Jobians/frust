//! Salient-subject segmentation: [`VisionModels::segment_subject`].

use super::VisionModels;
use crate::catalog::VisionTask;
use crate::{OnnxError, Plane, RgbaView};

impl VisionModels {
    /// A soft mask of the image's main subject: one sample per pixel of `image`, 0 (background)
    /// to 1 (subject).
    ///
    /// The image is stretched to the model's input (320x320 for the catalog's U2-NetP), RGB with
    /// ImageNet mean and standard deviation; the model's first output is min-max normalised to
    /// 0..1 and resized bilinearly back to the image. An image with no distinct subject still
    /// spans 0..1, so threshold with care.
    ///
    /// **Blocking**: loads the model on first use, then runs it on the calling thread. Fails with
    /// [`OnnxError::ModelMissing`] when the model is not in the store.
    pub fn segment_subject(&self, image: RgbaView<'_>) -> Result<Plane, OnnxError> {
        self.normalised_plane(VisionTask::Segmentation, image)
    }
}
