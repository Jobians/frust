//! ONNX model inference for Frust apps, over ONNX Runtime.
//!
//! A [`Session`] loads a model from a file or from bytes, reports its named inputs and outputs
//! ([`TensorInfo`]), and runs it on typed [`Tensor`]s, either blocking ([`Session::run_blocking`])
//! or on the session's own worker thread with a cancellable [`RunHandle`] ([`Session::run`]).
//! Inputs are checked against the model (names, element types, static dimensions) before the
//! runtime sees them. [`SessionOptions`] choose the [`Accelerator`] (CoreML on Apple targets by
//! default, the CPU elsewhere; an unusable one falls back to the CPU), the thread count, the
//! optimisation level and the memory behaviour. A [`SessionCache`] keeps a bounded set warm.
//!
//! [`image`] turns RGBA pixels into normalised input tensors and resizes result planes; the
//! default `catalog` feature adds [`catalog`], a verified list of downloadable vision models,
//! and the default `vision` feature adds [`vision`]: subject masks, face boxes and depth maps
//! from pixels in one blocking call each, through [`VisionModels`]. The default `frust-api`
//! feature adds [`api`]: `VisionHandle` runs those tasks on a worker thread and publishes their
//! results and its status as signals a component reads.
//!
//! One ONNX Runtime environment serves the whole process. It is created on the first session (or
//! [`runtime_info`] call); a failed creation is cached and returned to every later caller as
//! [`OnnxError::Runtime`].
//!
//! # Linking
//!
//! On Linux (glibc, x86_64/aarch64), Apple-silicon macOS, Windows (MSVC) and arm64 Android the
//! runtime is linked statically from the prebuilt archive `ort` downloads at build time; on Android
//! the app must also package the NDK's `libc++_shared.so`, which that archive needs. Every other
//! target, iOS and the other Android ABIs included, loads the runtime library at run time
//! ([`LinkKind::Dynamic`]): the library named by `ORT_DYLIB_PATH`, else the platform's
//! `onnxruntime` library on the loader path.
//!
//! # Example
//!
//! ```no_run
//! use frust_onnx::{Session, Tensor};
//!
//! let session = Session::from_file("add_f32.onnx")?;
//! let a = Tensor::from_f32([1, 3], vec![1.0, 2.0, 3.0])?;
//! let b = Tensor::from_f32([1, 3], vec![10.0, 20.0, 30.0])?;
//! let outputs = session.run_blocking(&[("a", a), ("b", b)])?;
//! assert_eq!(outputs[0].1.as_f32(), Some(&[11.0, 22.0, 33.0][..]));
//! # Ok::<(), frust_onnx::OnnxError>(())
//! ```
//!
//! `run_blocking` parks the calling thread for the whole inference: call it from a worker thread,
//! never from the UI thread, or use [`Session::run`].

#![forbid(unsafe_code)]

mod accelerator;
#[cfg(feature = "frust-api")]
pub mod api;
mod cache;
#[cfg(feature = "catalog")]
pub mod catalog;
mod error;
pub mod image;
mod runtime;
mod session;
mod tensor;
#[cfg(feature = "vision")]
pub mod vision;
mod worker;

pub use accelerator::{Accelerator, CoreMlUnits, Provider};
pub use cache::SessionCache;
pub use error::OnnxError;
pub use image::{
    ChannelOrder, Normalization, Plane, RgbaView, normalize_plane, resize_plane, to_nchw_f32,
};
pub use runtime::{LinkKind, RuntimeInfo, runtime_info};
pub use session::{Optimization, Session, SessionOptions};
pub use tensor::{DType, Tensor, TensorData, TensorInfo};
#[cfg(feature = "vision")]
pub use vision::{DepthConvention, Face, VisionModels};
pub use worker::{RunEvent, RunHandle, RunSink};
