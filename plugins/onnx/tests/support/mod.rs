//! Shared fixtures for the integration tests.

#![allow(dead_code)] // each test binary uses its own subset

use std::path::PathBuf;

use frust_onnx::{Session, Tensor};

/// `tests/fixtures/add_f32.onnx`: f32 inputs `a` and `b`, both `[1, 3]`; output `sum = a + b`.
pub fn add_f32_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/add_f32.onnx")
}

pub fn add_f32_bytes() -> Vec<u8> {
    std::fs::read(add_f32_path()).expect("the add_f32 fixture is readable")
}

pub fn add_f32_session() -> Session {
    Session::from_file(add_f32_path()).expect("the add_f32 fixture loads")
}

pub fn row(values: [f32; 3]) -> Tensor {
    Tensor::from_f32([1, 3], values.to_vec()).expect("a [1, 3] tensor holds three values")
}

/// Owned inputs for the add model, as `Session::run` takes them.
pub fn add_inputs(a: [f32; 3], b: [f32; 3]) -> Vec<(String, Tensor)> {
    vec![("a".to_owned(), row(a)), ("b".to_owned(), row(b))]
}

/// A fresh, empty model store under the system temp directory, unique to this process and `tag`.
#[cfg(feature = "catalog")]
pub fn empty_store(tag: &str) -> frust_models::ModelStore {
    let dir = std::env::temp_dir().join(format!("frust-onnx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    frust_models::ModelStore::with_root(dir).expect("a temp store opens")
}

/// A store holding every catalog model, downloaded on first use (about 32 MB) and kept between
/// runs. `FRUST_ONNX_MODEL_STORE` picks the directory; the default is under `target/`.
#[cfg(feature = "catalog")]
pub fn catalog_store() -> frust_models::ModelStore {
    use std::sync::{Mutex, PoisonError};

    use frust_onnx::catalog::VISION_CATALOG;

    static DOWNLOADS: Mutex<()> = Mutex::new(());
    let _one_at_a_time = DOWNLOADS.lock().unwrap_or_else(PoisonError::into_inner);
    let root = std::env::var_os("FRUST_ONNX_MODEL_STORE").map_or_else(
        || PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("frust-onnx-vision-models"),
        PathBuf::from,
    );
    let store = frust_models::ModelStore::with_root(root).expect("the model store opens");
    let hub = frust_models::HubClient::new().expect("an HTTPS client");
    for model in VISION_CATALOG {
        if store.get(&(model.id)()).is_some() {
            continue;
        }
        frust_models::Downloader::start(
            &hub,
            &store,
            model.download_request(),
            std::sync::Arc::new(|_| {}),
        )
        .and_then(|handle| handle.wait())
        .unwrap_or_else(|err| panic!("downloading {}: {err}", model.key));
    }
    store
}
