//! Vision tasks from pixels to a result in one call: a subject mask
//! ([`segment_subject`](VisionModels::segment_subject)), face boxes with landmarks
//! ([`detect_faces`](VisionModels::detect_faces)) and a relative depth map
//! ([`estimate_depth`](VisionModels::estimate_depth)).
//!
//! [`VisionModels`] finds each task's model in a [`ModelStore`] (the [`catalog`]
//! row for the task, or a custom file given with [`with_model`](VisionModels::with_model)), loads
//! it through its own [`SessionCache`] and checks the model's inputs and outputs fit the task
//! before running it. A model that is not in the store is [`OnnxError::ModelMissing`]; nothing
//! is ever substituted for a missing or failing model.
//!
//! Every task function is **blocking**: it may load a model and then runs it on the calling
//! thread. Call it from a worker thread (in a frust app, inside `frust::spawn_blocking`), never
//! from the UI thread.

mod depth;
mod faces;
mod segment;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use frust_models::{ModelId, ModelStore};

use crate::catalog::{self, VisionModel, VisionTask};
use crate::image::{ChannelOrder, Normalization, Plane, RgbaView, to_nchw_f32};
use crate::{
    DType, OnnxError, Provider, Session, SessionCache, SessionOptions, Tensor, normalize_plane,
};

pub use depth::DepthConvention;
pub use faces::{
    DEFAULT_MIN_SCORE, Face, NMS_IOU, TOP_K, YUNET_STRIDES, YunetLevel, decode_yunet, map_to_image,
    nms,
};

/// Every task, in catalog order.
const TASKS: [VisionTask; 3] = [
    VisionTask::Segmentation,
    VisionTask::FaceDetection,
    VisionTask::Depth,
];

/// One completed run of a vision task: the provider that ran it and how long it took.
///
/// The reactive handle reports one per task (`api::VisionHandle::last_run`, behind the
/// `frust-api` feature), written only for a run whose result it published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisionRun {
    /// The execution provider of the session that ran it ([`Session::provider`]); `None` when
    /// the handle cannot tell, as when the session was dropped before the run ended.
    ///
    /// [`Session::provider`]: crate::Session::provider
    pub provider: Option<Provider>,
    /// Wall time from taking the request off the queue to its result: loading the model
    /// when [`loaded_model`](Self::loaded_model) is set, the pixel conversion, the inference and
    /// the decode. Time spent waiting in the queue is not included.
    pub duration: Duration,
    /// Whether the session was loaded during this run (the task's first run, or the first after
    /// its session was evicted or the cache cleared), so [`duration`](Self::duration) includes
    /// the load. Read from the session cache's load count, so a load another thread makes on
    /// the same [`VisionModels`] meanwhile counts too.
    pub loaded_model: bool,
}

/// The models behind the vision tasks: where they are stored, how their sessions are loaded,
/// and the sessions already loaded.
///
/// `VisionModels` is `Send + Sync`; share one behind an `Arc`. Its cache holds at most
/// [`SessionCache::DEFAULT_MAX_SESSIONS`] sessions, one per task: call
/// [`cache().clear()`](SessionCache::clear) when the platform reports memory pressure.
#[derive(Debug)]
pub struct VisionModels {
    cache: SessionCache,
    store: ModelStore,
    options: SessionOptions,
    custom: HashMap<VisionTask, ModelId>,
}

/// A task's session with the input it is fed through.
struct Loaded {
    session: Arc<Session>,
    input: String,
    width: u32,
    height: u32,
}

impl VisionModels {
    /// Models found in `store`, loaded with `options`. Nothing is read until a task runs.
    pub fn new(store: ModelStore, options: SessionOptions) -> Self {
        Self {
            cache: SessionCache::default(),
            store,
            options,
            custom: HashMap::new(),
        }
    }

    /// Uses the store file `id` for `task` instead of the catalog model.
    ///
    /// The file must take and return what the catalog model does (one `[1, 3, H, W]` f32 input;
    /// for faces, the twelve YuNet outputs); that is checked when the model is first loaded, and
    /// a model that does not fit is [`OnnxError::Unsupported`]. Static input sides are used as
    /// declared; dynamic ones take the catalog model's size.
    #[must_use]
    pub fn with_model(mut self, task: VisionTask, id: ModelId) -> Self {
        self.custom.insert(task, id);
        self
    }

    /// The store the models are read from.
    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    /// The options every session is loaded with.
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// The loaded sessions.
    pub fn cache(&self) -> &SessionCache {
        &self.cache
    }

    /// Whether the model `task` uses is in the store.
    pub fn is_installed(&self, task: VisionTask) -> bool {
        self.model_id(task)
            .is_some_and(|id| self.store.get(&id).is_some())
    }

    /// The execution provider of `task`'s session when it is loaded, `None` when it is not (or
    /// its model is not in the store). Never loads the model; reads the store's directory.
    pub fn loaded_provider(&self, task: VisionTask) -> Option<Provider> {
        let local = self.store.get(&self.model_id(task)?)?;
        self.cache
            .peek(&local.path, &self.options)
            .map(|session| session.provider())
    }

    /// The catalog models still to download, in catalog order. A task given a custom model with
    /// [`with_model`](Self::with_model) is not listed: the app installs that file itself.
    pub fn missing(&self) -> Vec<&'static VisionModel> {
        TASKS
            .iter()
            .filter(|task| !self.custom.contains_key(task))
            .filter_map(|&task| catalog::for_task(task).next())
            .filter(|model| self.store.get(&(model.id)()).is_none())
            .collect()
    }

    fn model_id(&self, task: VisionTask) -> Option<ModelId> {
        match self.custom.get(&task) {
            Some(id) => Some(id.clone()),
            None => catalog::for_task(task).next().map(|model| (model.id)()),
        }
    }

    /// The session for `task`, loaded on first use and checked against the task: one image
    /// input, and outputs `outputs_fit` accepts.
    fn load(
        &self,
        task: VisionTask,
        outputs_fit: fn(&Session) -> Result<(), String>,
    ) -> Result<Loaded, OnnxError> {
        let default = catalog::for_task(task)
            .next()
            .ok_or_else(|| OnnxError::Unsupported(format!("the catalog has no {task:?} model")))?;
        let (id, key) = match self.custom.get(&task) {
            Some(id) => (id.clone(), id.to_string()),
            None => ((default.id)(), default.key.to_owned()),
        };
        let Some(local) = self.store.get(&id) else {
            return Err(OnnxError::ModelMissing { key });
        };
        let session = self.cache.get_or_load(&local.path, &self.options)?;
        let fit = image_input(&session, default.input)
            .and_then(|input| outputs_fit(&session).map(|()| input));
        let (input, width, height) = fit.map_err(|reason| {
            self.cache.evict(&local.path);
            OnnxError::Unsupported(format!("`{key}` is not a {task:?} model: {reason}"))
        })?;
        Ok(Loaded {
            session,
            input,
            width,
            height,
        })
    }

    /// Runs `task` on `image` resized to the model's input with ImageNet statistics (RGB), and
    /// returns its first output's first plane min-max normalised to 0..1 at the image's size.
    fn normalised_plane(&self, task: VisionTask, image: RgbaView<'_>) -> Result<Plane, OnnxError> {
        let model = self.load(task, first_output_is_a_plane)?;
        let input = to_nchw_f32(
            image,
            model.width,
            model.height,
            Normalization::IMAGENET,
            ChannelOrder::Rgb,
        )?;
        let outputs = model
            .session
            .run_blocking(&[(model.input.as_str(), input)])?;
        let (_, first) = outputs
            .first()
            .ok_or_else(|| OnnxError::Run("the model returned no output".to_owned()))?;
        let (mut data, width, height) = first_plane(first)?;
        normalize_plane(&mut data);
        let (out_w, out_h) = (image.width() as usize, image.height() as usize);
        let resized = crate::resize_plane(&data, width, height, out_w, out_h);
        Plane::new(out_w, out_h, resized)
    }
}

/// The model's single image input: its name and the side lengths to feed it, a dynamic side
/// taking `default`'s.
fn image_input(session: &Session, default: (u32, u32)) -> Result<(String, u32, u32), String> {
    let [input] = session.inputs() else {
        return Err(format!(
            "it takes {} inputs, not one image",
            session.inputs().len()
        ));
    };
    let fits = |dim: Option<usize>, want: usize| dim.is_none_or(|dim| dim == want);
    let side = |dim: Option<usize>, default: u32| match dim {
        None => Ok(default),
        Some(0) => Err(format!("input `{}` has a zero side", input.name)),
        Some(dim) => u32::try_from(dim).map_err(|_| format!("input `{}` is too large", input.name)),
    };
    match input.shape.as_slice() {
        [batch, channels, height, width]
            if input.dtype == DType::F32 && fits(*batch, 1) && fits(*channels, 3) =>
        {
            Ok((
                input.name.clone(),
                side(*width, default.0)?,
                side(*height, default.1)?,
            ))
        }
        shape => Err(format!(
            "input `{}` is {} {shape:?}, not f32 [1, 3, H, W]",
            input.name, input.dtype
        )),
    }
}

fn first_output_is_a_plane(session: &Session) -> Result<(), String> {
    match session.outputs().first() {
        Some(output) if output.dtype == DType::F32 && output.shape.len() >= 2 => Ok(()),
        Some(output) => Err(format!(
            "output `{}` is {} of rank {}, not an f32 plane",
            output.name,
            output.dtype,
            output.shape.len()
        )),
        None => Err("it has no output".to_owned()),
    }
}

/// The first `H x W` plane of an f32 output shaped `[.., H, W]`, with its width and height.
fn first_plane(tensor: &Tensor) -> Result<(Vec<f32>, usize, usize), OnnxError> {
    let bad = || {
        OnnxError::Run(format!(
            "the output is {} {:?}, not an f32 plane",
            tensor.dtype(),
            tensor.shape()
        ))
    };
    let (&[.., height, width], Some(data)) = (tensor.shape(), tensor.as_f32()) else {
        return Err(bad());
    };
    let len = width
        .checked_mul(height)
        .filter(|&len| len > 0 && len <= data.len());
    let len = len.ok_or_else(bad)?;
    Ok((data[..len].to_vec(), width, height))
}
