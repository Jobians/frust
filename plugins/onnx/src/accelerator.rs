//! Execution-provider choice: what a session asks for ([`Accelerator`]) and what it got
//! ([`Provider`]). An accelerator that cannot be used falls back to the CPU unless the session's
//! options make it strict; see `Session::from_file_with`.

use std::fmt;

use crate::{DType, TensorInfo};

/// The hardware a session asks to run on.
///
/// `Auto` picks per target and per model. On Apple targets it is CoreML (all compute units) only
/// when every model input has a fully static shape; a model with any dynamic input dimension
/// loads on the CPU provider, and the load logs why. Everywhere else `Auto` is the CPU, Android
/// included (the Android build of ONNX Runtime has no XNNPACK provider).
///
/// The rule exists because of what was measured on an Apple M4 (macOS 27, 640x480 input,
/// default options). Depth Anything V2 Small, whose input `pixel_values` is `[?, 3, ?, ?]`: the
/// CPU provider ran a warm median of 232 ms at 473 MB, CoreML (all units) 728 ms at 5.1 to
/// 5.3 GB. U2-NetP, static `[1, 3, 320, 320]`: the CPU 90 ms at 432 MB, CoreML 75 ms at 564 MB
/// (2.8 s on the first run). CoreML pays for dynamic shapes; it is only a win on static ones.
/// Asking for [`Accelerator::CoreMl`] explicitly still forces CoreML on a dynamic-shape model,
/// at the caller's risk of that time and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Accelerator {
    /// The best provider this target ships; never fails for want of one.
    #[default]
    Auto,
    /// ONNX Runtime's own CPU kernels.
    Cpu,
    /// Apple's CoreML (iOS 13+, macOS), on the given compute units. Nodes CoreML cannot run stay
    /// on the CPU.
    CoreMl(CoreMlUnits),
    /// XNNPACK. Not compiled into any build of this crate yet: asking for it falls back to the
    /// CPU, or is [`OnnxError::Unsupported`](crate::OnnxError::Unsupported) when strict.
    Xnnpack,
}

/// Which compute units CoreML may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CoreMlUnits {
    /// CPU, GPU and Neural Engine, as CoreML sees fit.
    #[default]
    All,
    /// CPU and GPU, never the Neural Engine.
    CpuAndGpu,
    /// CPU and Neural Engine, never the GPU.
    CpuAndNeuralEngine,
}

/// The execution provider a session actually runs on, reported by
/// [`Session::provider`](crate::Session::provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Provider {
    /// ONNX Runtime's CPU kernels.
    Cpu,
    /// CoreML, with the CPU taking the nodes CoreML does not support.
    CoreMl,
    /// XNNPACK, with the CPU taking the nodes XNNPACK does not support.
    Xnnpack,
}

impl Accelerator {
    /// What `Auto` stands for on this target for a model with static input shapes; any other
    /// value is returned unchanged. Use [`resolve_for`](Self::resolve_for) when the model's
    /// inputs are known.
    pub fn resolve(self) -> Accelerator {
        match self {
            Accelerator::Auto if cfg!(target_vendor = "apple") => {
                Accelerator::CoreMl(CoreMlUnits::All)
            }
            Accelerator::Auto => Accelerator::Cpu,
            other => other,
        }
    }

    /// Like [`resolve`](Self::resolve), but `Auto` becomes CoreML on an Apple target only when
    /// every one of `inputs` has a fully static shape (see the rule on [`Accelerator::Auto`]);
    /// otherwise it is the CPU. Any other value is returned unchanged.
    pub fn resolve_for(self, inputs: &[TensorInfo]) -> Accelerator {
        self.resolve_for_target(inputs, cfg!(target_vendor = "apple"))
    }

    fn resolve_for_target(self, inputs: &[TensorInfo], apple: bool) -> Accelerator {
        match self {
            Accelerator::Auto if apple && inputs.iter().all(is_static) => {
                Accelerator::CoreMl(CoreMlUnits::All)
            }
            Accelerator::Auto => Accelerator::Cpu,
            other => other,
        }
    }

    /// The provider this accelerator asks for, `Auto` resolved first.
    pub fn provider(self) -> Provider {
        match self.resolve() {
            Accelerator::CoreMl(_) => Provider::CoreMl,
            Accelerator::Xnnpack => Provider::Xnnpack,
            Accelerator::Auto | Accelerator::Cpu => Provider::Cpu,
        }
    }
}

impl Provider {
    /// Whether this build of the crate can register the provider at all. Even when it can, the
    /// ONNX Runtime library loaded at run time may lack it, which falls back like any other
    /// registration failure.
    pub fn is_compiled_in(self) -> bool {
        match self {
            Provider::Cpu => true,
            Provider::CoreMl => cfg!(target_vendor = "apple"),
            Provider::Xnnpack => false,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Provider::Cpu => "CPU",
            Provider::CoreMl => "CoreML",
            Provider::Xnnpack => "XNNPACK",
        })
    }
}

/// Whether `info` is a tensor whose every dimension is fixed.
fn is_static(info: &TensorInfo) -> bool {
    !matches!(info.dtype, DType::Other(_)) && info.shape.iter().all(Option::is_some)
}

/// Registers `accelerator`'s provider (never `Auto`, never `Cpu`) on `builder`, failing rather
/// than letting ONNX Runtime skip it quietly. `cache_models` allows a compiled-model cache, which
/// only a model loaded from a file can key.
pub(crate) fn register(
    builder: ort::session::builder::SessionBuilder,
    accelerator: Accelerator,
    cache_models: bool,
) -> Result<ort::session::builder::SessionBuilder, String> {
    match accelerator {
        #[cfg(target_vendor = "apple")]
        Accelerator::CoreMl(units) => apple::register(builder, units, cache_models),
        other => {
            let _ = (builder, cache_models);
            Err(format!(
                "the {} execution provider is not compiled into this build",
                other.provider()
            ))
        }
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use ort::ep::coreml::{ComputeUnits, CoreML};
    use ort::session::builder::SessionBuilder;

    use super::CoreMlUnits;

    pub(super) fn register(
        builder: SessionBuilder,
        units: CoreMlUnits,
        cache_models: bool,
    ) -> Result<SessionBuilder, String> {
        let mut coreml = CoreML::default().with_compute_units(match units {
            CoreMlUnits::All => ComputeUnits::All,
            CoreMlUnits::CpuAndGpu => ComputeUnits::CPUAndGPU,
            CoreMlUnits::CpuAndNeuralEngine => ComputeUnits::CPUAndNeuralEngine,
        });
        if cache_models && let Some(dir) = model_cache_dir() {
            coreml = coreml.with_model_cache_dir(dir.display());
        }
        builder
            .with_execution_providers([coreml.build().error_on_failure()])
            .map_err(|err| err.to_string())
    }

    /// `<cache_dir>/frust-onnx/coreml`, created; `None` (no cache) when there is no cache
    /// directory or it cannot be created.
    fn model_cache_dir() -> Option<std::path::PathBuf> {
        let dir = frust_paths::cache_dir()?.join("frust-onnx").join("coreml");
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(err) => {
                log::debug!(
                    "frust-onnx: no CoreML model cache at {}: {err}",
                    dir.display()
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(shape: &[Option<usize>]) -> TensorInfo {
        TensorInfo::new("x".to_owned(), shape.to_vec(), DType::F32)
    }

    #[test]
    fn auto_on_apple_takes_coreml_only_for_static_inputs() {
        let coreml = Accelerator::CoreMl(CoreMlUnits::All);
        let fixed = info(&[Some(1), Some(3), Some(320), Some(320)]);
        let dynamic = info(&[None, Some(3), None, None]);
        let batch_only = info(&[None, Some(3), Some(8), Some(8)]);
        assert_eq!(
            Accelerator::Auto.resolve_for_target(std::slice::from_ref(&fixed), true),
            coreml
        );
        assert_eq!(Accelerator::Auto.resolve_for_target(&[], true), coreml);
        assert_eq!(
            Accelerator::Auto.resolve_for_target(std::slice::from_ref(&dynamic), true),
            Accelerator::Cpu
        );
        assert_eq!(
            Accelerator::Auto.resolve_for_target(&[batch_only], true),
            Accelerator::Cpu
        );
        assert_eq!(
            Accelerator::Auto.resolve_for_target(&[fixed, dynamic], true),
            Accelerator::Cpu
        );
    }

    #[test]
    fn non_tensor_input_counts_as_dynamic() {
        let other = TensorInfo::new("seq".to_owned(), Vec::new(), DType::Other("seq".into()));
        assert_eq!(
            Accelerator::Auto.resolve_for_target(&[other], true),
            Accelerator::Cpu
        );
    }

    #[test]
    fn auto_off_apple_is_always_cpu() {
        let fixed = info(&[Some(1), Some(3)]);
        assert_eq!(
            Accelerator::Auto.resolve_for_target(&[fixed], false),
            Accelerator::Cpu
        );
    }

    #[test]
    fn explicit_choices_are_untouched() {
        let dynamic = info(&[None]);
        let coreml = Accelerator::CoreMl(CoreMlUnits::CpuAndGpu);
        for apple in [true, false] {
            assert_eq!(
                coreml.resolve_for_target(std::slice::from_ref(&dynamic), apple),
                coreml
            );
            assert_eq!(
                Accelerator::Cpu.resolve_for_target(std::slice::from_ref(&dynamic), apple),
                Accelerator::Cpu
            );
        }
    }
}
