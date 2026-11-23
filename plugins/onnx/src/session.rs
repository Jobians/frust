//! A loaded model, the options it was loaded with, and its blocking run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ort::ep::CPU;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::{RunOptions, SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Outlet, TensorElementType, TensorRef, ValueType};

use crate::accelerator::{self, Accelerator, Provider};
use crate::runtime::environment;
use crate::worker::Worker;
use crate::{DType, OnnxError, Tensor, TensorData, TensorInfo};

/// Upper bound of the default intra-op thread count.
const DEFAULT_MAX_THREADS: usize = 4;

/// How much graph optimisation ONNX Runtime applies while loading a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Optimization {
    /// Semantics-preserving rewrites only: constant folding, redundant-node removal, simple
    /// fusions.
    Basic,
    /// `Basic` plus complex node fusions.
    Extended,
    /// Everything, memory-layout optimisations included. The runtime's own default.
    #[default]
    All,
}

/// How a session is loaded and run. Compare and hash them: [`SessionCache`](crate::SessionCache)
/// keys on them.
///
/// Build one from [`SessionOptions::default`] with the `with_*` methods, or set the fields. The
/// memory defaults (arena and memory pattern off, unlike ONNX Runtime's own) trade a few percent
/// of speed for a much smaller footprint; the README's "Sessions and accelerators" section
/// records what each option costs and saves.
///
/// [`accelerator`](Self::accelerator) left at `Auto` means CoreML on Apple targets only for a
/// model whose inputs are all fully static; a dynamic-shape model loads on the CPU, because
/// CoreML measured 3 times slower and about 10 times larger on one (see [`Accelerator::Auto`]).
/// Setting `Accelerator::CoreMl` forces CoreML regardless, at the caller's risk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionOptions {
    /// The provider to ask for. Default [`Accelerator::Auto`].
    pub accelerator: Accelerator,
    /// When set, an explicitly chosen accelerator (anything but `Auto`) that cannot be used is
    /// [`OnnxError::Unsupported`] instead of a fall back to the CPU. Default `false`.
    pub strict_accelerator: bool,
    /// Threads one run may use, the calling one included; `None` is `min(4, available
    /// parallelism)`, and `Some(0)` counts as 1.
    pub intra_threads: Option<usize>,
    /// Graph optimisation level. Default [`Optimization::All`].
    pub optimization: Optimization,
    /// ONNX Runtime's CPU memory arena: allocations are carved from large blocks it keeps for
    /// the session's life, so every session holds its own peak. Off by default: without it a
    /// run's buffers go back to the system allocator when the run ends, for a few percent more
    /// time per run.
    pub cpu_arena: bool,
    /// Pre-plan one allocation for a run's intermediate tensors from the first run's shapes.
    /// Off by default: with the arena it holds that whole plan, rounded up, for every later run.
    pub memory_pattern: bool,
    /// Give the arena's unused blocks back after every run. Only meaningful with
    /// [`cpu_arena`](Self::cpu_arena) on. Default `false`.
    pub arena_shrinkage: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            accelerator: Accelerator::Auto,
            strict_accelerator: false,
            intra_threads: None,
            optimization: Optimization::All,
            cpu_arena: false,
            memory_pattern: false,
            arena_shrinkage: false,
        }
    }
}

impl SessionOptions {
    /// Sets [`accelerator`](Self::accelerator).
    #[must_use]
    pub fn with_accelerator(mut self, accelerator: Accelerator) -> Self {
        self.accelerator = accelerator;
        self
    }

    /// Sets [`strict_accelerator`](Self::strict_accelerator).
    #[must_use]
    pub fn with_strict_accelerator(mut self, strict: bool) -> Self {
        self.strict_accelerator = strict;
        self
    }

    /// Sets [`intra_threads`](Self::intra_threads) to `Some(threads)`.
    #[must_use]
    pub fn with_intra_threads(mut self, threads: usize) -> Self {
        self.intra_threads = Some(threads);
        self
    }

    /// Sets [`optimization`](Self::optimization).
    #[must_use]
    pub fn with_optimization(mut self, optimization: Optimization) -> Self {
        self.optimization = optimization;
        self
    }

    /// Sets [`cpu_arena`](Self::cpu_arena).
    #[must_use]
    pub fn with_cpu_arena(mut self, enable: bool) -> Self {
        self.cpu_arena = enable;
        self
    }

    /// Sets [`memory_pattern`](Self::memory_pattern).
    #[must_use]
    pub fn with_memory_pattern(mut self, enable: bool) -> Self {
        self.memory_pattern = enable;
        self
    }

    /// Sets [`arena_shrinkage`](Self::arena_shrinkage).
    #[must_use]
    pub fn with_arena_shrinkage(mut self, enable: bool) -> Self {
        self.arena_shrinkage = enable;
        self
    }

    /// The intra-op thread count a session loaded with these options uses.
    pub fn resolved_intra_threads(&self) -> usize {
        self.intra_threads.map_or_else(
            || {
                std::thread::available_parallelism()
                    .map_or(1, |threads| threads.get().min(DEFAULT_MAX_THREADS))
            },
            |threads| threads.max(1),
        )
    }

    fn shrinks_arena(&self) -> bool {
        self.cpu_arena && self.arena_shrinkage
    }
}

/// A model loaded into the shared ONNX Runtime environment.
///
/// `Session` is `Send + Sync`: share it between threads behind an `Arc`. Runs on one session are
/// serialised internally (ONNX Runtime's own run is not safe to call concurrently); for parallel
/// inference, load one session per thread.
pub struct Session {
    inner: Mutex<ort::session::Session>,
    inputs: Vec<TensorInfo>,
    outputs: Vec<TensorInfo>,
    options: SessionOptions,
    provider: Provider,
    pub(crate) worker: Worker,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// Where a model comes from.
#[derive(Clone, Copy)]
enum Source<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl Source<'_> {
    fn path(self) -> Option<PathBuf> {
        match self {
            Source::File(path) => Some(path.to_path_buf()),
            Source::Bytes(_) => None,
        }
    }
}

impl Session {
    /// Loads the model at `path` (an `.onnx` or `.ort` file) with the default
    /// [`SessionOptions`].
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, OnnxError> {
        Self::from_file_with(path, &SessionOptions::default())
    }

    /// Loads the model at `path` with `options`.
    ///
    /// When the accelerator's provider is not in this build or the runtime refuses it (on
    /// registration or while preparing the model), the failure is logged at warn and the model
    /// is loaded on the CPU instead; [`provider`](Self::provider) reports which happened. With
    /// [`SessionOptions::strict_accelerator`] and an accelerator other than `Auto`, that case is
    /// [`OnnxError::Unsupported`] instead, while a model that fails on the CPU too is still
    /// [`OnnxError::Load`].
    pub fn from_file_with(
        path: impl AsRef<Path>,
        options: &SessionOptions,
    ) -> Result<Self, OnnxError> {
        Self::load(Source::File(path.as_ref()), options)
    }

    /// Loads a model from its serialized bytes with the default [`SessionOptions`]. The bytes are
    /// copied; the caller may drop them.
    pub fn from_bytes(model: &[u8]) -> Result<Self, OnnxError> {
        Self::from_bytes_with(model, &SessionOptions::default())
    }

    /// Loads a model from its serialized bytes with `options`, falling back as
    /// [`from_file_with`](Self::from_file_with) does. CoreML keeps no compiled-model cache for a
    /// model given as bytes.
    pub fn from_bytes_with(model: &[u8], options: &SessionOptions) -> Result<Self, OnnxError> {
        Self::load(Source::Bytes(model), options)
    }

    fn load(source: Source<'_>, options: &SessionOptions) -> Result<Self, OnnxError> {
        let load_error = |reason: String| OnnxError::Load {
            path: source.path(),
            reason,
        };
        let env = environment()?;
        let wanted = resolve_accelerator(env, source, options)?;
        let provider = wanted.provider();
        if provider == Provider::Cpu {
            let session = commit(env, source, options, Accelerator::Cpu).map_err(load_error)?;
            return Self::wrap(session, source, options, Provider::Cpu);
        }
        let refused = if provider.is_compiled_in() {
            match commit(env, source, options, wanted) {
                Ok(session) => return Self::wrap(session, source, options, provider),
                Err(reason) => reason,
            }
        } else {
            format!("the {provider} execution provider is not compiled into this build")
        };
        let on_cpu = commit(env, source, options, Accelerator::Cpu).map_err(load_error)?;
        if options.strict_accelerator && options.accelerator != Accelerator::Auto {
            return Err(OnnxError::Unsupported(format!(
                "the {provider} accelerator cannot run this model: {refused}"
            )));
        }
        log::warn!("frust-onnx: {provider} is unavailable ({refused}); running on the CPU");
        Self::wrap(on_cpu, source, options, Provider::Cpu)
    }

    fn wrap(
        session: ort::session::Session,
        source: Source<'_>,
        options: &SessionOptions,
        provider: Provider,
    ) -> Result<Self, OnnxError> {
        let inputs = session.inputs().iter().map(describe).collect();
        let outputs = session.outputs().iter().map(describe).collect();
        log::debug!(
            "frust-onnx: loaded {} on {provider} ({inputs:?} -> {outputs:?})",
            source.path().map_or_else(
                || "a model from memory".to_owned(),
                |p| p.display().to_string()
            )
        );
        Ok(Self {
            inner: Mutex::new(session),
            inputs,
            outputs,
            options: options.clone(),
            provider,
            worker: Worker::default(),
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

    /// The options the session was loaded with, as given (`Auto` and `None` unresolved).
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// The execution provider the session actually runs on: the one asked for, or
    /// [`Provider::Cpu`] after a fall back. For `Auto` on an Apple target that is CoreML only
    /// when every model input has a fully static shape, and the CPU otherwise: on an Apple M4 a
    /// dynamic-shape depth model ran 232 ms at 473 MB on the CPU against 728 ms at about
    /// 5.2 GB on CoreML. An explicit [`Accelerator::CoreMl`] is honoured on any model, at the
    /// caller's risk.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Runs the model on `inputs` (one entry per model input, by name, in any order) and returns
    /// every output, named, in declaration order.
    ///
    /// Blocks the calling thread until the run finishes, and waits first for any run already in
    /// progress on this session. Input names, element types, ranks and static dimensions are
    /// checked before the runtime is called; a mismatch is [`OnnxError::InvalidInput`]. For a run
    /// off the calling thread, see [`Session::run`].
    pub fn run_blocking(
        &self,
        inputs: &[(&str, Tensor)],
    ) -> Result<Vec<(String, Tensor)>, OnnxError> {
        let pairs: Vec<(&str, &Tensor)> = inputs.iter().map(|(name, t)| (*name, t)).collect();
        let run_options = if self.options.shrinks_arena() {
            Some(self.run_options()?)
        } else {
            None
        };
        self.run_pairs(&pairs, run_options.as_ref())
    }

    /// Fresh run options carrying the session's per-run settings.
    pub(crate) fn run_options(&self) -> Result<RunOptions, OnnxError> {
        let run_error = |err: ort::Error| OnnxError::Run(err.to_string());
        let mut run_options = RunOptions::new().map_err(run_error)?;
        if self.options.shrinks_arena() {
            run_options
                .set("memory.enable_memory_arena_shrinkage", "cpu:0")
                .map_err(run_error)?;
        }
        Ok(run_options)
    }

    /// The checked run behind [`run_blocking`](Self::run_blocking) and [`Session::run`].
    pub(crate) fn run_pairs(
        &self,
        inputs: &[(&str, &Tensor)],
        run_options: Option<&RunOptions>,
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
        let outputs = match run_options {
            Some(run_options) => session.run_with_options(values, run_options),
            None => session.run(values),
        }
        .map_err(|err| OnnxError::Run(err.to_string()))?;
        collect_outputs(outputs)
    }

    fn check_inputs(&self, inputs: &[(&str, &Tensor)]) -> Result<(), OnnxError> {
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

/// The accelerator to load with. Only `Auto` on an Apple target needs the model: its inputs are
/// read from a metadata-only load (CPU, one thread, graph optimisation off, dropped before the
/// real load), because CoreML is chosen only for fully static input shapes. Every other case
/// is decided without touching the model.
fn resolve_accelerator(
    env: &ort::environment::Environment,
    source: Source<'_>,
    options: &SessionOptions,
) -> Result<Accelerator, OnnxError> {
    if options.accelerator != Accelerator::Auto || !cfg!(target_vendor = "apple") {
        return Ok(options.accelerator.resolve());
    }
    let probe = SessionOptions {
        intra_threads: Some(1),
        optimization: Optimization::Basic,
        ..options.clone()
    };
    let session =
        commit(env, source, &probe, Accelerator::Cpu).map_err(|reason| OnnxError::Load {
            path: source.path(),
            reason,
        })?;
    let inputs: Vec<TensorInfo> = session.inputs().iter().map(describe).collect();
    drop(session);
    let resolved = Accelerator::Auto.resolve_for(&inputs);
    if resolved == Accelerator::Cpu {
        log::info!(
            "frust-onnx: the model has a dynamic input shape, so Accelerator::Auto runs it on the \
             CPU (CoreML is slower and far larger on dynamic shapes); ask for \
             Accelerator::CoreMl to force it"
        );
    }
    Ok(resolved)
}

/// Builds a session for `source` on `accelerator` (`Cpu` or a resolved provider), the runtime's
/// explanation as the error.
fn commit(
    env: &ort::environment::Environment,
    source: Source<'_>,
    options: &SessionOptions,
    accelerator: Accelerator,
) -> Result<ort::session::Session, String> {
    let mut builder = configure(env, options, accelerator, matches!(source, Source::File(_)))?;
    match source {
        Source::File(path) => builder.commit_from_file(path),
        Source::Bytes(model) => builder.commit_from_memory(model),
    }
    .map_err(|err| err.to_string())
}

fn configure(
    env: &ort::environment::Environment,
    options: &SessionOptions,
    accelerator: Accelerator,
    from_file: bool,
) -> Result<SessionBuilder, String> {
    let text = |err: ort::Error<SessionBuilder>| err.to_string();
    let mut builder = ort::session::Session::builder(env)
        .map_err(|err| err.to_string())?
        .with_optimization_level(match options.optimization {
            Optimization::Basic => GraphOptimizationLevel::Level1,
            Optimization::Extended => GraphOptimizationLevel::Level2,
            Optimization::All => GraphOptimizationLevel::All,
        })
        .map_err(text)?
        .with_intra_threads(options.resolved_intra_threads())
        .map_err(text)?
        .with_memory_pattern(options.memory_pattern)
        .map_err(text)?;
    if accelerator != Accelerator::Cpu {
        builder = accelerator::register(builder, accelerator, from_file)?;
    }
    // Registering the CPU provider only toggles the arena; it is always appended last.
    builder
        .with_execution_providers([CPU::default()
            .with_arena_allocator(options.cpu_arena)
            .build()
            .error_on_failure()])
        .map_err(text)
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
