//! A loaded model and its blocking run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ort::session::{SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Outlet, TensorElementType, TensorRef, ValueType};

use crate::runtime::environment;
use crate::{DType, OnnxError, Tensor, TensorData, TensorInfo};

/// A model loaded into the shared ONNX Runtime environment.
///
/// `Session` is `Send + Sync`: share it between threads behind an `Arc`. Runs on one session are
/// serialised internally (ONNX Runtime's own run is not safe to call concurrently); for parallel
/// inference, load one session per thread.
pub struct Session {
    inner: Mutex<ort::session::Session>,
    inputs: Vec<TensorInfo>,
    outputs: Vec<TensorInfo>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Loads the model at `path` (an `.onnx` or `.ort` file).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, OnnxError> {
        let path = path.as_ref();
        let load_error = |reason: String| OnnxError::Load {
            path: Some(path.to_path_buf()),
            reason,
        };
        let env = environment()?;
        let session = ort::session::Session::builder(env)
            .and_then(|mut builder| builder.commit_from_file(path))
            .map_err(|err| load_error(err.to_string()))?;
        Self::wrap(session, Some(path.to_path_buf()))
    }

    /// Loads a model from its serialized bytes. The bytes are copied; the caller may drop them.
    pub fn from_bytes(model: &[u8]) -> Result<Self, OnnxError> {
        let env = environment()?;
        let session = ort::session::Session::builder(env)
            .and_then(|mut builder| builder.commit_from_memory(model))
            .map_err(|err| OnnxError::Load {
                path: None,
                reason: err.to_string(),
            })?;
        Self::wrap(session, None)
    }

    fn wrap(session: ort::session::Session, path: Option<PathBuf>) -> Result<Self, OnnxError> {
        let inputs = session.inputs().iter().map(describe).collect();
        let outputs = session.outputs().iter().map(describe).collect();
        log::debug!(
            "frust-onnx: loaded {} ({inputs:?} -> {outputs:?})",
            path.as_deref().map_or_else(
                || "a model from memory".to_owned(),
                |p| p.display().to_string()
            )
        );
        Ok(Self {
            inner: Mutex::new(session),
            inputs,
            outputs,
        })
    }

    /// The model's inputs, in declaration order.
    pub fn inputs(&self) -> &[TensorInfo] {
        &self.inputs
    }

    /// The model's outputs, in declaration order.
    pub fn outputs(&self) -> &[TensorInfo] {
        &self.outputs
    }

    /// Runs the model on `inputs` (one entry per model input, by name, in any order) and returns
    /// every output, named, in declaration order.
    ///
    /// Blocks the calling thread until the run finishes, and waits first for any run already in
    /// progress on this session. Input names, element types, ranks and static dimensions are
    /// checked before the runtime is called; a mismatch is [`OnnxError::InvalidInput`].
    pub fn run_blocking(
        &self,
        inputs: &[(&str, Tensor)],
    ) -> Result<Vec<(String, Tensor)>, OnnxError> {
        self.check_inputs(inputs)?;
        let mut values: Vec<(&str, SessionInputValue<'_>)> = Vec::with_capacity(inputs.len());
        for (name, tensor) in inputs {
            values.push((
                name,
                borrow(tensor).map_err(|err| OnnxError::Run(err.to_string()))?,
            ));
        }
        let mut session = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session
            .run(values)
            .map_err(|err| OnnxError::Run(err.to_string()))?;
        collect_outputs(outputs)
    }

    fn check_inputs(&self, inputs: &[(&str, Tensor)]) -> Result<(), OnnxError> {
        let mut seen = HashSet::with_capacity(inputs.len());
        for (name, tensor) in inputs {
            let Some(info) = self.inputs.iter().find(|info| info.name == *name) else {
                return Err(OnnxError::InvalidInput(format!(
                    "the model has no input `{name}` (its inputs: {})",
                    self.input_names()
                )));
            };
            if !seen.insert(*name) {
                return Err(OnnxError::InvalidInput(format!(
                    "input `{name}` is given twice"
                )));
            }
            check_tensor(info, tensor)?;
        }
        if let Some(missing) = self
            .inputs
            .iter()
            .find(|info| !seen.contains(info.name.as_str()))
        {
            return Err(OnnxError::InvalidInput(format!(
                "input `{}` is missing (the model's inputs: {})",
                missing.name,
                self.input_names()
            )));
        }
        Ok(())
    }

    fn input_names(&self) -> String {
        let names: Vec<_> = self
            .inputs
            .iter()
            .map(|info| format!("`{}`", info.name))
            .collect();
        names.join(", ")
    }
}

fn check_tensor(info: &TensorInfo, tensor: &Tensor) -> Result<(), OnnxError> {
    let name = &info.name;
    let dtype = tensor.dtype();
    if dtype != info.dtype {
        return Err(OnnxError::InvalidInput(format!(
            "input `{name}` takes {} elements, not {dtype}",
            info.dtype
        )));
    }
    let shape = tensor.shape();
    if shape.len() != info.shape.len() {
        return Err(OnnxError::InvalidInput(format!(
            "input `{name}` takes rank {} (shape {}), not shape {shape:?}",
            info.shape.len(),
            declared(&info.shape)
        )));
    }
    let fits = info
        .shape
        .iter()
        .zip(shape)
        .all(|(want, got)| want.is_none_or(|want| want == *got));
    if !fits {
        return Err(OnnxError::InvalidInput(format!(
            "input `{name}` takes shape {}, not {shape:?}",
            declared(&info.shape)
        )));
    }
    Ok(())
}

/// `[1, ?, 3]`: a declared shape with dynamic dimensions as `?`.
fn declared(shape: &[Option<usize>]) -> String {
    let dims: Vec<_> = shape
        .iter()
        .map(|dim| dim.map_or_else(|| "?".to_owned(), |dim| dim.to_string()))
        .collect();
    format!("[{}]", dims.join(", "))
}

/// A zero-copy runtime view of `tensor`, borrowed for the run.
fn borrow(tensor: &Tensor) -> ort::Result<SessionInputValue<'_>> {
    let shape = tensor.shape();
    Ok(match tensor.data() {
        TensorData::F32(data) => TensorRef::from_array_view((shape, data.as_slice()))?.into(),
        TensorData::U8(data) => TensorRef::from_array_view((shape, data.as_slice()))?.into(),
        TensorData::I64(data) => TensorRef::from_array_view((shape, data.as_slice()))?.into(),
    })
}

fn collect_outputs(outputs: SessionOutputs<'_>) -> Result<Vec<(String, Tensor)>, OnnxError> {
    outputs
        .into_iter()
        .map(|(name, value)| Ok((name.to_owned(), copy_out(name, &value)?)))
        .collect()
}

fn copy_out(name: &str, value: &DynValue) -> Result<Tensor, OnnxError> {
    let run_error = |err: ort::Error| OnnxError::Run(format!("reading output `{name}`: {err}"));
    let ValueType::Tensor { ty, .. } = value.dtype() else {
        return Err(OnnxError::Unsupported(format!(
            "output `{name}` is not a tensor ({})",
            value.dtype()
        )));
    };
    let (shape, data) = match ty {
        TensorElementType::Float32 => {
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(run_error)?;
            (dims(shape)?, TensorData::F32(data.to_vec()))
        }
        TensorElementType::Uint8 => {
            let (shape, data) = value.try_extract_tensor::<u8>().map_err(run_error)?;
            (dims(shape)?, TensorData::U8(data.to_vec()))
        }
        TensorElementType::Int64 => {
            let (shape, data) = value.try_extract_tensor::<i64>().map_err(run_error)?;
            (dims(shape)?, TensorData::I64(data.to_vec()))
        }
        other => {
            return Err(OnnxError::Unsupported(format!(
                "output `{name}` has element type {other}, which frust-onnx cannot hold"
            )));
        }
    };
    Tensor::new(shape, data).map_err(|err| OnnxError::Run(err.to_string()))
}

/// A concrete runtime shape as `usize` dimensions; a run's outputs never carry a dynamic one.
fn dims(shape: &[i64]) -> Result<Vec<usize>, OnnxError> {
    shape
        .iter()
        .map(|&dim| {
            usize::try_from(dim).map_err(|_| {
                OnnxError::Run(format!("output shape {shape:?} has a negative dimension"))
            })
        })
        .collect()
}

fn describe(outlet: &Outlet) -> TensorInfo {
    let name = outlet.name().to_owned();
    match outlet.dtype() {
        ValueType::Tensor { ty, shape, .. } => {
            let shape = shape.iter().map(|&dim| usize::try_from(dim).ok()).collect();
            TensorInfo::new(name, shape, dtype_of(*ty))
        }
        other => TensorInfo::new(name, Vec::new(), DType::Other(other.to_string())),
    }
}

fn dtype_of(ty: TensorElementType) -> DType {
    match ty {
        TensorElementType::Float32 => DType::F32,
        TensorElementType::Uint8 => DType::U8,
        TensorElementType::Int64 => DType::I64,
        other => DType::Other(other.to_string()),
    }
}
