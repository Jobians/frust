//! Shared fixtures for the integration tests.

use std::path::PathBuf;

use frust_onnx::Session;

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
