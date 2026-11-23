//! Relative depth estimation: [`VisionModels::estimate_depth`].

use super::VisionModels;
use crate::catalog::VisionTask;
use crate::{OnnxError, Plane, RgbaView};

/// Which end of a relative depth plane is near the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepthConvention {
    /// 1 is the nearest point, 0 the farthest. What [`VisionModels::estimate_depth`] returns.
    NearIsOne,
    /// 0 is the nearest point, 1 the farthest: a [`NearIsOne`](Self::NearIsOne) plane after
    /// [`Plane::inverted`].
    FarIsOne,
}

impl VisionModels {
    /// The convention of [`estimate_depth`](Self::estimate_depth)'s planes.
    pub const DEPTH_CONVENTION: DepthConvention = DepthConvention::NearIsOne;

    /// Relative depth: one sample per pixel of `image`, 0..1, in
    /// [`DepthConvention::NearIsOne`] (call [`Plane::inverted`] for far-is-one).
    ///
    /// The values are relative to this image only: the model predicts disparity up to an unknown
    /// scale and shift, which is min-max normalised here, so neither metres nor values from two
    /// images compare. The image is stretched to the model's input (518x518 for the catalog's
    /// Depth Anything V2 Small), RGB with ImageNet mean and standard deviation, and the first
    /// output is resized bilinearly back to the image.
    ///
    /// **Blocking**: loads the model on first use, then runs it on the calling thread. Fails with
    /// [`OnnxError::ModelMissing`] when the model is not in the store.
    pub fn estimate_depth(&self, image: RgbaView<'_>) -> Result<Plane, OnnxError> {
        self.normalised_plane(VisionTask::Depth, image)
    }
}

impl Plane {
    /// Every sample `v` replaced by `1 - v`: a 0..1 mask or depth plane flipped end for end
    /// ([`DepthConvention::NearIsOne`] to [`FarIsOne`](DepthConvention::FarIsOne) and back).
    #[must_use]
    pub fn inverted(mut self) -> Self {
        for v in &mut self.data {
            *v = 1.0 - *v;
        }
        self
    }
}
