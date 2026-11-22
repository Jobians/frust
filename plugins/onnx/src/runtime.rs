//! The process-wide ONNX Runtime environment.
//!
//! Created once, lazily, by the first session or [`runtime_info`] call, and kept for the life of
//! the process. The outcome is cached either way, so a failed creation is reported to every
//! caller instead of being retried or panicking.

use std::sync::OnceLock;

use ort::environment::Environment;

use crate::OnnxError;

/// How the ONNX Runtime library is linked into this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// Linked statically from the prebuilt archive fetched at build time.
    Static,
    /// Loaded from a shared library at run time.
    Dynamic,
}

/// What [`runtime_info`] reports about the runtime in use.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuntimeInfo {
    /// The ONNX Runtime release, e.g. `1.31.0`, as its build string reports it; the whole build
    /// string when it names no release.
    pub ort_version: String,
    /// How the runtime is linked.
    pub link: LinkKind,
}

/// Creates the environment if needed and describes the runtime behind it.
pub fn runtime_info() -> Result<RuntimeInfo, OnnxError> {
    environment()?;
    Ok(RuntimeInfo {
        ort_version: release_from_build_info(ort::info()),
        link: LINK,
    })
}

/// The shared environment, created on first use; the cached error if creation failed.
pub(crate) fn environment() -> Result<&'static Environment, OnnxError> {
    static ENVIRONMENT: OnceLock<Result<Environment, OnnxError>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            let built = create();
            if let Err(err) = &built {
                log::warn!("frust-onnx: {err}");
            }
            built
        })
        .as_ref()
        .map_err(Clone::clone)
}

// These two cfg sets mirror the `ort` target rows in this crate's Cargo.toml: the static rows enable
// `download-binaries`, every other row `load-dynamic`.
#[cfg(any(
    all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
    all(
        target_os = "windows",
        target_env = "msvc",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
mod link {
    use super::*;

    pub(super) const LINK: LinkKind = LinkKind::Static;

    pub(super) fn builder() -> Result<ort::environment::EnvironmentBuilder, OnnxError> {
        Ok(ort::init())
    }
}

#[cfg(not(any(
    all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
    all(
        target_os = "windows",
        target_env = "msvc",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod link {
    use super::*;

    pub(super) const LINK: LinkKind = LinkKind::Dynamic;

    /// Loads the library through `ort::init_from`, which reports a missing or too-old library as
    /// an error; letting `ort` load it implicitly on first use would panic instead.
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    pub(super) fn builder() -> Result<ort::environment::EnvironmentBuilder, OnnxError> {
        let path = std::env::var_os("ORT_DYLIB_PATH")
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let consts = (std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
                format!("{}onnxruntime{}", consts.0, consts.1).into()
            });
        ort::init_from(&path).map_err(|err| OnnxError::Runtime(err.to_string()))
    }

    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    pub(super) fn builder() -> Result<ort::environment::EnvironmentBuilder, OnnxError> {
        Err(OnnxError::Unsupported(
            "ONNX Runtime is not available on this target".to_owned(),
        ))
    }
}

use link::LINK;

fn create() -> Result<Environment, OnnxError> {
    link::builder()?
        .with_name("frust-onnx")
        .with_telemetry(false)
        .build()
        .map_err(|err| OnnxError::Runtime(err.to_string()))
}

/// `1.31.0` from a build string carrying `git-branch=rel-1.31.0`; the trimmed string otherwise.
fn release_from_build_info(info: &str) -> String {
    info.split([',', ' '])
        .find_map(|part| part.trim().strip_prefix("git-branch=rel-"))
        .filter(|release| !release.is_empty())
        .map_or_else(|| info.trim().to_owned(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::release_from_build_info;

    #[test]
    fn release_is_read_from_the_branch_tag() {
        let info =
            "ORT Build Info: git-branch=rel-1.31.0, git-commit-id=abc1234, build type=Release";
        assert_eq!(release_from_build_info(info), "1.31.0");
    }

    #[test]
    fn untagged_build_info_is_returned_whole() {
        assert_eq!(release_from_build_info("  custom build  "), "custom build");
    }
}
